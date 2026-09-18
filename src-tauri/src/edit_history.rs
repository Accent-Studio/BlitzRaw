//! A photo's edit history, kept in its sidecar so it outlives the session.
//!
//! # Why it lives here rather than in the front end
//!
//! History began as a list in the editor's store: per photo since phase one,
//! but only for as long as the application was open, and only for photos the
//! editor had actually opened. Two things followed from that, and they were
//! reported together as one complaint.
//!
//! Restarting lost everything. And applying an adjustment across a selection
//! recorded nothing for any photo except the one on screen, because the other
//! forty were never opened, so there was no list to add to and no record of
//! what they looked like before.
//!
//! The second one cannot be fixed where it happens. A photo that has never been
//! opened has no "before" state anywhere in the front end. It has one here, in
//! the sidecar that is about to be overwritten, which is the argument for the
//! whole of this module: **whoever writes the adjustments writes the step**.
//!
//! That also settles the trap this project has already paid for once. Two
//! writers for one value is the bug. The history goes out in the same
//! `fs::write` as the adjustments it describes, never from a second writer
//! racing it.
//!
//! # Deltas, not snapshots, and the reason is measured
//!
//! Sidecars in the working set run 3,090 bytes at the smallest, 3,305 at the
//! median and 21,631 at the largest, the spread being masks. A hundred whole
//! snapshots of a masked photo is a megabyte, and a shoot with two hundred
//! edited frames would carry 60 to 200 MB of history against sidecars totalling
//! about 4 MB today.
//!
//! So a step stores only the adjustments that moved, each as `[before, after]`.
//! Most steps are one number and cost a couple of hundred bytes. Keeping the
//! before value as well as the after is what makes stepping backwards a direct
//! assignment rather than a replay from the beginning of the log.
//!
//! # What is authoritative
//!
//! `adjustments` in the sidecar, exactly as before. The history sits beside it
//! and describes how it got there. Deleting the whole `history` key loses the
//! way back and touches nothing about how the photo looks, which is the
//! property that makes this safe to add to files that already exist.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The format of the log. Anything else is replaced rather than guessed at,
/// since a history is a convenience and a photo's edits are not.
pub const HISTORY_VERSION: u32 = 1;

/// Steps kept before the oldest fold into the base state.
pub const DEFAULT_STEP_LIMIT: usize = 100;
pub const MIN_STEP_LIMIT: usize = 20;
pub const MAX_STEP_LIMIT: usize = 1000;

/// A photo's history: where it started, and everything done to it since.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct EditHistory {
    pub version: u32,
    /// The state the log starts from. Steps that fall off the front are folded
    /// into this, so the earliest reachable point is always a real state and
    /// never a broken reference.
    pub base: Value,
    pub entries: Vec<Step>,
    // ============ BLITZRAW: the numbers that make an undo possible ============
    // The application remembers the order things
    // were done in and, for each photo, the number it moved from and the number
    // it moved to. It holds no values at all. These three make that work.
    /// Which step this photo is sitting on. Its `adjustments` are the state at
    /// this number, always.
    ///
    /// `base_n` means the base, before any step that is still here. An undo
    /// moves this down and writes nothing; a redo moves it back up.
    #[serde(default)]
    pub at: u64,
    /// The number the base state stands at.
    ///
    /// Zero until steps start folding into it. After that it is the number of
    /// the last step folded away, so the base is still a real point the
    /// bookmark can name.
    #[serde(default)]
    pub base_n: u64,
    /// The next number to hand out.
    ///
    /// Only ever goes up. A step dropped because an edit was made after an undo
    /// takes its number with it, so a reference to it reads as "not here"
    /// rather than as some later step that happens to share the number.
    #[serde(default)]
    pub next_n: u64,
    // ========== BLITZRAW END: the numbers that make an undo possible ==========
}

/// One thing that happened to a photo.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Step {
    /// When, as RFC 3339. Read by people and by the panel, never by the rule.
    pub at: String,
    /// What to call it, when the action knew better than a list of changed
    /// keys would read. `None` leaves the naming to the editor's own diff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Adjustment name to `[before, after]`. Absent only on a pinned step,
    /// which carries a whole state instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<Map<String, Value>>,
    /// Why this step is exempt from the limit: `export` or `manual`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    /// The whole state, on a pinned step, so it survives everything around it
    /// being folded away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Value>,
    /// BLITZRAW: this step's own number, unique within this photo and never
    /// reused.
    ///
    /// What the application's list of what I did points at. Not the position in
    /// `entries`, which shifts every time the oldest steps fold into the base,
    /// and not the timestamp, which is minted per photo and rewritten whenever a
    /// change joins the step above it.
    #[serde(default)]
    pub n: u64,
    /// BLITZRAW: which single thing the user did, as one name shared by every
    /// photo that thing touched.
    ///
    /// **Information, never identity.** Kept because it makes a settings file
    /// readable when something has gone wrong. Nothing finds anything by it:
    /// that was tried, and a save that had moved nothing borrowed the name of
    /// the action still open, which made the photo refuse to be stepped back.
    ///
    /// A step is otherwise identified only by its timestamp, and that timestamp
    /// is minted separately for each photo inside a parallel loop, then
    /// overwritten whenever a change joins the step above it. So there was no
    /// way to ask which photos one action wrote, which is the question an
    /// application level undo is made of.
    ///
    /// Optional and skipped when absent, so a log written before this field
    /// existed still reads, and one written with it still reads on a build
    /// that does not know it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

/// What a caller knows about the change it is recording.
pub struct Recording<'a> {
    /// A name for the step, when the action has one worth more than a diff.
    pub label: Option<&'a str>,
    /// BLITZRAW: the one thing the user did that this change is part of, shared
    /// by every photo that thing touched. `None` records nothing.
    pub action: Option<&'a str>,
    /// How many steps this photo keeps.
    pub limit: usize,
    /// Now, as RFC 3339. Passed in so the rule can be tested against a clock.
    pub at: String,
}

/// How long a step stays open to being added to.
///
/// A run of nudges on one slider is one move and should undo in one press. A
/// change of slider is a change of mind and is worth its own line however fast
/// it happens. Those two together are the whole rule.
///
/// # The same rule lives in the editor as well
///
/// `COALESCE_WINDOW_MS` in `utils/editHistory.ts` is this number, and
/// `joinsTheStepAtTheTop` there is this rule. The editor needs it to draw an
/// undo list the moment a slider moves, and this needs it because the write
/// that reaches disk is the one that has to be right. Neither can wait for the
/// other: the save and the editor's own push are separately rate-limited and
/// arrive in whichever order they arrive.
///
/// Two implementations of one rule is a thing to watch, so both are stated the
/// same way and both are tested the same way. If they ever disagree the stored
/// log wins, because it is what a photo comes back as.
const COALESCE_WINDOW: chrono::Duration = chrono::Duration::milliseconds(2500);

