/**
 * BLITZRAW: where the selection goes when the photo it was on stops being shown.
 *
 * Pure, and separate from the hook that calls it, so the rule can be checked
 * against a list without a running application. The rule is the part that is
 * easy to get subtly wrong; the wiring is not.
 */

/**
 * The nearest photo to `activePath` that is still visible.
 *
 * Nearest is measured in `fullOrder`, which is the folder as it stands with no
 * filter applied, because that is the order the photographer walked through.
 * **Forwards first:** a cull runs forwards, and being thrown backwards mid-pass
 * is worse than useless. Backwards only when there is nothing ahead, which is
 * the last photo in the folder.
 *
 * Returns `null` when nothing is visible at all, which the caller must treat as
 * "leave the selection alone" rather than "clear it": filtering a folder down
 * to nothing is a thing people do on purpose, and it is not a reason to forget
 * where they were.
 */
export function findNearestVisible(
  fullOrder: Array<{ path: string }>,
  visiblePaths: Set<string>,
  activePath: string,
): string | null {
  const at = fullOrder.findIndex((image) => image.path === activePath);

  if (at === -1) {
    // The photo has left the folder entirely, not just the filter. There is no
    // "near" any more, so the first thing still showing is the honest answer.
    for (const image of fullOrder) {
      if (visiblePaths.has(image.path)) {
        return image.path;
      }
    }
    return null;
  }

  for (let i = at + 1; i < fullOrder.length; i += 1) {
    if (visiblePaths.has(fullOrder[i].path)) {
      return fullOrder[i].path;
    }
  }
  for (let i = at - 1; i >= 0; i -= 1) {
    if (visiblePaths.has(fullOrder[i].path)) {
      return fullOrder[i].path;
    }
  }
  return null;
}

/**
 * BLITZRAW: where the selection goes when a change is about to hide the photo
 * it is on, or `null` when it should stay where it is.
 *
 * Called with the list as it will be **after** the change, before the change is
 * written. That ordering is the whole point: deciding afterwards leaves a
 * render in which the editor holds a photo the list no longer has, and the move
 * that follows opens a second photo, so a single key press pays for two full
 * decodes instead of one.
 *
 * Two cases deliberately do not move. A list that has filtered down to nothing
 * is something people do on purpose to check, and jumping somewhere arbitrary
 * on the way back would lose their place. A photo still showing after the
 * change has not gone anywhere and does not need moving off.
 */
export function selectionAfterHiding(
  fullOrder: Array<{ path: string }>,
  visibleAfter: Array<{ path: string }>,
  activePath: string | null,
): string | null {
  if (!activePath || visibleAfter.length === 0) {
    return null;
  }

  const visible = new Set(visibleAfter.map((image) => image.path));
  if (visible.has(activePath)) {
    return null;
  }

  const next = findNearestVisible(fullOrder, visible, activePath);
  return next && next !== activePath ? next : null;
}
