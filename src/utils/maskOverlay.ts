import { INITIAL_MASK_ADJUSTMENTS } from './adjustments';
import { sameValue } from './editHistory';

/**
 * When the red mask overlay is drawn.
 *
 * # Why this is a module and not three conditions in a render
 *
 * The rule has been rewritten three times, and each time it lived inside the
 * expression that computed a CSS opacity, spread across a hover flag in one
 * file, an interaction flag in another and a slider in a third. It was
 * impossible to read off what the rule actually was, which is how it ended up
 * backwards: the overlay was on by default and switched off while the pointer
 * sat over the mask's own sliders, so moving onto the photo turned it on.
 *
 * The rule now has one home and one name.
 *
 * # The rule
 *
 * A mask nobody has adjusted yet is a shape and nothing else. There is nothing
 * to see except where it is, so it is shown, always: while it is being drawn,
 * and afterwards until it is given something to do.
 *
 * Once it has adjustments the red is in the way, because what you want to look
 * at is the effect. So it goes, and it comes back only while the pointer is on
 * that mask's row in the list, which is the one gesture that means "show me
 * where this one is".
 *
 * Dragging the mask and moving its opacity deliberately do **not** bring it
 * back. Those are the moments you are watching the effect change.
 */

/**
 * Keys that say how the panel is drawn rather than what the mask does.
 *
 * A collapsed section and a curve editor set to points instead of parametric
 * are not edits, and counting them as edits would make a brand new mask look
 * adjusted the moment anything was folded away.
 */
const PRESENTATION_ONLY = new Set(['sectionVisibility', 'curveMode']);

/** Whether anything has been done to this mask beyond making it. */
export function maskHasAdjustments(adjustments: any): boolean {
  if (!adjustments || typeof adjustments !== 'object') {
    return false;
  }
  const initial = INITIAL_MASK_ADJUSTMENTS as Record<string, unknown>;
  const names = new Set([...Object.keys(adjustments), ...Object.keys(initial)]);
  for (const name of names) {
    if (PRESENTATION_ONLY.has(name)) {
      continue;
    }
    // By value, not by how it is spelled. A mask read back from a sidecar has
    // the same numbers in a different order, and comparing the text would call
    // every reopened mask adjusted.
    if (!sameValue(adjustments[name], initial[name])) {
      return true;
    }
  }
  return false;
}

export interface MaskOverlayState {
  /** Anything has been done to the mask being drawn. */
  hasAdjustments: boolean;
  /** The pointer is on that mask's row in the list at the top. */
  isHoveredInList: boolean;
}

/** Whether to draw the red, stated once. */
export function shouldShowMaskOverlay({ hasAdjustments, isHoveredInList }: MaskOverlayState): boolean {
  return hasAdjustments ? isHoveredInList : true;
}
