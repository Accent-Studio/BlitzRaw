/**
 * BLITZRAW: the action currently being made, and nothing else.
 *
 * Split from `actionId.ts` on purpose. That file decides; this one remembers.
 * Only this one touches the clock and mints a name, and only that one needs
 * checking, which is what keeps the rule verifiable in a project with no
 * front-end test runner.
 *
 * One variable, module scope, session only. It is never written to disk: what
 * reaches a photo is the name alone.
 */

import { OpenAction, actionFor } from './actionId';

let open: OpenAction | null = null;
let minted = 0;

/** Fixed once per session, so names from two runs cannot be confused. */
const SESSION = Date.now() % 0xffffff;

/**
 * A short name, unique within a session.
 *
 * A counter and the session's start, rather than a random string, so a name is
 * readable in a log and in a settings file, and so two names never collide
 * inside one run however fast the presses land.
 */
function mint(): string {
  minted += 1;
  return `a${SESSION.toString(36)}-${minted.toString(36)}`;
}

/**
 * The action this change belongs to, opening a new one when it does not join
 * the last.
 *
 * `keys` are the adjustments that moved. Pass `named` for a deliberate event
 * such as a paste or a reset: nothing joins one and one joins nothing.
 *
 * Returns null only when nothing has moved and nothing is open.
 */
export function actionForChange(keys: Array<string>, named = false): string | null {
  open = actionFor(open, keys, named, Date.now(), mint);
  return open?.id ?? null;
}

/** The action currently open, without opening one. */
export function openActionId(): string | null {
  return open?.id ?? null;
}

/**
 * Forgets the open action, so the next change starts a new one.
 *
 * Called when the open photo changes. A change made on the next photo must
 * never join an action made on the last one, or an undo would step back a photo
 * that was never part of it.
 */
export function closeAction(): void {
  open = null;
}
