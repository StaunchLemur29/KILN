//! The person's own presets on disk, kept safely: every save is checked first, the previous file is
//! kept as a backup and as a dated snapshot (the newest 100 are kept), the new file is written
//! atomically and read back, and a failed save puts the previous file back. Loading falls back
//! to the backup and then the newest readable snapshot when the main file is damaged.

use crate::params::{self, Params, Preset};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const BANK: &str = "user-presets.json";
const BACKUP: &str = "user-presets.backup.json";
const SNAPSHOTS: &str = "Preset Backups";
const SNAPSHOT_PREFIX: &str = "user-presets-";
const CORRUPT_PREFIX: &str = "user-presets.corrupt-";
/// trimSnapshots deletes everything past the 100th newest snapshot
const KEEP_SNAPSHOTS: usize = 100;

pub struct Store {
    root: PathBuf,
}

impl Default for Store {
    fn default() -> Self {
        Store { root: params::support_dir() }
    }
}

/// What loading found, worded as the original's preset notices.
pub struct Loaded {
    pub presets: Vec<Preset>,
    pub notice: Option<&'static str>,
}

/// A fresh random id in UUID form, so presets keep their identity across renames.
pub fn new_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut words = [0u64; 2];
    for (i, w) in words.iter_mut().enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        h.write_usize(i);
        *w = h.finish();
    }
    let (a, b) = (words[0], words[1]);
    format!(
        "{:08X}-{:04X}-4{:03X}-{:04X}-{:012X}",
        a >> 32,
        (a >> 16) & 0xffff,
        a & 0xfff,
        0x8000 | ((b >> 48) & 0x3fff),
        b & 0xffff_ffff_ffff
    )
}

/// UTC time as ISO 8601, e.g. 2026-11-17T09:41:07Z.
fn iso_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil date from days since 1970-01-01
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem / 60 % 60, rem % 60)
}

/// validate: every preset needs a name once spaces are trimmed, ids are unique,
/// and every value must be a finite number.
pub fn validate(presets: &[Preset]) -> Result<(), String> {
    let mut ids = std::collections::HashSet::new();
    for p in presets.iter().filter(|p| !p.factory) {
        if p.name.trim().is_empty() {
            return Err("A preset has no name.".into());
        }
        if !ids.insert(p.id.as_str()) {
            return Err(format!("Duplicate preset id {}.", p.id));
        }
        if p.params.v.iter().any(|v| !v.is_finite()) {
            return Err(format!("{} contains a non-finite value.", p.name));
        }
    }
    Ok(())
}

fn preset_json(p: &Preset) -> Value {
    json!({ "id": p.id, "name": p.name, "values": p.params.to_values() })
}

/// decodeBank: the revision and presets of one bank file, or why it can't be used.
fn decode(path: &Path) -> Result<(u64, Vec<Preset>), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let root: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let list = root["presets"].as_array().ok_or("The preset bank is invalid. ")?;
    let mut out = Vec::new();
    for p in list {
        let name = p["name"].as_str().ok_or("The preset bank is invalid. ")?.to_string();
        let values = p["values"].as_object().ok_or("The preset bank is invalid. ")?;
        // banks written before ids were kept get one now
        let id = p["id"].as_str().map(str::to_string).unwrap_or_else(new_id);
        out.push(Preset { id, name, params: Params::from_values(values), factory: false, category: None, description: None });
    }
    validate(&out)?;
    Ok((root["revision"].as_u64().unwrap_or(0), out))
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

impl Store {
    #[cfg(test)]
    fn at(root: PathBuf) -> Store {
        Store { root }
    }

    fn bank(&self) -> PathBuf {
        self.root.join(BANK)
    }

    fn backup(&self) -> PathBuf {
        self.root.join(BACKUP)
    }

    fn snapshot_dir(&self) -> PathBuf {
        self.root.join(SNAPSHOTS)
    }

