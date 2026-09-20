//! Persisting stacks: which captures the photographer wants shown as one item.
//!
//! Stored one file per folder rather than in each image's `.rrdata` sidecar.
//! A stack is a property of a set of files sitting together, so a folder-level
//! record matches the shape of the thing. It also means confirming a hundred
//! brackets rewrites one small file instead of touching three hundred sidecars,
//! and a bug here can never damage anyone's adjustments.
//!
//! Members are recorded by file name, not full path, so moving or renaming the
//! folder keeps the stacks intact.
//!
//! This is deliberately separate from `group_id`, which ties together several
//! files of one capture. The two nest: a three-shot bracket shot RAW+JPEG is
//! three groups inside one stack.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::legacy_names;

/// Sits beside the images it describes. Dot-prefixed so it stays out of the way.
const STACK_FILE_NAME: &str = ".blitzraw-stacks.json";

/// One stack on disk.
///
/// Recorded either as a bare list of members, which is how stacks were written
/// before they could have an explicit leader, or as members plus a leader. Both
/// shapes are read so existing files keep working; anything rewritten comes
/// back in the newer shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StackRecord {
    Detailed {
        members: Vec<String>,
        /// The file shown when the stack is closed. Falls back to display order
        /// when absent, which is what every stack written before HDR merging
        /// existed will do.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        leader: Option<String>,
    },
    MembersOnly(Vec<String>),
}

impl StackRecord {
    pub fn members(&self) -> &[String] {
        match self {
            Self::Detailed { members, .. } => members,
            Self::MembersOnly(members) => members,
        }
    }

    fn members_mut(&mut self) -> &mut Vec<String> {
        match self {
            Self::Detailed { members, .. } => members,
            Self::MembersOnly(members) => members,
        }
    }

    pub fn leader(&self) -> Option<&str> {
        match self {
            Self::Detailed { leader, .. } => leader.as_deref(),
            Self::MembersOnly(_) => None,
        }
    }

