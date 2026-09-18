import { Adjustments, normalizeLoadedAdjustments } from './adjustments';

/**
 * Remembering each photo's edit history for as long as the application is open.
 *
 * History used to be one list that was thrown away and started again every time
 * a different photo was opened, so stepping back through what you had just done
 * to a frame was impossible the moment you glanced at the next one. It is one
 * list per photo now.
 *
 * Keyed by the path the editor uses, which already carries `?vc=` for a virtual
 * copy, so a copy and the photo it came from keep separate histories without
 * anything here knowing what a virtual copy is.
 *
 * # The rule: history is only ever added to
 *
 * Nothing automatic removes an entry. Not resetting a photo, not pasting a
 * preset over it, not another window rewriting its sidecar. Those are things
 * that happened to the photo, so they are **steps**, and the whole point of a
 * history is to be able to walk back out of one. Trying three presets and
 * disliking all three has to leave you where you started, and it cannot do that
 * if applying one wipes the way back.
 *
 * The first version of this got that wrong: it dropped a remembered history
 * whenever the photo turned up in a state that history did not lead to. That is
 * exactly backwards. A photo in an unexpected state does not mean the history is
 * worthless; it means something happened that the history has not recorded yet.
 * So `historyOnOpen` **appends** that state instead.
 *
 * The only things that shorten a history are the per-photo step limit, which
 * drops the oldest, and an explicit request from the user, which does not exist
 * yet and will want confirming twice when it does.
 *
 * # Every photo, not a recent few
 *
 * There is no limit on how many photos keep a history. A session that touches
 * six hundred frames keeps six hundred histories, because a history you might
 * not get back to is still a history. The cost is watched at the other end
 * instead: fewer steps recorded per photo, and eventually the log living in the
 * sidecar rather than in memory.
 */

/**
 * A history: the states, and a name for each.
 *
 * `labels` runs alongside `entries` and is always exactly as long. A name is
 * either something the action gave itself, like "Reset", or null for the
 * ordinary case where the toolbar works one out by comparing a state against
 * the one before it. Only actions that would read badly under that comparison
 * bother to name themselves: a reset changes forty things at once and reads as
 * a list of forty things.
 *
 * Two arrays rather than one array of pairs, because `entries` is read as a
 * list of `Adjustments` in several places and pairs would touch all of them.
 * The risk with two is that they drift apart, so every change to either goes
 * through `appendStep`, which is the only thing in this file that builds them
 * and always builds both.
 */
export interface HistoryList {
  entries: Array<Adjustments>;
  labels: Array<string | null>;
  /**
   * BLITZRAW: each entry's number in the photo's own file, or `UNNUMBERED`.
   *
   * The application's list of what I did is made of these numbers, so clicking
   * a row in the History panel can be recorded and taken back like anything
   * else. A step made this session has no number until the write comes back
   * from the backend saying which one it got.
   */
  numbers: Array<number>;
  index: number;
  /**
   * When the newest entry was last written, so a run of nudges on one slider
   * can join it rather than following it. Null means the step at the top is
   * closed and the next change starts a new one.
   *
   * One number rather than one per entry, because only the newest step is ever
   * open to being added to.
   */
  lastStepAt: number | null;
}

/** What `appendStep` needs beyond the state itself. */
export interface StepOptions {
  /** A name the action gave itself, or null to let the toolbar work one out. */
  label?: string | null;
  /** How many steps this photo keeps. */
  limit?: number;
  /** Now, in milliseconds. Passed in so the rule can be tested against a clock. */
  at?: number;
  /** When the newest entry was written. See `HistoryList.lastStepAt`. */
  lastStepAt?: number | null;
}

/** One photo's history, as it is put aside. */
export interface RememberedHistory extends HistoryList {
  key: string;
}

/**
 * How many steps one photo keeps before the oldest start to fall off.
 *
 * A hundred rather than the fifty it was, and settable, since what counts as
 * enough depends on how heavily a frame gets worked. It is a memory cost per
 * open photo, which is why it is offered as a setting rather than made
 * unlimited.
 */
export const DEFAULT_HISTORY_STEPS = 100;
export const MIN_HISTORY_STEPS = 20;
export const MAX_HISTORY_STEPS = 1000;

