//! Where BlitzRaw keeps its own data, and how it makes sure that place works.
//!
//! # Why this exists
//!
//! Settings stopped reaching the disk. Not with an error: `fs::write` returned
//! success five times in one session while `settings.json` in
//! `AppData\Roaming\io.github.Guts501.BlitzRaw` kept a size and a timestamp
//! from six days earlier. Measured rather than assumed, and checked from
//! outside the application: another process could write a file into that same
//! folder and it persisted, so the folder was not read-only and the disk was
//! not full. Writes from this process, to that folder, reported success and
//! were undone.
//!
//! The same application writes a log file into `AppData\Local` every session
//! and has never lost one. So it is that one folder, not the machine, and
//! something between this process and the disk is putting it back. Ransomware
//! rollback in a security suite behaves exactly like that, and it does not
//! quarantine anything, which is why nothing showed up in a quarantine list.
//!
//! # What is done about it
//!
//! Rather than argue with whatever it is, the data folder is now **chosen** and
//! **proved** rather than assumed:
//!
//! 1. `BLITZRAW_DATA_DIR`, if it is set. The escape hatch: point it anywhere.
//! 2. `BlitzRaw-Data` beside the executable, if that folder already exists.
//!    Creating it is how you ask for a portable install; nothing creates it for
//!    you, so this never triggers by accident.
//! 3. The local app data directory, which is where the logs already go.
//! 4. The roaming app data directory, which is where everything used to go.
//!
//! Each candidate is tried in turn and has to **pass a probe**: a file is
//! written, read back, compared byte for byte and removed. A folder that
//! accepts a write and quietly discards it fails that and is skipped, which is
//! the exact failure this module exists for. A folder that merely does not
//! exist yet is created and then probed.
//!
//! The choice is made once and logged, so the log always says where the data of
//! that session went.
//!
//! # Moving in
//!
//! Whatever was in the old roaming folder is moved across the first time a new
//! folder is chosen: presets, albums, LUTs, the AI models and the settings.
//! Moved rather than copied, because a rename inside one volume is instant and
//! the models alone are 640 MB. Anything already present in the new folder is
//! left alone, so this cannot overwrite newer data with older.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tauri::{AppHandle, Manager};

/// Set once, so the answer cannot change under a running application.
static CHOSEN: OnceLock<PathBuf> = OnceLock::new();

/// What the choosing found, kept until there is a logger to say it to.
///
/// The data directory is settled before the logger exists, because the logger
/// reads settings to know where to write, and settings live in the directory
/// being chosen. So the one line that says where a session's data went was
/// written to a logger that was not listening and never appeared anywhere. That
/// is precisely the sort of missing line this whole feature has been paying for
/// all week, so the account is kept and said later instead.
static REPORT: OnceLock<Vec<(log::Level, String)>> = OnceLock::new();

/// Says where the data went, once there is somewhere to say it.
///
/// Call after the logger is installed. Safe to call more than once and safe to
/// call before anything has asked for the directory, in which case it asks.
pub fn report_choice(app_handle: &AppHandle) {
    let _ = data_dir(app_handle);
    for (level, line) in REPORT.get().map(Vec::as_slice).unwrap_or(&[]) {
        log::log!(*level, "{line}");
    }
}

/// The folder a portable install keeps its data in, beside the executable.
const PORTABLE_FOLDER: &str = "BlitzRaw-Data";

/// What this application was called before, in reverse domain form.
///
/// Tauri builds the data folder path out of the identifier in
/// `tauri.conf.json`, so renaming the application moves its folder and leaves
/// everything behind: the settings, the presets, the albums, the LUTs and the
/// 640 MB of AI models. The old identifiers are kept here and their folders are
/// emptied into whichever folder is chosen, so a rename costs nothing.
///
/// Add to this list, never edit it. A name taken out of here is a user whose
/// data stops arriving.
const LEGACY_IDENTIFIERS: &[&str] = &["io.github.Guts501.Darkroom"];

/// The old folders that still exist, beside the current ones.
///
/// `app_data_dir` and `app_local_data_dir` are both `<root>/<identifier>`, so
/// the old folder is the sibling named after the old identifier. Only folders
/// that are really there come back, so a fresh install looks at nothing.
fn legacy_folders(roaming: Option<&Path>, local: Option<&Path>) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    for root in [roaming, local].into_iter().flatten() {
        let Some(parent) = root.parent() else {
            continue;
        };
        for id in LEGACY_IDENTIFIERS {
            let old = parent.join(id);
            if old.is_dir() && !found.contains(&old) {
                found.push(old);
            }
        }
    }
    found
}

/// Where this session's data lives.
///
/// Everything that used to ask for `app_data_dir` or `app_config_dir` asks for
/// this instead, so there is one home rather than two that happen to be the
/// same folder on Windows and different ones elsewhere.
pub fn data_dir(app_handle: &AppHandle) -> PathBuf {
    CHOSEN.get_or_init(|| choose(app_handle)).clone()
}

/// A path inside the data folder.
pub fn data_path(app_handle: &AppHandle, name: &str) -> PathBuf {
    data_dir(app_handle).join(name)
}

fn choose(app_handle: &AppHandle) -> PathBuf {
    let mut report: Vec<(log::Level, String)> = Vec::new();
    let chosen = choose_and_describe(app_handle, &mut report);
    let _ = REPORT.set(report);
    chosen
}

