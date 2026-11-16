//! KILN — an analog video emulator: composite signal, tape damage and picture-tube looks
//! for videos and pictures, with presets, knobs and clean exports.

mod fx;
mod headless;
mod media;
mod params;
mod rx;
mod store;
mod ui;

use eframe::egui;
use std::path::PathBuf;

/// macOS asks about camera access on behalf of the "responsible" app: started from a terminal or
/// another app, that is the starter, which may have no reason (or no right) to use the camera, so
/// the request fails quietly. Re-executing KILN once with responsibility disclaimed makes KILN
/// answer for itself, so the system asks the person about KILN using the reason in its embedded
/// Info.plist. Same process id, same arguments; skipped for screenshot runs.
#[cfg(target_os = "macos")]
fn become_responsible_for_itself() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    const FLAG: &str = "KILN_SELF_RESPONSIBLE";
    if std::env::var_os(FLAG).is_some() {
        return;
    }
    unsafe extern "C" {
        fn responsibility_spawnattrs_setdisclaim(attrs: *mut libc::posix_spawnattr_t, disclaim: libc::c_int) -> libc::c_int;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let Ok(path) = CString::new(exe.as_os_str().as_bytes()) else { return };
    let argv: Vec<CString> = std::env::args_os().filter_map(|a| CString::new(a.as_bytes()).ok()).collect();
    let mut envv: Vec<CString> = std::env::vars_os()
        .filter_map(|(k, v)| {
            let mut kv = k.as_bytes().to_vec();
            kv.push(b'=');
            kv.extend_from_slice(v.as_bytes());
            CString::new(kv).ok()
        })
        .collect();
    envv.push(CString::new(format!("{FLAG}=1")).unwrap());
    let mut argp: Vec<*mut libc::c_char> = argv.iter().map(|a| a.as_ptr() as *mut _).collect();
    argp.push(std::ptr::null_mut());
    let mut envp: Vec<*mut libc::c_char> = envv.iter().map(|a| a.as_ptr() as *mut _).collect();
    envp.push(std::ptr::null_mut());
    unsafe {
        let mut attr: libc::posix_spawnattr_t = std::ptr::null_mut();
        if libc::posix_spawnattr_init(&mut attr) != 0 {
            return;
        }
        let ok = libc::posix_spawnattr_setflags(&mut attr, libc::POSIX_SPAWN_SETEXEC as libc::c_short) == 0
            && responsibility_spawnattrs_setdisclaim(&mut attr, 1) == 0;
        if ok {
            let mut pid: libc::pid_t = 0;
            // replaces this process on success and does not return
            libc::posix_spawn(&mut pid, path.as_ptr(), std::ptr::null(), &attr, argp.as_ptr(), envp.as_ptr());
        }
        libc::posix_spawnattr_destroy(&mut attr);
    }
}

/// Started from KILN.app, macOS already treats KILN as the app asking for the camera.
#[cfg(target_os = "macos")]
fn in_app_bundle() -> bool {
    std::env::current_exe().map(|e| e.to_string_lossy().contains(".app/Contents/MacOS/")).unwrap_or(false)
}

fn window_icon() -> egui::IconData {
    let img = image::load_from_memory(include_bytes!("../assets/icon-256.png")).map(|i| i.to_rgba8()).unwrap_or_default();
    let (width, height) = img.dimensions();
    egui::IconData { rgba: img.into_raw(), width, height }
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == headless::FLAG) {
        headless::run(&args);
    }
    #[cfg(target_os = "macos")]
    if !args.iter().any(|a| a == "--screenshot") && !in_app_bundle() {
        become_responsible_for_itself();
    }
    let mut screenshot: Option<PathBuf> = None;
    let mut open: Option<PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--screenshot" && i + 1 < args.len() {
            screenshot = Some(PathBuf::from(&args[i + 1]));
            i += 2;
        } else {
            if !args[i].starts_with('-') {
                open = Some(PathBuf::from(&args[i]));
            }
            i += 1;
        }
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("KILN — analog video emulator")
        .with_inner_size([1440.0, 900.0])
        .with_min_inner_size([1000.0, 640.0])
        .with_app_id("app.kiln.analog-video-emulator")
        .with_icon(std::sync::Arc::new(window_icon()));
    let mut options = eframe::NativeOptions::default();
    if screenshot.is_some() {
        viewport = viewport.with_active(false);
        #[cfg(target_os = "windows")]
        {
            viewport = viewport.with_window_level(egui::WindowLevel::AlwaysOnBottom);
        }
        #[cfg(target_os = "macos")]
        {
            options.event_loop_builder = Some(Box::new(|builder| {
                use winit::platform::macos::EventLoopBuilderExtMacOS;
                builder.with_activate_ignoring_other_apps(false);
            }));
        }
    }
    options.viewport = viewport;
    options.run_and_return = true;

    eframe::run_native("KILN", options, Box::new(move |cc| Ok(Box::new(ui::App::new(cc, screenshot, open)))))
}