export function usableStepLimit(limit: unknown): number {
  if (typeof limit !== 'number' || !Number.isFinite(limit)) {
    return DEFAULT_HISTORY_STEPS;
  }
  return Math.round(Math.min(MAX_HISTORY_STEPS, Math.max(MIN_HISTORY_STEPS, limit)));
}

/**
 * Whether two values are the same, whatever objects they are held in.
 *
 * Not `JSON.stringify`, which is sensitive to the order keys happen to be in: a
 * value that came back from a sidecar has them sorted and one built here has
 * them in the order they were written, and those are the same value.
 *
 * Lives here rather than beside its user because both the history reader and
 * the step namer need it and two of them would be one too many.
 */
export function sameValue(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (a === null || b === null || typeof a !== 'object' || typeof b !== 'object') return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  if (Array.isArray(a) && Array.isArray(b)) {
    return a.length === b.length && a.every((item, index) => sameValue(item, b[index]));
  }
  const names = Object.keys(a as object);
  if (names.length !== Object.keys(b as object).length) return false;
  return names.every(
    (name) =>
      Object.prototype.hasOwnProperty.call(b, name) &&
      sameValue((a as Record<string, unknown>)[name], (b as Record<string, unknown>)[name]),
  );
}

/** Whether two adjustment states are the same, by value. */
/**
 * BLITZRAW: settings that live among the adjustments but are not edits.
 *
 * Both of these are kept per photo, which is right: a photo should reopen with
 * the sections you had open and the clipping warning as you left it. Neither is
 * something you did to the picture, so neither belongs in its history.
 *
 * Until this existed, opening an accordion in the Adjustments panel added a step
 * that changed nothing anybody could see, and a run of opening and closing them
 * filled the list. The same rule exists in `edit_history.rs`, because the front
 * end and the photo's own log have to agree on what a step is.
 */
export const NOT_AN_EDIT: ReadonlyArray<string> = ['sectionVisibility', 'showClipping'];

/** The same two states, judged only on the parts that are edits. */
export function sameEdits(a: Adjustments | null | undefined, b: Adjustments | null | undefined): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return changedKeys(a, b).length === 0;
}

/**
 * BLITZRAW: by value, never by the text of it.
 *
 * `JSON.stringify` compares the **order the keys happen to be written in**. A
 * crop built in the front end reads
 * `{"unit","x","y","width","height"}`; the same crop read back out of the
 * photo's own file reads `{"height","unit","width","x","y"}`, because the
 * backend writes its keys in alphabetical order. Identical rectangle, different
 * text, and everything comparing the text called it a change.
 *
 * That one mistake caused three separate faults: a step appeared in the history
 * whose name was "Adjustment", because the thing that decides what a step is
 * saw a change and the thing that names it did not; a photo said "Applied
 * elsewhere" on being opened, because its state on disk looked unlike the one
 * in memory; and a fan-out could carry a crop nobody had touched.
 *
 * `sameValue` compares structure and contents, so the order stops mattering.
 * The backend has always done it this way, in `same_value`.
 */
export function sameAdjustments(a: Adjustments | null | undefined, b: Adjustments | null | undefined): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return sameValue(a, b);
}

/**
 * How long a step stays open to being added to, in milliseconds.
 *
 * A run of nudges on one slider is one move and should undo in one press. A
 * change of slider is a change of mind and is worth its own line however fast
 * it happens. Those two together are the whole rule, and this is the only
 * number in it.
 *
 * Rolling rather than fixed: the window is measured from the **last** change in
 * the entry, not from when it opened. Ten nudges a second apart are one move
 * even though the move took ten seconds, which is what a slow drag actually
 * looks like. A pause longer than this ends it.
 */
export const COALESCE_WINDOW_MS = 2500;

/** Which adjustments differ between two states. */
export function changedKeys(
  before: Adjustments | null | undefined,
  after: Adjustments | null | undefined,
): Array<string> {
  const names = new Set([...Object.keys(before ?? {}), ...Object.keys(after ?? {})]);
  const moved: Array<string> = [];
  for (const name of names) {
    // BLITZRAW: a view setting is not something that was done to the photo.
    if (NOT_AN_EDIT.includes(name)) {
      continue;
    }
    const a = (before as any)?.[name];
    const b = (after as any)?.[name];
    // By value. See the note on `sameAdjustments`: comparing the text of it
    // made a crop read back from disk look unlike the same crop in memory.
    if (!sameValue(a, b)) {
      moved.push(name);
    }
  }
  return moved;
}

