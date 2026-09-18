/**
 * How tall each scope in the column is drawn.
 *
 * The column used to be a fixed height per scope, which is right until you want
 * a tall waveform over a short histogram, or a vectorscope square rather than
 * letterboxed. So each scope carries its own height and a handle sits under it.
 *
 * The saved list and the column can disagree, because the column is changed by
 * the plus and the minus and the saved list is whatever was written last. So
 * the list is fitted to the column rather than trusted: a scope with no
 * remembered height gets the default, and heights belonging to scopes that are
 * gone are dropped. Nothing here throws on a bad value; a settings file is not
 * a promise.
 */

/** Comfortable for a waveform, and what every scope started at. */
export const DEFAULT_SCOPE_HEIGHT = 224;

/** Below this a trace is a smear; above it one scope fills any panel. */
export const MIN_SCOPE_HEIGHT = 110;
export const MAX_SCOPE_HEIGHT = 700;

export function clampScopeHeight(height: number): number {
  if (!Number.isFinite(height)) {
    return DEFAULT_SCOPE_HEIGHT;
  }
  return Math.round(Math.min(MAX_SCOPE_HEIGHT, Math.max(MIN_SCOPE_HEIGHT, height)));
}

/**
 * One height per scope in the column, in the column's own order.
 *
 * Always exactly as long as the column, so the caller can index it without
 * checking. Added scopes come in at the default; removed ones take their height
 * with them.
 */
export function scopeHeightsFor(scopeCount: number, saved: Array<number> | null | undefined): Array<number> {
  const out: Array<number> = [];
  for (let i = 0; i < Math.max(0, Math.floor(scopeCount)); i += 1) {
    const remembered = saved?.[i];
    out.push(typeof remembered === 'number' ? clampScopeHeight(remembered) : DEFAULT_SCOPE_HEIGHT);
  }
  return out;
}

/** The column with one scope's height changed, ready to be saved. */
export function withScopeHeight(heights: Array<number>, index: number, height: number): Array<number> {
  if (index < 0 || index >= heights.length) {
    return heights;
  }
  const next = [...heights];
  next[index] = clampScopeHeight(height);
  return next;
}
