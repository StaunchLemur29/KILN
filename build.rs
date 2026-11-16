fn main() {
    let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());

    // Factory looks: a private presets.json beside this file when there is one (it is kept out of
    // the public repository), otherwise KILN's own presets-kiln.json. KILN_PUBLIC_PRESETS=1 forces
    // the public set, to try what a fresh copy of the repository builds.
    println!("cargo:rerun-if-changed=presets.json");
    println!("cargo:rerun-if-changed=presets-kiln.json");
    println!("cargo:rerun-if-env-changed=KILN_PUBLIC_PRESETS");
    let private = dir.join("presets.json");
    let public = std::env::var("KILN_PUBLIC_PRESETS").is_ok_and(|v| v == "1");
    let chosen = if private.exists() && !public { private } else { dir.join("presets-kiln.json") };
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("factory-presets.json");
    std::fs::copy(&chosen, out).expect("factory presets");

    // Embed Info.plist in the program on macOS, so the system knows why KILN asks for the camera
    // (NSCameraUsageDescription) even when it runs as a plain program rather than an .app bundle.
    println!("cargo:rerun-if-changed=Info.plist");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let plist = dir.join("Info.plist");
        println!("cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{}", plist.display());
    }
}
