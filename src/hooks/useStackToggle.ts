import { useCallback } from 'react';
import { useLibraryStore } from '../store/useLibraryStore';
import { toggledAllStacks, toggledStack } from '../utils/imageStacking';

/**
 * Opening and closing stacks, from wherever the user reaches for it.
 *
 * Which stacks are open is one piece of state for the whole library, so the
 * grid, the filmstrip and the bottom bar are three ways into the same thing:
 * opening a stack in the strip opens it in the grid, and closing it anywhere
 * closes it everywhere. That is the point, not a side effect.
 *
 * The rules themselves are pure and live in `imageStacking`, so they can be
 * checked without a browser. This is the thin part that reaches the store.
 */
export function useStackToggle() {
  const setLibrary = useLibraryStore((state) => state.setLibrary);

  /** Opens a closed stack, closes an open one. */
  const toggleStack = useCallback(
    (stackId: string) => {
      setLibrary((state) => ({ expandedStacks: toggledStack(state.expandedStacks, stackId) }));
    },
    [setLibrary],
  );

  /** Opens every stack given, or closes them all if none is closed. */
  const toggleAllStacks = useCallback(
    (stackIds: Array<string>) => {
      setLibrary((state) => ({ expandedStacks: toggledAllStacks(state.expandedStacks, stackIds) }));
    },
    [setLibrary],
  );

  return { toggleStack, toggleAllStacks };
}
