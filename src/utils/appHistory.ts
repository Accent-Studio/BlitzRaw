/**
 * BLITZRAW: the one list of what I did, held for the session.
 *
 * Split from `appActions.ts` the same way `currentAction.ts` is split from
 * `actionId.ts`: that file decides, this one remembers. Only this one touches
 * the clock, and only that one needs checking.
 *
 * **Deliberately not written to disk.** The photos are the record; this is only
 * the order things happened in. After a restart there is nothing to undo, and
 * Ctrl+Z does nothing until something is done. Older states are still reachable,
 * through the photo's own History panel, which is a new action rather than an
 * undo.
 */

import {
  ActionList,
  ActionKind,
  AppAction,
  StepMoved,
  EMPTY_ACTION_LIST,
  afterRedo,
  afterUndo,
  forgetPaths,
  nextRedo,
  nextUndo,
  recordAction,
} from './appActions';

let list: ActionList = EMPTY_ACTION_LIST;

export function actionList(): ActionList {
  return list;
}

/**
 * Records something that was done, with the selection and view it was done in.
 *
 * Called wherever an action reaches photos. Several calls carrying the same
 * `id` are one thing the person did and become one entry: a change made in the
 * editor is written for the open photo and sent to the rest of the selection
 * separately, and both are the same action.
 */
export function recordAppAction(action: {
  id: string | null;
  kind: ActionKind;
  label: string;
  photos: Array<StepMoved>;
  selection: Array<string>;
  openPath: string | null;
  inEditor: boolean;
}): void {
  if (!action.id || action.photos.length === 0) {
    return;
  }
  list = recordAction(list, { ...action, id: action.id, at: Date.now() });
}

// ============ BLITZRAW: a press that arrives before the write does ============
// An entry cannot be recorded until the write comes back saying which numbers
// it moved the photo between, and the editor's save waits 300 ms before it even
// starts. Press Ctrl+Z quickly after an edit and there was nothing in the list
// yet, so the press did nothing and the edit had to be undone twice.
//
// Counted rather than guessed at: undo waits for what is already on its way.
let inFlight = 0;
let waiting: Array<() => void> = [];

/** Called when a write that will record an entry goes out. */
export function recordingStarted(): void {
  inFlight += 1;
}

/** Called when it comes back, whether it recorded anything or not. */
export function recordingFinished(): void {
  inFlight = Math.max(0, inFlight - 1);
  if (inFlight === 0) {
    const due = waiting;
    waiting = [];
    due.forEach((resolve) => resolve());
  }
}

/** Whether anything is still on its way. */
export function isRecording(): boolean {
  return inFlight > 0;
}

/** Resolves once everything on its way has been recorded. */
export function recordingSettled(): Promise<void> {
  if (inFlight === 0) {
    return Promise.resolve();
  }
  return new Promise((resolve) => waiting.push(resolve));
}
// ========== BLITZRAW END: a press that arrives before the write does ==========

/** What a Ctrl+Z would take back, or null when there is nothing. */
export function undoTarget(): AppAction | null {
  return nextUndo(list);
}

/** What a redo would put back, or null. */
export function redoTarget(): AppAction | null {
  return nextRedo(list);
}

/** Moves the list back one, once the undo has actually been carried out. */
export function commitUndo(): void {
  list = afterUndo(list);
}

/** Moves the list forward one, once the redo has actually been carried out. */
export function commitRedo(): void {
  list = afterRedo(list);
}

/**
 * Drops every action that touched a photo that is gone.
 *
 * An entry aimed at a deleted photo cannot be carried out, and carrying out
 * half of it is worse than not offering it at all.
 */
export function forgetDeleted(paths: Array<string>): void {
  list = forgetPaths(list, paths);
}

/** Empties the list. Used when the whole library changes underneath it. */
export function forgetEverything(): void {
  list = EMPTY_ACTION_LIST;
}
