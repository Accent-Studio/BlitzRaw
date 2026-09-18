/**
 * BLITZRAW: when the picture behind the crop box has to be drawn again.
 *
 * The Crop panel shows the whole frame, dimmed outside the rectangle, so the
 * rectangle can be dragged over something visible. That picture is a full
 * render of the photo **without** the crop applied, which costs a GPU pass over
 * 1920x1280 and about a third of a second.
 *
 * It used to be asked for on **every** change to the adjustments while the panel
 * was open, straight away, with nothing counting how many were already running.
 * Dragging a crop handle or a rotation slider is a stream of changes, so they
 * piled up: 57 of them in nine seconds, each slower than the last as they fought
 * over the same GPU, each holding its own copy of the image. That is the freeze
 * and the memory spike.
 *
 * Two things stop it, and this file is the first.
 *
 * **The crop rectangle is not one of the things this picture shows.** It is the
 * uncropped preview; the rectangle is drawn over it by the front end. So moving
 * the rectangle, which is what somebody in the Crop panel does most, needs no
 * new render at all.
 *
 * The second is a queue: one render at a time, and a newer request replaces a
 * waiting one rather than joining it. That lives in `useImageProcessing`.
 */

import { Adjustments } from './adjustments';
import { changedKeys } from './editHistory';

/**
 * What this picture cannot see.
 *
 * Only the crop rectangle. Rotation, straightening and the lens profile all
 * change it, because the frame itself is warped before the rectangle goes on
 * top, so those must still redraw.
 */
export const UNCROPPED_PREVIEW_IGNORES: ReadonlyArray<string> = ['crop'];

/**
 * Whether the picture behind the crop box has to be drawn again.
 *
 * `null` for `before` means this photo has not been drawn yet, which always
 * needs one.
 */
export function uncroppedPreviewNeedsRedraw(
  before: Adjustments | null | undefined,
  after: Adjustments,
): boolean {
  if (!before) {
    return true;
  }
  return changedKeys(before, after).some((key) => !UNCROPPED_PREVIEW_IGNORES.includes(key));
}