    /// Snapshots, newest first (their names start with the time they were taken).
    fn snapshots(&self) -> Vec<PathBuf> {
        let mut list: Vec<PathBuf> = std::fs::read_dir(self.snapshot_dir())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(SNAPSHOT_PREFIX) && n.ends_with(".json")))
            .collect();
        list.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        list
    }

    /// trimSnapshots: keep the newest 100.
    fn trim_snapshots(&self) {
        for old in self.snapshots().into_iter().skip(KEEP_SNAPSHOTS) {
            let _ = std::fs::remove_file(old);
        }
    }

    /// load with loadFirstValidBank: the main file, else the backup, else the newest
    /// readable snapshot. A damaged main file is set aside (never overwritten) before the
    /// recovered bank replaces it; with nothing readable, nothing is written.
    pub fn load(&self) -> Loaded {
        let bank = self.bank();
        if !bank.exists() && !self.backup().exists() && self.snapshots().is_empty() {
            return Loaded { presets: Vec::new(), notice: None };
        }
        match decode(&bank) {
            Ok((_, presets)) => Loaded { presets, notice: None },
            Err(_) => {
                let mut candidates = vec![self.backup()];
                candidates.extend(self.snapshots());
                let Some(presets) = candidates.iter().find_map(|c| decode(c).ok().map(|(_, p)| p)) else {
                    return Loaded { presets: Vec::new(), notice: Some("PRESET STORAGE ERROR") };
                };
                // restoreRecovered: keep the damaged file beside it, then write the recovered bank
                if bank.exists() {
                    let stamp = iso_now().replace(':', "-");
                    let _ = std::fs::rename(&bank, self.root.join(format!("{CORRUPT_PREFIX}{stamp}.json")));
                }
                let _ = self.write_bank(0, &presets);
                Loaded { presets, notice: Some("PRESETS RECOVERED FROM BACKUP") }
            }
        }
    }

    fn write_bank(&self, revision: u64, presets: &[Preset]) -> Result<(), String> {
        let list: Vec<Value> = presets.iter().filter(|p| !p.factory).map(preset_json).collect();
        let doc = json!({ "revision": revision, "savedAt": iso_now(), "presets": list });
        let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
        write_atomic(&self.bank(), &text).map_err(|e| e.to_string())?;
        // read back and compare before calling it saved
        match decode(&self.bank()) {
            Ok((rev, read)) if rev == revision && read.iter().map(preset_json).collect::<Vec<_>>() == list => Ok(()),
            Ok(_) => Err("Read-back verification did not match.".into()),
            Err(e) => Err(e),
        }
    }

    /// persist: validate; keep the previous bank as the backup and as a snapshot
    /// named user-presets-<time>-r<revision>-<id>.json; write revision + 1; read it back; on any
    /// failure put the previous bank back.
    pub fn save(&self, presets: &[Preset]) -> Result<(), String> {
        validate(presets)?;
        let fail = |e: String| format!("The preset could not be saved safely. The prior bank was preserved. {e}");
        std::fs::create_dir_all(self.snapshot_dir()).map_err(|e| fail(e.to_string()))?;
        let bank = self.bank();
        let had_primary = bank.exists();
        let mut revision = 0;
        if had_primary {
            // a bank that can't be read is never replaced here: load() recovers it first
            let (rev, _) = decode(&bank).map_err(|e| format!("The saved preset bank could not be read. Existing files were not overwritten. {e}"))?;
            revision = rev;
            std::fs::copy(&bank, self.backup()).map_err(|e| fail(e.to_string()))?;
            let stamp = iso_now().replace(':', "-");
            let snap = self.snapshot_dir().join(format!("{SNAPSHOT_PREFIX}{stamp}-r{rev}-{}.json", new_id()));
            std::fs::copy(&bank, snap).map_err(|e| fail(e.to_string()))?;
        }
        match self.write_bank(revision + 1, presets) {
            Ok(()) => {
                self.trim_snapshots();
                Ok(())
            }
            Err(e) => {
                let _ = std::fs::remove_file(&bank);
                if had_primary && std::fs::copy(self.backup(), &bank).is_err() {
                    return Err(format!("Preset save failed and automatic backup restoration also failed: {e}"));
                }
                Err(fail(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(name: &str, x: f32) -> Preset {
        let mut params = Params::default();
        params.v[0] = x;
        Preset { id: new_id(), name: name.into(), params, factory: false, category: None, description: None }
    }

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kiln-store-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_backs_up_and_recovers() {
        let root = temp_root("recover");
        let store = Store::at(root.clone());
        let a = vec![preset("ONE", 0.25)];
        store.save(&a).unwrap();
        let b = vec![preset("ONE", 0.25), preset("TWO", 0.5)];
        store.save(&b).unwrap();
        assert_eq!(store.load().presets.len(), 2);
        assert!(root.join(BACKUP).exists());
        assert_eq!(store.snapshots().len(), 1);
        // damage the main file: the backup (the first save) comes back
        std::fs::write(root.join(BANK), "{ not json").unwrap();
        let loaded = store.load();
        assert_eq!(loaded.notice, Some("PRESETS RECOVERED FROM BACKUP"));
        assert_eq!(loaded.presets.len(), 1);
        assert!(std::fs::read_dir(&root).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(CORRUPT_PREFIX)));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn validation_rules() {
        assert_eq!(validate(&[preset("   ", 0.0)]).unwrap_err(), "A preset has no name.");
        let p = preset("A", 0.0);
        let dup = Preset { id: p.id.clone(), name: "B".into(), params: Params::default(), factory: false, category: None, description: None };
        assert!(validate(&[p, dup]).unwrap_err().starts_with("Duplicate preset id"));
        assert!(validate(&[preset("BAD", f32::NAN)]).unwrap_err().ends_with("contains a non-finite value."));
    }

    #[test]
    fn keeps_the_newest_hundred_snapshots() {
        let root = temp_root("trim");
        let store = Store::at(root.clone());
        std::fs::create_dir_all(store.snapshot_dir()).unwrap();
        for i in 0..105 {
            std::fs::write(store.snapshot_dir().join(format!("{SNAPSHOT_PREFIX}2026-01-01T00-00-{i:03}Z-r{i}-X.json")), "{}").unwrap();
        }
        store.trim_snapshots();
        let left = store.snapshots();
        assert_eq!(left.len(), KEEP_SNAPSHOTS);
        assert!(left[0].to_string_lossy().contains("-r104-"));
        let _ = std::fs::remove_dir_all(root);
    }
}
