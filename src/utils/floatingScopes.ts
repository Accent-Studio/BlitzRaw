/**
 * The scopes, when the Scopes panel is in the floating window.
 *
 * # Why this has to be sent
 *
 * The floating panel window is a second webview with a store of its own. It has
 * no library, no grid and no filmstrip, so it never learns what the pointer is
 * over, and `useScopeSource` (which is the whole feature of scopes that follow
 * the pointer and read from a preview rather than waiting on a decode) runs in
 * the main window and writes into the main window's store.
 *
 * So the answer was being worked out correctly and then thrown away. The panel
 * over there had exactly one feed, a direct listener on `analytics-update`,
 * which the backend emits only after a full decode. That is precisely the wait
 * the feature exists to avoid, and it meant that for anyone with the Scopes
 * panel floating, the feature did not exist.
 *
 * The Navigator had the same problem and is solved the same way: the main
 * window sends the one thing the panel needs. See floatingNavigator.ts.
 *
 * # Why the floating window no longer listens to the decode as well
 *
 * Because the main window has already decided. `useScopeSource` holds the
 * accurate post-decode pair for the open photo separately from whatever is on
 * screen, so that unhovering restores it rather than leaving preview-quality
 * scopes on the photo being worked on. Letting the decode write to the floating
 * window directly as well would land the open photo's scopes on top of a photo
 * the pointer had moved to, and there is no ordering between the two that fixes
 * that. One source, decided in one place, sent.
 */

export const FLOATING_SCOPES_EVENT = 'floating-scopes';

export interface FloatingScopes {
  /** Which photo these describe, for the panel's own label. */
  path: string | null;
  histogram: unknown;
  waveform: unknown;
}