    fn set_leader(&mut self, name: String) {
        let members = self.members().to_vec();
        *self = Self::Detailed {
            members,
            leader: Some(name),
        };
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StackFile {
    #[serde(default = "default_version")]
    pub version: u32,
    /// Stack id to the stack it names.
    #[serde(default)]
    pub stacks: HashMap<String, StackRecord>,
}

fn default_version() -> u32 {
    1
}

/// The folder's stack record, whatever it is currently called.
///
/// The one place the name is resolved, so `load` and `save` can never disagree
/// about which file they mean. A record written before the rename is renamed
/// here, once, the first time the folder is touched.
///
/// If that rename cannot be done, on a read-only card or a NAS that refuses it,
/// the old file is used where it lies rather than a second record being started
/// beside it. A folder has one stack record or none.
fn stack_file_path(dir: &Path) -> PathBuf {
    legacy_names::migrate(dir, legacy_names::LEGACY_STACK_FILE_NAME, STACK_FILE_NAME);

    let current = dir.join(STACK_FILE_NAME);
    if current.exists() {
        return current;
    }

    let legacy = dir.join(legacy_names::LEGACY_STACK_FILE_NAME);
    if legacy.exists() { legacy } else { current }
}

fn empty_stacks() -> StackFile {
    StackFile {
        version: 1,
        stacks: HashMap::new(),
    }
}

// ========== BLITZRAW: never write over a record we could not read ==========
/// Whether the record on disk can be safely replaced.
///
/// Three states, and the difference between the last two is a folder's stacks.
///
/// - **Absent.** No file. Nothing to lose, and writing one is how the first
///   stack in a folder is recorded.
/// - **Read.** The file is there and it parsed. What is about to be written was
///   built from it, so replacing it loses nothing.
/// - **Unreadable.** The file is there and could not be read or could not be
///   parsed. **Its contents are unknown**, so anything written over it is
///   written over something.
///
/// The third case used to be treated as the first. `load` answered "no stacks"
/// for it, whatever the reason, and every writer then persisted that emptiness:
/// `save` deletes the file when the map is empty, so a folder whose record was
/// briefly unreadable lost every stack in it the next time anything was
/// unstacked. Briefly unreadable is not exotic on this library, where shoots sit
/// on a NAS and on drives an antivirus and a sync client both walk.
///
/// One of those folders holds 296 stacks and has no copy anywhere else.
#[derive(Debug, PartialEq, Eq)]
pub enum RecordState {
    Absent,
    Read,
    Unreadable,
}

fn record_state(path: &Path) -> (RecordState, StackFile) {
    match fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<StackFile>(&text) {
            Ok(parsed) => (RecordState::Read, parsed),
            Err(e) => {
                log::warn!(
                    "The stack file at {} is there but will not parse, so it is left alone: {e}",
                    path.display()
                );
                (RecordState::Unreadable, empty_stacks())
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (RecordState::Absent, empty_stacks()),
        Err(e) => {
            log::warn!(
                "The stack file at {} could not be read, so it is left alone: {e}",
                path.display()
            );
            (RecordState::Unreadable, empty_stacks())
        }
    }
}
// ======== BLITZRAW END: never write over a record we could not read ========

/// Reads a folder's stacks. A missing file means "no stacks", which is the
/// normal case; see `record_state` for why an unreadable one is not the same
/// thing and is never overwritten.
pub fn load(dir: &Path) -> StackFile {
    record_state(&stack_file_path(dir)).1
}

/// Writes a folder's stacks, or removes the file when nothing is left.
///
/// Written to a temporary file and renamed, so an interrupted write cannot
/// leave a half-parsed file behind.
pub fn save(dir: &Path, file: &StackFile) -> Result<(), String> {
    let path = stack_file_path(dir);

    // BLITZRAW: the guard. Whatever is in a record that would not read, it is
    // not what is about to be written, because that was built from an empty
    // one. See `record_state`.
    if record_state(&path).0 == RecordState::Unreadable {
        return Err(format!(
            "The stack file at {} could not be read, so it has been left alone rather than \
             overwritten. Move it aside if you want to start the folder's stacks again.",
            path.display()
        ));
    }

    if file.stacks.is_empty() {
        // Leaving an empty record behind would be litter in the user's folder.
        match fs::remove_file(&path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("Could not remove {}: {e}", path.display())),
        }
    }

    let json = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    let temp = path.with_extension("json.tmp");

    fs::write(&temp, json).map_err(|e| format!("Could not write {}: {e}", temp.display()))?;
    fs::rename(&temp, &path).map_err(|e| {
        let _ = fs::remove_file(&temp);
        format!("Could not replace {}: {e}", path.display())
    })
}

/// Maps each file name in a folder to the stack it belongs to.
pub fn stack_ids_by_file_name(dir: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (stack_id, record) in load(dir).stacks {
        for member in record.members() {
            out.insert(member.clone(), stack_id.clone());
        }
    }
    out
}

/// The file name that leads each stack, where one was chosen explicitly.
pub fn leaders_by_stack_id(dir: &Path) -> HashMap<String, String> {
    load(dir)
        .stacks
        .into_iter()
        .filter_map(|(id, record)| record.leader().map(|l| (id, l.to_string())))
        .collect()
}

/// A stack id derived from its members, so re-running detection over the same
/// frames produces the same id rather than a new one every time.
fn derive_stack_id(members: &[String]) -> String {
    let mut sorted = members.to_vec();
    sorted.sort();
    let digest = blake3::hash(sorted.join("\u{0}").as_bytes());
    digest.to_hex()[..16].to_string()
}

fn file_name_of(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
}

/// Strips the virtual-copy suffix; copies share the file the stack refers to.
fn source_of(path: &str) -> &str {
    path.split("?vc=").next().unwrap_or(path)
}

fn parent_of(path: &str) -> Option<PathBuf> {
    Path::new(source_of(path)).parent().map(|p| p.to_path_buf())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackWriteSummary {
    pub stacks_written: usize,
    pub frames_stacked: usize,
    pub folders_touched: usize,
}

/// Follows a batch of file renames through the stack records.
///
/// Membership is stored by file name, and nothing used to update it, so
/// renaming a stacked file silently dropped it out of its stack. The file kept
/// its sidecar and its edits, but the bracket it belonged to lost a frame and
/// the stack quietly shrank, or fell below two members and vanished. Renaming
/// a whole stack at once, which is now what renaming a peer stack does, would
/// have destroyed the stack outright.
///
/// Takes the map of old to new full paths that `rename_files` already builds,
/// and rewrites one stack file per folder touched. Renames that leave a folder
/// are not handled here: a move is not a rename, and stacks are per folder.
pub fn rename_members(renames: &HashMap<String, String>) -> usize {
    let mut by_folder: HashMap<PathBuf, Vec<(String, String)>> = HashMap::new();

    for (old_path, new_path) in renames {
        let (Some(dir), Some(old_name), Some(new_name)) = (
            parent_of(old_path),
            file_name_of(source_of(old_path)),
            file_name_of(source_of(new_path)),
        ) else {
            continue;
        };
        // A file that moved folders is not something a stack can follow.
        if parent_of(new_path).as_deref() != Some(dir.as_path()) {
            continue;
        }
        if old_name == new_name {
            continue;
        }
        by_folder.entry(dir).or_default().push((old_name, new_name));
    }

    let mut updated = 0;

    for (dir, pairs) in by_folder {
        let mut file = load(&dir);
        if file.stacks.is_empty() {
            continue;
        }

        let lookup: HashMap<&str, &str> = pairs
            .iter()
            .map(|(old, new)| (old.as_str(), new.as_str()))
            .collect();

        let mut changed = false;
        for record in file.stacks.values_mut() {
            for member in record.members_mut().iter_mut() {
                if let Some(new_name) = lookup.get(member.as_str()) {
                    *member = (*new_name).to_string();
                    changed = true;
                }
            }
            // The leader is a member by name too, so it moves with it or the
            // stack ends up led by a file that no longer exists.
            if let Some(leader) = record.leader()
                && let Some(new_name) = lookup.get(leader)
            {
                let new_name = (*new_name).to_string();
                record.set_leader(new_name);
                changed = true;
            }
        }

        if changed && save(&dir, &file).is_ok() {
            updated += 1;
        }
    }

    updated
}

/// Which of the given paths already belong to a stack.
///
/// Answers per folder so one stack file is read per directory rather than one
/// per path. Used by auto-stack, which leaves confirmed stacks alone.
pub fn stacked_file_names(paths: &[String]) -> std::collections::HashSet<String> {
    let mut folders: HashMap<PathBuf, StackFile> = HashMap::new();
    let mut out = std::collections::HashSet::new();

    for path in paths {
        let (Some(dir), Some(name)) = (parent_of(path), file_name_of(source_of(path))) else {
            continue;
        };
        let file = folders.entry(dir.clone()).or_insert_with(|| load(&dir));
        if file
            .stacks
            .values()
            .any(|record| record.members().contains(&name))
        {
            out.insert(path.clone());
        }
    }

    out
}

/// Records stacks, replacing any previous stack that shared a member.
///
/// Groups spanning folders are handled by writing to each folder involved.
#[tauri::command]
pub fn set_stacks(stacks: Vec<Vec<String>>) -> Result<StackWriteSummary, String> {
    let mut by_folder: HashMap<PathBuf, Vec<Vec<String>>> = HashMap::new();

    for stack in stacks {
        // A stack of one is not a stack; ignore rather than record noise.
        if stack.len() < 2 {
            continue;
        }
        let Some(dir) = stack.first().and_then(|p| parent_of(p)) else {
            continue;
        };
        by_folder.entry(dir).or_default().push(stack);
    }

    let mut stacks_written = 0;
    let mut frames_stacked = 0;
    let folders_touched = by_folder.len();

    for (dir, folder_stacks) in by_folder {
        let mut file = load(&dir);

        for stack in folder_stacks {
            let members: Vec<String> = stack
                .iter()
                .filter(|p| parent_of(p).as_deref() == Some(dir.as_path()))
                .filter_map(|p| file_name_of(source_of(p)))
                .collect();

            if members.len() < 2 {
                continue;
            }

            // A frame belongs to one stack at a time, so clear it out of any
            // stack it was in before recording the new one.
            file.stacks
                .values_mut()
                .for_each(|existing| existing.members_mut().retain(|m| !members.contains(m)));

            frames_stacked += members.len();
            stacks_written += 1;
            file.stacks.insert(
                derive_stack_id(&members),
                StackRecord::Detailed {
                    members,
                    leader: None,
                },
            );
        }

        file.stacks.retain(|_, record| record.members().len() >= 2);
        save(&dir, &file)?;
    }

    Ok(StackWriteSummary {
        stacks_written,
        frames_stacked,
        folders_touched,
    })
}

/// Removes the given files from whatever stacks they are in.
#[tauri::command]
pub fn clear_stacks(paths: Vec<String>) -> Result<usize, String> {
    let mut by_folder: HashMap<PathBuf, Vec<String>> = HashMap::new();
    for path in &paths {
        if let (Some(dir), Some(name)) = (parent_of(path), file_name_of(source_of(path))) {
            by_folder.entry(dir).or_default().push(name);
        }
    }

    let mut removed = 0;

    for (dir, names) in by_folder {
        let mut file = load(&dir);
        let before: usize = file.stacks.len();

        file.stacks
            .values_mut()
            .for_each(|record| record.members_mut().retain(|m| !names.contains(m)));
        file.stacks.retain(|_, record| record.members().len() >= 2);

        removed += before.saturating_sub(file.stacks.len());
        save(&dir, &file)?;
    }

    Ok(removed)
}

/// Adds a file to the stack a sibling belongs to and puts it on top.
///
/// Used after an HDR merge: the merged result joins the bracket it came from
/// and becomes the frame shown when the stack is closed, which is what makes a
/// merged stack read as one finished photograph.
#[tauri::command]
pub fn set_stack_leader(new_member_path: String, sibling_path: String) -> Result<(), String> {
    let (Some(dir), Some(sibling_name)) = (
        parent_of(&sibling_path),
        file_name_of(source_of(&sibling_path)),
    ) else {
        return Err("Could not resolve the stack from that file.".to_string());
    };
    let Some(new_name) = file_name_of(source_of(&new_member_path)) else {
        return Err("Could not resolve the new stack member.".to_string());
    };

    let mut file = load(&dir);

    let Some(stack_id) = file
        .stacks
        .iter()
        .find(|(_, record)| record.members().contains(&sibling_name))
        .map(|(id, _)| id.clone())
    else {
        return Err("That file is not part of a stack.".to_string());
    };

    // The new file may already be listed if this ran before; keep it once.
    if let Some(record) = file.stacks.get_mut(&stack_id) {
        let members = record.members_mut();
        if !members.contains(&new_name) {
            members.insert(0, new_name.clone());
        }
        record.set_leader(new_name);
    }

    save(&dir, &file)
}

#[cfg(test)]
mod tests {

    // ===== BLITZRAW: a record that will not read is never written over =====
    /// The live fault: a stack file that cannot be parsed, plus one unstack,
    /// used to delete every stack in the folder.
    ///
    /// `load` answered "no stacks" for an unreadable file, the caller removed
    /// nothing from that emptiness, and `save` deletes the file when the map is
    /// empty. One transient read failure on a NAS and a folder of 296 stacks
    /// was gone.
    #[test]
    fn a_stack_file_that_will_not_parse_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STACK_FILE_NAME);
        std::fs::write(&path, b"{ this is not json").unwrap();

        let held = std::fs::read(&path).unwrap();
        let outcome = save(dir.path(), &empty_stacks());

        assert!(outcome.is_err(), "refused rather than written");
        assert!(path.exists(), "and still there");
        assert_eq!(std::fs::read(&path).unwrap(), held, "byte for byte");
    }

    /// The same guard must not stop the ordinary case: a folder with no record
    /// yet is how every first stack is made.
    #[test]
    fn a_folder_with_no_record_can_still_have_one_written() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = empty_stacks();
        file.stacks.insert(
            "s1".to_string(),
            StackRecord::Detailed {
                members: vec!["a.nef".to_string(), "b.nef".to_string()],
                leader: None,
            },
        );

        save(dir.path(), &file).expect("written");
        assert_eq!(load(dir.path()).stacks.len(), 1);
    }

