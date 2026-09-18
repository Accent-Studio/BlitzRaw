/**
 * BLITZRAW: what **I** did, in order. The application's own history.
 *
 * # The distinction this exists to hold
 *
 * There are two histories and they are not the same thing.
 *
 * - **A photo's history** is what was done to that one photo. It lives in the
 *   photo's own settings file, it survives a restart, and it is the truth.
 * - **The application's history** is what the person did, in order. It lives
 *   here, only for the session, and it holds **no values at all**.
 *
 * An entry says which photos an action touched and, for each of them, **the
 * number its bookmark moved from and the number it moved to**. Two numbers per
 * photo and nothing else. It does not say what changed. That is written per
 * photo, per adjustment, with the value before and after, and copying it here
 * would be a second record that can disagree with the first. The front end's
 * copy would be the one that is wrong, since it is taken before the write
 * rather than after it.
 *
 * So undoing an entry re-selects its photos and tells each one to put its own
 * bookmark back on its own number. Each of them looks up what that means in its
 * own file. See `edit_history.rs`.
 *
 * Two numbers rather than one, because once numbers have gaps "the one before"
 * is not always "one less", and because it makes clicking a step in the History
 * panel the identical mechanism: from 12, to 7. One rule covers everything.
 *
 * # Fanning out is not multi-undo
 *
 * They look alike and they are opposite. A fan-out sends the open photo's
 * values to every other photo and overwrites theirs. A real multi-undo steps
 * each photo back through its own history, so ten photos that started from ten
 * different exposures go back to ten different exposures.
 *
 * That is also why undo does not read the Copy and Paste tick boxes. Those
 * decide what **spreads** when a change is made. They have nothing to say about
 * putting things back, and reading them would mean that changing a tick box
 * after the fact quietly changed what an undo would restore.
 *
 * # What is not an entry
 *
 * Selecting photos, going back to the grid and changing a filter are recorded
 * **as context on the next entry**, never as entries of their own. Otherwise a
 * run of clicking around the grid would have to be walked back one press at a
 * time before reaching the edit that was actually wanted.
 */

export type ActionKind = 'adjustments' | 'ratings';

/** One photo, and the two numbers this action moved its bookmark between. */
export interface StepMoved {
  path: string;
  /** Where its bookmark was. An undo puts it back here. */
  from: number;
  /** Where this action left it. A redo puts it back here. */
  to: number;
}

export interface AppAction {
  /**
   * Groups the writes that make up one thing the person did.
   *
   * A change made in the editor is written for the open photo and sent to the
   * rest of the selection separately, and one action that moves two different
   * sliders is two steps in each photo. This is how those are recognised as one
   * entry. It never leaves the session and nothing is ever found by it: the
   * numbers below are the reference.
   */
  id: string;
  kind: ActionKind;
  /** What to show a person. */
  label: string;
  /** The photos this action actually wrote, and where each of them moved. */
  photos: Array<StepMoved>;
  /** What was selected when it happened, so an undo can put it back. */
  selection: Array<string>;
  /** The photo that was open, or null if this was done from the grid. */
  openPath: string | null;
  /** Whether it was done in the editor. An undo returns to the same view. */
  inEditor: boolean;
  at: number;
}

export interface ActionList {
  entries: Array<AppAction>;
  /**
   * How many entries have been done.
   *
   * Everything below this has happened; everything from here up has been undone
   * and is waiting for a redo. A new action throws the waiting ones away, which
   * is what every undo stack does and what makes the list linear.
   */
  index: number;
}

export const EMPTY_ACTION_LIST: ActionList = { entries: [], index: 0 };

/**
 * How many actions are worth keeping.
 *
 * Deep enough to walk out of a long wrong turn, and cheap: an entry is a few
 * hundred paths at worst, and the paths are the same strings the image list is
 * already holding.
 */
export const ACTION_LIST_LIMIT = 200;

