/**
 * What the Navigator needs when it is in the floating window.
 *
 * The panel draws the thumbnail of whichever frame the pointer is over. Both of
 * those live in the main window: `hoveredPath` in the library store and the
 * thumbnails in the process store, filled by the grid and the filmstrip as they
 * scroll. The floating window has no library and no filmstrip, so it has
 * neither, and the panel came up empty there for exactly that reason.
 *
 * Rather than give the floating window a library, the main window sends the one
 * frame the Navigator is currently pointing at. That is a path and a picture,
 * and usually the picture is a short `asset://` URL to a file the backend has
 * already written, so what crosses is two strings.
 *
 * Sent only while the Navigator is actually showing over there, because
 * otherwise it would be a message for every frame the pointer passes over with
 * nothing at the other end to draw it.
 */

export const FLOATING_NAVIGATOR_EVENT = 'floating-navigator-frame';

export interface FloatingNavigatorFrame {
  /** The frame the Navigator should be showing, or null if there is not one. */
  path: string | null;
  /** Its thumbnail, as the main window has it: an asset URL or a data URL. */
  thumbnail: string | null;
  /** Whether the pointer is over it, as opposed to it just being the open photo. */
  hovering: boolean;
}