    /// And a record that reads is replaced as before, including the removal
    /// when the last stack in a folder goes.
    #[test]
    fn a_record_that_reads_is_replaced_as_before() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = empty_stacks();
        file.stacks.insert(
            "s1".to_string(),
            StackRecord::Detailed {
                members: vec!["a.nef".to_string(), "b.nef".to_string()],
                leader: None,
            },
        );
        save(dir.path(), &file).expect("written");

        save(dir.path(), &empty_stacks()).expect("emptied");
        assert!(
            !dir.path().join(STACK_FILE_NAME).exists(),
            "the last stack going takes the file with it"
        );
    }

    #[test]
    fn the_three_states_are_told_apart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STACK_FILE_NAME);

        assert_eq!(record_state(&path).0, RecordState::Absent);

        std::fs::write(&path, br#"{"version":1,"stacks":{}}"#).unwrap();
        assert_eq!(record_state(&path).0, RecordState::Read);

        std::fs::write(&path, b"half a file").unwrap();
        assert_eq!(record_state(&path).0, RecordState::Unreadable);
    }
    // === BLITZRAW END: a record that will not read is never written over ===

    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("blitzraw-stacks-test-{label}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn p(dir: &Path, name: &str) -> String {
        dir.join(name).to_string_lossy().into_owned()
    }

    #[test]
    fn a_missing_stack_file_reads_as_no_stacks() {
        let dir = temp_dir("missing");
        assert!(load(&dir).stacks.is_empty());
    }

    #[test]
    fn corrupt_json_is_ignored_rather_than_failing_the_folder() {
        let dir = temp_dir("corrupt");
        fs::write(stack_file_path(&dir), "{ not json at all").unwrap();
        assert!(load(&dir).stacks.is_empty());
    }

    #[test]
    fn a_stack_round_trips_through_disk() {
        let dir = temp_dir("roundtrip");
        set_stacks(vec![vec![
            p(&dir, "a.nef"),
            p(&dir, "b.nef"),
            p(&dir, "c.nef"),
        ]])
        .unwrap();

        let ids = stack_ids_by_file_name(&dir);
        assert_eq!(ids.len(), 3);
        assert_eq!(ids["a.nef"], ids["b.nef"]);
        assert_eq!(ids["b.nef"], ids["c.nef"]);
    }

    #[test]
    fn the_same_members_always_produce_the_same_id() {
        let dir = temp_dir("stable-id");
        let stack = vec![p(&dir, "a.nef"), p(&dir, "b.nef"), p(&dir, "c.nef")];

        set_stacks(vec![stack.clone()]).unwrap();
        let first = stack_ids_by_file_name(&dir)["a.nef"].clone();

        set_stacks(vec![stack]).unwrap();
        assert_eq!(stack_ids_by_file_name(&dir)["a.nef"], first);
    }

    #[test]
    fn a_frame_can_only_belong_to_one_stack() {
        let dir = temp_dir("exclusive");
        set_stacks(vec![vec![
            p(&dir, "a.nef"),
            p(&dir, "b.nef"),
            p(&dir, "c.nef"),
        ]])
        .unwrap();
        // Re-stack b with different partners; it must leave the first stack.
        set_stacks(vec![vec![
            p(&dir, "b.nef"),
            p(&dir, "d.nef"),
            p(&dir, "e.nef"),
        ]])
        .unwrap();

        let ids = stack_ids_by_file_name(&dir);
        assert_eq!(ids["b.nef"], ids["d.nef"]);
        assert_ne!(ids["b.nef"], ids["a.nef"]);
        // a and c are now only two, which is still a stack.
        assert_eq!(ids["a.nef"], ids["c.nef"]);
    }

    #[test]
    fn a_stack_reduced_below_two_members_disappears() {
        let dir = temp_dir("collapse");
        set_stacks(vec![vec![p(&dir, "a.nef"), p(&dir, "b.nef")]]).unwrap();
        clear_stacks(vec![p(&dir, "a.nef")]).unwrap();

        assert!(stack_ids_by_file_name(&dir).is_empty());
    }

    #[test]
    fn clearing_every_stack_removes_the_file_entirely() {
        let dir = temp_dir("cleanup");
        set_stacks(vec![vec![
            p(&dir, "a.nef"),
            p(&dir, "b.nef"),
            p(&dir, "c.nef"),
        ]])
        .unwrap();
        assert!(stack_file_path(&dir).exists());

        clear_stacks(vec![p(&dir, "a.nef"), p(&dir, "b.nef"), p(&dir, "c.nef")]).unwrap();
        assert!(!stack_file_path(&dir).exists(), "empty record left behind");
    }

    #[test]
    fn a_lone_frame_is_not_recorded_as_a_stack() {
        let dir = temp_dir("single");
        set_stacks(vec![vec![p(&dir, "a.nef")]]).unwrap();
        assert!(stack_ids_by_file_name(&dir).is_empty());
    }

    #[test]
    fn virtual_copies_resolve_to_the_file_they_copy() {
        let dir = temp_dir("virtual");
        let a = p(&dir, "a.nef");
        set_stacks(vec![vec![format!("{a}?vc=abc123"), p(&dir, "b.nef")]]).unwrap();

        let ids = stack_ids_by_file_name(&dir);
        assert!(
            ids.contains_key("a.nef"),
            "expected the source name, got {ids:?}"
        );
    }

    #[test]
    fn a_merged_result_joins_the_stack_and_leads_it() {
        let dir = temp_dir("leader");
        set_stacks(vec![vec![
            p(&dir, "a.nef"),
            p(&dir, "b.nef"),
            p(&dir, "c.nef"),
        ]])
        .unwrap();

        set_stack_leader(p(&dir, "a_Hdr.png"), p(&dir, "b.nef")).unwrap();

        let file = load(&dir);
        let record = file.stacks.values().next().unwrap();
        assert_eq!(record.leader(), Some("a_Hdr.png"));
        assert!(record.members().contains(&"a_Hdr.png".to_string()));
        assert!(record.members().contains(&"a.nef".to_string()));
        assert_eq!(record.members().len(), 4);
    }

    #[test]
    fn promoting_the_same_file_twice_does_not_duplicate_it() {
        let dir = temp_dir("leader-twice");
        set_stacks(vec![vec![p(&dir, "a.nef"), p(&dir, "b.nef")]]).unwrap();

        set_stack_leader(p(&dir, "a_Hdr.png"), p(&dir, "a.nef")).unwrap();
        set_stack_leader(p(&dir, "a_Hdr.png"), p(&dir, "a.nef")).unwrap();

        let file = load(&dir);
        let record = file.stacks.values().next().unwrap();
        assert_eq!(record.members().len(), 3);
    }

    #[test]
    fn promoting_a_file_that_is_in_no_stack_is_an_error_not_a_silent_no_op() {
        let dir = temp_dir("leader-orphan");
        assert!(set_stack_leader(p(&dir, "x_Hdr.png"), p(&dir, "lonely.nef")).is_err());
    }

    #[test]
    fn renaming_a_member_keeps_it_in_its_stack() {
        let dir = temp_dir("rename-member");
        set_stacks(vec![vec![
            p(&dir, "a.nef"),
            p(&dir, "b.nef"),
            p(&dir, "c.nef"),
        ]])
        .unwrap();

        let mut renames = HashMap::new();
        renames.insert(p(&dir, "b.nef"), p(&dir, "shoot_002.nef"));
        assert_eq!(rename_members(&renames), 1);

        let file = load(&dir);
        let record = file.stacks.values().next().expect("the stack survives");
        assert_eq!(record.members().len(), 3, "the stack lost a frame");
        assert!(record.members().iter().any(|m| m == "shoot_002.nef"));
        assert!(!record.members().iter().any(|m| m == "b.nef"));
    }

    #[test]
    fn renaming_the_leader_carries_the_leader_with_it() {
        let dir = temp_dir("rename-leader");
        set_stacks(vec![vec![p(&dir, "a.nef"), p(&dir, "b.nef")]]).unwrap();
        set_stack_leader(p(&dir, "a_Hdr.tiff"), p(&dir, "a.nef")).unwrap();

        let mut renames = HashMap::new();
        renames.insert(p(&dir, "a_Hdr.tiff"), p(&dir, "final_Hdr.tiff"));
        rename_members(&renames);

        let file = load(&dir);
        let record = file.stacks.values().next().expect("the stack survives");
        assert_eq!(
            record.leader(),
            Some("final_Hdr.tiff"),
            "the stack is led by a file that no longer exists"
        );
        assert!(record.members().iter().any(|m| m == "final_Hdr.tiff"));
    }

    #[test]
    fn renaming_every_member_at_once_keeps_the_stack_whole() {
        let dir = temp_dir("rename-all");
        set_stacks(vec![vec![
            p(&dir, "a.nef"),
            p(&dir, "b.nef"),
            p(&dir, "c.nef"),
        ]])
        .unwrap();

        // What renaming a selected peer stack now does.
        let mut renames = HashMap::new();
        for (old, new) in [
            ("a.nef", "s_1.nef"),
            ("b.nef", "s_2.nef"),
            ("c.nef", "s_3.nef"),
        ] {
            renames.insert(p(&dir, old), p(&dir, new));
        }
        rename_members(&renames);

        let file = load(&dir);
        let record = file.stacks.values().next().expect("the stack survives");
        let mut members: Vec<&str> = record.members().iter().map(|m| m.as_str()).collect();
        members.sort();
        assert_eq!(members, vec!["s_1.nef", "s_2.nef", "s_3.nef"]);
    }

    #[test]
    fn a_rename_that_touches_no_stack_writes_nothing() {
        let dir = temp_dir("rename-none");
        let mut renames = HashMap::new();
        renames.insert(p(&dir, "lonely.nef"), p(&dir, "still_lonely.nef"));
        assert_eq!(rename_members(&renames), 0);
    }

    #[test]
    fn a_confirmed_stack_is_left_alone_by_a_second_detection_run() {
        let dir = temp_dir("already-stacked");
        set_stacks(vec![vec![p(&dir, "a.nef"), p(&dir, "b.nef")]]).unwrap();

        let paths = vec![p(&dir, "a.nef"), p(&dir, "b.nef"), p(&dir, "loose.nef")];
        let stacked = stacked_file_names(&paths);

        assert_eq!(stacked.len(), 2);
        assert!(stacked.contains(&p(&dir, "a.nef")));
        assert!(
            !stacked.contains(&p(&dir, "loose.nef")),
            "a free file was skipped"
        );
    }

    #[test]
    fn a_stack_written_before_leaders_existed_still_loads() {
        let dir = temp_dir("legacy");
        fs::write(
            stack_file_path(&dir),
            r#"{"version":1,"stacks":{"abc123":["a.nef","b.nef","c.nef"]}}"#,
        )
        .unwrap();

        let ids = stack_ids_by_file_name(&dir);
        assert_eq!(ids.len(), 3, "legacy record failed to load: {ids:?}");
        assert_eq!(ids["a.nef"], "abc123");
        assert!(load(&dir).stacks["abc123"].leader().is_none());
    }

    #[test]
    fn members_are_stored_by_name_so_the_folder_can_move() {
        let dir = temp_dir("portable");
        set_stacks(vec![vec![p(&dir, "a.nef"), p(&dir, "b.nef")]]).unwrap();

        let text = fs::read_to_string(stack_file_path(&dir)).unwrap();
        assert!(text.contains("a.nef"));
        assert!(
            !text.contains(&dir.to_string_lossy().to_string()),
            "absolute paths leaked into the record"
        );
    }
}
