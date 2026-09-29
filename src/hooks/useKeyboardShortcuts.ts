import { useCallback, useEffect, useRef } from 'react';
import { toast } from 'react-toastify';
import { ImageFile, Panel, ExifOverlay } from '../components/ui/AppProperties';
import { KEYBIND_DEFINITIONS, normalizeCombo } from '../utils/keyboardUtils';
import { actionForChange } from '../utils/currentAction';
import { redoTarget, undoTarget } from '../utils/appHistory';
import {
  isRecording,
  recordAppAction,
  recordingFinished,
  recordingStarted,
} from '../utils/appHistory';
import { debouncedSave } from './useEditorActions';
import { useAppUndo } from './useAppUndo';
import { nameForKeys } from '../utils/historyNames';
import { allQuickAdjustments, quickActionName, quickKeybindDefinitions } from '../utils/quickAdjustments';
import { adjustmentsAreLoadedFor, applyNudgeSteps, recordNudge } from '../utils/pendingNudges';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { EDIT_RULE } from '../utils/imageStacking';
import { selectionFor } from '../utils/selection';
import { extendRange, sameSelection } from '../utils/rangeSelection';
import { useSettingsStore } from '../store/useSettingsStore';
import { isPanelShowing, useUIStore } from '../store/useUIStore';
import { useProcessStore } from '../store/useProcessStore';
import { useEditorActions } from './useEditorActions';
import { useLibraryActions } from './useLibraryActions';
import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { INITIAL_ADJUSTMENTS, normalizeLoadedAdjustments } from '../utils/adjustments';
import { globalImageCache } from '../utils/ImageLRUCache';

interface KeyboardShortcutsProps {
  sortedImageList: Array<ImageFile>;
  /** The same ref useImageProcessing compares against, so a nudge is not read as an edit to sync. */
  prevAdjustmentsRef: React.RefObject<any>;
  handleBackToLibrary(): void;
  handleDeleteSelected(): void;
  handleImageSelect(path: string, openInEditor?: boolean): void;
  handlePasteFiles(str: string): void;
  handleToggleFullScreen(): void;
  handleZoomChange(zoomValue: number, fitToWindow?: boolean): void;
}


