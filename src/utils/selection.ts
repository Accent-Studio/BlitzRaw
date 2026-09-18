import { useLibraryStore } from '../store/useLibraryStore';
import { resolveSelection, summariseSelection, SelectionRule, SelectionSummary } from './imageStacking';

/**
 * Reading the current selection the way a particular action needs it.
 *
 * Kept apart from `imageStacking`, which is pure and knows nothing about the
 * store, so the resolving logic stays testable on plain arrays. This is the
 * thin part that goes and gets the library.
 *
 * Every action states its own rule, and each one was argued over once before
 * it was written down. Change one and read its reason first. The short version is
 * that a merged stack keeps its result and a peer stack opens up, except for
 * the handful of actions that want one file per stack whatever it holds.
 */
export function selectionFor(rule: SelectionRule, paths?: Array<string>): Array<string> {
  const { imageList, expandedStacks, multiSelectedPaths } = useLibraryStore.getState();
  return resolveSelection(imageList, expandedStacks ?? [], paths ?? multiSelectedPaths, rule);
}

/** The same, for telling the user what they are about to act on. */
export function currentSelectionSummary(paths?: Array<string>): SelectionSummary {
  const { imageList, expandedStacks, multiSelectedPaths } = useLibraryStore.getState();
  return summariseSelection(imageList, expandedStacks ?? [], paths ?? multiSelectedPaths);
}
