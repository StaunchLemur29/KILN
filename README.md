# KILN

An analog video emulator for macOS. KILN runs a video, a still picture, a camera or a stream
through a simulated composite signal, tape transport and CRT screen, and exports the result to
H.264, HEVC, ProRes 422 or ProRes 4444.

KILN is an independent project. It isn't affiliated with, endorsed by or connected to any other
analog-video product or its makers.

## Build

Needs Rust (stable) and FFmpeg (`brew install ffmpeg`).

```sh
cargo build --release          # the program: target/release/kiln
bash package/make_app.sh       # KILN.app and an installer disk image in dist/
```

`make_app.sh` copies FFmpeg and the libraries it uses into the app, so the finished app runs
on Macs without Homebrew.

## Render from the command line

```sh
kiln --headless-export input.mp4 output.mov "PRESET=My Look" HI prores4444
```

After the two files, any of: a preset name or `PRESET=<name>`, `SD` / `HI` / `SOURCE`
(quality), `DISPLAY=0`–`4` (screen type) and a format (`h264`, `hevc`, `prores422`,
`prores4444`). Later words win. Exit codes: 0 done, 1 render failed, 64 wrong usage, 79 preset
not found. Inside the app the program is `KILN.app/Contents/MacOS/KILN`.

## Licences

- KILN's own code: choose a licence before publishing (none is granted yet).
- Fonts in `assets/fonts`: SIL Open Font License (licence files beside them).
- FFmpeg (bundled in built apps only): LGPL/GPL, depending on how it was built. A Homebrew
  build includes GPL parts (x264, x265), so anyone you give a built app to must also be able to
  get FFmpeg's source. The licence texts go into the app under `Contents/Resources/Licenses`.