/// Reads the history out of a sidecar without letting a bad one take the
/// photo's edits with it.
///
/// `load_sidecar` falls back to a default `ImageMetadata` when the file will not
/// parse, so a `history` in a shape this cannot read would throw away the
/// `adjustments` sitting next to it. That is the whole photo, lost to a
/// convenience. Anything unreadable here becomes no history instead, and the
/// next write starts a fresh log from wherever the photo actually is.
pub fn lenient<'de, D>(deserializer: D) -> Result<Option<EditHistory>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value::<Option<EditHistory>>(value)
        .ok()
        .flatten())
}

pub fn usable_step_limit(limit: Option<u32>) -> usize {
    limit
        .map(|n| (n as usize).clamp(MIN_STEP_LIMIT, MAX_STEP_LIMIT))
        .unwrap_or(DEFAULT_STEP_LIMIT)
}

/// Which adjustments differ, each as `[before, after]`.
///
/// Keys present in one state and not the other count as changed, with `null`
/// standing in for the side that has nothing, so a preset that introduces a key
/// is a step like any other.
/// BLITZRAW: settings that live among the adjustments but are not edits.
///
/// Both are kept per photo, which is right: a photo should reopen with the
/// sections you had open and the clipping warning as you left it. Neither is
/// something that was done to the picture, so neither belongs in its history.
///
/// The same rule exists in `utils/editHistory.ts`, because the front end and
/// this log have to agree on what a step is.
pub const NOT_AN_EDIT: [&str; 2] = ["sectionVisibility", "showClipping"];

pub fn changed_between(before: &Value, after: &Value) -> Map<String, Value> {
    let empty = Map::new();
    let before_map = before.as_object().unwrap_or(&empty);
    let after_map = after.as_object().unwrap_or(&empty);

    let mut moved = Map::new();
    for name in before_map.keys().chain(after_map.keys()) {
        if moved.contains_key(name) {
            continue;
        }
        if NOT_AN_EDIT.contains(&name.as_str()) {
            continue;
        }
        let was = before_map.get(name).unwrap_or(&Value::Null);
        let now = after_map.get(name).unwrap_or(&Value::Null);
        if !same_value(was, now) {
            moved.insert(name.clone(), Value::Array(vec![was.clone(), now.clone()]));
        }
    }
    moved
}

/// Whether two values are the same, ignoring how a number happens to be
/// written.
///
/// `serde_json` compares a number by its representation, so `4700` and `4700.0`
/// are different values to it. They are the same number. JavaScript has one
/// number type and writes anything integral without its decimal point, so every
/// round trip through the front end flips some of them, and a plain `!=` then
/// reports a change where nothing moved.
///
/// It was not theoretical. Every save recorded a step for `lensDistortionParams`
/// because its `model` field is a whole number, so the history of a multi-select
/// edit read as "Lens Distortion Params" over and over with the exposure and
/// white balance changes buried among them. The log itself held the proof: the
/// before and after of those steps were character for character the same.
fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(one), Value::Number(other)) => match (one.as_f64(), other.as_f64()) {
            (Some(one), Some(other)) => one == other,
            _ => one == other,
        },
        (Value::Array(one), Value::Array(other)) => {
            one.len() == other.len()
                && one
                    .iter()
                    .zip(other.iter())
                    .all(|(one, other)| same_value(one, other))
        }
        (Value::Object(one), Value::Object(other)) => {
            one.len() == other.len()
                && one.iter().all(|(name, value)| {
                    other
                        .get(name)
                        .is_some_and(|against| same_value(value, against))
                })
        }
        _ => a == b,
    }
}

/// The "after" half of a step, as a map to lay over a state.
fn afters(step: &Step) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(changed) = &step.changed {
        for (name, pair) in changed {
            if let Some(after) = pair.as_array().and_then(|pair| pair.get(1)) {
                out.insert(name.clone(), after.clone());
            }
        }
    }
    out
}

fn lay_over(state: &mut Value, changes: &Map<String, Value>) {
    if !state.is_object() {
        *state = Value::Object(Map::new());
    }
    if let Some(map) = state.as_object_mut() {
        for (name, value) in changes {
            map.insert(name.clone(), value.clone());
        }
    }
}

/// Records what a write did to a photo.
///
/// Returns the history to store beside the new adjustments. A write that
/// changed nothing returns the history untouched, because nothing happened and
/// a step that undoes to where you already are is worse than no step at all.
pub fn record(
    history: Option<EditHistory>,
    before: &Value,
    after: &Value,
    how: Recording<'_>,
) -> EditHistory {
    let mut history = match history {
        Some(held) if held.version == HISTORY_VERSION => held,
        // No history, or one written by a version that is not this one. Either
        // way the log starts here, from the state the photo is in now.
        _ => EditHistory {
            version: HISTORY_VERSION,
            base: before.clone(),
            entries: Vec::new(),
            at: 0,
            base_n: 0,
            next_n: 1,
        },
    };
    // A log written before numbers existed hands out its first one here.
    if history.next_n == 0 {
        history.next_n = history.entries.last().map(|s| s.n).unwrap_or(0) + 1;
    }

    let changed = changed_between(before, after);
    if changed.is_empty() {
        return history;
    }

    // ============ BLITZRAW: an edit made after an undo ============
    // Everything above the bookmark is a path nobody took. It goes, the way it
    // goes in every undo stack, and it takes its numbers with it: `next_n` never
    // comes back down, so a reference to a dropped step reads as "not here"
    // rather than as a later step wearing the same number.
    if !history.entries.is_empty() && history.entries.last().map(|s| s.n) != Some(history.at) {
        history.entries.retain(|step| step.n <= history.at);
    }
    // ========== BLITZRAW END: an edit made after an undo ==========

    if joins_the_step_at_the_top(
        history.entries.last(),
        &changed,
        how.label,
        how.action,
        &how.at,
    ) {
        let last = history
            .entries
            .last_mut()
            .expect("joinable already proved there is one");
        let entry = last.changed.get_or_insert_with(Map::new);
        for (name, pair) in changed {
            let arriving = pair.as_array().cloned().unwrap_or_default();
            match entry.get_mut(&name) {
                // The step keeps the "before" it opened with, so a run of ten
                // nudges reads from where it started to where it ended.
                Some(existing) => {
                    if let (Some(existing), Some(now)) =
                        (existing.as_array_mut(), arriving.get(1))
                    {
                        if existing.len() == 2 {
                            existing[1] = now.clone();
                        }
                    }
                }
                None => {
                    entry.insert(name, Value::Array(arriving));
                }
            }
        }
        // Anything nudged back to where it started is not part of the move.
        entry.retain(|_, pair| {
            pair.as_array()
                .map(|pair| pair.len() == 2 && pair[0] != pair[1])
                .unwrap_or(false)
        });
        last.at = how.at;
        // And a run that ended exactly where it began did not happen.
        if entry.is_empty() {
            let dropped = history.entries.pop();
            // The bookmark comes back to whatever is now on top, or to the base.
            if let Some(dropped) = dropped {
                if history.at == dropped.n {
                    history.at = history.entries.last().map(|s| s.n).unwrap_or(history.base_n);
                }
            }
        }
        return history;
    }

    let n = history.next_n;
    history.next_n += 1;
    history.entries.push(Step {
        at: how.at,
        n,
        label: how.label.map(str::to_string),
        changed: Some(changed),
        pin: None,
        state: None,
        action: how.action.map(str::to_string),
    });
    history.at = n;
    fold_to_limit(&mut history, how.limit);
    history
}

