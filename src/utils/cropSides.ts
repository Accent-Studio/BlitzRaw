/**
 * BLITZRAW: resizing a crop from the middle of an edge, with the ratio locked.
 *
 * react-image-crop hides its own edge handles the moment an aspect ratio is
 * set, and it is right to: moving one edge of a locked rectangle cannot leave
 * the other three alone, so a control that looks like it moves one edge would
 * be lying. Four corners are all it offers, and a corner is the wrong grip for
 * "a bit wider" on a 16:9 crop that is already where you want it.
 *
 * So the edge grips are ours, and they say what they do: the **opposite edge
 * stays put** and the crop grows about the **centre of the other axis**. Drag
 * the right edge of a 3:2 crop and the left edge does not move while the top
 * and bottom open evenly; drag the top and the bottom stays while the sides
 * open evenly. That is the only reading of "resize from this edge" that a
 * locked ratio allows, and it is what Lightroom does.
 *
 * Everything here is in the pixels of the displayed image, with the origin at
 * its top left corner. Percentages are the caller's problem.
 */

export type CropSide = 'top' | 'right' | 'bottom' | 'left';

export interface CropRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Below this the crop cannot be grabbed by anything any more. */
export const MIN_CROP_PX = 24;

/**
 * Where the crop ends up when one edge is dragged to `pointer`.
 *
 * `aspect` is width over height and is always honoured. `bounds` is the image,
 * and the result is always inside it: a drag that would take the crop over an
 * edge stops at the largest rectangle of the right shape that still fits,
 * rather than sliding the crop sideways to make room, which would move the
 * anchored edge and is the one thing an edge grip must not do.
 */
export function resizeFromSide(
  side: CropSide,
  crop: CropRect,
  aspect: number,
  pointer: { x: number; y: number },
  bounds: { width: number; height: number },
): CropRect {
  if (!(aspect > 0) || !(bounds.width > 0) || !(bounds.height > 0)) {
    return crop;
  }

  const right = crop.x + crop.width;
  const bottom = crop.y + crop.height;
  const centreX = crop.x + crop.width / 2;
  const centreY = crop.y + crop.height / 2;

  // The one number the drag actually chooses. Everything else follows from it
  // and from the ratio.
  let width: number;
  switch (side) {
    case 'right':
      width = pointer.x - crop.x;
      break;
    case 'left':
      width = right - pointer.x;
      break;
    case 'bottom':
      width = (pointer.y - crop.y) * aspect;
      break;
    case 'top':
      width = (bottom - pointer.y) * aspect;
      break;
  }

  // How large the crop is allowed to get before it leaves the image, measured
  // from the edge that is staying still. Worked out per side, because the room
  // available depends on which way it is growing.
  const horizontal = side === 'right' || side === 'left';
  const roomAlong = horizontal
    ? side === 'right'
      ? bounds.width - crop.x
      : right
    : (side === 'bottom' ? bounds.height - crop.y : bottom) * aspect;

  // The other axis opens both ways from its centre, so it runs out of room at
  // twice the distance to the nearer edge.
  const roomAcross = horizontal
    ? 2 * Math.min(centreY, bounds.height - centreY) * aspect
    : 2 * Math.min(centreX, bounds.width - centreX);

  width = Math.min(width, roomAlong, roomAcross);
  width = Math.max(width, MIN_CROP_PX, MIN_CROP_PX * aspect);

  const height = width / aspect;

  const next: CropRect = horizontal
    ? {
        x: side === 'right' ? crop.x : right - width,
        y: centreY - height / 2,
        width,
        height,
      }
    : {
        x: centreX - width / 2,
        y: side === 'bottom' ? crop.y : bottom - height,
        width,
        height,
      };

  // A last nudge for the rounding, so a crop that is exactly as large as the
  // image cannot sit a fraction of a pixel outside it.
  next.x = Math.min(Math.max(next.x, 0), Math.max(0, bounds.width - next.width));
  next.y = Math.min(Math.max(next.y, 0), Math.max(0, bounds.height - next.height));

  return next;
}