/**
 * Records an action, or folds it into the one already open.
 *
 * **One action is one entry, however many photos or writes it took.** A change
 * made in the editor is saved for the open photo and sent to the rest of the
 * selection as two separate writes, and one action that moves two different
 * sliders is two steps in each photo. All of those carry the same name, so all
 * of them belong to one entry here, and the photos they touched are gathered.
 */
export function recordAction(
  list: ActionList,
  action: AppAction,
  limit: number = ACTION_LIST_LIMIT,
): ActionList {
  // BLITZRAW: a write that moved no photo's bookmark is not something I did.
  //
  // Recording one would throw away everything waiting to be redone, for a step
  // that never happened. The editor saves the open photo whenever it settles,
  // and it settles for a dozen reasons that are not edits.
  //
  // Ratings are exempt: they are not steps in a photo's edit history and have no
  // numbers to move between, so their own stack is the record and this list only
  // says when they happened relative to everything else.
  if (action.kind !== 'ratings' && action.photos.every((photo) => photo.from === photo.to)) {
    return list;
  }

  const done = list.entries.slice(0, list.index);
  const top = done.length - 1;

  if (top >= 0 && done[top].id === action.id) {
    const merged: AppAction = {
      ...done[top],
      // Later news about the same action, not a replacement for it.
      photos: gather(done[top].photos, action.photos),
      selection: action.selection.length ? action.selection : done[top].selection,
      label: action.label || done[top].label,
      at: action.at,
    };
    done[top] = merged;
    return { entries: done, index: done.length };
  }

  done.push(action);
  const overflow = Math.max(0, done.length - limit);
  const kept = overflow > 0 ? done.slice(overflow) : done;
  return { entries: kept, index: kept.length };
}

/**
 * Two batches of the same action, as one.
 *
 * A photo written twice by one action keeps **the number it started from** and
 * **the number it ended on**, so undoing the action takes back the whole of it
 * rather than the last half.
 */
function gather(a: Array<StepMoved>, b: Array<StepMoved>): Array<StepMoved> {
  if (b.length === 0) return a;
  const out = [...a];
  const where = new Map(out.map((moved, index) => [moved.path, index]));
  for (const moved of b) {
    const at = where.get(moved.path);
    if (at === undefined) {
      where.set(moved.path, out.length);
      out.push(moved);
    } else {
      out[at] = { path: moved.path, from: Math.min(out[at].from, moved.from), to: moved.to };
    }
  }
  return out;
}

/** The action a Ctrl+Z would take back, or null when there is nothing to undo. */
export function nextUndo(list: ActionList): AppAction | null {
  return list.index > 0 ? list.entries[list.index - 1] : null;
}

/** The action a Ctrl+Y would put back, or null. */
export function nextRedo(list: ActionList): AppAction | null {
  return list.index < list.entries.length ? list.entries[list.index] : null;
}

/** The list after an undo has been carried out. */
export function afterUndo(list: ActionList): ActionList {
  return list.index > 0 ? { entries: list.entries, index: list.index - 1 } : list;
}

/** The list after a redo has been carried out. */
export function afterRedo(list: ActionList): ActionList {
  return list.index < list.entries.length ? { entries: list.entries, index: list.index + 1 } : list;
}

/**
 * Forgets every action that touched a photo that no longer exists.
 *
 * An entry aimed at a deleted photo cannot be carried out, and half carrying it
 * out is worse than not offering it. Called when photos are removed.
 */
export function forgetPaths(list: ActionList, gone: Array<string>): ActionList {
  if (gone.length === 0) return list;
  const lost = new Set(gone);
  const entries: Array<AppAction> = [];
  let index = 0;
  list.entries.forEach((entry, at) => {
    const touched = entry.photos.some((moved) => lost.has(moved.path));
    if (touched) {
      return;
    }
    entries.push(entry);
    if (at < list.index) {
      index += 1;
    }
  });
  return { entries, index };
}