/// Whether a change belongs to the step already at the top rather than to a new
/// one.
///
/// Both conditions have to hold, and each earns its place:
///
/// - **The same adjustments.** Every name this change touches is one the step
///   at the top already touched. A name it has not touched is a different tool
///   and a different intention.
/// - **Within the window.** A pause is a decision. Coming back to the same
///   slider after ten seconds is a second look at it, not a continuation.
///
/// A pinned step is a fixed point and a named one is a deliberate event, so
/// neither absorbs anything, and neither does a step arriving with a name.
fn joins_the_step_at_the_top(
    last: Option<&Step>,
    changed: &Map<String, Value>,
    label: Option<&str>,
    action: Option<&str>,
    at: &str,
) -> bool {
    if label.is_some() {
        return false;
    }
    let Some(step) = last else {
        return false;
    };
    if step.pin.is_some() || step.label.is_some() {
        return false;
    }
    // BLITZRAW: two things the user did are two steps, however fast they land.
    //
    // The rest of this rule asks whether the same adjustment moved again
    // recently, which is the right question for a run of slider moves and the
    // wrong one for two separate things the user did. Two presses on one slider
    // inside the window became a single step, so one undo took back both, which
    // is the worst thing an undo can do.
    //
    // Only a change that names its action is held apart. A change carrying no
    // action is judged exactly as it was before, so nothing that worked without
    // this field changes.
    if (action.is_some() || step.action.is_some()) && step.action.as_deref() != action {
        return false;
    }
    let Some(already) = &step.changed else {
        return false;
    };
    if changed.is_empty() || already.is_empty() {
        return false;
    }
    if !changed.keys().all(|name| already.contains_key(name)) {
        return false;
    }

    // Rolling, measured from the last change in the step rather than from when
    // it opened, so a slow drag stays one move for as long as it keeps moving.
    let (Ok(then), Ok(now)) = (
        chrono::DateTime::parse_from_rfc3339(&step.at),
        chrono::DateTime::parse_from_rfc3339(at),
    ) else {
        // An unreadable timestamp is not evidence of anything, so it starts a
        // new step rather than quietly swallowing a change into an old one.
        return false;
    };
    let since = now.signed_duration_since(then);
    since >= chrono::Duration::zero() && since <= COALESCE_WINDOW
}

// ============ BLITZRAW: moving one photo's bookmark ============
// The half of an application level undo that only a photo can answer.
//
// The application remembers the order things were done in and, for each photo,
// the number it moved **from** and the number it moved **to**. It holds no
// values at all: those are here, in the photo's own file, and this file is the
// record.
//
// So an undo is not a write of new values. It moves a bookmark down. Nothing is
// added, nothing is deleted, and the step it left is still there to go back to.

/// Why a photo could not be moved to a number.
#[derive(serde::Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CannotGo {
    /// That step folded into the base, or was dropped by an edit made after an
    /// undo. Either way it is gone, and nothing near it is a substitute.
    NotHere,
    /// The photo has no log at all.
    NoHistory,
}

/// Where a photo is sitting, and what it looks like there.
#[derive(Debug, Clone, PartialEq)]
pub struct AtStep {
    pub n: u64,
    pub state: Value,
}

/// The state this photo is in at a given number.
///
/// The base counts, because it is a real state and the point every log starts
/// from. Nothing is guessed: a number that is not here is refused rather than
/// rounded to the nearest step, because a photo quietly sent to the wrong state
/// is worse than one that says it cannot go.
pub fn state_at(history: Option<&EditHistory>, n: u64) -> Result<AtStep, CannotGo> {
    let Some(history) = history else {
        return Err(CannotGo::NoHistory);
    };
    if n < history.base_n {
        return Err(CannotGo::NotHere);
    }
    if n > history.base_n && !history.entries.iter().any(|step| step.n == n) {
        return Err(CannotGo::NotHere);
    }

    let mut state = history.base.clone();
    for step in &history.entries {
        if step.n > n {
            break;
        }
        match &step.state {
            // A pinned step carries a whole state, which is what lets it
            // survive the base being folded past it.
            Some(pinned) => state = pinned.clone(),
            None => lay_over(&mut state, &afters(step)),
        }
    }
    Ok(AtStep { n, state })
}

/// Moves the bookmark, and hands back what the photo looks like there.
///
/// The only thing an undo or a redo does to a photo. The log is untouched.
pub fn go_to(history: &mut EditHistory, n: u64) -> Result<AtStep, CannotGo> {
    let landed = state_at(Some(history), n)?;
    history.at = landed.n;
    Ok(landed)
}

/// The number a photo is sitting on.
///
/// A log written before numbers existed reports its top step, which is where
/// such a photo is by definition: nothing could ever have moved its bookmark.
pub fn bookmark(history: Option<&EditHistory>) -> u64 {
    let Some(history) = history else { return 0 };
    if history.next_n == 0 {
        return history.entries.last().map(|s| s.n).unwrap_or(history.base_n);
    }
    history.at
}