/**
 * Whether a change belongs to the step already at the top rather than to a new
 * one.
 *
 * Both conditions have to hold, and each earns its place:
 *
 * - **The same adjustments.** Every key this change touches is one the step at
 *   the top already touched. A key it has not touched is a different tool and a
 *   different intention.
 * - **Within the window.** A pause is a decision. Coming back to the same
 *   slider after ten seconds is a second look at it, not a continuation.
 *
 * Three things are never joined, whatever the clock says:
 *
 * - **The first entry**, which is the state the photo was opened at. Merging
 *   into it would leave nothing to go back to.
 * - **A named step**, either the one arriving or the one at the top. "Reset"
 *   and "Applied elsewhere" are deliberate events and deserve their own line.
 * - **A change made after stepping back.** `appendStep` has already dropped
 *   what was ahead by then, so the top is a real state, but the run that was
 *   being made ended when the undo happened.
 */
function joinsTheStepAtTheTop(
  entries: Array<Adjustments>,
  labels: Array<string | null>,
  next: Adjustments,
  label: string | null,
  at: number,
  lastStepAt: number | null,
  steppedBack: boolean,
): boolean {
  const top = entries.length - 1;
  if (top < 1 || steppedBack) {
    return false;
  }
  if (label !== null || labels[top] !== null) {
    return false;
  }
  if (lastStepAt === null) {
    return false;
  }
  const since = at - lastStepAt;
  if (!(since >= 0 && since <= COALESCE_WINDOW_MS)) {
    return false;
  }

  const alreadyTouched = changedKeys(entries[top - 1], entries[top]);
  const touchingNow = changedKeys(entries[top], next);
  if (touchingNow.length === 0 || alreadyTouched.length === 0) {
    return false;
  }
  return touchingNow.every((name) => alreadyTouched.includes(name));
}

/**
 * Adds a step, or extends the one at the top, dropping the oldest if the photo
 * is at its limit.
 *
 * Anything ahead of where you currently are is dropped first, which is ordinary
 * undo behaviour: step back three, do something new, and the three you stepped
 * back over are no longer reachable because they are no longer what happened.
 *
 * # A run of nudges is one step
 *
 * A slider is not moved once. It is moved twenty times over a few seconds until
 * it looks right, and every one of those used to be an entry, so the list read
 * as twenty lines saying "Exposure" and it took twenty presses to undo one
 * decision.
 *
 * A change that touches the same adjustments as the step at the top, within
 * `COALESCE_WINDOW_MS` of the last change in it, **replaces** that step rather
 * than following it. The entry below is untouched, so the step still reads from
 * where the run started to where it ended, and one press undoes the whole move.
 * Substeps are not kept at all: they cost nothing to store, nothing to draw and
 * nothing to read back.
 *
 * # Nothing is not a step
 *
 * A state identical to the one at the top is not recorded. It used to be, so
 * anything that re-applied what was already there added a line that undid to
 * exactly where it already was.
 */
