/**
 * What the Metadata panel needs when it is in the floating window.
 *
 * # Why it has to be sent
 *
 * The floating panel window is a second webview with stores of its own and no
 * library in it. The Metadata panel reads the open photo from the editor store,
 * its rating and its tags from the library store, and its thumbnail from the
 * process store, and in that window all three are empty. So the panel drew a
 * frame with nothing in it.
 *
 * The Navigator and the Scopes had the same problem and are solved the same
 * way: the main window sends the one photo's worth of facts the panel needs.
 * See floatingNavigator.ts and floatingScopes.ts.
 *
 * # Why the tag buttons still work over there
 *
 * Adding and removing a tag is a call to the backend, and both windows talk to
 * the same backend. So the write lands correctly from either. What does not
 * cross by itself is the *result*, which is why `paths` is sent: a tag added in
 * the floating window has to be applied to the photos the main window has
 * selected, not to whatever that window happens to think is selected. The main
 * window sees the new tag on its next send.
 *
 * This is the one floating panel that writes anything. Everything else over
 * there displays only, deliberately, because a change that has to travel back
 * across the gap in order is a different and much larger piece of work. A tag
 * is the exception because it is a single call with no ordering to get wrong.
 */

export const FLOATING_METADATA_EVENT = 'floating-metadata';

export interface FloatingMetadata {
  /** The open photo, as the editor store holds it, or null when none is. */
  selectedImage: unknown | null;
  /** What a tag added over there should be applied to. */
  paths: Array<string>;
  /** Its stars, which the panel shows and the library store normally holds. */
  rating: number;
  /** Its tags, likewise. */
  tags: Array<string>;
  /** Its picture, as an asset URL or a data URL. */
  thumbnail: string | null;
}