// ========== BLITZRAW END: moving one photo's bookmark ==========

/// Adds a step that holds a whole state and is never culled.
///
/// Used for exports, so that what left the building can always be got back to,
/// and for snapshots taken by hand. A whole state rather than a delta because
/// it has to survive everything around it being folded away.
pub fn pin(history: Option<EditHistory>, state: &Value, label: &str, kind: &str, at: String) -> EditHistory {
    let mut history = match history {
        Some(held) if held.version == HISTORY_VERSION => held,
        _ => EditHistory {
            version: HISTORY_VERSION,
            base: state.clone(),
            entries: Vec::new(),
            at: 0,
            base_n: 0,
            next_n: 1,
        },
    };

    // A pin on a state already pinned at the top says nothing new.
    if let Some(last) = history.entries.last()
        && last.pin.as_deref() == Some(kind)
        && last.state.as_ref() == Some(state)
    {
        return history;
    }

    if history.next_n == 0 {
        history.next_n = history.entries.last().map(|s| s.n).unwrap_or(0) + 1;
    }
    let n = history.next_n;
    history.next_n += 1;
    history.entries.push(Step {
        at,
        n,
        label: Some(label.to_string()),
        changed: None,
        pin: Some(kind.to_string()),
        state: Some(state.clone()),
        // A pin is a fixed point rather than something the user did in the
        // editor, so it belongs to no action.
        action: None,
    });
    history.at = n;
    history
}

/// Folds the oldest steps into the base until the log is back inside its limit.
///
/// Folding rather than dropping, so the earliest point you can still reach is a
/// real state of the photo rather than a reference to something that is gone.
///
/// A pinned step is never folded. If one is the oldest, folding stops there and
/// the log stays longer than the limit, which is what "pinned entries are
/// exempt, however old" has to mean. There is no reordering: a log is what
/// happened, in the order it happened.
fn fold_to_limit(history: &mut EditHistory, limit: usize) {
    let limit = limit.clamp(MIN_STEP_LIMIT, MAX_STEP_LIMIT);
    while history.entries.len() > limit {
        let Some(oldest) = history.entries.first() else {
            break;
        };
        if oldest.pin.is_some() {
            break;
        }
        let folded_n = oldest.n;
        let folding = afters(oldest);
        lay_over(&mut history.base, &folding);
        history.entries.remove(0);
        // The base now stands where that step stood, so it is still a real
        // point the bookmark can name. The numbers of what is left do not move,
        // which is the whole reason a number beats a position in the list.
        history.base_n = folded_n;
        if history.at < history.base_n {
            history.at = history.base_n;
        }
    }
}

