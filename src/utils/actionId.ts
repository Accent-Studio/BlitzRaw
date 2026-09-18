/**
 * BLITZRAW: which single thing the user did a change belongs to.
 *
 * # Why a name is needed at all
 *
 * A photo's history already records what happened to it, key by key, with the
 * value before and after, and it survives a restart because it lives in the
 * photo's own settings file. What it cannot answer is **which photos one
 * action touched**, and that is the question an application level undo is made
 * of: undoing a change made across two hundred photos means finding those two
 * hundred and stepping each of them back through its own history.
 *
 * Nothing already stored can answer it:
 *
 * - **The timestamp cannot.** It is minted separately for each photo, inside a
 *   parallel loop, and it is overwritten again whenever a later change joins
 *   the step above it.
 * - **The position in the list cannot.** Old steps fold into the base at a
 *   different rate on every photo, so the same action sits at a different index
 *   on each of them.
 * - **The label cannot.** A change sent across a selection deliberately carries
 *   no label, because a stored label beats the one the editor works out from
 *   what moved, and stamping every photo with one constant name gave each of
 *   them a history of identical lines.
 *
 * So one short name travels with the change, from the editor to every photo the
 * change reaches, and each photo writes it beside the step. See
 * `edit_history.rs`.
 *
 * # The rule, which exists twice
 *
 * The same joining rule is applied in the backend, in `joins_the_step_at_the_top`,
 * because the backend is the thing that holds the previous step. This copy
 * decides which **action** a change belongs to, before it is sent. They are
 * stated the same way and checked the same way, and if they disagree the stored
 * log wins, because that is what a photo comes back as.
 *
 * # What this fixes on its own, before any undo is built
 *
 * Two deliberate presses on one slider inside the window used to become a
 * single stored step, so one undo took back both of them. With a name on each,
 * two things the user did are two steps, however fast they land.
 */

/**
 * How long an action stays open to being added to.
 *
 * The same 2.5 seconds the editor's own coalescing rule and the sidecar's rule
 * already use, and deliberately the same constant, since three different
 * windows on the same run of presses would group it three different ways.
 */
export const ACTION_WINDOW_MS = 2500;

export interface OpenAction {
  /** The name every photo this action touches will store. */
  id: string;
  /** Every adjustment this action has moved so far. */
  keys: Array<string>;
  /**
   * When it last moved.
   *
   * The window rolls from here rather than from when the action opened, so a
   * slow drag stays one action for as long as it keeps moving. Ten nudges a
   * second apart are one move, and that move takes ten seconds.
   */
  lastAt: number;
  /** A deliberate event such as a paste or a reset. Nothing ever joins one. */
  named: boolean;
}

/**
 * Whether an arriving change belongs to the action already open.
 *
 * Two conditions, both of which must hold, and they are the backend's two:
 *
 * - it moves **only adjustments this action has already moved**, and
 * - it lands **within the window** of the last change in it.
 *
 * Either failing starts a new action. The second matters as much as the first:
 * a change of tool is a change of mind and is worth its own step even when it
 * happens fast.
 */
export function joinsTheOpenAction(
  open: OpenAction | null | undefined,
  keys: Array<string>,
  named: boolean,
  at: number,
): boolean {
  if (named || !open || open.named) {
    return false;
  }
  if (keys.length === 0) {
    return false;
  }
  const already = new Set(open.keys);
  if (!keys.every((key) => already.has(key))) {
    return false;
  }
  const since = at - open.lastAt;
  return since >= 0 && since <= ACTION_WINDOW_MS;
}

/**
 * The action an arriving change belongs to.
 *
 * Returns the open action with its window rolled forward when the change joins
 * it, a new one when it does not, and whatever was open unchanged when nothing
 * moved at all, so an effect that re-runs without an edit does not hold an
 * action open for ever.
 *
 * `mint` is handed in rather than called here, so this stays a pure function of
 * its arguments and can be checked directly.
 */
export function actionFor(
  open: OpenAction | null | undefined,
  keys: Array<string>,
  named: boolean,
  at: number,
  mint: () => string,
): OpenAction | null {
  if (keys.length === 0) {
    return open ?? null;
  }
  if (joinsTheOpenAction(open, keys, named, at)) {
    return { ...(open as OpenAction), lastAt: at };
  }
  return { id: mint(), keys: [...keys], lastAt: at, named };
}
