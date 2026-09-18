/**
 * BLITZRAW: clicking a step in a photo's History panel.
 *
 * A thin wrapper so the panel and the editor toolbar's step list do the same
 * thing, and so neither has to know about the selection, the view or the list
 * of what I did.
 *
 * A click is **not an undo**. It is a thing I did, so it goes into the list and
 * Ctrl+Z takes the jump back: from step 12 to step 7, then Ctrl+Z, and you are
 * on 12 again.
 */

import { useAppUndo } from './useAppUndo';

export function useHistoryJump() {
  // Neither of these matters for a jump: it never leaves the editor and it
  // never changes the selection. Passed so the one hook covers both uses.
  const { jumpToStep } = useAppUndo(() => undefined);
  return jumpToStep;
}