/// Every state the photo has been in, oldest first.
///
/// The base, then one for each step. The editor works in whole states, so this
/// is what it is handed; the log is only how they are stored.
pub fn states(history: &EditHistory) -> Vec<Value> {
    let mut out = Vec::with_capacity(history.entries.len() + 1);
    let mut current = history.base.clone();
    out.push(current.clone());
    for step in &history.entries {
        current = match &step.state {
            // A pinned step carries its own, which is what lets it survive the
            // base being folded past it.
            Some(state) => state.clone(),
            None => {
                lay_over(&mut current, &afters(step));
                current.clone()
            }
        };
        out.push(current.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A timestamp this many milliseconds after a fixed start.
    fn clock(after_ms: i64) -> String {
        let start = chrono::DateTime::parse_from_rfc3339("2026-08-26T10:00:00Z").unwrap();
        (start + chrono::Duration::milliseconds(after_ms)).to_rfc3339()
    }

    fn how(at_ms: i64, label: Option<&'static str>) -> Recording<'static> {
        Recording {
            label,
            action: None,
            limit: DEFAULT_STEP_LIMIT,
            at: clock(at_ms),
        }
    }

    /// The same, for a change that says which action it belongs to.
    fn how_action(at_ms: i64, action: Option<&'static str>) -> Recording<'static> {
        Recording {
            label: None,
            action,
            limit: DEFAULT_STEP_LIMIT,
            at: clock(at_ms),
        }
    }

    fn recorded(before: Value, after: Value) -> EditHistory {
        record(None, &before, &after, how(0, None))
    }

    #[test]
    fn a_step_holds_only_what_moved() {
        let history = recorded(
            json!({ "exposure": 0.0, "contrast": 10, "blacks": 0 }),
            json!({ "exposure": 0.5, "contrast": 10, "blacks": 0 }),
        );
        let changed = history.entries[0].changed.as_ref().unwrap();
        assert_eq!(changed.len(), 1, "{changed:?}");
        assert_eq!(changed["exposure"], json!([0.0, 0.5]));
    }

    #[test]
    fn the_base_is_where_the_photo_was_before_the_first_step() {
        let history = recorded(json!({ "exposure": 0.2 }), json!({ "exposure": 0.9 }));
        assert_eq!(history.base, json!({ "exposure": 0.2 }));
        assert_eq!(states(&history).last().unwrap(), &json!({ "exposure": 0.9 }));
    }

    /// A number that came back from the front end without its decimal point is
    /// the same number.
    ///
    /// Checked against the old behaviour rather than assumed: with a plain `!=`
    /// every one of these is a step, which is what filled a real history with
    /// "Lens Distortion Params" while the exposure changes hid among them.
    #[test]
    fn a_number_written_differently_is_not_a_change() {
        for (before, after) in [
            (json!({ "kelvin": 4700.0 }), json!({ "kelvin": 4700 })),
            (json!({ "model": 1 }), json!({ "model": 1.0 })),
            (json!({ "p": { "model": 1, "k1": 0.5 } }), json!({ "p": { "model": 1.0, "k1": 0.5 } })),
            (json!({ "p": [1, 2.0] }), json!({ "p": [1.0, 2] })),
        ] {
            assert!(
                changed_between(&before, &after).is_empty(),
                "{before} and {after} are the same numbers"
            );
        }
    }

    /// And a number that really moved still moves.
    #[test]
    fn a_number_that_really_moved_is_still_a_change() {
        for (before, after) in [
            (json!({ "kelvin": 4700 }), json!({ "kelvin": 4701 })),
            (json!({ "p": { "model": 1 } }), json!({ "p": { "model": 2 } })),
            (json!({ "p": [1, 2] }), json!({ "p": [1, 3] })),
            (json!({ "p": [1, 2] }), json!({ "p": [1] })),
            (json!({ "p": { "a": 1 } }), json!({ "p": { "a": 1, "b": 2 } })),
        ] {
            assert!(
                !changed_between(&before, &after).is_empty(),
                "{before} and {after} are different"
            );
        }
    }

    #[test]
    fn a_write_that_changed_nothing_is_not_a_step() {
        let same = json!({ "exposure": 0.2 });
        assert!(recorded(same.clone(), same).entries.is_empty());
    }

    /// A key a preset introduces is a change, with nothing on the near side.
    #[test]
    fn a_key_that_was_not_there_before_still_counts() {
        let history = recorded(json!({}), json!({ "exposure": 0.5 }));
        assert_eq!(history.entries[0].changed.as_ref().unwrap()["exposure"], json!([null, 0.5]));
    }

    #[test]
    fn a_run_on_one_slider_stays_one_step_and_keeps_where_it_started() {
        let mut history = recorded(json!({ "exposure": 0.0 }), json!({ "exposure": 0.1 }));
        // A second apart, ten times over, which is a slow drag and one move.
        for (index, value) in [0.2, 0.3, 0.4].into_iter().enumerate() {
            history = record(
                Some(history),
                &json!({ "exposure": value - 0.1 }),
                &json!({ "exposure": value }),
                how(1000 * (index as i64 + 1), None),
            );
        }
        assert_eq!(history.entries.len(), 1, "{:?}", history.entries);
        assert_eq!(
            history.entries[0].changed.as_ref().unwrap()["exposure"],
            json!([0.0, 0.4]),
            "the step must read from where the run started to where it ended"
        );
    }

    #[test]
    fn a_pause_longer_than_the_window_starts_a_new_step() {
        let history = recorded(json!({ "exposure": 0.0 }), json!({ "exposure": 0.4 }));
        let paused = record(
            Some(history.clone()),
            &json!({ "exposure": 0.4 }),
            &json!({ "exposure": 0.6 }),
            how(COALESCE_WINDOW.num_milliseconds() + 1, None),
        );
        assert_eq!(paused.entries.len(), 2, "a pause is a decision");

        let inside = record(
            Some(history),
            &json!({ "exposure": 0.4 }),
            &json!({ "exposure": 0.6 }),
            how(COALESCE_WINDOW.num_milliseconds(), None),
        );
        assert_eq!(inside.entries.len(), 1, "and one just inside it is not");
    }

    #[test]
    fn a_different_adjustment_starts_a_new_step_however_fast() {
        let history = recorded(
            json!({ "exposure": 0.0, "contrast": 0 }),
            json!({ "exposure": 0.4, "contrast": 0 }),
        );
        let history = record(
            Some(history),
            &json!({ "exposure": 0.4, "contrast": 0 }),
            &json!({ "exposure": 0.4, "contrast": 20 }),
            how(10, None),
        );
        assert_eq!(history.entries.len(), 2, "a change of tool is a change of mind");
    }

    /// A timestamp nothing can read is not evidence that a run is still going.
    #[test]
    fn an_unreadable_timestamp_starts_a_new_step() {
        let mut history = recorded(json!({ "exposure": 0.0 }), json!({ "exposure": 0.4 }));
        history.entries[0].at = "the other day".to_string();
        let history = record(
            Some(history),
            &json!({ "exposure": 0.4 }),
            &json!({ "exposure": 0.6 }),
            how(10, None),
        );
        assert_eq!(history.entries.len(), 2);
    }

    #[test]
    fn a_run_that_ended_where_it_began_did_not_happen() {
        let history = recorded(json!({ "exposure": 0.0 }), json!({ "exposure": 0.4 }));
        let history = record(
            Some(history),
            &json!({ "exposure": 0.4 }),
            &json!({ "exposure": 0.0 }),
            how(1000, None),
        );
        assert!(history.entries.is_empty(), "{:?}", history.entries);
    }

    #[test]
    fn a_named_step_neither_joins_nor_is_joined() {
        let history = recorded(json!({ "exposure": 0.0 }), json!({ "exposure": 0.4 }));
        let history = record(
            Some(history),
            &json!({ "exposure": 0.4 }),
            &json!({ "exposure": 0.0 }),
            how(1000, Some("Reset")),
        );
        assert_eq!(history.entries.len(), 2);
        let history = record(
            Some(history),
            &json!({ "exposure": 0.0 }),
            &json!({ "exposure": 0.2 }),
            how(2000, None),
        );
        assert_eq!(history.entries.len(), 3, "nothing joins a named step");
    }

    // ======== BLITZRAW: a view setting is not an edit ========

    #[test]
    fn opening_a_section_is_not_a_step() {
        // The Adjustments panel keeps which accordions are open per photo, among
        // the adjustments. Opening one used to add a step that changed nothing
        // anybody could see, and a run of opening and closing them filled the list.
        let history = record(
            None,
            &json!({ "exposure": 1.0, "sectionVisibility": { "basic": true } }),
            &json!({ "exposure": 1.0, "sectionVisibility": { "basic": false } }),
            how(0, None),
        );
        assert!(history.entries.is_empty(), "a view setting is not something done to the photo");
    }

    #[test]
    fn turning_the_clipping_warning_on_is_not_a_step() {
        let history = record(
            None,
            &json!({ "exposure": 1.0, "showClipping": false }),
            &json!({ "exposure": 1.0, "showClipping": true }),
            how(0, None),
        );
        assert!(history.entries.is_empty());
    }

    #[test]
    fn a_real_edit_beside_a_view_setting_is_still_a_step() {
        // And the view setting is left out of it, so the step reads as the one
        // thing that actually moved.
        let history = record(
            None,
            &json!({ "exposure": 0.0, "showClipping": false }),
            &json!({ "exposure": 1.0, "showClipping": true }),
            how(0, None),
        );
        assert_eq!(history.entries.len(), 1);
        let changed = history.entries[0].changed.as_ref().unwrap();
        assert!(changed.contains_key("exposure"));
        assert!(!changed.contains_key("showClipping"), "the view setting is not part of the step");
    }

    // ====== BLITZRAW END: a view setting is not an edit ======

    // ======== BLITZRAW: the bookmark ========

    #[test]
    fn every_step_gets_its_own_number_and_the_bookmark_follows() {
        let mut h = record(None, &json!({ "exposure": 0.0 }), &json!({ "exposure": 1.0 }), how(0, None));
        assert_eq!(h.entries[0].n, 1);
        assert_eq!(h.at, 1, "a new step is where the photo now is");
        h = record(Some(h), &json!({ "exposure": 1.0 }), &json!({ "contrast": 5.0 }), how(60_000, None));
        assert_eq!(h.entries[1].n, 2);
        assert_eq!(h.at, 2);
    }

    #[test]
    fn an_undo_moves_the_bookmark_and_writes_nothing() {
        let mut h = record(None, &json!({ "exposure": 0.0 }), &json!({ "exposure": 1.0 }), how(0, None));
        h = record(Some(h), &json!({ "exposure": 1.0 }), &json!({ "exposure": 2.0 }), how(60_000, None));
        let before = h.entries.clone();

        let landed = go_to(&mut h, 1).unwrap();
        assert_eq!(landed.state["exposure"], json!(1.0));
        assert_eq!(h.at, 1, "the bookmark moved");
        assert_eq!(h.entries, before, "and nothing in the log did");
    }

    #[test]
    fn a_redo_is_the_same_move_the_other_way() {
        let mut h = record(None, &json!({ "exposure": 0.0 }), &json!({ "exposure": 1.0 }), how(0, None));
        h = record(Some(h), &json!({ "exposure": 1.0 }), &json!({ "exposure": 2.0 }), how(60_000, None));
        go_to(&mut h, 1).unwrap();
        let landed = go_to(&mut h, 2).unwrap();
        assert_eq!(landed.state["exposure"], json!(2.0));
        assert_eq!(h.at, 2);
    }

    #[test]
    fn the_base_is_a_real_place_to_go_back_to() {
        let mut h = record(None, &json!({ "exposure": 0.0 }), &json!({ "exposure": 1.0 }), how(0, None));
        let landed = go_to(&mut h, 0).unwrap();
        assert_eq!(landed.state["exposure"], json!(0.0), "all the way back to where it started");
        assert_eq!(h.at, 0);
    }

    #[test]
    fn an_edit_after_an_undo_drops_what_was_above_and_never_reuses_the_number() {
        let mut h = record(None, &json!({ "exposure": 0.0 }), &json!({ "exposure": 1.0 }), how(0, None));
        h = record(Some(h), &json!({ "exposure": 1.0 }), &json!({ "exposure": 2.0 }), how(60_000, None));
        h = record(Some(h), &json!({ "exposure": 2.0 }), &json!({ "exposure": 3.0 }), how(120_000, None));
        go_to(&mut h, 1).unwrap();

        h = record(Some(h), &json!({ "exposure": 1.0 }), &json!({ "exposure": 9.0 }), how(180_000, None));
        assert_eq!(h.entries.len(), 2, "the path nobody took is gone");
        assert_eq!(h.entries[1].n, 4, "and the new step gets a number never used before");
        assert_eq!(h.at, 4);
        assert!(
            matches!(state_at(Some(&h), 2), Err(CannotGo::NotHere)),
            "a reference to a dropped step reads as gone, not as some later step"
        );
    }

    #[test]
    fn folding_moves_the_base_and_leaves_the_other_numbers_alone() {
        // The property the whole design rests on. If numbers shifted when the
        // oldest steps folded away, every reference the application holds would
        // quietly come to mean something else.
        let mut h: Option<EditHistory> = None;
        for step in 1..=(MIN_STEP_LIMIT + 5) {
            h = Some(record(
                h,
                &json!({ "exposure": (step - 1) as f64 }),
                &json!({ "exposure": step as f64 }),
                Recording {
                    label: None,
                    action: None,
                    limit: MIN_STEP_LIMIT,
                    at: clock(step as i64 * 60_000),
                },
            ));
        }
        let h = h.unwrap();
        assert_eq!(h.entries.len(), MIN_STEP_LIMIT);
        let top = (MIN_STEP_LIMIT + 5) as u64;
        assert_eq!(h.entries.last().unwrap().n, top, "the newest keeps its number");
        assert_eq!(h.at, top, "and the photo is sitting on it");
        assert_eq!(h.base_n, top - MIN_STEP_LIMIT as u64, "the base stands where the last folded step stood");
        assert!(matches!(state_at(Some(&h), 1), Err(CannotGo::NotHere)), "what folded away is gone");
        assert!(state_at(Some(&h), h.base_n).is_ok(), "and the base itself is reachable");
    }

    #[test]
    fn a_number_this_photo_never_had_is_refused_rather_than_guessed_at() {
        let h = record(None, &json!({ "exposure": 0.0 }), &json!({ "exposure": 1.0 }), how(0, None));
        assert!(matches!(state_at(Some(&h), 99), Err(CannotGo::NotHere)));
        assert!(matches!(state_at(None, 1), Err(CannotGo::NoHistory)));
    }

    #[test]
    fn a_run_on_one_slider_keeps_one_number() {
        // Joining is what makes ten nudges one step. It must not hand out ten
        // numbers, or the application would hold ten entries for one move.
        let mut h = record(None, &json!({ "exposure": 0.0 }), &json!({ "exposure": 0.5 }), how(0, None));
        h = record(Some(h), &json!({ "exposure": 0.5 }), &json!({ "exposure": 1.0 }), how(200, None));
        assert_eq!(h.entries.len(), 1);
        assert_eq!(h.entries[0].n, 1);
        assert_eq!(h.at, 1);
        assert_eq!(h.next_n, 2, "and no number was spent on the join");
    }

    #[test]
    fn a_log_written_before_numbers_existed_still_works() {
        // Files on disk have no numbers and no bookmark. They must read, and the
        // first step written after that must start numbering from the top.
        let older = r#"{"version":1,"base":{"exposure":0.0},
            "entries":[{"at":"2026-08-26T10:00:00+00:00","changed":{"exposure":[0.0,0.5]}}]}"#;
        let read: EditHistory = serde_json::from_str(older).unwrap();
        assert_eq!(read.at, 0);
        assert_eq!(read.next_n, 0);
        assert_eq!(bookmark(Some(&read)), 0, "its one step is unnumbered, so it reads as the base");

        let grown = record(
            Some(read),
            &json!({ "exposure": 0.5 }),
            &json!({ "contrast": 3.0 }),
            how(600_000, None),
        );
        assert_eq!(grown.entries.last().unwrap().n, 1);
        assert_eq!(grown.at, 1);
    }

    // ====== BLITZRAW END: the bookmark ======

    // ======== BLITZRAW: one action is one step, and two are two ========

    #[test]
    fn two_actions_on_one_slider_are_two_steps_however_fast() {
        // The fault this field exists to fix. Two deliberate presses on the
        // same slider inside the window used to become a single step, so one
        // undo took back both of them.
        let mut history = record(
            None,
            &json!({ "exposure": 0.0 }),
            &json!({ "exposure": 0.5 }),
            how_action(0, Some("act-1")),
        );
        history = record(
            Some(history),
            &json!({ "exposure": 0.5 }),
            &json!({ "exposure": 1.0 }),
            how_action(200, Some("act-2")),
        );
        assert_eq!(
            history.entries.len(),
            2,
            "two things the user did are two steps, whatever the clock says"
        );
    }

    #[test]
    fn one_action_on_one_slider_is_still_one_step() {
        // The other half. A run of presses inside one action is one move, which
        // is the whole point of the coalescing rule.
        let mut history = record(
            None,
            &json!({ "exposure": 0.0 }),
            &json!({ "exposure": 0.5 }),
            how_action(0, Some("act-1")),
        );
        history = record(
            Some(history),
            &json!({ "exposure": 0.5 }),
            &json!({ "exposure": 1.0 }),
            how_action(200, Some("act-1")),
        );
        assert_eq!(history.entries.len(), 1);
        let changed = history.entries[0].changed.as_ref().unwrap();
        assert_eq!(
            changed["exposure"],
            json!([0.0, 1.0]),
            "the step keeps the value it opened with"
        );
    }

    #[test]
    fn a_change_with_no_action_behaves_exactly_as_it_used_to() {
        // Nothing that worked before this field existed may change. Every other
        // test in this module relies on it.
        let mut history = record(
            None,
            &json!({ "exposure": 0.0 }),
            &json!({ "exposure": 0.5 }),
            how(0, None),
        );
        history = record(
            Some(history),
            &json!({ "exposure": 0.5 }),
            &json!({ "exposure": 1.0 }),
            how(200, None),
        );
        assert_eq!(history.entries.len(), 1);
        assert_eq!(history.entries[0].action, None);
    }

    #[test]
    fn an_action_is_stored_so_the_photos_it_touched_can_be_found() {
        let history = record(
            None,
            &json!({ "exposure": 0.0 }),
            &json!({ "exposure": 0.5 }),
            how_action(0, Some("act-7")),
        );
        assert_eq!(history.entries[0].action.as_deref(), Some("act-7"));
    }

    #[test]
    fn a_step_that_belongs_to_an_action_does_not_absorb_a_loose_change() {
        // A nudge from the grid carries no action today. It must not be
        // swallowed into a step that belongs to something the editor did, or
        // undoing that action would take the nudge with it.
        let mut history = record(
            None,
            &json!({ "exposure": 0.0 }),
            &json!({ "exposure": 0.5 }),
            how_action(0, Some("act-1")),
        );
        history = record(
            Some(history),
            &json!({ "exposure": 0.5 }),
            &json!({ "exposure": 1.0 }),
            how(200, None),
        );
        assert_eq!(history.entries.len(), 2);
    }

    #[test]
    fn an_action_is_written_and_read_back_unchanged() {
        // The field is optional on both sides, so a round trip is the only
        // proof that it survives a write and a read.
        let history = record(
            None,
            &json!({ "exposure": 0.0 }),
            &json!({ "exposure": 0.5 }),
            how_action(0, Some("act-9")),
        );
        let text = serde_json::to_string(&history).unwrap();
        assert!(text.contains("act-9"), "the action has to reach the file");
        let read: EditHistory = serde_json::from_str(&text).unwrap();
        assert_eq!(read, history);
    }

    #[test]
    fn a_log_written_before_this_field_existed_still_reads() {
        // The lenient reader already covers a log it cannot parse at all. This
        // covers the ordinary case: an older log has no action on any step, and
        // must come back as a real history rather than as nothing.
        let older = r#"{"version":1,"base":{"exposure":0.0},
            "entries":[{"at":"2026-08-26T10:00:00+00:00","changed":{"exposure":[0.0,0.5]}}]}"#;
        let read: EditHistory = serde_json::from_str(older).unwrap();
        assert_eq!(read.entries.len(), 1);
        assert_eq!(read.entries[0].action, None);
    }

    // ====== BLITZRAW END: one action is one step, and two are two ======

    #[test]
    fn every_state_can_be_rebuilt_from_the_log() {
        let mut history = recorded(json!({ "exposure": 0.0, "contrast": 0 }), json!({ "exposure": 0.5, "contrast": 0 }));
        history = record(
            Some(history),
            &json!({ "exposure": 0.5, "contrast": 0 }),
            &json!({ "exposure": 0.5, "contrast": 20 }),
            how(60_000, None),
        );
        assert_eq!(
            states(&history),
            vec![
                json!({ "exposure": 0.0, "contrast": 0 }),
                json!({ "exposure": 0.5, "contrast": 0 }),
                json!({ "exposure": 0.5, "contrast": 20 }),
            ]
        );
    }

    #[test]
    fn the_oldest_steps_fold_into_the_base_rather_than_vanishing() {
        let mut history: Option<EditHistory> = None;
        for step in 1..=(MIN_STEP_LIMIT + 5) {
            let before = json!({ "exposure": (step - 1) as f64 });
            let after = json!({ "exposure": step as f64 });
            history = Some(record(
                history,
                &before,
                &after,
                Recording {
                    label: None,
                    action: None,
                    limit: MIN_STEP_LIMIT,
                    at: clock(step as i64 * 60_000),
                },
            ));
        }
        let history = history.unwrap();
        assert_eq!(history.entries.len(), MIN_STEP_LIMIT);
        // The earliest reachable point is still a real state of the photo.
        assert_eq!(history.base, json!({ "exposure": 5.0 }));
        assert_eq!(
            states(&history).last().unwrap(),
            &json!({ "exposure": (MIN_STEP_LIMIT + 5) as f64 })
        );
    }

    #[test]
    fn a_pinned_step_is_never_folded_away() {
        let mut history = pin(None, &json!({ "exposure": 0.0 }), "Exported", "export", clock(0));
        for step in 1..=(MIN_STEP_LIMIT + 5) {
            history = record(
                Some(history),
                &json!({ "exposure": (step - 1) as f64 }),
                &json!({ "exposure": step as f64 }),
                Recording {
                    label: None,
                    action: None,
                    limit: MIN_STEP_LIMIT,
                    at: clock(step as i64 * 60_000),
                },
            );
        }
        assert_eq!(
            history.entries[0].pin.as_deref(),
            Some("export"),
            "the pin has to still be the oldest thing there"
        );
        assert!(
            history.entries.len() > MIN_STEP_LIMIT,
            "a pin in front of the limit holds the log open rather than being dropped"
        );
    }

    #[test]
    fn a_pinned_state_survives_the_base_being_folded_past_it() {
        let history = pin(None, &json!({ "exposure": 7.0 }), "Exported", "export", clock(0));
        let history = record(
            Some(history),
            &json!({ "exposure": 7.0 }),
            &json!({ "exposure": 8.0 }),
            how(60_000, None),
        );
        assert_eq!(states(&history)[1], json!({ "exposure": 7.0 }));
        assert_eq!(states(&history)[2], json!({ "exposure": 8.0 }));
    }

    #[test]
    fn a_log_from_another_version_is_replaced_rather_than_guessed_at() {
        let stale = EditHistory {
            version: HISTORY_VERSION + 1,
            base: json!({ "exposure": 99.0 }),
            entries: vec![],
            at: 7,
            base_n: 7,
            next_n: 8,
        };
        let history = record(
            Some(stale),
            &json!({ "exposure": 0.0 }),
            &json!({ "exposure": 0.5 }),
            how(0, None),
        );
        assert_eq!(history.version, HISTORY_VERSION);
        assert_eq!(history.base, json!({ "exposure": 0.0 }));
    }

    /// Writes a log for the editor's own checks to read.
    ///
    /// The two sides have to agree on this format exactly: this writes it and
    /// `historyFromLog` in `utils/editHistory.ts` reads it. Agreeing by
    /// inspection is how a format drifts, so a real log made by this code goes
    /// out to a file with the states it means, and the other side asserts it
    /// rebuilds them. Env-gated on `RAPIDRAW_TEST_OUT`, so an ordinary run
    /// skips it.
    #[test]
    fn write_a_log_for_the_editor_to_read_back() {
        let Ok(out) = std::env::var("RAPIDRAW_TEST_OUT") else {
            return;
        };
        let mut history = record(
            None,
            &json!({ "exposure": 0.0, "contrast": 0, "blacks": 0 }),
            &json!({ "exposure": 0.4, "contrast": 0, "blacks": 0 }),
            how(0, None),
        );
        // A run that joins, so the step reads from where it started.
        history = record(
            Some(history),
            &json!({ "exposure": 0.4, "contrast": 0, "blacks": 0 }),
            &json!({ "exposure": 0.7, "contrast": 0, "blacks": 0 }),
            how(1000, None),
        );
        // A different slider, so a new step.
        history = record(
            Some(history),
            &json!({ "exposure": 0.7, "contrast": 0, "blacks": 0 }),
            &json!({ "exposure": 0.7, "contrast": 25, "blacks": 0 }),
            how(2000, None),
        );
        // A named one.
        history = record(
            Some(history),
            &json!({ "exposure": 0.7, "contrast": 25, "blacks": 0 }),
            &json!({ "exposure": 0.0, "contrast": 0, "blacks": 0 }),
            how(9000, Some("Reset")),
        );
        // And a pin, which carries a whole state.
        history = pin(
            Some(history),
            &json!({ "exposure": 0.0, "contrast": 0, "blacks": 0 }),
            "Exported",
            "export",
            clock(10_000),
        );

        let fixture = json!({ "log": history, "states": states(&history) });
        let path = std::path::Path::new(&out).join("history-fixture.json");
        let _ = std::fs::create_dir_all(&out);
        std::fs::write(&path, serde_json::to_string_pretty(&fixture).unwrap()).expect("write");
        eprintln!("wrote {}", path.display());
    }

    /// Every sidecar written before histories existed has to keep working.
    #[test]
    fn a_sidecar_with_no_history_still_reads() {
        let json = r#"{ "version": 1, "rating": 3, "adjustments": { "exposure": 0.4 } }"#;
        let metadata: crate::image_processing::ImageMetadata =
            serde_json::from_str(json).expect("a sidecar from before histories");
        assert!(metadata.history.is_none());
        assert_eq!(metadata.adjustments, json!({ "exposure": 0.4 }));
        assert_eq!(metadata.rating, 3);
    }

    /// And a history nothing can read must not take the photo's edits with it.
    ///
    /// `load_sidecar` falls back to a default `ImageMetadata` when a file will
    /// not parse, so without the lenient reader a malformed log would lose the
    /// adjustments beside it, which is the whole photo.
    #[test]
    fn a_history_that_cannot_be_read_costs_only_the_history() {
        for bad in [
            r#""not an object""#,
            r#"{ "version": "one" }"#,
            r#"{ "version": 1, "entries": "several" }"#,
            r#"42"#,
        ] {
            let json = format!(
                r#"{{ "version": 1, "rating": 2, "adjustments": {{ "exposure": 0.4 }}, "history": {bad} }}"#
            );
            let metadata: crate::image_processing::ImageMetadata =
                serde_json::from_str(&json).unwrap_or_else(|e| panic!("{bad} should not fail: {e}"));
            assert!(metadata.history.is_none(), "{bad}");
            assert_eq!(metadata.adjustments, json!({ "exposure": 0.4 }), "{bad}");
            assert_eq!(metadata.rating, 2, "{bad}");
        }
    }

    /// A real log survives the trip through a sidecar and back.
    #[test]
    fn a_history_round_trips_through_the_sidecar() {
        let history = recorded(json!({ "exposure": 0.0 }), json!({ "exposure": 0.4 }));
        let mut metadata = crate::image_processing::ImageMetadata::default();
        metadata.adjustments = json!({ "exposure": 0.4 });
        metadata.history = Some(history.clone());

        let written = serde_json::to_string_pretty(&metadata).expect("write");
        let read: crate::image_processing::ImageMetadata =
            serde_json::from_str(&written).expect("read");
        assert_eq!(read.history, Some(history));
    }

    #[test]
    fn the_limit_is_held_inside_what_is_sensible() {
        assert_eq!(usable_step_limit(None), DEFAULT_STEP_LIMIT);
        assert_eq!(usable_step_limit(Some(1)), MIN_STEP_LIMIT);
        assert_eq!(usable_step_limit(Some(999_999)), MAX_STEP_LIMIT);
        assert_eq!(usable_step_limit(Some(250)), 250);
    }
}
