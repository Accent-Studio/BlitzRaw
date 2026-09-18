/**
 * BLITZRAW: growing and shrinking a selection one frame at a time.
 *
 * Alt and an arrow key extends the selection from wherever it started. The
 * shape of it is a signed reach from an anchor rather than a set of paths, and
 * that one choice is what makes the gesture behave:
 *
 *     three rights  ->  anchor and the next three
 *     one left      ->  anchor and the next two, the far one let go
 *     five more     ->  anchor and the three before it, nothing after
 *
 * Left does not mean "select the one on the left". It means "one less reach",
 * and reach is allowed to go negative, so the same key that is shrinking a
 * selection to the right carries straight on into growing one to the left. A
 * set of paths cannot do that: once the reach has collapsed to nothing, a set
 * has forgotten which way it had been going and the next press has to guess.
 *
 * The anchor is a path rather than an index, because sorting or filtering can
 * move a frame while a selection is held, and the frame is what was meant.
 */

export interface RangeStep {
  /** The frame the reach is measured from. */
  anchorPath: string;
  /** Signed. Positive reaches forward, negative back, zero is the anchor alone. */
  extent: number;
}

export interface RangeResult extends RangeStep {
  /** The selection, in list order. Always at least the anchor. */
  paths: Array<string>;
  /**
   * The frame at the far end of the reach, which is the anchor itself when the
   * reach is nothing.
   *
   * This is what the view scrolls to, and it is deliberately not what becomes
   * the current frame: the current one stays on the anchor, so the grid follows
   * the frames being taken in without the photo being worked on walking away.
   */
  edgePath: string;
}

/**
 * Applies one press to a reach and returns the selection it makes.
 *
 * `delta` is +1 for a press that reaches forward and -1 for one that reaches
 * back. The reach is clamped to the ends of the list, so holding a key at the
 * top of a folder stops rather than wrapping; wrapping a range selection would
 * quietly select the whole folder from both ends.
 *
 * Returns null when there is nothing to work on, which is a folder with no
 * frames or an anchor that has since been filtered out of the list. The caller
 * re-anchors on null rather than this inventing a frame nobody chose.
 */
export function extendRange(
  paths: Array<string>,
  anchorPath: string,
  extent: number,
  delta: number,
): RangeResult | null {
  const anchorIndex = paths.indexOf(anchorPath);
  if (anchorIndex === -1) {
    return null;
  }

  const reach = Math.min(paths.length - 1 - anchorIndex, Math.max(-anchorIndex, extent + delta));

  const from = Math.min(anchorIndex, anchorIndex + reach);
  const to = Math.max(anchorIndex, anchorIndex + reach);

  return {
    anchorPath,
    extent: reach,
    paths: paths.slice(from, to + 1),
    edgePath: paths[anchorIndex + reach],
  };
}

/**
 * Is this selection still the one the last press made?
 *
 * A click, a plain arrow, select-all or a filter all replace the selection, and
 * the reach that produced the old one means nothing against the new. Comparing
 * what was produced against what is there now is how the next Alt press knows
 * to start again from the current frame instead of carrying on from a reach
 * that belongs to a selection nobody can see any more.
 *
 * Order matters and is not sorted away: both lists come from the same display
 * list, so a difference in order is a real difference.
 */
export function sameSelection(produced: Array<string>, current: Array<string>): boolean {
  if (produced.length !== current.length) {
    return false;
  }
  return produced.every((path, index) => path === current[index]);
}
