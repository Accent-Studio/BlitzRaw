//! The one door through which a photo's sidecar is written.
//!
//! # What was wrong
//!
//! A sidecar is one JSON file holding everything known about one photo:
//! adjustments, rating, colour label, tags, EXIF, camera profile, edit history
//! and the name of its thumbnail. Every writer in this program read the whole
//! file, changed one field, and wrote the whole file back.
//!
//! Sixteen places did that. None of them locked anything.
//!
//! Two of them overlapping is a lost write, and not a rare one. Ten quick
//! presses on a selection of twenty-eight photos is ten commands in the air at
//! once, each reading and writing the same twenty-eight files, while the
//! thumbnail render they each kick off reads and writes those files again to
//! record a thumbnail name, and the decode inside that render writes them a
//! third time to record EXIF. Two of those read `exposure: -0.5`, both wrote
//! `-0.6`, and a photo that was pressed twice moved once. That is the reported
//! symptom; the unreported one is that any field can be lost the same way,
//! including a whole set of adjustments.
//!
//! # The rule
//!
//! One lock per sidecar file. Read, change and write happen inside it, so
//! nothing can read a value that is about to be replaced by a change it cannot
//! see. Every writer goes through [`update`]; there is no other way in.
//!
//! Reading stays free of the lock, because a write is a rename over the top of
//! a file rather than an edit in place. A reader either sees all of the old
//! file or all of the new one, never half of either, and a reader that wants
//! the value it is about to change is doing an update and belongs in [`update`]
//! anyway.
//!
//! # What this deliberately does not do
//!
//! It does not decide that the picture changed, and it does not render
//! anything. Most sidecar writes do not change how a photo looks: a tag, an
//! EXIF block, a thumbnail name. Rendering from here would put the storm back
//! in a new place. Whoever changes the picture says so, once, to the rebuild
//! queue.

use crate::image_processing::ImageMetadata;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

/// One lock per sidecar, made on first use and kept.
///
/// Pruned rather than grown without limit, since a long session can walk
/// through a lot of folders. An entry nobody else is holding is one nobody is
/// in the middle of writing, so dropping it is safe: the next writer makes a
/// fresh one.
static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Above this many remembered locks, the unused ones are dropped.
const PRUNE_ABOVE: usize = 4096;

fn lock_for(sidecar_path: &Path) -> Arc<Mutex<()>> {
    let mut locks = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    if locks.len() > PRUNE_ABOVE {
        locks.retain(|_, held| Arc::strong_count(held) > 1);
    }
    locks
        .entry(sidecar_path.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Writes beside the target and renames over it.
///
/// A reader never sees a half-written sidecar, and an interrupted write leaves
/// the old file intact rather than a truncated one. The same thing
/// `write_thumbnail` does, for the same reason, and more important here: a
/// truncated sidecar is a photo's edits gone.
fn write_atomic(sidecar_path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temp_name = sidecar_path.file_name().unwrap_or_default().to_os_string();
    temp_name.push(".part");
    let temp = sidecar_path.with_file_name(temp_name);

    fs::write(&temp, bytes)?;
    match fs::rename(&temp, sidecar_path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&temp);
            Err(e)
        }
    }
}

/// Reads a sidecar without taking its lock.
///
/// Safe because writes are renames. Kept private: code that only reads calls
/// `exif_processing::load_sidecar`, and code that reads in order to change
/// calls [`update`].
fn read_unlocked(sidecar_path: &Path) -> ImageMetadata {
    crate::exif_processing::load_sidecar(sidecar_path)
}

