/**
 * What the front end asks the backend to draw, as one string.
 *
 * The request travels a long way: a command parameter, an analytics job, a
 * worker thread and finally `calculate_waveform_from_image`, through four
 * structs and eight signatures, none of which are ours. So everything a scope
 * needs to say about itself rides in the string rather than in parameters
 * beside it. A scope is its name, optionally followed by a colon and a number,
 * and only the vectorscope reads the number: it is the gain on the trace.
 *
 * Two columns can be asking at once, the sidebar's and a floating window's, and
 * the backend fills whatever is asked for in a single pass over the pixels. So
 * the two lists are merged rather than sent separately, and a scope named twice
 * has to collapse to one entry or the gain read from it would be whichever
 * happened to come first.
 */

/** The name of a scope, without whatever it carries after the colon. */
export function scopeName(token: string): string {
  return token.split(':')[0].trim();
}

/** The gain a token carries, or 1 for one that carries none. */
export function scopeGain(token: string): number {
  const [, raw] = token.split(':');
  const gain = Number.parseFloat(raw ?? '');
  return Number.isFinite(gain) && gain >= 1 ? gain : 1;
}

/** A scope written the way the backend reads it. Gain of one is left off. */
export function scopeToken(name: string, gain = 1): string {
  return gain > 1 ? `${name}:${gain}` : name;
}

/**
 * Every column's request, merged into the one string the backend takes.
 *
 * Order follows first appearance, so the sidebar's column stays in the order it
 * is drawn in. A scope in both columns is kept once, at the larger of the two
 * gains: if one window is looking at a magnified vectorscope, drawing it flat
 * would be wrong for that window, whereas drawing it magnified is only more
 * than the other window asked for and it is the same picture.
 *
 * An empty result means nothing is being looked at. That is not the same as
 * asking for nothing: the backend reads an empty request as "all of them", so
 * the caller has to decide not to ask at all rather than to ask for "".
 */
export function mergeScopeRequests(...columns: Array<Array<string> | null | undefined>): string {
  const best = new Map<string, number>();

  for (const column of columns) {
    for (const token of column ?? []) {
      const name = scopeName(token);
      if (!name) continue;
      const gain = scopeGain(token);
      const previous = best.get(name);
      if (previous === undefined || gain > previous) {
        best.set(name, gain);
      }
    }
  }

  return [...best.entries()].map(([name, gain]) => scopeToken(name, gain)).join(',');
}
