/**
 * BLITZRAW: which stack Ctrl+Z acts on.
 *
 * There are two, and they are not the same kind of thing. Adjustments live in
 * the editor's history and only exist while a photo is open. Ratings live in
 * the library store, usually land on a whole selection, and are most often
 * changed in the grid, where there is no open photo to have a history at all.
 * That last point is why Ctrl+Z in the grid used to do nothing.
 *
 * Pure and separate from the key handler so the rule can be checked directly.
 */

export interface UndoClocks {
  /** When the rating stacks last moved, undo included. */
  ratingChangedAt: number | null;
  /** When the adjustment history last moved, undo included. */
  historyChangedAt: number | null;
}

/**
 * Whether the rating stack should go first.
 *
 * The more recently changed of the two wins, and both stamp themselves on every
 * move rather than only on a new step. That second part is what makes a run of
 * Ctrl+Z stay in the stack it started in until that stack runs out; without it
 * a single key would hop between the two and neither would feel like it was
 * undoing anything.
 *
 * When only one of them has anything left, that one, whatever the clocks say.
 */
export function preferRatings(
  clocks: UndoClocks,
  adjustmentsAvailable: boolean,
  ratingsAvailable: boolean,
): boolean {
  if (!ratingsAvailable) {
    return false;
  }
  if (!adjustmentsAvailable) {
    return true;
  }
  // Ties go to ratings. A rating is one keystroke with one obvious meaning, and
  // being surprised by it is cheaper than being surprised by an adjustment.
  return (clocks.ratingChangedAt ?? 0) >= (clocks.historyChangedAt ?? 0);
}