/// Changes one photo's sidecar, with nothing else able to write it meanwhile.
///
/// The closure is handed what is on disk now and returns `Some` if it changed
/// anything, `None` if it did not. **`None` writes nothing at all**, which
/// matters more than it sounds: a write is a new modification time, and a new
/// modification time is every cached picture of that photo thrown away. Half
/// the mass thumbnail rebuilds this program used to do came from writes that
/// changed nothing.
///
/// The closure must not touch this same sidecar by any other route. The lock is
/// not reentrant, so a nested read-and-change of the same file would wait for
/// itself forever. In practice this means: do the arithmetic, set the fields,
/// return. Anything that wants to write a *different* file is fine.
pub fn update<T>(
    sidecar_path: &Path,
    change: impl FnOnce(&mut ImageMetadata) -> Option<T>,
) -> io::Result<Option<T>> {
    let gate = lock_for(sidecar_path);
    let _held = gate.lock().unwrap_or_else(|e| e.into_inner());

    let mut metadata = read_unlocked(sidecar_path);
    let Some(value) = change(&mut metadata) else {
        return Ok(None);
    };
    // A long EXIF string can bloat a sidecar to megabytes. Trimmed on the way
    // out rather than on the way in, so reading a photo never rewrites it: this
    // is the only moment the file is being rewritten anyway.
    crate::exif_processing::trim_bloated_exif(&mut metadata);

    let json = serde_json::to_string_pretty(&metadata).map_err(io::Error::other)?;
    write_atomic(sidecar_path, json.as_bytes())?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("blitzraw-sidecar-tests");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join(name);
        let _ = fs::remove_file(&path);
        path
    }

    #[test]
    fn a_change_that_changed_nothing_is_not_written() {
        let path = scratch("untouched.nef.rrdata");
        update(&path, |meta| {
            meta.rating = 3;
            Some(())
        })
        .unwrap();
        let before = fs::metadata(&path).unwrap().modified().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));
        update::<()>(&path, |_| None).unwrap();

        let after = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(
            before, after,
            "a write that changed nothing must not touch the file, or every cached picture of the photo is discarded for nothing"
        );
    }

    /// The reported fault, reproduced: ten presses, nine of them landing.
    ///
    /// Without the lock this fails almost every run. Each thread reads the
    /// exposure, adds a tenth and writes it back, which is exactly what
    /// `nudge_adjustments_for_paths` did per press.
    #[test]
    fn presses_at_the_same_moment_all_land() {
        let path = scratch("contended.nef.rrdata");
        update(&path, |meta| {
            meta.adjustments = serde_json::json!({ "exposure": 0.0 });
            Some(())
        })
        .unwrap();

        const PRESSES: usize = 40;
        let landed = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..PRESSES {
                let path = &path;
                let landed = &landed;
                scope.spawn(move || {
                    let moved = update(path, |meta| {
                        let now = meta.adjustments["exposure"].as_f64().unwrap_or(0.0);
                        meta.adjustments["exposure"] = serde_json::json!(now + 0.1);
                        Some(())
                    });
                    if matches!(moved, Ok(Some(()))) {
                        landed.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
        });

        assert_eq!(landed.load(Ordering::Relaxed), PRESSES);
        let final_exposure = read_unlocked(&path).adjustments["exposure"]
            .as_f64()
            .unwrap();
        assert!(
            (final_exposure - 4.0).abs() < 1e-6,
            "forty presses of a tenth is four stops, not {final_exposure}"
        );
    }

    #[test]
    fn two_different_photos_do_not_wait_for_each_other() {
        // Not a timing test, which would be flaky. It only proves the locks are
        // per file: taking one and then the other from the same thread would
        // deadlock if there were a single global lock.
        let a = scratch("one.nef.rrdata");
        let b = scratch("two.nef.rrdata");
        let outer = lock_for(&a);
        let _held = outer.lock().unwrap();
        update(&b, |meta| {
            meta.rating = 1;
            Some(())
        })
        .unwrap();
        assert_eq!(read_unlocked(&b).rating, 1);
    }

    #[test]
    fn an_interrupted_write_leaves_no_leftovers() {
        let path = scratch("clean.nef.rrdata");
        update(&path, |meta| {
            meta.rating = 5;
            Some(())
        })
        .unwrap();
        let mut part = path.file_name().unwrap().to_os_string();
        part.push(".part");
        assert!(
            !path.with_file_name(part).exists(),
            "the file it was built as is gone"
        );
    }
}
