import { Adjustments } from './adjustments';
import { sameValue } from './editHistory';

/**
 * Sending one photo's edits to the rest of the selection.
 *
 * # The damage this exists to prevent
 *
 * A crop was set on a selection of seven photos. The filter was then cleared,
 * two hundred photos were selected, and a white balance was moved. All two
 * hundred came back cropped to 16:9.
 *
 * The crop never went to two hundred photos on purpose. It was **waiting**. A
 * change is held for a moment before it is sent, so a run of slider moves
 * reaches the rest of the selection once instead of sixty times. The record of
 * what was waiting was then overwritten with the new, larger selection, while
 * the change inside it was still the crop. So the crop was quietly re-aimed at
 * photos it was never meant for.
 *
 * Two things made it worse than a narrow race. The wait is a trailing delay
 * with nothing calling time on it, and it was restarted by a dozen unrelated
 * things: a slider being touched, the scopes changing what they ask for, the
 * view changing. So the window was not seven hundred milliseconds, it was as
 * long as the user kept working. And the reference the change is measured from
 * is only moved forward when the send finally happens, so everything done in
 * that window accumulated into one delta.
 *
 * # The rule
 *
 * **A change belongs to the selection it was made for.** If the selection moves
 * on while something is waiting, the waiting change is sent to the photos it
 * was aimed at, before anything is recorded against the new ones. It is never
 * re-aimed.
 */

export interface PendingSync {
  /** The photo the change was made on. */
  path: string;
  /** The photos it is to be sent to, as they were when it was made. */
  paths: Array<string>;
  adjustments: Adjustments;
  /**
   * BLITZRAW: the one thing the user did that this fan-out is part of.
   *
   * Carried, never read here. It travels to every photo so that all of them,
   * and the one in the editor, can later be found again as one action. See
   * utils/actionId.ts.
   */
  actionId: string | null;
}

export interface SyncReference {
  path: string;
  adjustments: Adjustments;
}

/**
 * Whether two fan-outs are aimed at the same photos.
 *
 * By membership, not by order. Selecting the same photos a second time can
 * produce them in a different order, and treating that as a different selection
 * would send a change early for no reason.
 */
export function sameTargets(a: Array<string>, b: Array<string>): boolean {
  if (a.length !== b.length) {
    return false;
  }
  const held = new Set(a);
  return b.every((path) => held.has(path));
}

/**
 * What has moved on the open photo since the last fan-out, and may travel.
 *
 * Measured against a reference for the **same photo**. A reference belonging to
 * another photo is not a starting point for this one, so nothing travels: that
 * is deliberately a refusal rather than a guess, because guessing here is how a
 * crop reaches two hundred photos.
 *
 * Everything sent is an absolute value rather than an increment, which is what
 * makes a fan-out safe to supersede: a batch that a later one replaces has lost
 * nothing.
 */
export function syncDelta(
  reference: SyncReference | null | undefined,
  pending: PendingSync,
  includedKeys: Array<string>,
): Partial<Adjustments> {
  const delta: Partial<Adjustments> = {};
  if (!reference || reference.path !== pending.path) {
    return delta;
  }

  for (const key of Object.keys(pending.adjustments) as Array<keyof Adjustments>) {
    if (!includedKeys.includes(key as string)) {
      continue;
    }
    // BLITZRAW: by value, not by the text of it. A crop built here and the same
    // crop read back out of a photo's file differ only in the order their keys
    // are written, and comparing the text put it in the delta and sent it to
    // every other photo. See the note in editHistory.ts.
    if (!sameValue(pending.adjustments[key], reference.adjustments[key])) {
      (delta as any)[key] = pending.adjustments[key];
    }
  }
  return delta;
}

/**
 * What should happen to the rest of the selection, given everything going on.
 *
 * Written as one function returning one answer because it has been got wrong in
 * six different ways, each of them a guard added to a different branch. The
 * whole rule is here, and the four answers are exhaustive:
 *
 * - `none`: there is nothing open, so there is nothing to say.
 * - `advance`: move the reference forward and send nothing. This is the answer
 *   whenever a change should not travel but has still happened, and getting it
 *   wrong is what let a crop accumulate into a later, unrelated fan-out.
 * - `hold`: record what would be sent and to whom, but do not send it yet.
 * - `send`: record it and send it.
 *
 * **Hold records its targets.** That is the difference between this and what it
 * replaced. A hold used to record nothing at all, so when the hold ended the
 * change was aimed at whatever happened to be selected by then. Typing a number
 * into a field can hold for minutes.
 */
export type SyncPlan =
  | { kind: 'none' }
  | { kind: 'advance' }
  | { kind: 'hold'; paths: Array<string> }
  | { kind: 'send'; paths: Array<string> };

export interface SyncSituation {
  autoSyncOn: boolean;
  /**
   * Whether the editor is showing rather than the grid.
   *
   * Recorded, and no longer refused on. See `autoSyncPlan`.
   */
  inEditor: boolean;
  openPath: string | null;
  /** The selection with stacks resolved, including the open photo if it is in it. */
  resolvedTargets: Array<string>;
  /** This state arrived from an undo, a redo or a click in the history list. */
  fromHistoryMove: boolean;
  /** A number is being typed into a field, arriving one character at a time. */
  typing: boolean;
  /** A LUT is being hovered, so what is on screen is not this photo's state. */
  previewing: boolean;
}

export function autoSyncPlan(situation: SyncSituation): SyncPlan {
  const { openPath, resolvedTargets } = situation;
  if (!openPath) {
    return { kind: 'none' };
  }

  // Walking back through a photo's own history is the opposite of an edit to
  // spread. Undoing a fan-out would otherwise fan the undone state out again,
  // which turns recovering from a mistake into repeating it.
  if (situation.fromHistoryMove) {
    return { kind: 'advance' };
  }

  if (!situation.autoSyncOn) {
    return { kind: 'advance' };
  }

  // A photo can be open behind the grid, or left open while a range that does
  // not contain it is selected elsewhere. Editing it should not write photos it
  // is not one of: that is the rule pasting already implies.
  //
  // ============ BLITZRAW: and this is the guard that matters ============
  // There used to be a second one above it refusing to send anything at all
  // from the grid, on the grounds that the grid is for choosing photos rather
  // than editing one into the others. That is a matter of taste rather than
  // safety, and it is the wrong taste: selecting five photos in the grid and
  // moving white balance should move all five, which is what everyone expects
  // and what the nudge already did.
  //
  // The two are not equally load bearing. This one is what stops a change
  // reaching photos it was not aimed at, and it holds just as well in the grid:
  // clicking a photo there makes it the open one **and keeps it inside the
  // selection**, so an edit made in the grid always has the open photo among
  // its targets. A photo left open behind a selection that does not contain it
  // still sends nothing.
  // ========== BLITZRAW END: and this is the guard that matters ==========
  if (!resolvedTargets.includes(openPath)) {
    return { kind: 'advance' };
  }

  const others = resolvedTargets.filter((path) => path !== openPath);
  if (others.length === 0) {
    return { kind: 'advance' };
  }

  if (situation.typing || situation.previewing) {
    return { kind: 'hold', paths: others };
  }
  return { kind: 'send', paths: others };
}