export function appendStep(
  entries: Array<Adjustments>,
  labels: Array<string | null>,
  index: number,
  next: Adjustments,
  options: StepOptions = {},
  // BLITZRAW: the numbers beside them, when they are known. A new step has none
  // until the write comes back from the backend saying which one it was given.
  numbers: Array<number> = [],
): HistoryList {
  const { label = null, limit = DEFAULT_HISTORY_STEPS, at = Date.now(), lastStepAt = null } = options;

  // Anything ahead means undo was pressed, which ends whatever run was being
  // made. Worked out before the slice, which is what removes the evidence.
  const steppedBack = index < entries.length - 1;

  const keptEntries = entries.slice(0, index + 1);
  // Padded rather than assumed: a history that predates labels, or one read
  // from somewhere that did not write them, still lines up.
  const keptLabels = labels.slice(0, index + 1);
  while (keptLabels.length < keptEntries.length) keptLabels.push(null);
  const keptNumbers = numbers.slice(0, index + 1);
  while (keptNumbers.length < keptEntries.length) keptNumbers.push(UNNUMBERED);

  const top = keptEntries.length - 1;

  // BLITZRAW: `sameEdits`, not `sameAdjustments`. Opening an accordion changes
  // the adjustments and changes nothing about the photo, so it is not a step.
  if (top >= 0 && sameEdits(keptEntries[top], next)) {
    return { entries: keptEntries, labels: keptLabels, numbers: keptNumbers, index: top, lastStepAt };
  }

  if (joinsTheStepAtTheTop(keptEntries, keptLabels, next, label, at, lastStepAt, steppedBack)) {
    keptEntries[top] = next;
    // Rolling, so a slow drag stays one move for as long as it keeps moving.
    // The number does not change: joining does not spend one.
    return { entries: keptEntries, labels: keptLabels, numbers: keptNumbers, index: top, lastStepAt: at };
  }

  keptEntries.push(next);
  keptLabels.push(label);
  keptNumbers.push(UNNUMBERED);

  const overflow = Math.max(0, keptEntries.length - usableStepLimit(limit));
  return {
    entries: overflow > 0 ? keptEntries.slice(overflow) : keptEntries,
    labels: overflow > 0 ? keptLabels.slice(overflow) : keptLabels,
    numbers: overflow > 0 ? keptNumbers.slice(overflow) : keptNumbers,
    index: Math.min(keptEntries.length, usableStepLimit(limit)) - 1,
    lastStepAt: at,
  };
}

/**
 * Puts one photo's history aside.
 *
 * A history of a single entry is the state the photo was opened at with nothing
 * done to it, which is what opening it again would produce anyway.
 */
export function rememberHistory(
  remembered: Array<RememberedHistory>,
  key: string | null,
  entries: Array<Adjustments>,
  labels: Array<string | null>,
  index: number,
  numbers: Array<number> = [],
): Array<RememberedHistory> {
  if (!key || entries.length <= 1) {
    return remembered;
  }
  const without = remembered.filter((held) => held.key !== key);
  // Put aside closed, so coming back to a photo never adds to a step made
  // before you walked away from it. Leaving one is a decision like any pause.
  return [{ key, entries, labels, numbers, index, lastStepAt: null }, ...without];
}

/**
 * The history a photo should have the moment it is opened.
 *
 * Three cases, and none of them throws anything away:
 *
 * - Never seen this session: a history of one, the state it opened at.
 * - Seen, and opening exactly where it was left: carry on, on the same step,
 *   with everything ahead still there to redo.
 * - Seen, but opening somewhere else: something changed it while it was closed,
 *   most likely a preset or an adjustment applied across a selection. That is a
 *   step like any other and is appended, so the way back is still there.
 */
export function historyOnOpen(
  remembered: Array<RememberedHistory>,
  key: string | null,
  openingAt: Adjustments,
  limit: number = DEFAULT_HISTORY_STEPS,
  labelForChange: string | null = null,
): HistoryList {
  const held = key ? remembered.find((entry) => entry.key === key) : undefined;

  if (!held || held.entries.length === 0) {
    return { entries: [openingAt], labels: [null], numbers: [UNNUMBERED], index: 0, lastStepAt: null };
  }

  const index = Math.min(Math.max(held.index, 0), held.entries.length - 1);
  if (sameAdjustments(held.entries[index], openingAt)) {
    return {
      entries: held.entries,
      labels: held.labels ?? [],
      numbers: held.numbers ?? [],
      index,
      lastStepAt: null,
    };
  }

  // No `lastStepAt`, so what happened while the photo was closed always gets
  // its own line rather than joining whatever was last done by hand.
  return appendStep(
    held.entries,
    held.labels ?? [],
    index,
    openingAt,
    {
      label: labelForChange,
    limit,
  });
}

/**
 * A photo's history as it is stored in its sidecar.
 *
 * Written by the backend, never from here: whoever writes the adjustments
 * writes the step, in the same write, because two writers for one value is the
 * fault this project has already paid for once. See `edit_history.rs`. This
 * side only reads it.
 *
 * Deltas rather than snapshots, because a hundred whole states of a masked
 * photo is a megabyte and a shoot would carry hundreds of them.
 */
