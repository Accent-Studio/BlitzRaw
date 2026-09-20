import { useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import debounce from 'lodash.debounce';
import { toast } from 'react-toastify';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { EDIT_RULE } from '../utils/imageStacking';
import { selectionFor } from '../utils/selection';
import { useSettingsStore } from '../store/useSettingsStore';
import { useProcessStore } from '../store/useProcessStore';
import {
  Adjustments,
  INITIAL_ADJUSTMENTS,
  COPYABLE_ADJUSTMENT_KEYS,
  PasteMode,
  LensAdjustment,
  normalizeLoadedAdjustments,
} from '../utils/adjustments';
import { calculateCenteredCrop } from '../utils/cropUtils';
import { actionForChange } from '../utils/currentAction';
import { recordAppAction, recordingFinished, recordingStarted } from '../utils/appHistory';
import { Invokes } from '../components/ui/AppProperties';
import { globalImageCache } from '../utils/ImageLRUCache';

/**
 * The one rate limiter on history, shared by every `setAdjustments` there is.
 *
 * There used to be two, this one and another built inside `Editor.tsx`, each
 * with its own timer on the same value. Two writers for one value is the fault
 * this project has already paid for once, and the grouping rule in `appendStep`
 * only works if every change goes through one clock.
 *
 * It limits churn; it does not decide what a step is. A drag fires this sixty
 * times a second and there is no reason to touch the store that often, but what
 * counts as one move is `appendStep`'s rule and not this delay.
 *
 * The path travels with the change so that a push still in the air when a
 * different photo is opened is dropped rather than recorded against the wrong
 * photo. See `pushHistory`.
 */
export const debouncedSetHistory = debounce((newAdj: Adjustments, forPath: string | null) => {
  useEditorStore.getState().pushHistory(newAdj, null, forPath);
}, 500);

export const debouncedSave = debounce(
  (
    path: string,
    adjustmentsToSave: Adjustments,
    actionId: string | null = null,
    // BLITZRAW: told which numbers this write moved the photo between, once the
    // backend has actually written it. Passed as a callback rather than
    // returned, because this is rate limited and the caller is long gone.
    stepped: ((photo: { path: string; from: number; to: number }) => void) | null = null,
  ) => {
    // BLITZRAW: the name of the step, when the action had one worth more than a
    // diff would read. Whether this write continues the step at the top is not
    // sent, because this and the editor's own history push are separately rate
    // limited and arrive in whichever order they arrive; the backend works that
    // out from the log it is already holding. See edit_history.rs.
    const { historyPendingLabel } = useEditorStore.getState();
    if (historyPendingLabel) {
      useEditorStore.setState({ historyPendingLabel: null });
    }
    // BLITZRAW: a Ctrl+Z pressed before this comes back waits for it, rather
    // than finding nothing and doing nothing. See utils/appHistory.ts.
    recordingStarted();
    invoke(Invokes.SaveMetadataAndUpdateThumbnail, {
      path,
      adjustments: adjustmentsToSave,
      historyLabel: historyPendingLabel,
      // BLITZRAW: which one thing the user did this write is part of.
      //
      // Passed in rather than read here. `historyPendingLabel` above is read at
      // the moment this fires, which is 300 ms after the edit, and that is right
      // for a label because a label belongs to whatever named itself last. It
      // would be wrong for an action: by then the next action may have opened,
      // and this write would be filed under it. See utils/actionId.ts.
      historyAction: actionId,
    })
      .then((photo: any) => {
        if (photo && stepped) {
          stepped(photo);
        }
      })
      .catch((err) => {
        console.error('Auto-save failed:', err);
        toast.error(`Failed to save changes: ${err}`);
      })
      .finally(recordingFinished);
  },
  300,
);

export function useEditorActions() {
  const setEditor = useEditorStore((s) => s.setEditor);

  const setAdjustments = useCallback(
    // BLITZRAW: `label` is what the step is called, for an action that knows its
    // own name.
    //
    // Without it a step is named by listing what moved, which is right for a
    // slider and useless for anything that moves forty things at once: applying
    // a preset read as "Blacks, Brightness, Clarity, ..." and said nothing about
    // which preset. A named step goes in at once rather than on the usual
    // delay, so nothing else can join it: a preset is one event.
    (value: Partial<Adjustments> | ((prev: Adjustments) => Adjustments), label: string | null = null) => {
      setEditor((state) => {
        const prev = state.adjustments;
        const newAdjustments = typeof value === 'function' ? value(prev) : { ...prev, ...value };
        if (label) {
          debouncedSetHistory.cancel();
        } else {
          debouncedSetHistory(newAdjustments, state.selectedImage?.path ?? null);
        }
        return { adjustments: newAdjustments };
      });
      if (label) {
        const state = useEditorStore.getState();
        state.pushHistory(state.adjustments, label);
      }
    },
    [setEditor],
  );

  const handleRotate = useCallback(
    (degrees: number) => {
      const { selectedImage, adjustments } = useEditorStore.getState();
      const increment = degrees > 0 ? 1 : 3;
      const newAspectRatio =
        adjustments.aspectRatio && adjustments.aspectRatio !== 0 ? 1 / adjustments.aspectRatio : null;
      const newOrientationSteps = ((adjustments.orientationSteps || 0) + increment) % 4;
      const newCrop =
        selectedImage?.width && selectedImage?.height
          ? calculateCenteredCrop(selectedImage.width, selectedImage.height, newOrientationSteps, newAspectRatio)
          : null;

      setAdjustments((prev) => ({
        ...prev,
        aspectRatio: newAspectRatio,
        orientationSteps: newOrientationSteps,
        rotation: 0,
        crop: newCrop,
      }));
    },
    [setAdjustments],
  );

  const handleAutoAdjustments = useCallback(async () => {
    const selectedImage = useEditorStore.getState().selectedImage;
    if (!selectedImage?.isReady) return;
    try {
      const autoAdjustments: Adjustments = await invoke(Invokes.CalculateAutoAdjustments);
      setAdjustments((prev: Adjustments) => ({
        ...prev,
        ...autoAdjustments,
        sectionVisibility: { ...prev.sectionVisibility, ...autoAdjustments.sectionVisibility },
      }));
    } catch (err) {
      toast.error(`Failed to apply auto adjustments: ${err}`);
    }
  }, [setAdjustments]);

  const handleLutSelect = useCallback(
    async (path: string) => {
      const isAndroid = useSettingsStore.getState().osPlatform === 'android';
      try {
        const result: { size: number } = await invoke('load_and_parse_lut', { path });
        let name =
          isAndroid && path.startsWith('content://')
            ? await invoke<string>('resolve_android_content_uri_name', { uriStr: path })
            : path.split(/[\\/]/).pop() || 'LUT';
        setAdjustments((prev: Adjustments) => ({
          ...prev,
          lutPath: path,
          lutName: name,
          lutSize: result.size,
          lutIntensity: 100,
          sectionVisibility: { ...(prev.sectionVisibility || INITIAL_ADJUSTMENTS.sectionVisibility), effects: true },
        }));
      } catch (err) {
        toast.error(`Failed to load LUT: ${err}`);
      }
    },
    [setAdjustments],
  );

  const setLutPreviewOverride = useCallback(
    (path: string | null) => {
      setEditor((state) => {
        if (!path) return { previewOverride: null };
        const name = path.split(/[\\/]/).pop() || 'LUT';
        return {
          previewOverride: {
            ...state.adjustments,
            lutPath: path,
            lutName: name,
            lutIntensity: state.adjustments.lutIntensity,
          },
        };
      });
    },
    [setEditor],
  );

  const handleResetAdjustments = useCallback(
    (paths?: string[]) => {
      const { multiSelectedPaths, libraryActivePath, setLibrary } = useLibraryStore.getState();
      const { selectedImage } = useEditorStore.getState();
      // A merged result resets alone; a peer stack resets together.
      const pathsToReset = selectionFor(EDIT_RULE, paths || multiSelectedPaths);
      if (pathsToReset.length === 0) return;

      pathsToReset.forEach((p) => globalImageCache.delete(p));
      debouncedSetHistory.cancel();

      // BLITZRAW: a reset is one deliberate event, so it opens an action of its
      // own that nothing joins. The open photo is skipped in the backend because
      // the editor pushes its own step for it, the same as a paste.
      const resetAction = actionForChange(['reset'], true);
      recordingStarted();
      invoke(Invokes.ResetAdjustmentsForPaths, {
        paths: pathsToReset,
        skipHistoryFor: selectedImage?.path ?? null,
      })
        .then((photos: any) => {
          // The hard rule: anything that writes a step into a photo writes an
          // entry here. Reset used to write photos and record nothing at all,
          // so it could not be taken back on any photo but the open one.
          recordAppAction({
            id: resetAction,
            kind: 'adjustments',
            label: 'Reset',
            photos: photos ?? [],
            selection: pathsToReset,
            openPath: selectedImage?.path ?? null,
            inEditor: !!selectedImage,
          });
          if (libraryActivePath && pathsToReset.includes(libraryActivePath))
            setLibrary({ libraryActiveAdjustments: { ...INITIAL_ADJUSTMENTS } });
          if (selectedImage && pathsToReset.includes(selectedImage.path)) {
            const aspect =
              selectedImage.width && selectedImage.height ? selectedImage.width / selectedImage.height : null;
            const resetData = { ...INITIAL_ADJUSTMENTS, aspectRatio: aspect, aiPatches: [] };
            // A step, not a fresh start. Resetting a photo is a thing you did to
            // it, and being able to walk back out of it is the point of having
            // a history at all.
            useEditorStore.getState().pushHistory(resetData, 'Reset');
            setEditor({ adjustments: resetData });
          }
        })
        .catch((err) => toast.error(`Failed to reset adjustments: ${err}`))
        .finally(recordingFinished);
    },
    [setEditor],
  );

  const handleCopyAdjustments = useCallback(async (pathOrEvent?: string | any) => {
    const pathOverride = typeof pathOrEvent === 'string' ? pathOrEvent : undefined;
    const { selectedImage, adjustments } = useEditorStore.getState();
    const { libraryActivePath, multiSelectedPaths } = useLibraryStore.getState();
    let sourceAdjustments: any = null;

    const pathToCopyFrom =
      pathOverride || (selectedImage ? selectedImage.path : libraryActivePath || multiSelectedPaths[0]);

    if (selectedImage && pathToCopyFrom === selectedImage.path) {
      sourceAdjustments = adjustments;
    } else if (pathToCopyFrom) {
      try {
        const meta: any = await invoke(Invokes.LoadMetadata, { path: pathToCopyFrom });
        if (meta?.adjustments && !meta.adjustments.is_null) {
          sourceAdjustments = normalizeLoadedAdjustments(meta.adjustments);
        } else {
          sourceAdjustments = INITIAL_ADJUSTMENTS;
        }
      } catch (err) {
        toast.error(`Failed to load metadata for copying: ${err}`);
        return;
      }
    }

    if (!sourceAdjustments) return;

    const adjustmentsToCopy: any = {};

    for (const key of COPYABLE_ADJUSTMENT_KEYS) {
      if (Object.prototype.hasOwnProperty.call(sourceAdjustments, key)) {
        adjustmentsToCopy[key] = structuredClone(sourceAdjustments[key]);
      }
    }
    useEditorStore.getState().setEditor({ copiedAdjustments: adjustmentsToCopy });
    useProcessStore.getState().setProcess({ isCopied: true });
  }, []);

  const handlePasteAdjustments = useCallback(
    (paths?: string[]) => {
      const { copiedAdjustments, selectedImage, adjustments } = useEditorStore.getState();
      const { multiSelectedPaths } = useLibraryStore.getState();
      const { appSettings } = useSettingsStore.getState();
      const { setProcess } = useProcessStore.getState();

      if (!copiedAdjustments || !appSettings) return;

      const { mode, includedAdjustments } = appSettings.copyPasteSettings;
      const adjustmentsToApply: Partial<Adjustments> = {};

      for (const key of includedAdjustments) {
        if (Object.prototype.hasOwnProperty.call(copiedAdjustments, key)) {
          const value = copiedAdjustments[key as keyof Adjustments];
          if (mode === PasteMode.Merge) {
            const defaultValue = INITIAL_ADJUSTMENTS[key as keyof Adjustments];
            if (JSON.stringify(value) !== JSON.stringify(defaultValue))
              adjustmentsToApply[key as keyof Adjustments] = value;
          } else {
            adjustmentsToApply[key as keyof Adjustments] = value;
          }
        }
      }

      if (includedAdjustments.includes(LensAdjustment.LensMaker)) {
        if (!adjustmentsToApply.lensMaker) {
          adjustmentsToApply.lensDistortionParams = null;
        }
      }

      if (Object.keys(adjustmentsToApply).length === 0) {
        setProcess({ isPasted: true });
        return;
      }

      const pathsToUpdate = selectionFor(
        EDIT_RULE,
        paths || (multiSelectedPaths.length > 0 ? multiSelectedPaths : selectedImage ? [selectedImage.path] : []),
      );
      if (pathsToUpdate.length === 0) return;

      pathsToUpdate.forEach((p) => globalImageCache.delete(p));

      // BLITZRAW: a paste is one deliberate event, so it opens an action of its
      // own that nothing before or after it joins, however fast they land. See
      // utils/actionId.ts.
      const pasteAction = actionForChange(Object.keys(adjustmentsToApply), true);

      if (selectedImage && pathsToUpdate.includes(selectedImage.path)) {
        // BLITZRAW: named, because pasting is one event and the list of things
        // it moved says nothing about what was pasted.
        setAdjustments({ ...adjustments, ...adjustmentsToApply }, 'Pasted settings');
      }

      recordingStarted();
      invoke(Invokes.ApplyAdjustmentsToPaths, {
        paths: pathsToUpdate,
        adjustments: adjustmentsToApply,
        // BLITZRAW: the open photo pushes its own step above and saves it a
        // moment later, so recording one for it here as well would give it the
        // same change twice.
        skipHistoryFor: selectedImage?.path ?? null,
        // BLITZRAW: a paste is one deliberate event, so it opens an action of
        // its own that nothing before or after it joins, however fast they
        // land. See utils/actionId.ts.
        historyAction: pasteAction,
      })
        .then((photos: any) => {
          // BLITZRAW: recorded once the photos have reported which numbers they
          // moved between, so a paste can be taken back like anything else.
          recordAppAction({
            id: pasteAction,
            kind: 'adjustments',
            label: 'Pasted settings',
            photos: photos ?? [],
            selection: pathsToUpdate,
            openPath: selectedImage?.path ?? null,
            inEditor: !!selectedImage,
          });
          if (selectedImage && pathsToUpdate.includes(selectedImage.path)) {
            invoke('load_metadata', { path: selectedImage.path }).then((meta: any) => {
              if (meta.adjustments) {
                setAdjustments((prev: any) => ({
                  ...prev,
                  lensMaker: meta.adjustments.lensMaker,
                  lensModel: meta.adjustments.lensModel,
                  lensDistortionParams: meta.adjustments.lensDistortionParams,
                }));
              }
            });
          }
        })
        .catch((err) => toast.error(`Failed to paste adjustments: ${err}`))
        .finally(recordingFinished);

      setProcess({ isPasted: true });
    },
    [setAdjustments],
  );

  const handleZoomChange = useCallback((zoomValue: number, fitToWindow: boolean = false) => {
    const { originalSize, baseRenderSize, adjustments } = useEditorStore.getState();
    const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
    let targetZoomPercent: number;

    const orientationSteps = adjustments.orientationSteps || 0;
    const isSwapped = orientationSteps === 1 || orientationSteps === 3;
    const effectiveOriginalWidth = isSwapped ? originalSize.height : originalSize.width;
    const effectiveOriginalHeight = isSwapped ? originalSize.width : originalSize.height;

    if (fitToWindow) {
      if (
        effectiveOriginalWidth > 0 &&
        effectiveOriginalHeight > 0 &&
        baseRenderSize.width > 0 &&
        baseRenderSize.height > 0
      ) {
        const originalAspect = effectiveOriginalWidth / effectiveOriginalHeight;
        const baseAspect = baseRenderSize.width / baseRenderSize.height;
        targetZoomPercent =
          originalAspect > baseAspect
            ? baseRenderSize.width / effectiveOriginalWidth
            : baseRenderSize.height / effectiveOriginalHeight;
      } else {
        targetZoomPercent = 1.0;
      }
    } else {
      targetZoomPercent = zoomValue / dpr;
    }

    targetZoomPercent = Math.max(0.1 / dpr, Math.min(2.0, targetZoomPercent));

    let transformZoom = 1.0;
    if (
      effectiveOriginalWidth > 0 &&
      effectiveOriginalHeight > 0 &&
      baseRenderSize.width > 0 &&
      baseRenderSize.height > 0
    ) {
      const originalAspect = effectiveOriginalWidth / effectiveOriginalHeight;
      const baseAspect = baseRenderSize.width / baseRenderSize.height;
      if (originalAspect > baseAspect) {
        transformZoom = (targetZoomPercent * effectiveOriginalWidth) / baseRenderSize.width;
      } else {
        transformZoom = (targetZoomPercent * effectiveOriginalHeight) / baseRenderSize.height;
      }
    }
    useEditorStore.getState().setEditor({ zoom: transformZoom });
  }, []);

  return {
    setAdjustments,
    handleRotate,
    handleAutoAdjustments,
    handleLutSelect,
    setLutPreviewOverride,
    handleResetAdjustments,
    handleCopyAdjustments,
    handlePasteAdjustments,
    handleZoomChange,
  };
}
