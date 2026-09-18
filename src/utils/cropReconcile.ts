/**
 * BLITZRAW: whether a photo's crop really needs refitting, or only looks like it.
 *
 * # The damage this exists to prevent
 *
 * A 16:9 crop was set on seven photos and each was then framed by hand, some
 * high in the frame, some low. Minutes later a filter was cleared, two hundred
 * photos were selected, and every one of them came back carrying the **centred**
 * 16:9 rectangle: `y=430` on an 8256 by 5504 frame. The seven hand framings
 * were destroyed with the rest.
 *
 * The value that spread was not a stale copy of anything. It was manufactured
 * at that moment. The crop panel keeps a note of the geometry it last fitted a
 * crop at, compares the photo in front of it against that note, and refits when
 * they differ. The note was kept **per mount** rather than per photo, and it
 * starts empty. So on the first run after the panel's component mounted,
 * `undefined !== 0` for orientation, and the panel concluded the frame had been
 * turned on its side. That is one of the two cases where it recentres a crop
 * outright instead of refitting it, so a hand framing became the centred
 * rectangle, and auto-sync then sent that rectangle to everything selected.
 *
 * Proven from the sidecars of the shoot it happened to: `_DSC6468` records
 * `crop y=573 -> y=430` at the same instant as one hundred and ninety-nine
 * other photos, five seconds **before** the white balance that was blamed for
 * it.
 *
 * # The rule
 *
 * **Geometry has changed only when it has changed for this photo.** The first
 * time the panel sees a photo it has nothing to compare against, so nothing has
 * changed, so nothing is refitted. Its sibling in the same file already worked
 * this way, with a comment saying why:
 *
 * > Kept per image, because a reference left over from the previous photo would
 * > be answering the maximised question about a crop it knows nothing about.
 *
 * That reasoning applies here and was simply not applied here.
 */

export interface CropParams {
  /** The photo these values belong to. Absent from the old version, and that was the fault. */
  path: string;
  rotation: number;
  aspectRatio: number | null;
  orientationSteps: number;
}

export interface CropReconcileDecision {
  /** True the first time this photo is seen, when there is nothing to compare against. */
  firstSight: boolean;
  rotationChanged: boolean;
  aspectChanged: boolean;
  orientationChanged: boolean;
  /** Any of the three, which is what decides whether a refit is considered at all. */
  geometryChanged: boolean;
  /** The rotation a crop should be measured against: this photo's last, or its own. */
  referenceRotation: number;
  /** What to remember for next time. */
  next: CropParams;
}

/**
 * What has really changed, given what was remembered and what is on screen now.
 *
 * `remembered` belonging to another photo is treated exactly like remembering
 * nothing. That is deliberately a refusal rather than a guess: guessing here is
 * how one photo's framing reaches two hundred.
 */
export function reconcileCropParams(
  remembered: CropParams | null | undefined,
  current: CropParams,
): CropReconcileDecision {
  const mine = remembered && remembered.path === current.path ? remembered : null;

  if (!mine) {
    return {
      firstSight: true,
      rotationChanged: false,
      aspectChanged: false,
      orientationChanged: false,
      geometryChanged: false,
      referenceRotation: current.rotation,
      next: current,
    };
  }

  const rotationChanged = mine.rotation !== current.rotation;
  const aspectChanged = mine.aspectRatio !== current.aspectRatio;
  const orientationChanged = mine.orientationSteps !== current.orientationSteps;

  return {
    firstSight: false,
    rotationChanged,
    aspectChanged,
    orientationChanged,
    geometryChanged: rotationChanged || aspectChanged || orientationChanged,
    referenceRotation: mine.rotation,
    next: current,
  };
}