export interface StoredStep {
  at: string;
  /** BLITZRAW: this step's own number, unique within the photo. */
  n?: number;
  label?: string | null;
  /** Adjustment name to `[before, after]`. Absent on a pinned step. */
  changed?: Record<string, [unknown, unknown]> | null;
  /** Why this step is exempt from the limit: `export` or `manual`. */
  pin?: string | null;
  /** The whole state, on a pinned step. */
  state?: unknown;
}

export interface StoredHistory {
  version: number;
  base: unknown;
  entries: Array<StoredStep>;
  /** BLITZRAW: which step number this photo is sitting on. */
  at?: number;
  /** BLITZRAW: the number the base state stands at. */
  base_n?: number;
}

/** A step whose number is not known yet, because the write has not come back. */
export const UNNUMBERED = -1;

/** The format this reads. Anything else is treated as no history at all. */
export const STORED_HISTORY_VERSION = 1;

/** The "after" half of a step, as something to lay over a state. */
function afters(step: StoredStep): Partial<Adjustments> {
  const out: Record<string, unknown> = {};
  for (const [name, pair] of Object.entries(step.changed ?? {})) {
    if (Array.isArray(pair) && pair.length === 2) {
      out[name] = pair[1];
    }
  }
  return out as Partial<Adjustments>;
}

/**
 * The history a photo opens with, rebuilt from what is in its sidecar.
 *
 * The stored log is where the photo started and every step since. This walks it
 * forward into the whole states the editor works in, which is the only shape
 * undo, redo and the toolbar's list understand.
 *
 * # The photo's actual state is the truth, not the log
 *
 * If the log does not end where the photo actually is, something wrote the
 * sidecar without recording a step. That is appended rather than treated as a
 * reason to distrust the log, for the same reason `historyOnOpen` does it: a
 * photo in an unexpected state does not mean its history is worthless, it means
 * something happened that the history has not recorded yet.
 */
export function historyFromLog(
  log: StoredHistory | null | undefined,
  openingAt: Adjustments,
  limit: number = DEFAULT_HISTORY_STEPS,
): HistoryList {
  if (!log || log.version !== STORED_HISTORY_VERSION || !Array.isArray(log.entries)) {
    return { entries: [openingAt], labels: [null], numbers: [UNNUMBERED], index: 0, lastStepAt: null };
  }

  const entries: Array<Adjustments> = [normalizeLoadedAdjustments(log.base as any)];
  const labels: Array<string | null> = [null];
  // The base stands at its own number, so a jump all the way back is a real
  // place the photo can be told to go to.
  const numbers: Array<number> = [log.base_n ?? 0];

  for (const step of log.entries) {
    const previous = entries[entries.length - 1];
    const next =
      step.state != null
        ? normalizeLoadedAdjustments(step.state as any)
        : ({ ...previous, ...afters(step) } as Adjustments);
    // A step that moved nothing is not a step. Nothing writes one any more, but
    // logs written before numbers were compared by value rather than by how
    // they were spelled are full of them: a white balance of 4700 rewritten as
    // 4700.0 was recorded as a change on every save. Passing over one here
    // removes nothing from the file and puts nothing in the list that would
    // undo to where it already is.
    if (step.label == null && step.pin == null && sameValue(previous, next)) {
      continue;
    }
    entries.push(next);
    labels.push(step.label ?? null);
    numbers.push(step.n ?? UNNUMBERED);
  }

  // ============ BLITZRAW: open where the photo was left ============
  // Not at the newest step. The photo's file says which one it is sitting on,
  // so an undo made before walking away is still an undo when you come back,
  // with everything above it still there to redo.
  //
  // Before this, opening a photo always jumped to the top, and a photo whose
  // history came from disk was not even remembered in memory, so an undo was
  // quietly lost by arrowing to the next photo and back.
  const top = entries.length - 1;
  const bookmarked = log.at == null ? top : numbers.indexOf(log.at);
  const index = bookmarked >= 0 ? bookmarked : top;
  // ========== BLITZRAW END: open where the photo was left ==========

  if (!sameAdjustments(entries[index], openingAt)) {
    return appendStep(
      entries,
      labels,
      index,
      openingAt,
      { label: 'Applied elsewhere', limit },
      numbers,
    );
  }

  // Opened closed: nothing joins a step made before the photo was opened.
  return { entries, labels, numbers, index, lastStepAt: null };
}