fn choose_and_describe(app_handle: &AppHandle, report: &mut Vec<(log::Level, String)>) -> PathBuf {
    let roaming = app_handle.path().app_data_dir().ok();
    let local = app_handle.path().app_local_data_dir().ok();

    let mut candidates: Vec<(&str, PathBuf)> = Vec::new();

    if let Ok(from_env) = std::env::var("BLITZRAW_DATA_DIR")
        && !from_env.trim().is_empty()
    {
        candidates.push(("BLITZRAW_DATA_DIR", PathBuf::from(from_env)));
    }

    // Beside the executable, and only if somebody has already made the folder.
    // Making it is how a portable install is asked for; nothing here creates it,
    // so a normal install never quietly becomes a portable one.
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let portable = dir.join(PORTABLE_FOLDER);
        if portable.is_dir() {
            candidates.push(("portable", portable));
        }
    }

    if let Some(local) = local.clone() {
        candidates.push(("local app data", local));
    }
    if let Some(roaming) = roaming.clone() {
        candidates.push(("roaming app data", roaming));
    }

    for (why, candidate) in candidates {
        match probe(&candidate) {
            Ok(()) => {
                report.push((
                    log::Level::Info,
                    format!("BlitzRaw data directory: {} ({why})", candidate.display()),
                ));
                if let Some(old) = roaming.as_ref()
                    && old != &candidate
                {
                    move_in(old, &candidate, report);
                }
                // The application was called something else once. Whatever the
                // old name still holds is moved across here, so a rename never
                // costs a user their presets, their LUTs or the AI models.
                for old in legacy_folders(roaming.as_deref(), local.as_deref()) {
                    if old != candidate {
                        report.push((
                            log::Level::Info,
                            format!("Taking over the data left behind at {}", old.display()),
                        ));
                        move_in(&old, &candidate, report);
                    }
                }
                return candidate;
            }
            Err(e) => {
                report.push((
                    log::Level::Warn,
                    format!("Not using {} for data ({why}): {e}", candidate.display()),
                ));
            }
        }
    }

    // Nothing passed. Use the roaming folder anyway rather than refusing to
    // start: an application that will not open is worse than one that cannot
    // remember, and the log above says exactly what happened.
    let last_resort = roaming.unwrap_or_else(|| PathBuf::from("."));
    report.push((
        log::Level::Error,
        format!(
            "No usable data directory. Falling back to {} even though it failed its probe.",
            last_resort.display()
        ),
    ));
    last_resort
}

/// Writes a file, reads it back and removes it.
///
/// The read-back is the whole point. A folder that takes a write and reports
/// success while discarding it passes every ordinary check and is the reason
/// this module exists, so "can I create a file here" is not the question. The
/// question is whether what comes back is what went in.
fn probe(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create it: {e}"))?;

    let path = dir.join(".blitzraw-write-probe");
    let contents = "blitzraw probe";
    std::fs::write(&path, contents).map_err(|e| format!("cannot write in it: {e}"))?;

    let read_back =
        std::fs::read_to_string(&path).map_err(|e| format!("cannot read it back: {e}"))?;
    let _ = std::fs::remove_file(&path);

    if read_back != contents {
        return Err("a file written there does not read back as what was written".to_string());
    }
    Ok(())
}

/// Moves what was in the old folder into the new one, once.
///
/// Moved rather than copied: a rename inside one volume is instant, and the AI
/// models alone are 640 MB. Anything already in the new folder is left where it
/// is, so this can never put older data over newer.
fn move_in(old: &Path, new: &Path, report: &mut Vec<(log::Level, String)>) {
    let Ok(entries) = std::fs::read_dir(old) else {
        return;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let target = new.join(&name);
        if target.exists() {
            continue;
        }
        match std::fs::rename(entry.path(), &target) {
            Ok(()) => report.push((
                log::Level::Info,
                format!("Moved {} into the data directory", name.to_string_lossy()),
            )),
            Err(e) => report.push((
                log::Level::Warn,
                format!(
                    "Could not move {} into the data directory: {e}. It stays at {}.",
                    name.to_string_lossy(),
                    entry.path().display()
                ),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_that_keeps_what_it_is_given_passes() {
        let dir = std::env::temp_dir().join("blitzraw-probe-ok");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            probe(&dir).is_ok(),
            "an ordinary temporary folder should pass"
        );
        assert!(
            !dir.join(".blitzraw-write-probe").exists(),
            "and the probe should not be left behind"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_probe_creates_what_is_missing() {
        // A first run has no data folder at all, so a candidate that does not
        // exist yet has to be made rather than rejected.
        let dir = std::env::temp_dir().join("blitzraw-probe-new/deeper/still");
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("blitzraw-probe-new"));
        assert!(probe(&dir).is_ok());
        assert!(dir.is_dir());
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("blitzraw-probe-new"));
    }

    #[test]
    fn moving_in_never_overwrites_what_is_already_there() {
        let root = std::env::temp_dir().join("blitzraw-move-in");
        let _ = std::fs::remove_dir_all(&root);
        let old = root.join("old");
        let new = root.join("new");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();

        std::fs::write(old.join("settings.json"), "the old one").unwrap();
        std::fs::write(new.join("settings.json"), "the new one").unwrap();
        std::fs::write(old.join("presets.json"), "only in the old one").unwrap();

        let mut report = Vec::new();
        move_in(&old, &new, &mut report);

        assert_eq!(
            std::fs::read_to_string(new.join("settings.json")).unwrap(),
            "the new one",
            "a file already in the new folder is the one that counts"
        );
        assert_eq!(
            std::fs::read_to_string(new.join("presets.json")).unwrap(),
            "only in the old one",
            "and one that is only in the old folder comes across"
        );
        assert!(
            old.join("settings.json").exists(),
            "the one that did not come across is still where it was"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
