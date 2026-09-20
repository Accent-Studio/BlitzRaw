//! What this application used to call the things it leaves in a photo folder.
//!
//! The application was called Darkroom before it was called BlitzRaw, and it
//! wrote two things beside the photographs: a `.darkroom-previews` folder of
//! rendered previews, and a `.darkroom-stacks.json` file holding which frames
//! belong to which bracket. Shoots edited before the rename still carry both,
//! and some of them sit on drives that are not plugged in today.
//!
//! So the old name is never simply dropped. The first time a folder is asked
//! for one of these, whatever is there under the old name is renamed to the new
//! one. A rename inside one folder is atomic and instant, it happens once, and
//! a shoot that comes back off a shelf in a year migrates itself the moment it
//! is opened.
//!
//! Renaming rather than accepting both names is deliberate. Two accepted names
//! is a rule that every future write path has to remember, and one of them
//! eventually will not. One name is the rule; this module is the door the old
//! one comes through.

use std::path::Path;

/// What the rendered preview folder was called.
pub const LEGACY_CACHE_DIR_NAME: &str = ".darkroom-previews";

/// What the stack record was called.
pub const LEGACY_STACK_FILE_NAME: &str = ".darkroom-stacks.json";

/// Renames `old` to `new` inside `folder`, if that is what is called for.
///
/// Does nothing when the new name is already there, which is the normal case
/// and costs one look at the folder. Does nothing when the old name is not
/// there either, which is every folder written since the rename.
///
/// A failure is logged and swallowed. The caller decides what to do without the
/// rename, because the answer differs: a preview that cannot be migrated is
/// rendered again and costs only time, while a stack record that cannot be
/// migrated must keep being read where it lies.
pub fn migrate(folder: &Path, old: &str, new: &str) {
    let new_path = folder.join(new);
    if new_path.exists() {
        return;
    }

    let old_path = folder.join(old);
    if !old_path.exists() {
        return;
    }

    match std::fs::rename(&old_path, &new_path) {
        Ok(()) => log::info!("Renamed {old} to {new} in {}", folder.display()),
        Err(e) => log::warn!(
            "Could not rename {old} to {new} in {}: {e}",
            folder.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_old_name_becomes_the_new_one() {
        let dir = std::env::temp_dir().join("blitzraw-legacy-rename");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(LEGACY_STACK_FILE_NAME), "{}").unwrap();

        migrate(&dir, LEGACY_STACK_FILE_NAME, ".blitzraw-stacks.json");

        assert!(
            dir.join(".blitzraw-stacks.json").exists(),
            "the record should now be under the new name"
        );
        assert!(
            !dir.join(LEGACY_STACK_FILE_NAME).exists(),
            "and should no longer be under the old one"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_record_already_under_the_new_name_is_left_alone() {
        let dir = std::env::temp_dir().join("blitzraw-legacy-keeps-newer");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(LEGACY_STACK_FILE_NAME), "old").unwrap();
        std::fs::write(dir.join(".blitzraw-stacks.json"), "new").unwrap();

        migrate(&dir, LEGACY_STACK_FILE_NAME, ".blitzraw-stacks.json");

        assert_eq!(
            std::fs::read_to_string(dir.join(".blitzraw-stacks.json")).unwrap(),
            "new",
            "the current record must never be written over by the old one"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_with_neither_name_is_untouched() {
        let dir = std::env::temp_dir().join("blitzraw-legacy-nothing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        migrate(&dir, LEGACY_STACK_FILE_NAME, ".blitzraw-stacks.json");

        assert!(
            !dir.join(".blitzraw-stacks.json").exists(),
            "nothing should be created out of nothing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
