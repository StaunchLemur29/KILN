"""Copy ffmpeg and ffprobe into KILN.app with every library they need, so the app runs on a Mac
without Homebrew. Libraries go to Contents/Frameworks and every reference is rewritten to
@executable_path/../Frameworks/<name>.

usage: python3 -I bundle_ffmpeg.py <KILN.app> <ffmpeg> <ffprobe>
"""
import os
import shutil
import subprocess
import sys

app, tools = sys.argv[1], sys.argv[2:]
macos = os.path.join(app, "Contents", "MacOS")
frameworks = os.path.join(app, "Contents", "Frameworks")
os.makedirs(frameworks, exist_ok=True)


def run(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True).stdout


def rpaths(path):
    out, lines = [], run("otool", "-l", path).splitlines()
    for i, line in enumerate(lines):
        if line.strip() == "cmd LC_RPATH":
            for j in range(i + 1, min(i + 4, len(lines))):
                parts = lines[j].split()
                if parts and parts[0] == "path":
                    out.append(parts[1])
    return out


def deps(path):
    """(reference as written, real file) for each non-system library the file links."""
    real_dir = os.path.dirname(os.path.realpath(path))
    out = []
    for line in run("otool", "-L", path).splitlines()[1:]:
        ref = line.strip().split(" (")[0]
        if ref.startswith(("/System/", "/usr/lib/")):
            continue
        if ref.startswith("@loader_path/"):
            cand = [os.path.join(real_dir, ref[len("@loader_path/"):])]
        elif ref.startswith("@rpath/"):
            cand = []
            for rp in rpaths(path):
                rp = rp.replace("@loader_path", real_dir).replace("@executable_path", real_dir)
                cand.append(os.path.join(rp, ref[len("@rpath/"):]))
        else:
            cand = [ref]
        found = next((c for c in cand if os.path.exists(c)), None)
        if found is None:
            sys.exit(f"cannot find {ref} needed by {path}")
        out.append((ref, os.path.realpath(found)))
    return out


copied = {}  # real source path -> bundled name
queue = []
for tool in tools:
    dest = os.path.join(macos, os.path.basename(tool))
    shutil.copy2(os.path.realpath(tool), dest)
    os.chmod(dest, 0o755)
    queue.append((dest, os.path.realpath(tool)))

while queue:
    bundled, original = queue.pop()
    for ref, real in deps(original):
        name = os.path.basename(real)
        if real not in copied:
            copied[real] = name
            dest = os.path.join(frameworks, name)
            shutil.copy2(real, dest)
            os.chmod(dest, 0o644)
            run("install_name_tool", "-id", f"@executable_path/../Frameworks/{name}", dest)
            queue.append((dest, real))
        run("install_name_tool", "-change", ref, f"@executable_path/../Frameworks/{name}", bundled)

# drop build-machine run paths so nothing is looked up outside the app
for f in [os.path.join(macos, os.path.basename(t)) for t in tools] + [os.path.join(frameworks, n) for n in copied.values()]:
    for rp in rpaths(f):
        subprocess.run(["install_name_tool", "-delete_rpath", rp, f], capture_output=True)

print(f"bundled {len(tools)} tools and {len(copied)} libraries")