export const useKeyboardShortcuts = ({
  sortedImageList,
  prevAdjustmentsRef,
  handleBackToLibrary,
  handleDeleteSelected,
  handleImageSelect,
  handlePasteFiles,
  handleToggleFullScreen,
  handleZoomChange,
}: KeyboardShortcutsProps) => {
  const { handleRotate, handleCopyAdjustments, handlePasteAdjustments } = useEditorActions();
  const { handleRate, handleSetColorLabel } = useLibraryActions();
  const { walkAction } = useAppUndo(handleBackToLibrary, prevAdjustmentsRef);

  const sortedListRef = useRef(sortedImageList);
  useEffect(() => {
    sortedListRef.current = sortedImageList;
  }, [sortedImageList]);

  // BLITZRAW: how far Alt and an arrow have reached from where they started.
  // Outside the effect below on purpose: that effect is rebuilt whenever a
  // setting or a handler changes, and a reach declared inside it would be lost
  // mid-gesture. See utils/rangeSelection.ts for why it is a signed reach from
  // an anchor rather than a set of paths.
  const rangeReach = useRef<{ anchorPath: string; extent: number; produced: Array<string> } | null>(null);

  const handleCopyImagePaths = useCallback(async (paths: Array<string>) => {
    const physicalPaths = [...new Set(paths.map((path) => path.split('?vc=')[0]))];
    if (physicalPaths.length === 0) {
      return;
    }
    try {
      await navigator.clipboard.writeText(physicalPaths.join('\n'));
    } catch (err) {
      console.error('Failed to copy image path to clipboard', err);
      toast.error(`Failed to copy path: ${err}`);
    }
  }, []);

  useEffect(() => {
    const getStoreState = () => ({
      editor: useEditorStore.getState(),
      library: useLibraryStore.getState(),
      ui: useUIStore.getState(),
      settings: useSettingsStore.getState(),
      process: useProcessStore.getState(),
    });

    const comboMap = new Map<string, string>();
    const keybinds = useSettingsStore.getState().appSettings?.keybinds;

    const customQuick = useSettingsStore.getState().appSettings?.quickAdjustments;
    const quickSteps = useSettingsStore.getState().appSettings?.quickAdjustmentSteps;
    const quickDefs = quickKeybindDefinitions(customQuick);

    for (const def of [...KEYBIND_DEFINITIONS, ...quickDefs]) {
      const userCombo = keybinds?.[def.action];
      // BLITZRAW: an empty combo is a key cleared in Settings, which shows it as
      // "Not assigned". Falling back to the default here kept the cleared key
      // working, so a default could never be unbound.
      const effective = userCombo !== undefined ? userCombo : def.defaultCombo;
      if (effective && effective.length > 0) {
        comboMap.set(effective.join('+'), def.action);
      }
    }

    // BLITZRAW: Shift + Cmd + Z is redo in every Mac app, and Shift + Ctrl + Z
    // in many Windows ones. It works as a second key for redo, unless redo has
    // been unbound or that combination is bound to something else.
    if ([...comboMap.values()].includes('redo') && !comboMap.has('ctrl+shift+KeyZ')) {
      comboMap.set('ctrl+shift+KeyZ', 'redo');
    }

    const getImagePathsForCopy = (s: any): Array<string> => {
      if (s.editor.selectedImage) {
        return [s.editor.selectedImage.path];
      }
      const { libraryActivePath, multiSelectedPaths } = s.library;
      if (multiSelectedPaths.length > 0) {
        const listOrder = new Map(sortedListRef.current.map((image: ImageFile, index: number) => [image.path, index]));
        return [...multiSelectedPaths].sort(
          (a: string, b: string) =>
            (listOrder.get(a) ?? Number.MAX_SAFE_INTEGER) - (listOrder.get(b) ?? Number.MAX_SAFE_INTEGER),
        );
      }
      return libraryActivePath ? [libraryActivePath] : [];
    };

    // Lightroom's R and D jump straight to a tool from anywhere. From the grid
    // that has to open the image large first, otherwise the panel would change
    // behind a view where it is not visible.
    const openLoupeIfInGrid = (s: any) => {
      if (s.ui.activeView === 'library' && s.library.libraryActivePath) {
        handleImageSelect(s.library.libraryActivePath, true);
      }
    };

    // What a quick adjustment acts on when it is not one open image: the
    // selection, resolved the same way every other editing action resolves it,
    // so a collapsed peer stack moves together.
    const quickTargets = (s: any): Array<string> => {
      const clicked = s.library.multiSelectedPaths.length > 0
        ? s.library.multiSelectedPaths
        : s.library.libraryActivePath
          ? [s.library.libraryActivePath]
          : [];
      return clicked.length > 0 ? selectionFor(EDIT_RULE, clicked) : [];
    };

    // Where a file that has never had this setting touched should start.
    // White balance is left out on purpose: absent means as-shot, which the
    // backend reads from the camera profile rather than a constant.
    const quickFallback = (item: any): number | null => {
      if (item.path.includes('.')) return null;
      const value = (INITIAL_ADJUSTMENTS as any)[item.path];
      return typeof value === 'number' ? value : null;
    };

    // Who does the work for one press.
    //
    // The open image is the one with a slider on screen, so it is nudged in the
    // store and written by the same auto-save any other edit uses. Sending it
    // to the backend as well was the whole of the fighting: the backend wrote
    // the file, the answer came back behind a thumbnail render and therefore
    // out of order, and an old number landed in the store and was then saved
    // over the new one. Nineteen quick presses on two images left the open one
    // on 1.50 and the other on the correct 1.90.
    //
    // Everything else in the selection still goes to the backend, since it has
    // no slider to move and each file has to start from its own value.
    const quickPlan = (s: any) => {
      const targets = quickTargets(s);
      const open = s.editor.selectedImage;
      // The open photo is the front end's whether or not it has been decoded.
      // Whether its own values have arrived decides how the press is held, not
      // whether the press is accepted at all; see pendingNudges.ts. Handing a
      // half-open photo to the backend instead is what lost the press: the
      // backend wrote the file, the sidecar read already in flight came back
      // without the change, and that stale value was saved back over it.
      const openPath = open?.path ?? null;
      const inEditor = s.ui.activeView === 'editor' && !!openPath;
      const takesOpen = !!openPath && (targets.includes(openPath) || (inEditor && targets.length === 0));
      return {
        openPath: takesOpen ? openPath : null,
        backendPaths: takesOpen ? targets.filter((path: string) => path !== openPath) : targets,
      };
    };

    // One press on the open image, in the front end.
    //
    // `alsoBackend` says the rest of the selection is being nudged file by
    // file. Auto-sync would read this move as an ordinary edit and push one
    // number across all of them, replacing each file's own value, so the ref it
    // compares against is moved forward at the same time.
    //
    // A photo whose own adjustments have not arrived yet is not refused. The
    // press is counted and spent the moment they land, which is the whole of
    // pendingNudges.ts.
    const nudgeOpenImage = (openPath: string, item: any, direction: 'up' | 'down', alsoBackend: boolean) => {
      const isLoaded = adjustmentsAreLoadedFor(openPath);
      recordNudge(openPath, item, direction, isLoaded);
      if (!isLoaded) {
        return;
      }
      applyNudgeSteps(openPath, item, direction === 'up' ? 1 : -1, (merged) => {
        if (alsoBackend) {
          prevAdjustmentsRef.current = {
            path: openPath,
            adjustments: merged,
            setBy: 'a keyboard nudge',
            setAt: Date.now(),
          };
        }
      });
    };

    // ============== BLITZRAW: reaching the pen path that is being placed ==============
    // Masks live two deep: a container holds sub-masks, and the active one is
    // named by id rather than held directly. These three walk that, so the
    // keyboard branches above read as what they do rather than as three nested
    // finds each.
    //
    // Both ids are checked because the same sub-mask list is reached from the
    // masks panel and from the AI panel, and a path started in one has to be
    // finishable without knowing which panel opened it.
    const penBeingPlaced = (s: any): { containerId: string; subMask: any } | null => {
      const activeSubMaskId = s.editor.activeMaskId || s.editor.activeAiSubMaskId;
      if (!activeSubMaskId) return null;

      for (const container of s.editor.adjustments?.masks || []) {
        for (const subMask of container.subMasks || []) {
          if (subMask.id === activeSubMaskId && subMask.type === 'pen' && subMask.parameters?.isDrawing) {
            return { containerId: container.id, subMask };
          }
        }
      }
      return null;
    };

    const updateSubMaskParameters = (s: any, containerId: string, subMaskId: string, parameters: any) => {
      s.editor.setEditor((state: any) => ({
        adjustments: {
          ...state.adjustments,
          masks: state.adjustments.masks.map((container: any) =>
            container.id !== containerId
              ? container
              : {
                  ...container,
                  subMasks: container.subMasks.map((subMask: any) =>
                    subMask.id === subMaskId ? { ...subMask, parameters } : subMask,
                  ),
                },
          ),
        },
      }));
    };

    const dropSubMask = (s: any, containerId: string, subMaskId: string) => {
      s.editor.setEditor((state: any) => ({
        adjustments: {
          ...state.adjustments,
          masks: state.adjustments.masks.map((container: any) =>
            container.id !== containerId
              ? container
              : { ...container, subMasks: container.subMasks.filter((sub: any) => sub.id !== subMaskId) },
          ),
        },
        activeMaskId: null,
        activeAiSubMaskId: null,
      }));
    };
    // ============ BLITZRAW END: reaching the pen path that is being placed ============

    const actions: Record<string, any> = {
      open_image: {
        shouldFire: (s: any) => s.ui.activeView === 'library' && s.library.libraryActivePath !== null,
        execute: (e: any, s: any) => {
          e.preventDefault();
          handleImageSelect(s.library.libraryActivePath!, true);
        },
      },
      enter_loupe_view: {
        shouldFire: (s: any) => s.ui.activeView === 'library' && s.library.libraryActivePath !== null,
        execute: (e: any, s: any) => {
          e.preventDefault();
          handleImageSelect(s.library.libraryActivePath!, true);
        },
      },
      exit_to_grid: {
        shouldFire: (s: any) => s.ui.activeView === 'editor',
        execute: (e: any) => {
          e.preventDefault();
          handleBackToLibrary();
        },
      },
      deselect_all: {
        shouldFire: (s: any) => s.library.multiSelectedPaths.length > 0,
        execute: (e: any, s: any) => {
          e.preventDefault();
          // Clears the selection but keeps the active image, so arrow-key
          // navigation carries on from where you were.
          s.library.setLibrary({ multiSelectedPaths: [] });
        },
      },
      copy_adjustments: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleCopyAdjustments();
        },
      },
      paste_adjustments: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handlePasteAdjustments();
        },
      },
      copy_image_path: {
        shouldFire: (s: any) => getImagePathsForCopy(s).length > 0,
        execute: (e: any, s: any) => {
          e.preventDefault();
          handleCopyImagePaths(getImagePathsForCopy(s));
        },
      },
      copy_files: {
        shouldFire: (s: any) => s.library.multiSelectedPaths.length > 0,
        execute: (e: any, s: any) => {
          e.preventDefault();
          // Copying a peer stack copies the whole thing; a merged result
          // copies on its own.
          s.process.setProcess({ copiedFilePaths: selectionFor(EDIT_RULE, s.library.multiSelectedPaths) });
        },
      },
      paste_files: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handlePasteFiles('copy');
        },
      },
      select_all: {
        shouldFire: () => sortedListRef.current.length > 0,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const everything = sortedListRef.current.map((f: ImageFile) => f.path);
          s.library.setLibrary({ multiSelectedPaths: everything });
          if (s.ui.activeView === 'library') {
            // ============ BLITZRAW: selecting everything keeps me where I am ============
            // This used to make the **last** photo in the folder the active one.
            // So selecting all in a folder of 1,377 frames threw the grid to the
            // end, and the photo I had been working on stopped being the one the
            // panel was editing. Selecting everything is about the selection; it
            // is not a statement about which photo I am looking at.
            //
            // A photo is only picked when there is nothing to keep: no active
            // photo, or one that is not in this list any more because a filter
            // has moved on. Then the first, which is where the grid already is,
            // rather than the far end of it.
            const stay = s.library.libraryActivePath;
            if (!stay || !everything.includes(stay)) {
              handleImageSelect(everything[0], false);
            } else if (s.editor.selectedImage?.path !== stay) {
              // Active, but not the one loaded, so the panel would be editing
              // something other than the photo the grid says is current.
              handleImageSelect(stay, false);
            }
            // ========== BLITZRAW END: selecting everything keeps me where I am ==========
          }
        },
      },
      // ============ BLITZRAW: Alt and an arrow grow or shrink the selection ============
      // One press is one frame, and the direction is a reach rather than a
      // side: pressing left while reaching right lets the far frame go, and
      // carries straight on into reaching left once there is nothing left to
      // let go of. The arithmetic is in `extendRange`.
      //
      // It works on the list as filtered and sorted, and stores exactly the
      // rows it walked, so a collapsed stack counts as the one row it draws as.
      // That is the rule the whole selection model rests on: what is stored is
      // what was picked, and each action expands stacks when it reads it.
      //
      // The frame the run started on stays the current one, in both views. The
      // grid used to walk it out to the far end so the view would scroll along,
      // which read as the selection dragging me rather than me growing it: the
      // frame I was working from kept moving out from under the panel, and in
      // the editor it would have decoded a different raw on every press.
      ...Object.fromEntries(
        ([
          ['extend_selection_next', 1],
          ['extend_selection_prev', -1],
        ] as Array<[string, number]>).map(([name, delta]) => [
          name,
          {
            shouldFire: () => sortedListRef.current.length > 0,
            execute: (e: any, s: any) => {
              e.preventDefault();

              const paths = sortedListRef.current.map((image: ImageFile) => image.path);
              const selection = s.library.multiSelectedPaths;
              const held = rangeReach.current;

              // Anything that replaced the selection since the last press ends
              // the run, and this one starts again from the current frame.
              const carriesOn = held && sameSelection(held.produced, selection);
              const anchorPath = carriesOn ? held!.anchorPath : (s.library.libraryActivePath ?? selection[0] ?? paths[0]);
              const extent = carriesOn ? held!.extent : 0;

              if (!anchorPath) {
                return;
              }

              const step = extendRange(paths, anchorPath, extent, delta);
              if (!step) {
                rangeReach.current = null;
                return;
              }

              rangeReach.current = { anchorPath: step.anchorPath, extent: step.extent, produced: step.paths };

              // The grid follows the growing end without the current frame
              // going with it, so the frames being taken in stay on screen
              // while the one being worked on stays where it was put.
              s.library.setLibrary((state: any) => ({
                multiSelectedPaths: step.paths,
                scrollRequest: { path: step.edgePath, id: (state.scrollRequest?.id ?? 0) + 1 },
              }));
            },
          },
        ]),
      ),
      // ========== BLITZRAW END: Alt and an arrow grow or shrink the selection ==========

      // ==================== BLITZRAW: invert the selection ====================
      // Ctrl+I, as in Lightroom. It works on the list as filtered and sorted,
      // not on the folder, so inverting inside a three-star filter cannot pull
      // back the frames the filter is hiding.
      //
      // The active photo follows the same rule select_all settled on: keep it
      // where it is if the new selection still holds it, and only move when it
      // does not, because the panel must never be editing a photo the grid no
      // longer says is current.
      invert_selection: {
        shouldFire: () => sortedListRef.current.length > 0,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const selected = new Set(s.library.multiSelectedPaths);
          const inverted = sortedListRef.current
            .map((f: ImageFile) => f.path)
            .filter((path: string) => !selected.has(path));

          s.library.setLibrary({ multiSelectedPaths: inverted });

          if (inverted.length === 0 || s.ui.activeView !== 'library') {
            return;
          }

          const stay = s.library.libraryActivePath;
          if (!stay || !inverted.includes(stay)) {
            handleImageSelect(inverted[0], false);
          } else if (s.editor.selectedImage?.path !== stay) {
            handleImageSelect(stay, false);
          }
        },
      },
      // ================== BLITZRAW END: invert the selection ==================
      delete_selected: {
        shouldFire: (s: any) => !s.editor.activeMaskContainerId && !s.editor.activeAiPatchContainerId,
        execute: (e: any) => {
          e.preventDefault();
          handleDeleteSelected();
        },
      },
      preview_prev: {
        shouldFire: (s: any) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const currentIndex = sortedListRef.current.findIndex((img) => img.path === s.editor.selectedImage!.path);
          if (currentIndex === -1) return;
          let nextIndex = currentIndex - 1 < 0 ? sortedListRef.current.length - 1 : currentIndex - 1;
          handleImageSelect(sortedListRef.current[nextIndex].path, true);
        },
      },
      preview_next: {
        shouldFire: (s: any) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const currentIndex = sortedListRef.current.findIndex((img) => img.path === s.editor.selectedImage!.path);
          if (currentIndex === -1) return;
          let nextIndex = currentIndex + 1 >= sortedListRef.current.length ? 0 : currentIndex + 1;
          handleImageSelect(sortedListRef.current[nextIndex].path, true);
        },
      },
      zoom_in_step: {
        shouldFire: (s: any) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
          const currentPercent =
            s.editor.originalSize?.width > 0 && s.editor.displaySize?.width > 0
              ? (s.editor.displaySize.width * dpr) / s.editor.originalSize.width
              : 1.0;
          handleZoomChange(Math.min(currentPercent + 0.1, 2.0));
        },
      },
      zoom_out_step: {
        shouldFire: (s: any) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
          const currentPercent =
            s.editor.originalSize?.width > 0 && s.editor.displaySize?.width > 0
              ? (s.editor.displaySize.width * dpr) / s.editor.originalSize.width
              : 1.0;
          handleZoomChange(Math.max(currentPercent - 0.1, 0.1));
        },
      },
      cycle_zoom: {
        shouldFire: (s: any) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
          const { originalSize, displaySize, baseRenderSize } = s.editor;
          const currentPercent =
            originalSize?.width > 0 && displaySize?.width > 0
              ? Math.round(((displaySize.width * dpr) / originalSize.width) * 100)
              : 100;
          let fitPercent = 100;

          if (originalSize?.width > 0 && baseRenderSize?.width > 0) {
            const originalAspect = originalSize.width / originalSize.height;
            const baseAspect = baseRenderSize.width / baseRenderSize.height;
            fitPercent =
              originalAspect > baseAspect
                ? Math.round(((baseRenderSize.width * dpr) / originalSize.width) * 100)
                : Math.round(((baseRenderSize.height * dpr) / originalSize.height) * 100);
          }

          const doubleFitPercent = fitPercent * 2;
          if (Math.abs(currentPercent - fitPercent) < 5) {
            handleZoomChange(doubleFitPercent < 100 ? doubleFitPercent / 100 : 1.0);
          } else if (Math.abs(currentPercent - doubleFitPercent) < 5 && doubleFitPercent < 100) {
            handleZoomChange(1.0);
          } else {
            handleZoomChange(0, true);
          }
        },
      },
      zoom_in: {
        shouldFire: (s: any) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
          const currentPercent =
            s.editor.originalSize?.width > 0 && s.editor.displaySize?.width > 0
              ? (s.editor.displaySize.width * dpr) / s.editor.originalSize.width
              : 1.0;
          handleZoomChange(Math.min(currentPercent * 1.2, 2.0));
        },
      },
      zoom_out: {
        shouldFire: (s: any) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
          const currentPercent =
            s.editor.originalSize?.width > 0 && s.editor.displaySize?.width > 0
              ? (s.editor.displaySize.width * dpr) / s.editor.originalSize.width
              : 1.0;
          handleZoomChange(Math.max(currentPercent / 1.2, 0.1));
        },
      },
      zoom_fit: {
        shouldFire: (s: any) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e: any) => {
          e.preventDefault();
          handleZoomChange(0, true);
        },
      },
      zoom_100: {
        shouldFire: (s: any) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e: any) => {
          e.preventDefault();
          handleZoomChange(1.0);
        },
      },
      rotate_left: {
        shouldFire: (s: any) => !!s.editor.selectedImage || !!s.library.libraryActivePath,
        execute: (e: any) => {
          e.preventDefault();
          handleRotate(-90);
        },
      },
      rotate_right: {
        shouldFire: (s: any) => !!s.editor.selectedImage || !!s.library.libraryActivePath,
        execute: (e: any) => {
          e.preventDefault();
          handleRotate(90);
        },
      },
      // ============ BLITZRAW: ratings can be taken back ============
      // Two stacks, one key. Adjustments live in the editor's history and only
      // exist while a photo is open; ratings live in the library store and are
      // most often changed in the grid, where there is no open photo at all,
      // which is why Ctrl+Z used to do nothing there.
      //
      // The more recently changed stack goes first. Both stamp themselves on
      // every move, undo included, so once you start walking back through one
      // you keep walking through that one until it runs out. Without that, a
      // single Ctrl+Z would hop between the two and neither would feel like it
      // was undoing anything.
      // ============ BLITZRAW: undo acts on what I did, not on the open photo ============
      // One list, one behaviour, everywhere. An entry says which photos an
      // action touched, what was selected and which view it was done in, so
      // undoing it puts the selection and the view back and then asks **each
      // photo** what it was before that action.
      //
      // That last part is the whole point, and it is the opposite of a fan-out.
      // A fan-out would send the open photo's restored values to all the others
      // and overwrite theirs. This gives every photo back its own answer, out of
      // its own log on disk, so ten photos that started from ten different
      // exposures go back to ten different exposures.
      //
      // It also means undo does not read the Copy and Paste tick boxes. Those
      // decide what spreads when a change is made and have nothing to say about
      // putting things back. See utils/appActions.ts.
      undo: {
        // Fires while a write is still on its way too, or a press made straight
        // after an edit would fall through and do nothing. `walkAction` waits
        // for it. See utils/appHistory.ts.
        shouldFire: () => undoTarget() !== null || isRecording() || !!(debouncedSave as any).pending?.(),
        execute: (e: any) => {
          e.preventDefault();
          void walkAction(false);
        },
      },
      redo: {
        shouldFire: () => redoTarget() !== null,
        execute: (e: any) => {
          e.preventDefault();
          void walkAction(true);
        },
      },
      // ========== BLITZRAW END: undo acts on what I did, not on the open photo ==========
      // ========== BLITZRAW END: ratings can be taken back ==========
      toggle_fullscreen: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any) => {
          e.preventDefault();
          handleToggleFullScreen();
        },
      },
      show_original: {
        shouldFire: (s: any) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.editor.setEditor({ showOriginal: !s.editor.showOriginal });
        },
      },
      toggle_adjustments: {
        shouldFire: () => true,
        execute: (e: any, s: any) => {
          e.preventDefault();
          openLoupeIfInGrid(s);
          s.ui.setPanel(Panel.Adjustments);
        },
      },
      toggle_crop_panel: {
        shouldFire: () => true,
        execute: (e: any, s: any) => {
          e.preventDefault();
          openLoupeIfInGrid(s);
          s.ui.setPanel(Panel.Crop);
        },
      },
      toggle_masks: {
        shouldFire: () => true,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.ui.setPanel(Panel.Masks);
        },
      },
      toggle_ai: {
        shouldFire: () => true,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.ui.setPanel(Panel.Ai);
        },
      },
      toggle_presets: {
        shouldFire: () => true,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.ui.setPanel(Panel.Presets);
        },
      },
      toggle_metadata: {
        shouldFire: () => true,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.ui.setPanel(Panel.Metadata);
        },
      },
      toggle_folder_tree: {
        shouldFire: () => true,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.ui.setPanel(Panel.FolderTree);
        },
      },
      toggle_analytics: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.editor.setEditor({ isWaveformVisible: !s.editor.isWaveformVisible });
        },
      },
      toggle_export: {
        shouldFire: () => true,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.ui.setPanel(Panel.Export);
        },
      },
      toggle_left_panel: {
        shouldFire: () => true,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const isOpening = !s.ui.uiVisibility.leftPanel;
          s.ui.setUI((state: any) => ({
            uiVisibility: { ...state.uiVisibility, leftPanel: isOpening },
            leftPanelWidth: isOpening && state.leftPanelWidth < 250 ? 350 : state.leftPanelWidth,
          }));
        },
      },
      toggle_right_panel: {
        shouldFire: () => true,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const isOpening = !s.ui.uiVisibility.rightPanel;
          s.ui.setUI((state: any) => ({
            uiVisibility: { ...state.uiVisibility, rightPanel: isOpening },
            rightPanelWidth: isOpening && state.rightPanelWidth < 250 ? 350 : state.rightPanelWidth,
          }));
        },
      },
      toggle_bottom_panel: {
        shouldFire: (s: any) => s.ui.activeView !== 'library',
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.ui.setUI((state: any) => ({
            uiVisibility: { ...state.uiVisibility, filmstrip: !state.uiVisibility.filmstrip },
          }));
        },
      },
      toggle_library_exif: {
        shouldFire: (s: any) => s.ui.activeView === 'library',
        execute: (e: any, s: any) => {
          e.preventDefault();
          const current = s.settings.appSettings?.exifOverlay || ExifOverlay.Off;
          const nextState = {
            [ExifOverlay.Off]: ExifOverlay.Hover,
            [ExifOverlay.Hover]: ExifOverlay.Always,
            [ExifOverlay.Always]: ExifOverlay.Off,
          }[current as ExifOverlay];
          s.settings.handleSettingsChange({ ...s.settings.appSettings, exifOverlay: nextState });
        },
      },
      open_settings: {
        shouldFire: () => true,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.ui.setUI({ isSettingsOpen: true });
        },
      },
      focus_search: {
        shouldFire: (s: any) => s.ui.activeView === 'library',
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.ui.requestSearchFocus();
        },
      },
      toggle_crop: {
        shouldFire: (s: any) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          if (isPanelShowing(s.ui.activePanels, Panel.Crop)) {
            s.editor.setEditor({ isStraightenActive: !s.editor.isStraightenActive });
          } else {
            s.ui.setPanel(Panel.Crop);
            s.editor.setEditor({ isStraightenActive: true });
          }
        },
      },
      rate_0: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleRate(0);
        },
      },
      rate_1: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleRate(1);
        },
      },
      rate_2: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleRate(2);
        },
      },
      rate_3: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleRate(3);
        },
      },
      rate_4: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleRate(4);
        },
      },
      rate_5: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleRate(5);
        },
      },
      color_label_none: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleSetColorLabel(null);
        },
      },
      color_label_red: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleSetColorLabel('red');
        },
      },
      color_label_yellow: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleSetColorLabel('yellow');
        },
      },
      color_label_green: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleSetColorLabel('green');
        },
      },
      color_label_blue: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleSetColorLabel('blue');
        },
      },
      color_label_purple: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleSetColorLabel('purple');
        },
      },
      brush_size_up: {
        shouldFire: (s: any) =>
          s.ui.activeView === 'editor' &&
          !!s.editor.selectedImage &&
          !!s.editor.brushSettings &&
          s.ui.activePanel === Panel.Masks,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const newSize = Math.min((s.editor.brushSettings.size || 50) + 10, 200);
          s.editor.setEditor({ brushSettings: { ...s.editor.brushSettings, size: newSize } });
        },
      },
      brush_size_down: {
        shouldFire: (s: any) =>
          s.ui.activeView === 'editor' &&
          !!s.editor.selectedImage &&
          !!s.editor.brushSettings &&
          s.ui.activePanel === Panel.Masks,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const newSize = Math.max((s.editor.brushSettings.size || 50) - 10, 1);
          s.editor.setEditor({ brushSettings: { ...s.editor.brushSettings, size: newSize } });
        },
      },
    };

    // Two entries per quick adjustment, built from the same list Settings
    // shows, so anything added from a slider is bindable without another
    // switch statement to keep in step.
    for (const item of allQuickAdjustments(customQuick, quickSteps)) {
      for (const direction of ['up', 'down'] as const) {
        actions[quickActionName(item.id, direction)] = {
          shouldFire: (s: any) => {
            const plan = quickPlan(s);
            return !!plan.openPath || plan.backendPaths.length > 0;
          },
          execute: (e: any, s: any) => {
            e.preventDefault();
            const { openPath, backendPaths } = quickPlan(s);

            // On screen before the key is released, and undoable, because it is
            // an ordinary edit to the image being looked at.
            if (openPath) {
              nudgeOpenImage(openPath, item, direction, backendPaths.length > 0);
            }

            if (backendPaths.length === 0) {
              return;
            }

            // The rest of the selection has no slider, and each file starts
            // from its own value, so the nudge is a read and a write per file.
            const delta = direction === 'up' ? item.step : -item.step;
            // BLITZRAW: keyed on the top level of the setting name, because that
            // is the name a photo's own log records: `whiteBalance`, not
            // `whiteBalance.kelvin`. A run of presses on one key is then one
            // action, and a press on a different key starts another.
            const nudgeAction = actionForChange([item.path.split('.')[0]]);
            recordingStarted();
            invoke(Invokes.NudgeAdjustmentsForPaths, {
              paths: backendPaths,
              path: item.path,
              delta,
              min: item.min,
              max: item.max,
              fallback: quickFallback(item),
              // BLITZRAW: keyed on the top level of the setting name, because
              // that is the name a photo's own log records: `whiteBalance`, not
              // `whiteBalance.kelvin`. A run of presses on one key is then one
              // action, and a press on a different key starts another. See
              // utils/actionId.ts.
              historyAction: nudgeAction,
            })
              .then((photos: any) => {
                // BLITZRAW: recorded once the photos have reported which
                // numbers they moved between, so a press can be taken back.
                recordAppAction({
                  id: nudgeAction,
                  kind: 'adjustments',
                  label: nameForKeys([item.path.split('.')[0]]),
                  photos: photos ?? [],
                  selection: backendPaths,
                  openPath: s.editor.selectedImage?.path ?? null,
                  inEditor: s.ui.activeView === 'editor',
                });
                const changed = photos?.length ?? 0;
                if (changed > 0) {
                  // Only the decoded-image cache. The backend re-renders the
                  // thumbnails and emits them, so clearing the store here would
                  // just blank the grid until they arrived.
                  backendPaths.forEach((path: string) => globalImageCache.delete(path));
                }
              })
              .catch((err) => {
                console.error('Quick adjustment failed:', err);
                toast.error(`${err}`);
              })
              .finally(recordingFinished);
          },
        };
      }
    }

    const builtinShortcuts = [
      // ============ BLITZRAW: finishing a pen path from the keyboard ============
      // A path that is not closed still needs an ending, and the mouse has no
      // gesture for one: clicking anywhere else would only add another point.
      // Enter joins the ends, Escape leaves them open. Both stop the placing
      // mode, which is what actually hands the photo back to panning.
      //
      // These sit above Escape's own chain rather than inside it, because that
      // chain is a ladder of ways to back out of things and this is not one of
      // them: the path is finished and kept, not abandoned.
      {
        match: (e: KeyboardEvent, s: any) => {
          if (e.code !== 'Escape' && e.code !== 'Enter' && e.code !== 'NumpadEnter') {
            return false;
          }
          return penBeingPlaced(s) !== null;
        },
        execute: (e: KeyboardEvent, s: any) => {
          e.preventDefault();
          const found = penBeingPlaced(s);
          if (!found) return;

          const { containerId, subMask } = found;
          const points = subMask.parameters?.points || [];

          // Too few points to enclose anything, so there is no path to keep.
          // The sub-mask goes with it: an empty one would sit in the list
          // looking like a mask that had stopped working.
          if (points.length < 3) {
            dropSubMask(s, containerId, subMask.id);
            return;
          }

          updateSubMaskParameters(s, containerId, subMask.id, {
            ...subMask.parameters,
            closed: e.code !== 'Escape',
            isDrawing: false,
          });
        },
      },
      // ========== BLITZRAW END: finishing a pen path from the keyboard ==========
      {
        match: (e: KeyboardEvent) => e.code === 'Escape',
        execute: (e: KeyboardEvent, s: any) => {
          e.preventDefault();
          if (s.editor.isStraightenActive) s.editor.setEditor({ isStraightenActive: false });
          else if (s.ui.customEscapeHandler) s.ui.customEscapeHandler();
          else if (s.editor.activeAiSubMaskId) s.editor.setEditor({ activeAiSubMaskId: null });
          else if (s.editor.activeAiPatchContainerId) s.editor.setEditor({ activeAiPatchContainerId: null });
          else if (s.editor.activeMaskId) s.editor.setEditor({ activeMaskId: null });
          else if (s.editor.activeMaskContainerId) s.editor.setEditor({ activeMaskContainerId: null });
          else if (isPanelShowing(s.ui.activePanels, Panel.Crop)) s.ui.setPanel(Panel.Adjustments);
          else if (s.ui.isFullScreen) handleToggleFullScreen();
          else if (s.ui.activeView === 'editor') handleBackToLibrary();
        },
      },
      {
        match: (e: KeyboardEvent, s: any) => {
          const isDeleteKey = s.settings.osPlatform === 'macos' ? e.code === 'Backspace' : e.code === 'Delete';
          return isDeleteKey && (!!s.editor.activeMaskContainerId || !!s.editor.activeAiPatchContainerId);
        },
        execute: (e: KeyboardEvent, s: any) => {
          e.preventDefault();
          if (s.editor.activeMaskContainerId) {
            s.editor.setEditor((state: any) => ({
              adjustments: {
                ...state.adjustments,
                masks: state.adjustments.masks.filter((c: any) => c.id !== s.editor.activeMaskContainerId),
              },
              activeMaskContainerId: null,
              activeMaskId: null,
            }));
          } else if (s.editor.activeAiPatchContainerId) {
            s.editor.setEditor((state: any) => ({
              adjustments: {
                ...state.adjustments,
                aiPatches: state.adjustments.aiPatches.filter((c: any) => c.id !== s.editor.activeAiPatchContainerId),
              },
              activeAiPatchContainerId: null,
              activeAiSubMaskId: null,
            }));
          }
        },
      },
      {
        // Bare arrows only. This runs before user bindings are consulted, so
        // matching a modified arrow too would swallow every combo built on one,
        // which is where Ctrl and Shift plus an arrow went: they navigated the
        // grid instead of nudging anything.
        match: (e: KeyboardEvent, s: any) =>
          s.ui.activeView === 'library' &&
          !e.ctrlKey &&
          !e.metaKey &&
          !e.altKey &&
          !e.shiftKey &&
          ['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight'].includes(e.code),
        execute: (e: KeyboardEvent, s: any) => {
          e.preventDefault();
          const isNext = e.code === 'ArrowRight' || e.code === 'ArrowDown';
          const activePath = s.library.libraryActivePath;
          if (!activePath || sortedListRef.current.length === 0) return;
          const currentIndex = sortedListRef.current.findIndex((img) => img.path === activePath);
          if (currentIndex === -1) return;
          let nextIndex = isNext ? currentIndex + 1 : currentIndex - 1;
          if (nextIndex >= sortedListRef.current.length) nextIndex = 0;
          if (nextIndex < 0) nextIndex = sortedListRef.current.length - 1;
          const nextImage = sortedListRef.current[nextIndex];
          if (nextImage) {
            s.library.setLibrary({ libraryActivePath: nextImage.path, multiSelectedPaths: [nextImage.path] });
            handleImageSelect(nextImage.path, false);
          }
        },
      },
    ];

    const handleKeyDown = (event: KeyboardEvent) => {
      const state = getStoreState();

      const isModalOpen =
        state.ui.isCreateFolderModalOpen ||
        state.ui.isRenameFolderModalOpen ||
        state.ui.isRenameFileModalOpen ||
        state.ui.isImportModalOpen ||
        state.ui.isCopyPasteSettingsModalOpen ||
        state.ui.confirmModalState.isOpen ||
        state.ui.panoramaModalState.isOpen ||
        state.ui.cullingModalState.isOpen ||
        state.ui.collageModalState.isOpen ||
        state.ui.denoiseModalState.isOpen ||
        state.ui.negativeModalState.isOpen;

      if (isModalOpen) return;

      if (state.ui.isSettingsOpen) {
        if (event.code === 'Escape') {
          event.preventDefault();
          state.ui.setUI({ isSettingsOpen: false });
        }
        return;
      }

      // ============ BLITZRAW: a field keeps its own keystrokes ============
      // Asked of the element the key was pressed **in**, not of whatever holds
      // focus by the time this runs. A field that closes itself on Enter blurs
      // during its own handler, so by the time the window hears the same key
      // the focus is back on the body and the guard let it through: Enter in a
      // slider's number field committed the value and then opened the photo in
      // full view, which reloaded its adjustments from disk over the value that
      // had just been typed. The keystroke belongs to the field it happened in,
      // whatever happens to focus afterwards.
      const typedInto = event.target as HTMLElement | null;
      const isTypingTarget = (element: Element | null | undefined) =>
        element instanceof HTMLElement &&
        (element.tagName === 'INPUT' || element.tagName === 'TEXTAREA' || element.isContentEditable);

      if (isTypingTarget(typedInto) || isTypingTarget(document.activeElement)) return;
      // ========== BLITZRAW END: a field keeps its own keystrokes ==========

      for (const builtin of builtinShortcuts) {
        if (builtin.match(event, state)) {
          builtin.execute(event, state);
          return;
        }
      }

      const normalized = normalizeCombo(event, state.settings.osPlatform);
      const action = comboMap.get(normalized.join('+'));

      if (action) {
        const handler = actions[action];
        if (handler && (!handler.shouldFire || handler.shouldFire(state))) {
          handler.execute(event, state);
          return;
        }
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => {
      window.removeEventListener('keydown', handleKeyDown);
    };
  }, [
    handleBackToLibrary,
    handleDeleteSelected,
    handleImageSelect,
    handlePasteFiles,
    handleToggleFullScreen,
    handleZoomChange,
    handleRotate,
    handleCopyAdjustments,
    handleCopyImagePaths,
    handlePasteAdjustments,
    handleRate,
    handleSetColorLabel,
    walkAction,
  ]);
};
