import { Adjustments } from './adjustments';

/**
 * What moved since the photo was opened, for the picture that stands in while
 * the raw decodes.
 *
 * The picture on disk is the photo **as it was adjusted when it was written**,
 * not the photo. So what goes on top of it is the difference and only the
 * difference: apply the new exposure and it is applied twice.
 *
 * Both ends are measured here rather than in the backend, because the sidecar
 * is written on a delay and a held key would have it part-way through the run.
 * The editor knows exactly where the photo started; nothing else does.
 *
 * Only the two adjustments a keyboard nudge can reach and that a difference is
 * meaningful for. Everything else waits for the real render, which is a second
 * or two away.
 */

export interface WhitePoint {
  kelvin: number;
  tint: number;
}

export interface Nudge {
  /** Stops, signed. */
  exposure: number;
  /** The white balance the picture on disk was rendered at. */
  whiteBalanceFrom: WhitePoint | null;
  /** And the one wanted now. */
  whiteBalanceTo: WhitePoint | null;
}

/** Below this, nothing on screen would move. */
const STOP_EPSILON = 1e-6;
const KELVIN_EPSILON = 1e-3;

function whitePoint(adjustments: Adjustments | null | undefined): WhitePoint | null {
  const wb = (adjustments as any)?.whiteBalance;
  if (!wb || typeof wb.kelvin !== 'number' || !Number.isFinite(wb.kelvin)) {
    return null;
  }
  return { kelvin: wb.kelvin, tint: typeof wb.tint === 'number' ? wb.tint : 0 };
}

/**
 * The difference worth rendering for, or null when there is none.
 *
 * Null covers three cases that all mean "leave the picture alone": nothing
 * moved, a press that ran into a limit and therefore moved nothing, and a white
 * balance with only one end known. That last one matters: a photo's white
 * balance is absent until its camera's own as-shot value is read, and half a
 * difference is not a difference. Guessing the other end would re-white-balance
 * the photo on screen and then jump back when the real render landed.
 */
export function nudgeBetween(
  atOpen: Adjustments | null | undefined,
  now: Adjustments | null | undefined,
): Nudge | null {
  if (!atOpen || !now) {
    return null;
  }

  const exposureFrom = typeof atOpen.exposure === 'number' ? atOpen.exposure : 0;
  const exposureTo = typeof now.exposure === 'number' ? now.exposure : 0;
  const exposure = exposureTo - exposureFrom;

  const from = whitePoint(atOpen);
  const to = whitePoint(now);
  const whiteMoved =
    !!from &&
    !!to &&
    (Math.abs(to.kelvin - from.kelvin) > KELVIN_EPSILON || Math.abs(to.tint - from.tint) > KELVIN_EPSILON);

  if (Math.abs(exposure) <= STOP_EPSILON && !whiteMoved) {
    return null;
  }

  return {
    exposure,
    whiteBalanceFrom: whiteMoved ? from : null,
    whiteBalanceTo: whiteMoved ? to : null,
  };
}
