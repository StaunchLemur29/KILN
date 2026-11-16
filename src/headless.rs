//! Rendering a file without opening the window, following the original's command-line export
//! (MECHANICS.md §14):
//!
//!     kiln --headless-export <source> <destination> [options...]
//!
//! Options are read in order, ignoring case (a later one wins): a preset name or PRESET=<name>
//! picks the whole look, quality included; SD,
//! HI or SOURCE set the quality; DISPLAY=<0-4> picks the screen type; h264/avc, hevc/h265,
//! prores422/pr422/prores or prores4444/pr4444 pick the file format (dots, dashes, underscores
//! and spaces are ignored). Words starting with "-" and anything unrecognised are skipped.

use crate::media::{self, Format, Source};
use crate::params::{self, Params, Tier};
use crate::store;
use std::path::PathBuf;
use std::sync::Arc;

pub const FLAG: &str = "--headless-export";

/// Exit codes, as the original uses them: 64 wrong usage, 79 preset not found.
const EX_USAGE: i32 = 64;
const EX_NOPRESET: i32 = 79;
const EX_FAILED: i32 = 1;

/// ExportFormat.parse: lower-case, drop '.', '-', '_' and ' ', then match the known names.
pub fn parse_format(word: &str) -> Option<Format> {
    let w: String = word.to_lowercase().chars().filter(|c| !matches!(c, '.' | '-' | '_' | ' ')).collect();
    match w.as_str() {
        "h264" | "avc" => Some(Format::H264),
        "hevc" | "h265" => Some(Format::Hevc),
        "prores422" | "pr422" | "prores" => Some(Format::ProRes422),
        "prores4444" | "pr4444" => Some(Format::ProRes4444),
        _ => None,
    }
}

/// What the options ask for.
pub struct Settings {
    pub params: Params,
    pub format: Format,
}

/// Case-insensitive name match (the original compares with case and accent folding).
fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// Apply the options in order. Returns the preset name that wasn't found, if any.
pub fn apply_options(options: &[String], presets: &[params::Preset], start: Params) -> Result<Settings, String> {
    let mut s = Settings { params: start, format: Format::ProRes422 };
    for word in options {
        if word.starts_with('-') {
            continue;
        }
        let upper = word.to_uppercase();
        if let Some(name) = upper.strip_prefix("PRESET=") {
            // the name keeps the case it was typed in for the message
            let typed = &word[7..];
            match presets.iter().find(|p| same_name(&p.name, name)) {
                Some(p) => s.params = p.params.clone(),
                None => return Err(typed.to_string()),
            }
        } else if upper == "SOURCE" {
            s.params.tier = Tier::Full;
        } else if upper == "HI" {
            s.params.tier = Tier::Hi;
        } else if upper == "SD" {
            s.params.tier = Tier::Sd;
        } else if let Some(n) = upper.strip_prefix("DISPLAY=").and_then(|n| n.trim().parse::<f64>().ok()) {
            // rounded, then held to the five screen types 0..=4
            let n = n.round();
            let n = if n.is_nan() || n <= 0.0 { 0.0 } else { n.min(4.0) };
            s.params.v[params::idx("displayType")] = n as f32;
        } else if let Some(p) = presets.iter().find(|p| p.factory && same_name(&p.name, &upper)) {
            s.params = p.params.clone();
        } else if let Some(f) = parse_format(word) {
            s.format = f;
        }
    }
    Ok(s)
}

fn usage() -> ! {
    eprint!("Usage: kiln --headless-export <source> <destination> [preset] [format] [SD|HI|SOURCE] [DISPLAY=0-4]\n");
    std::process::exit(EX_USAGE);
}

/// Run the export and exit with its status. `args` is the whole command line.
pub fn run(args: &[String]) -> ! {
    let Some(at) = args.iter().position(|a| a == FLAG) else { usage() };
    let (Some(src), Some(dst)) = (args.get(at + 1), args.get(at + 2)) else { usage() };
    if src.starts_with("--") || dst.starts_with("--") {
        usage();
    }

    let mut presets = params::factory_presets();
    presets.extend(store::Store::default().load().presets);
    let start = presets.iter().find(|p| p.factory).map(|p| p.params.clone()).unwrap_or_default();
    let settings = match apply_options(&args[at + 3..], &presets, start) {
        Ok(s) => s,
        Err(name) => {
            eprintln!("KILN preset not found: {name}");
            std::process::exit(EX_NOPRESET);
        }
    };

    let src = PathBuf::from(src);
    let source = if media::is_image(&src) {
        match media::load_image(&src) {
            Ok(img) => Source::Image { img: Arc::new(img), path: src.clone() },
            Err(e) => fail(&e),
        }
    } else {
        match media::probe(&src) {
            Ok(info) => Source::Video { path: src.clone(), info },
            Err(e) => fail(&e),
        }
    };
    let mut out = PathBuf::from(dst);
    if out.extension().is_none() {
        out.set_extension(settings.format.ext());
    }

    let job = media::start_export(&source, settings.params, Vec::new(), settings.format, out, 5.0);
    let mut shown = -1;
    loop {
        if let Some(r) = job.result.lock().unwrap().take() {
            eprintln!();
            match r {
                Ok(p) => {
                    println!("{}", p.display());
                    std::process::exit(0);
                }
                Err(e) => fail(&e),
            }
        }
        let pct = (job.fraction() * 100.0) as i32;
        if pct != shown {
            eprint!("\rrendering {pct:3}%");
            shown = pct;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("KILN export failed: {msg}");
    std::process::exit(EX_FAILED);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_parse_like_the_original() {
        assert!(parse_format("H.264") == Some(Format::H264));
        assert!(parse_format("avc") == Some(Format::H264));
        assert!(parse_format("h-265") == Some(Format::Hevc));
        assert!(parse_format("ProRes 422") == Some(Format::ProRes422));
        assert!(parse_format("prores") == Some(Format::ProRes422));
        assert!(parse_format("PR_4444") == Some(Format::ProRes4444));
        assert!(parse_format("webm").is_none());
    }

    #[test]
    fn options_apply_in_order() {
        let presets = params::factory_presets();
        let first = presets[0].name.clone();
        let words: Vec<String> = ["--quiet", &first.to_lowercase(), "HI", "display=7.4", "hevc", "nonsense"].iter().map(|s| s.to_string()).collect();
        let s = apply_options(&words, &presets, Params::default()).unwrap();
        assert!(s.params.tier == Tier::Hi);
        assert!(s.format == Format::Hevc);
        assert_eq!(s.params.get("displayType"), 4.0);
        let missing = vec!["PRESET=No Such Look".to_string()];
        assert_eq!(apply_options(&missing, &presets, Params::default()).err().as_deref(), Some("No Such Look"));
        let neg = vec!["DISPLAY=-3".to_string()];
        assert_eq!(apply_options(&neg, &presets, Params::default()).unwrap().params.get("displayType"), 0.0);
    }
}
