import { useEffect, useRef } from 'react';
import { useLibraryStore } from '../store/useLibraryStore';
import { useEditorStore } from '../store/useEditorStore';
import { useUIStore } from '../store/useUIStore';
import { ImageFile } from '../components/ui/AppProperties';
import { findNearestVisible } from '../utils/filteredSelection';

/**
 * BLITZRAW: keeps the selection inside the list the filter is showing.
 *
 * Two reports, one cause. Dropping a photo to no stars while a one-star filter
 * is on takes it out of the filmstrip, correctly, and used to leave it filling
 * the editor: a photo nothing on screen agreed was there any more. Changing the
 * filter did the same thing more quietly, leaving the selection pointing at
 * something the grid was no longer drawing, which is also why the grid stopped
 * scrolling to it.
 *
 * The rule is one sentence. **If the selected photo is no longer in the list,
 * the selection moves to the nearest one that is.** Nearest is measured in the
 * unfiltered order of the folder, forwards first, because a cull runs forwards
 * and stopping to look backwards is not what anybody wants mid-pass. Backwards
 * only when there is nothing ahead, which is the last photo in the folder.
 *
 * Deliberately does nothing at all when the filtered list is empty. Filtering a
 * folder down to nothing is a thing people do to check, and jumping the
 * selection somewhere arbitrary on the way back would be worse than leaving it.
 */
export function useFilteredSelection(
  sortedImageList: ImageFile[],
  handleImageSelect: (path: string, openInEditor?: boolean) => void,
) {
  const imageList = useLibraryStore((state) => state.imageList);
  const libraryActivePath = useLibraryStore((state) => state.libraryActivePath);
  const selectedImagePath = useEditorStore((state: any) => state.selectedImage?.path ?? null);
  const activeView = useUIStore((state: any) => state.activeView);
  const isViewLoading = useLibraryStore((state) => state.isViewLoading);

  // The move is an effect on a list that changes for many reasons, so it has to
  // be able to tell "this photo just left" from "this photo is still leaving".
  // Without it, a folder that loads its ratings a moment after its files would
  // move the selection once per arriving rating.
  const lastMovedFrom = useRef<string | null>(null);

  useEffect(() => {
    // A folder arrives before its ratings do, so under a star filter the list
    // grows for a second or two after the files appear. Moving the selection
    // through that would look like the application choosing photos at random.
    if (isViewLoading) {
      return;
    }

    if (sortedImageList.length === 0) {
      lastMovedFrom.current = null;
      return;
    }

    const activePath = activeView === 'editor' ? selectedImagePath : libraryActivePath;
    if (!activePath) {
      lastMovedFrom.current = null;
      return;
    }

    const visiblePaths = new Set(sortedImageList.map((image) => image.path));
    if (visiblePaths.has(activePath)) {
      lastMovedFrom.current = null;
      return;
    }

    if (lastMovedFrom.current === activePath) {
      return;
    }

    const next = findNearestVisible(imageList, visiblePaths, activePath);
    if (!next || next === activePath) {
      return;
    }

    lastMovedFrom.current = activePath;

    useLibraryStore.getState().setLibrary({
      libraryActivePath: next,
      multiSelectedPaths: [next],
      selectionAnchorPath: next,
    });

    // In the editor the photo on screen has to change too, and only there:
    // asking for the editor from the grid would open one nobody asked to open.
    if (activeView === 'editor') {
      handleImageSelect(next, false);
    }
  }, [sortedImageList, imageList, libraryActivePath, selectedImagePath, activeView, isViewLoading, handleImageSelect]);
}
