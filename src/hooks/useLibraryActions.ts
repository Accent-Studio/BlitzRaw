import { useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import { useLibraryStore } from '../store/useLibraryStore';
import { BOTH } from '../utils/imageStacking';
import { selectionFor } from '../utils/selection';
import { useEditorStore } from '../store/useEditorStore';
import { useUIStore } from '../store/useUIStore';
import { Invokes, ImageFile, AlbumItem, Album, AlbumGroup } from '../components/ui/AppProperties';
import { globalImageCache } from '../utils/ImageLRUCache';
import { useSettingsStore } from '../store/useSettingsStore';
import { computeSortedLibrary } from './useSortedLibrary';

export function useLibraryActions(handleImageSelect?: (path: string, openInEditor?: boolean) => void) {
  const handleRate = useCallback((newRating: number, paths?: string[]) => {
    const { multiSelectedPaths, imageRatings, setLibrary, pushRatingChange } = useLibraryStore.getState();
    const { selectedImage } = useEditorStore.getState();

    const requested =
      paths || (multiSelectedPaths.length > 0 ? multiSelectedPaths : selectedImage ? [selectedImage.path] : []);
    if (requested.length === 0) return;

    // Toggling off keys on what was actually clicked, not on the first frame
    // the stack happens to expand to.
    const currentRating = imageRatings[requested[0]] || 0;
    const finalRating = newRating === currentRating ? 0 : newRating;

    // A rating belongs to the capture, so it reaches every frame of a stack of
    // either kind. One list now drives both the write and the optimistic
    // update, where the backend used to expand separately and the store had to
    // guess the same answer to stay in step.
    const pathsToRate = selectionFor(BOTH('fullStack'), requested);

    // BLITZRAW: what these photos were rated, so Ctrl+Z can put it back. Taken
    // before anything changes, and only for the photos actually touched.
    const before: Record<string, number> = {};
    const after: Record<string, number> = {};
    pathsToRate.forEach((p) => {
      before[p] = imageRatings[p] || 0;
      after[p] = finalRating;
    });

    setLibrary((state) => {
      const newRatings = { ...state.imageRatings };
      pathsToRate.forEach((p) => {
        newRatings[p] = finalRating;
      });
      return { imageRatings: newRatings };
    });

    // Nothing to take back when nothing moved, and recording it would push a
    // real change out of a bounded stack for no reason.
    const somethingChanged = pathsToRate.some((p) => before[p] !== after[p]);
    if (somethingChanged) {
      pushRatingChange({ before, after });
    }

    invoke(Invokes.SetRatingForPaths, { paths: pathsToRate, rating: finalRating }).catch((err) => {
      console.error(err);
      toast.error(`Failed to apply rating: ${err}`);
    });
  }, []);

  // ============ BLITZRAW: ratings can be taken back ============
  /**
   * Writes a set of ratings that are not all the same number.
   *
   * The backend command takes one rating for many paths, which is right for
   * rating a selection and wrong for putting one back: undoing a change across
   * a stack can restore three different numbers. Grouping by value and sending
   * one call per value is the whole of it, and there are at most six.
   *
   * The store is already correct by the time this runs, so a failure here is
   * reported rather than rolled back: the sidecar is behind the screen, and
   * saying so is more use than silently reverting what somebody just watched
   * happen.
   */
  const writeRatings = useCallback(async (ratings: Record<string, number>) => {
    const byRating = new Map<number, string[]>();
    Object.entries(ratings).forEach(([path, rating]) => {
      const group = byRating.get(rating);
      if (group) {
        group.push(path);
      } else {
        byRating.set(rating, [path]);
      }
    });

    for (const [rating, group] of byRating) {
      try {
        await invoke(Invokes.SetRatingForPaths, { paths: group, rating });
      } catch (err) {
        console.error(err);
        toast.error(`Failed to apply rating: ${err}`);
      }
    }
  }, []);

  /**
   * BLITZRAW: nudges every selected photo up or down from wherever it already
   * is, instead of setting them all to one number.
   *
   * Culling a set that already carries stars is the case: three ones and a two
   * promoted together should become three twos and a three, not four of
   * whatever was clicked. Each photo is read, moved and clamped on its own.
   *
   * Photos already at the end they are being pushed towards are left out of the
   * write entirely, so pressing "increase" on a five does not cost a file write
   * or a step in the undo stack.
   */
  const handleAdjustRating = useCallback(
    (delta: number, paths?: string[]) => {
      const { multiSelectedPaths, imageRatings, setLibrary, pushRatingChange } = useLibraryStore.getState();
      const { selectedImage } = useEditorStore.getState();

      const requested =
        paths || (multiSelectedPaths.length > 0 ? multiSelectedPaths : selectedImage ? [selectedImage.path] : []);
      if (requested.length === 0) return;

      // A rating belongs to the capture, so it reaches every frame of a stack,
      // exactly as it does when one is set outright.
      const pathsToRate = selectionFor(BOTH('fullStack'), requested);

      const before: Record<string, number> = {};
      const after: Record<string, number> = {};
      pathsToRate.forEach((path) => {
        const current = imageRatings[path] || 0;
        const next = Math.max(0, Math.min(5, current + delta));
        if (next !== current) {
          before[path] = current;
          after[path] = next;
        }
      });

      if (Object.keys(after).length === 0) return;

      setLibrary((state) => ({ imageRatings: { ...state.imageRatings, ...after } }));
      pushRatingChange({ before, after });
      void writeRatings(after);
    },
    [writeRatings],
  );

  const handleUndoRating = useCallback(() => {
    const change = useLibraryStore.getState().undoRating();
    if (change) {
      void writeRatings(change.before);
    }
    return Boolean(change);
  }, [writeRatings]);

  const handleRedoRating = useCallback(() => {
    const change = useLibraryStore.getState().redoRating();
    if (change) {
      void writeRatings(change.after);
    }
    return Boolean(change);
  }, [writeRatings]);
  // ========== BLITZRAW END: ratings can be taken back ==========

  const handleSetColorLabel = useCallback(async (color: string | null, paths?: string[]) => {
    const { multiSelectedPaths, libraryActivePath, imageList, setLibrary } = useLibraryStore.getState();
    const { selectedImage } = useEditorStore.getState();

    const requested =
      paths || (multiSelectedPaths.length > 0 ? multiSelectedPaths : selectedImage ? [selectedImage.path] : []);
    if (requested.length === 0) return;

    // Same reasoning as ratings: a colour label describes the capture.
    const pathsToUpdate = selectionFor(BOTH('fullStack'), requested);

    const primaryPath = selectedImage?.path || libraryActivePath;
    const primaryImage = imageList.find((img: ImageFile) => img.path === primaryPath);
    let currentColor = null;
    if (primaryImage && primaryImage.tags) {
      const colorTag = primaryImage.tags.find((tag: string) => tag.startsWith('color:'));
      if (colorTag) currentColor = colorTag.substring(6);
    }
    const finalColor = color !== null && color === currentColor ? null : color;

    try {
      await invoke(Invokes.SetColorLabelForPaths, { paths: pathsToUpdate, color: finalColor });
      setLibrary((state) => ({
        imageList: state.imageList.map((image: ImageFile) => {
          if (pathsToUpdate.includes(image.path)) {
            const otherTags = (image.tags || []).filter((tag: string) => !tag.startsWith('color:'));
            const newTags = finalColor ? [...otherTags, `color:${finalColor}`] : otherTags;
            return { ...image, tags: newTags };
          }
          return image;
        }),
      }));
    } catch (err) {
      toast.error(`Failed to set color label: ${err}`);
    }
  }, []);

  const handleTagsChanged = useCallback((changedPaths: string[], newTags: { tag: string; isUser: boolean }[]) => {
    useLibraryStore.getState().setLibrary((state) => ({
      imageList: state.imageList.map((image) => {
        if (changedPaths.includes(image.path)) {
          const colorTags = (image.tags || []).filter((t) => t.startsWith('color:'));
          const prefixedNewTags = newTags.map((t) => (t.isUser ? `user:${t.tag}` : t.tag));
          const finalTags = [...colorTags, ...prefixedNewTags].sort();
          return { ...image, tags: finalTags.length > 0 ? finalTags : null };
        }
        return image;
      }),
    }));
  }, []);

  const handleUpdateExif = useCallback(async (paths: Array<string> | undefined, updates: Record<string, string>) => {
    const { multiSelectedPaths, imageList, setLibrary } = useLibraryStore.getState();
    const { selectedImage, setEditor } = useEditorStore.getState();

    const requested =
      paths && paths.length > 0
        ? paths
        : multiSelectedPaths.length > 0
          ? multiSelectedPaths
          : selectedImage
            ? [selectedImage.path]
            : [];
    if (requested.length === 0) return;

    // Capture metadata, so the whole stack, both kinds.
    const pathsToUpdate = selectionFor(BOTH('fullStack'), requested);

    const physicalPathsSet = new Set(pathsToUpdate.map((p) => p.split('?vc=')[0]));
    const physicalPathsArray = Array.from(physicalPathsSet);

    try {
      await invoke(Invokes.UpdateExifFields, { paths: physicalPathsArray, updates });

      setEditor((state) => {
        if (!state.selectedImage || !physicalPathsSet.has(state.selectedImage.path.split('?vc=')[0])) return state;
        return { selectedImage: { ...state.selectedImage, exif: { ...(state.selectedImage.exif || {}), ...updates } } };
      });

      setLibrary((state) => ({
        imageList: state.imageList.map((img) => {
          if (physicalPathsSet.has(img.path.split('?vc=')[0])) {
            return { ...img, exif: { ...(img.exif || {}), ...updates } };
          }
          return img;
        }),
      }));

      pathsToUpdate.forEach((p) => {
        const cached = globalImageCache.get(p);
        if (cached && cached.selectedImage) {
          globalImageCache.set(p, {
            ...cached,
            selectedImage: { ...cached.selectedImage, exif: { ...(cached.selectedImage.exif || {}), ...updates } },
          });
        }
      });
    } catch (err) {
      toast.error(`Failed to update metadata: ${err}`);
    }
  }, []);

  const handleClearSelection = useCallback(() => {
    const activeView = useUIStore.getState().activeView;
    const { selectedImage } = useEditorStore.getState();

    if (activeView === 'editor' && selectedImage) {
      useLibraryStore.getState().setLibrary({
        multiSelectedPaths: [selectedImage.path],
        libraryActivePath: selectedImage.path,
        selectionAnchorPath: selectedImage.path,
      });
    } else {
      useLibraryStore.getState().setLibrary({
        multiSelectedPaths: [],
        libraryActivePath: null,
        selectionAnchorPath: null,
      });

      useEditorStore.getState().setEditor({
        selectedImage: null,
        finalPreviewUrl: null,
        uncroppedAdjustedPreviewUrl: null,
        histogram: null,
        waveform: null,
        activeMaskId: null,
        activeMaskContainerId: null,
        activeAiPatchContainerId: null,
        activeAiSubMaskId: null,
        isWbPickerActive: false,
        transformedOriginalUrl: null,
      });
    }
  }, []);

  const handleMultiSelectClick = useCallback(
    (
      path: string,
      event: any,
      options: {
        onSimpleClick(p: string, isAlreadySelected: boolean): void;
        updateLibraryActivePath: boolean;
        shiftAnchor: string | null;
      },
    ) => {
      const libraryState = useLibraryStore.getState();
      const { multiSelectedPaths, setLibrary } = libraryState;
      const { ctrlKey, metaKey, shiftKey } = event;
      const isCtrlPressed = ctrlKey || metaKey;
      const { shiftAnchor, onSimpleClick, updateLibraryActivePath } = options;

      const isAlreadySelected = multiSelectedPaths.includes(path);

      if (shiftKey && shiftAnchor) {
        const sortedImageList = computeSortedLibrary(libraryState, useSettingsStore.getState());
        const anchorIndex = sortedImageList.findIndex((f) => f.path === shiftAnchor);
        const currentIndex = sortedImageList.findIndex((f) => f.path === path);

        if (anchorIndex !== -1 && currentIndex !== -1) {
          const start = Math.min(anchorIndex, currentIndex);
          const end = Math.max(anchorIndex, currentIndex);
          const range = sortedImageList.slice(start, end + 1).map((f) => f.path);
          const baseSelection = isCtrlPressed ? multiSelectedPaths : [];
          const newSelection = Array.from(new Set([...baseSelection, ...range]));

          setLibrary({ multiSelectedPaths: newSelection, selectionAnchorPath: path });
          if (updateLibraryActivePath) setLibrary({ libraryActivePath: path });
        }
      } else if (isCtrlPressed) {
        const newSelection = new Set(multiSelectedPaths);
        if (newSelection.has(path)) newSelection.delete(path);
        else newSelection.add(path);

        const newSelectionArray = Array.from(newSelection);
        setLibrary({ multiSelectedPaths: newSelectionArray, selectionAnchorPath: path });

        if (updateLibraryActivePath) {
          if (newSelectionArray.includes(path)) setLibrary({ libraryActivePath: path });
          else if (newSelectionArray.length > 0)
            setLibrary({ libraryActivePath: newSelectionArray[newSelectionArray.length - 1] });
          else setLibrary({ libraryActivePath: null });
        }
      } else {
        onSimpleClick(path, isAlreadySelected);
      }
    },
    [],
  );

  const handleLibraryImageSingleClick = useCallback(
    (path: string, event: any) => {
      const { selectionAnchorPath, libraryActivePath, setLibrary } = useLibraryStore.getState();
      handleMultiSelectClick(path, event, {
        shiftAnchor: selectionAnchorPath ?? libraryActivePath,
        updateLibraryActivePath: true,
        onSimpleClick: (p: string, isAlreadySelected: boolean) => {
          if (isAlreadySelected) {
            setLibrary({ libraryActivePath: p, selectionAnchorPath: p });
          } else {
            setLibrary({ multiSelectedPaths: [p], libraryActivePath: p, selectionAnchorPath: p });
          }
          if (handleImageSelect) {
            handleImageSelect(p, false);
          }
        },
      });
    },
    [handleMultiSelectClick, handleImageSelect],
  );

  const handleImageClick = useCallback(
    (path: string, event: any) => {
      const { selectionAnchorPath, libraryActivePath, setLibrary } = useLibraryStore.getState();
      const { selectedImage } = useEditorStore.getState();
      const inEditor = !!selectedImage;

      handleMultiSelectClick(path, event, {
        shiftAnchor: selectionAnchorPath ?? (inEditor ? selectedImage.path : libraryActivePath),
        updateLibraryActivePath: !inEditor,
        onSimpleClick: (p: string, isAlreadySelected: boolean) => {
          if (!isAlreadySelected) {
            setLibrary({ multiSelectedPaths: [p] });
          }
          if (handleImageSelect) handleImageSelect(p);
          setLibrary({ selectionAnchorPath: p });
        },
      });
    },
    [handleMultiSelectClick, handleImageSelect],
  );

  const refreshAllFolderTrees = useCallback(async () => {
    const { rootPaths, expandedFolders, setLibrary } = useLibraryStore.getState();
    const { appSettings } = useSettingsStore.getState();

    const showImageCounts = appSettings?.enableFolderImageCounts ?? false;
    const pinnedFolders = appSettings?.pinnedFolders || [];
    const expandedArray = Array.from(expandedFolders);

    try {
      const updates: any = {};

      if (rootPaths && rootPaths.length > 0) {
        const treesData = await invoke(Invokes.GetPinnedFolderTrees, {
          paths: rootPaths,
          expandedFolders: expandedArray,
          showImageCounts,
        });
        updates.folderTrees = treesData;
      } else {
        updates.folderTrees = [];
      }

      if (pinnedFolders && pinnedFolders.length > 0) {
        const pinnedTreesData = await invoke(Invokes.GetPinnedFolderTrees, {
          paths: pinnedFolders,
          expandedFolders: expandedArray,
          showImageCounts,
        });
        updates.pinnedFolderTrees = pinnedTreesData;
      } else {
        updates.pinnedFolderTrees = [];
      }

      if (Object.keys(updates).length > 0) {
        setLibrary(updates);
      }
    } catch (err) {
      console.error('Failed to refresh folder trees:', err);
    }
  }, []);

  const handleTogglePinFolder = useCallback(async (path: string) => {
    const { appSettings, handleSettingsChange } = useSettingsStore.getState();
    const { expandedFolders, setLibrary } = useLibraryStore.getState();
    if (!appSettings) return;

    const currentPins = appSettings.pinnedFolders || [];
    const isPinned = currentPins.includes(path);
    const newPins = isPinned
      ? currentPins.filter((p: string) => p !== path)
      : [...currentPins, path].sort((a, b) => a.localeCompare(b));

    handleSettingsChange({ ...appSettings, pinnedFolders: newPins });

    try {
      const trees = await invoke(Invokes.GetPinnedFolderTrees, {
        paths: newPins,
        expandedFolders: Array.from(expandedFolders),
        showImageCounts: appSettings.enableFolderImageCounts ?? false,
      });
      setLibrary({ pinnedFolderTrees: trees });
    } catch (err) {
      toast.error(`Failed to refresh pinned folders: ${err}`);
    }
  }, []);

  const handleCreateAlbumItem = useCallback(async (name: string, type: 'album' | 'group') => {
    const { albumTree, setLibrary } = useLibraryStore.getState();
    const { albumActionTarget } = useUIStore.getState();

    const newTree = structuredClone(albumTree);
    const newItem: AlbumItem =
      type === 'album'
        ? ({ type: 'album', id: crypto.randomUUID(), name, images: [] } as Album)
        : ({ type: 'group', id: crypto.randomUUID(), name, children: [] } as AlbumGroup);

    let actualTarget = albumActionTarget;

    const findNode = (nodes: AlbumItem[], id: string): AlbumItem | undefined => {
      for (const n of nodes) {
        if (n.id === id) return n;
        if (n.type === 'group') {
          const found = findNode((n as AlbumGroup).children, id);
          if (found) return found;
        }
      }
      return undefined;
    };

    const findParentId = (nodes: AlbumItem[], childId: string, parentId: string | null): string | null | undefined => {
      for (const n of nodes) {
        if (n.id === childId) return parentId;
        if (n.type === 'group') {
          const found = findParentId((n as AlbumGroup).children, childId, n.id);
          if (found !== undefined) return found;
        }
      }
      return undefined;
    };

    if (actualTarget) {
      const targetNode = findNode(newTree, actualTarget);
      if (targetNode && targetNode.type === 'album') {
        const pId = findParentId(newTree, actualTarget, null);
        actualTarget = pId === undefined ? null : pId;
      }
    }

    const insert = (nodes: AlbumItem[], target: string | null): boolean => {
      if (!target) {
        nodes.push(newItem);
        return true;
      }
      for (const n of nodes) {
        if (n.id === target && n.type === 'group') {
          (n as AlbumGroup).children.push(newItem);
          return true;
        } else if (n.type === 'group') {
          if (insert((n as AlbumGroup).children, target)) return true;
        }
      }
      return false;
    };

    if (insert(newTree, actualTarget)) {
      try {
        await invoke(Invokes.SaveAlbums, { tree: newTree });
        const sortedTree = await invoke(Invokes.GetAlbums);
        setLibrary({ albumTree: sortedTree as AlbumItem[] });
      } catch (err) {
        toast.error(`Failed to create: ${err}`);
      }
    }
  }, []);

  const handleRenameAlbumItem = useCallback(async (newName: string) => {
    const { albumTree, setLibrary } = useLibraryStore.getState();
    const { albumActionTarget } = useUIStore.getState();
    if (!albumActionTarget) return;

    const newTree = structuredClone(albumTree);

    const rename = (nodes: AlbumItem[]) => {
      for (const n of nodes) {
        if (n.id === albumActionTarget) {
          n.name = newName;
          return true;
        }
        if (n.type === 'group' && rename((n as AlbumGroup).children)) return true;
      }
      return false;
    };

    if (rename(newTree)) {
      try {
        await invoke(Invokes.SaveAlbums, { tree: newTree });
        const sortedTree = await invoke(Invokes.GetAlbums);
        setLibrary({ albumTree: sortedTree as AlbumItem[] });
      } catch (err) {
        toast.error(`Failed to rename: ${err}`);
      }
    }
  }, []);

  return {
    handleRate,
    handleAdjustRating,
    handleUndoRating,
    handleRedoRating,
    handleSetColorLabel,
    handleTagsChanged,
    handleUpdateExif,
    handleClearSelection,
    handleLibraryImageSingleClick,
    handleImageClick,
    refreshAllFolderTrees,
    handleTogglePinFolder,
    handleCreateAlbumItem,
    handleRenameAlbumItem,
  };
}
