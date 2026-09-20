import React, { useCallback, useEffect, useRef, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import debounce from 'lodash.debounce';
import { useEditorStore } from '../store/useEditorStore';
import { isPanelShowing, useUIStore } from '../store/useUIStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { EDIT_RULE } from '../utils/imageStacking';
import { selectionFor } from '../utils/selection';
import { Adjustments, COPYABLE_ADJUSTMENT_KEYS, DisplayMode } from '../utils/adjustments';
import { PendingSync, autoSyncPlan, sameTargets, syncDelta } from '../utils/autoSync';
import { changedKeys } from '../utils/editHistory';
import { uncroppedPreviewNeedsRedraw } from '../utils/cropPreview';
import { actionForChange, closeAction } from '../utils/currentAction';
import { recordAppAction, recordingFinished, recordingStarted } from '../utils/appHistory';
import { nameForKeys } from '../utils/historyNames';
import { mergeScopeRequests, scopeName, scopeToken } from '../utils/scopeRequest';
import { useScopeSource } from './useScopeSource';
import { FLOATING_REGIONS } from '../utils/floatingLayout';

/** The four regions that are drawn in the main window. */
const SIDEBAR_REGIONS = ['leftTop', 'leftBottom', 'rightTop', 'rightBottom'] as const;
import { Invokes, Panel } from '../components/ui/AppProperties';
import { debouncedSave } from './useEditorActions';
import { globalImageCache } from '../utils/ImageLRUCache';

export function useImageProcessing(
  transformWrapperRef: any,
  prevAdjustmentsRef: React.RefObject<any>,
  renderRefs: {
    previewJobIdRef: React.RefObject<number>;
    latestRenderedJobIdRef: React.RefObject<number>;
    currentResRef: React.RefObject<number>;
  },
) {
  const { previewJobIdRef, latestRenderedJobIdRef, currentResRef } = renderRefs;

  const selectedImage = useEditorStore((state) => state.selectedImage);
  const adjustments = useEditorStore((state) => state.adjustments);
  const previewOverride = useEditorStore((state) => state.previewOverride);
  // The scopes are a panel now, and may be in a window of their own, so what
  // decides whether the backend computes a waveform is whether anyone is
  // looking at one: this window's layout, or a detached window that said so.
  // Split, because the two columns are different columns. The sidebar's is in
  // this window's own store; the floating one is reported by the window showing
  // it. Both are known here, since the main window owns the whole layout, so
  // whether a scope is being looked at is answered without waiting to be told.
  const scopesInSidebar = useUIStore((state) =>
    SIDEBAR_REGIONS.some((region) => state.activePanels[region] === Panel.Scopes),
  );
  const scopesFloating = useUIStore((state) =>
    FLOATING_REGIONS.some((region) => state.activePanels[region] === Panel.Scopes),
  );
  const scopesInLayout = scopesInSidebar;
  const detachedScopeChannels = useUIStore((state) => state.detachedScopeChannels);
  const ownScopeChannels = useEditorStore((state) => state.waveformChannels);
  const vectorscopeGain = useEditorStore((state) => state.vectorscopeGain);
  const isWaveformVisible = scopesInSidebar || scopesFloating;
  // The union of both columns: one pass fills whatever is asked for, so
  // computing a scope twice would be the same work again. Never empty while
  // something is wanted, because the backend reads an empty list as "all of
  // them" and would quietly compute five scopes to show one.
  //
  // The vectorscope's gain rides in its own name, `vectorscope:3`, because the
  // request is the only thing that travels the whole way to the backend. The
  // detached window sends its column already written that way. See
  // scopeRequest.ts.
  const activeWaveformChannel = useMemo(() => {
    if (!isWaveformVisible) return '';
    const own = scopesInLayout
      ? ownScopeChannels.map((mode) =>
          scopeName(mode) === DisplayMode.Vectorscope ? scopeToken(DisplayMode.Vectorscope, vectorscopeGain) : mode,
        )
      : [];
    return mergeScopeRequests(own, scopesFloating ? detachedScopeChannels : null) || 'luma';
  }, [isWaveformVisible, scopesInLayout, scopesFloating, ownScopeChannels, detachedScopeChannels, vectorscopeGain]);
  // BLITZRAW: the scopes follow the pointer, and fall back to the preview or
  // the thumbnail rather than waiting on a decode. Called from here because
  // this is where the request string is already worked out, and this hook is
  // mounted in every view rather than only in the editor.
  useScopeSource(activeWaveformChannel, isWaveformVisible);

  const displaySize = useEditorStore((state) => state.displaySize);
  const baseRenderSize = useEditorStore((state) => state.baseRenderSize);
  const originalSize = useEditorStore((state) => state.originalSize);
  const showOriginal = useEditorStore((state) => state.showOriginal);
  const isSliderDragging = useEditorStore((state) => state.isSliderDragging);
  // BLITZRAW: the backend has the decode. See the crop preview effect below.
  const isBackendReady = useEditorStore((state) => state.isBackendReady);
  // BLITZRAW: the crop panel is the one its sidebar is showing. Asking the
  // global `activePanel` meant the left sidebar could turn the crop tool off by
  // changing what it showed. See `isPanelShowing` in useUIStore.
  const isCropShowing = useUIStore((state) => isPanelShowing(state.activePanels, Panel.Crop));
  const isSliderTyping = useEditorStore((state) => state.isSliderTyping);
  const transformedOriginalUrl = useEditorStore((state) => state.transformedOriginalUrl);
  const setEditor = useEditorStore((state) => state.setEditor);

  const activeView = useUIStore((state) => state.activeView);
  const activePanel = useUIStore((state) => state.activePanel);
  const appSettings = useSettingsStore((state) => state.appSettings);
  const multiSelectedPaths = useLibraryStore((state) => state.multiSelectedPaths);

  // How long a run of adjustments has to settle before the rest of the
  // selection is told about it. Each fan-out is a decode and a render per file,
  // so six nudges across twenty photos is a hundred and twenty decodes where
  // twenty would do. Long enough to swallow a run of key presses, short enough
  // that letting go of a slider feels like it took effect.
  const AUTO_SYNC_DELAY_MS = 700;

  /// BLITZRAW: and the longest it may ever be put off.
  ///
  /// The delay above is a trailing one, restarted by every call, and the effect
  /// that calls it re-runs on a dozen things that are not edits: a slider being
  /// touched, the scopes changing what they ask for, the view changing, the
  /// selection changing. So while the user was working it could be pushed out
  /// indefinitely, and everything done in that window piled into one change
  /// waiting to be sent. A crop made on seven photos was still waiting when two
  /// hundred were selected.
  ///
  /// A ceiling means a change is never held for longer than this, whatever else
  /// is happening. See utils/autoSync.ts.
  const AUTO_SYNC_MAX_WAIT_MS = 1500;

  const pendingSyncRef = useRef<PendingSync | null>(null);
  /**
   * BLITZRAW: the adjustments this effect last saw, and whose photo they were.
   *
   * Only used to work out which adjustments moved, so a run of presses on one
   * slider is recognised as one thing the user did. Keyed to a photo, because a
   * state left over from the previous one would report every difference between
   * two photos as a change somebody had just made. See utils/actionId.ts.
   */
  const lastSeenAdjustmentsRef = useRef<{ path: string; adjustments: Adjustments } | null>(null);
  /** The last history move this hook has seen, so a repeat is not read as one. */
  const lastHistoryMoveRef = useRef<number | null>(null);

  const inFlightCountRef = useRef(0);
  const pendingApplyRef = useRef<{ adjustments: Adjustments; targetRes?: number } | null>(null);
  const currentOriginalResRef = useRef<number>(0);
  const dragIdleTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const activeWaveformChannelRef = useRef(activeWaveformChannel);
  activeWaveformChannelRef.current = activeWaveformChannel;

  const selectedImagePathRef = useRef<string | null>(null);
  useEffect(() => {
    selectedImagePathRef.current = selectedImage?.path ?? null;
  }, [selectedImage?.path]);

  // Sent once, when the run stops. Everything in the delta is an absolute
  // value rather than an increment, so this is idempotent: a batch the next one
  // supersedes has lost nothing, which is what makes it safe for the backend to
  // abandon a superseded render pass.
  const flushAutoSync = useMemo(
    () =>
      debounce(() => {
        const pending = pendingSyncRef.current;
        pendingSyncRef.current = null;
        if (!pending) {
          return;
        }

        // Exactly what the user ticked, and nothing subtracted from it. A crop
        // travels if Crop and Aspect Ratio is ticked, because that is what the
        // tick is for. Deciding here that geometry is too dangerous to send
        // would be overruling a choice the user made deliberately.
        const includedKeys =
          useSettingsStore.getState().appSettings?.copyPasteSettings?.includedAdjustments ||
          COPYABLE_ADJUSTMENT_KEYS;

        const delta = syncDelta(prevAdjustmentsRef.current, pending, includedKeys);

        // Advanced here rather than when the edit happened, so a whole run is
        // measured from where it started rather than from its last fragment.
        prevAdjustmentsRef.current = {
          path: pending.path,
          adjustments: pending.adjustments,
          setBy: 'a fan-out',
          setAt: Date.now(),
        };

        if (Object.keys(delta).length === 0) {
          return;
        }

        // ============ BLITZRAW: say what is about to be written ============
        // A crop meant for seven photos reached two hundred, and neither of the
        // two explanations that fit the code fits what actually happened. The
        // value that spread was the centred rectangle from the instant the
        // ratio was clicked, not any of the framings set by hand afterwards, so
        // it is a state that was held rather than a reference that went stale.
        //
        // This program's own rule is to read the log before theorising. There
        // was nothing in it about a fan-out, so the next one leaves a record:
        // what is going out, to how many files, and how old the reference it
        // was measured against is.
        const referenceAge = prevAdjustmentsRef.current?.setAt
          ? `${Math.round((Date.now() - prevAdjustmentsRef.current.setAt) / 1000)}s ago`
          : 'never set';
        console.info(
          `[auto-sync] sending ${Object.keys(delta).join(', ')} to ${pending.paths.length} photos ` +
            `as ${pending.actionId ?? 'no action'} ` +
            `from ${pending.path}; reference set by ${prevAdjustmentsRef.current?.setBy ?? 'nothing'} ${referenceAge}`,
          Object.fromEntries(
            Object.entries(delta).map(([key, value]) => [key, JSON.stringify(value)?.slice(0, 120)]),
          ),
        );
        // ========== BLITZRAW END: say what is about to be written ==========
        pending.paths.forEach((p) => globalImageCache.delete(p));
        recordingStarted();
        invoke(Invokes.ApplyAdjustmentsToPaths, {
          paths: pending.paths,
          adjustments: delta,
          // BLITZRAW: see the paste path. The open photo is the editor's.
          skipHistoryFor: pending.path ?? null,
          // BLITZRAW: the name that ties these photos to the one in the editor,
          // so an undo can find every file this action wrote.
          historyAction: pending.actionId ?? null,
        })
          .then((photos: any) => {
            // BLITZRAW: the same action, now that its other photos have
            // reported which numbers they moved between.
            recordAppAction({
              id: pending.actionId,
              kind: 'adjustments',
              label: nameForKeys(Object.keys(delta)),
              photos: photos ?? [],
              selection: [pending.path, ...pending.paths],
              openPath: pending.path,
              inEditor: true,
            });
          })
          .catch((err) => {
            console.error('Failed to apply adjustments to multi-selection:', err);
          })
          .finally(recordingFinished);
      }, AUTO_SYNC_DELAY_MS, { maxWait: AUTO_SYNC_MAX_WAIT_MS }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
  );

  // Anything that changes which photo is open sends what is waiting first, or a
  // run of adjustments would be lost by walking away from it.
  //
  // BLITZRAW: and anything that changes which photos are selected, for the
  // opposite reason: what is waiting must reach the photos it was aimed at
  // before the selection it was aimed at stops existing. Keyed on the
  // membership rather than the array, which is rebuilt on every library write.
  const selectionKey = useMemo(() => [...multiSelectedPaths].sort().join('\n'), [multiSelectedPaths]);
  useEffect(() => {
    return () => {
      flushAutoSync.flush();
      // BLITZRAW: after the flush, never before it. What was waiting still
      // belongs to the action it was made under; what comes next does not.
      closeAction();
    };
  }, [selectedImage?.path, selectionKey, flushAutoSync]);

  const geometricAdjustmentsKey = useMemo(() => {
    if (!adjustments) return '';
    const { crop, rotation, flipHorizontal, flipVertical, orientationSteps } = adjustments;
    return JSON.stringify({ crop, rotation, flipHorizontal, flipVertical, orientationSteps });
  }, [
    adjustments?.crop,
    adjustments?.rotation,
    adjustments?.flipHorizontal,
    adjustments?.flipVertical,
    adjustments?.orientationSteps,
  ]);

  const calculateROI = useCallback(() => {
    if (!transformWrapperRef.current) return null;
    const state = transformWrapperRef.current.instance.transformState;
    if (!state) return null;

    if (!baseRenderSize) return null;

    const { scale, positionX, positionY } = state;
    const { width: baseW, height: baseH, offsetX, offsetY, containerWidth, containerHeight } = baseRenderSize;

    if (!baseW || !baseH || !containerWidth || !containerHeight) return null;
    if (scale <= 1.01) return null;

    const paddingPixels = 2.0;
    const paddingX = paddingPixels / baseW;
    const paddingY = paddingPixels / baseH;

    const visibleLeft = -positionX / scale;
    const visibleTop = -positionY / scale;
    const visibleRight = visibleLeft + containerWidth / scale;
    const visibleBottom = visibleTop + containerHeight / scale;

    const imgLeft = offsetX;
    const imgTop = offsetY;
    const imgRight = offsetX + baseW;
    const imgBottom = offsetY + baseH;

    const intersectLeft = Math.max(visibleLeft, imgLeft);
    const intersectTop = Math.max(visibleTop, imgTop);
    const intersectRight = Math.min(visibleRight, imgRight);
    const intersectBottom = Math.min(visibleBottom, imgBottom);

    if (intersectLeft >= intersectRight || intersectTop >= intersectBottom) {
      return null;
    }

    let roiX = (intersectLeft - imgLeft) / baseW;
    let roiY = (intersectTop - imgTop) / baseH;
    let roiW = (intersectRight - intersectLeft) / baseW;
    let roiH = (intersectBottom - intersectTop) / baseH;

    const newRoiX = roiX - paddingX;
    const newRoiY = roiY - paddingY;
    const newRoiW = roiW + paddingX * 2;
    const newRoiH = roiH + paddingY * 2;

    const clampedX = Math.max(0, newRoiX);
    const clampedY = Math.max(0, newRoiY);
    const clampedW = Math.min(1 - clampedX, newRoiW);
    const clampedH = Math.min(1 - clampedY, newRoiH);

    if (clampedW > 0.999 && clampedH > 0.999) return null;

    return [clampedX, clampedY, clampedW, clampedH] as [number, number, number, number];
  }, [baseRenderSize, transformWrapperRef]);

  const executeApplyAdjustments = useCallback(
    async (currentAdjustments: Adjustments, dragging: boolean = false, targetRes?: number) => {
      const currentPath = selectedImage?.path;
      if (!currentPath) return;

      const payload = structuredClone(currentAdjustments);
      const { patchesSentToBackend } = useEditorStore.getState();
      const newlySentPatches = new Set<string>();

      const processSubMasks = (subMasks: any[]) => {
        if (!Array.isArray(subMasks)) return;
        subMasks.forEach((sm: any) => {
          if (sm.id && sm.parameters) {
            const keys = ['mask_data_base64', 'maskDataBase64'];
            let foundMaskData = false;

            for (const key of keys) {
              if (sm.parameters[key] !== undefined && sm.parameters[key] !== null) {
                foundMaskData = true;
                if (patchesSentToBackend.has(sm.id)) {
                  sm.parameters[key] = null;
                }
              }
            }
            if (foundMaskData && !patchesSentToBackend.has(sm.id)) {
              newlySentPatches.add(sm.id);
            }
          }
        });
      };

      if (payload.aiPatches && Array.isArray(payload.aiPatches)) {
        payload.aiPatches.forEach((p: any) => {
          if (p.id && p.patchData && !p.isLoading) {
            if (patchesSentToBackend.has(p.id)) {
              p.patchData = null;
            } else {
              newlySentPatches.add(p.id);
            }
          }
          if (p.subMasks) processSubMasks(p.subMasks);
        });
      }

      if (payload.masks && Array.isArray(payload.masks)) {
        payload.masks.forEach((container: any) => {
          if (container.subMasks) processSubMasks(container.subMasks);
        });
      }

      const jobId = ++previewJobIdRef.current;
      const roi = calculateROI();

      try {
        const buffer: ArrayBuffer = await invoke(Invokes.ApplyAdjustments, {
          jsAdjustments: payload,
          isInteractive: dragging,
          targetResolution: targetRes || null,
          roi: roi || null,
          computeWaveform: !!isWaveformVisible,
          activeWaveformChannel: activeWaveformChannelRef.current || null,
        });

        if (newlySentPatches.size > 0) {
          newlySentPatches.forEach((id) => patchesSentToBackend.add(id));
        }

        if (currentPath !== selectedImagePathRef.current) return;

        if (buffer && buffer.byteLength > 0 && jobId >= latestRenderedJobIdRef.current) {
          latestRenderedJobIdRef.current = jobId;

          const textDecoder = new TextDecoder();
          const prefix = textDecoder.decode(buffer.slice(0, 11));
          if (prefix === 'WGPU_RENDER') {
            setEditor((state) => {
              if (state.interactivePatch && state.interactivePatch.url) URL.revokeObjectURL(state.interactivePatch.url);
              return { interactivePatch: null };
            });
            return;
          }

          if (dragging) {
            const view = new DataView(buffer);
            const patchX = view.getUint32(0, true);
            const patchY = view.getUint32(4, true);
            const patchW = view.getUint32(8, true);
            const patchH = view.getUint32(12, true);
            const fullW = view.getUint32(16, true);
            const fullH = view.getUint32(20, true);

            const imageBuffer = buffer.slice(24);
            const blob = new Blob([imageBuffer], { type: 'image/jpeg' });
            const url = URL.createObjectURL(blob);

            setEditor((state) => {
              if (state.interactivePatch && state.interactivePatch.url)
                setTimeout(() => URL.revokeObjectURL(state.interactivePatch.url), 100);
              return {
                interactivePatch: {
                  url,
                  normX: patchX / fullW,
                  normY: patchY / fullH,
                  normW: patchW / fullW,
                  normH: patchH / fullH,
                },
              };
            });
          } else {
            const blob = new Blob([buffer], { type: 'image/jpeg' });
            const url = URL.createObjectURL(blob);

            if (currentPath !== selectedImagePathRef.current || jobId < latestRenderedJobIdRef.current) {
              URL.revokeObjectURL(url);
              return;
            }

            setEditor((state) => {
              const prevUrl = state.finalPreviewUrl;
              if (prevUrl && prevUrl.startsWith('blob:') && !globalImageCache.isProtected(prevUrl)) {
                setTimeout(() => {
                  if (!globalImageCache.isProtected(prevUrl)) {
                    URL.revokeObjectURL(prevUrl);
                  }
                }, 250);
              }
              return { finalPreviewUrl: url };
            });

            setEditor((state) => {
              if (state.interactivePatch && state.interactivePatch.url) {
                setTimeout(() => URL.revokeObjectURL(state.interactivePatch.url), 500);
              }
              return { interactivePatch: null };
            });
          }
        }
      } catch (err) {
        if (err !== 'Superseded or worker failed') {
          console.error('Failed to apply adjustments:', err);
        }
        if (!dragging) {
          setEditor((state) => {
            if (state.interactivePatch && state.interactivePatch.url) URL.revokeObjectURL(state.interactivePatch.url);
            return { interactivePatch: null };
          });
        }
      }
    },
    [selectedImage?.path, calculateROI, isWaveformVisible, setEditor, previewJobIdRef, latestRenderedJobIdRef],
  );

  const flushPipeline = useCallback(() => {
    if (inFlightCountRef.current >= 3) return;
    if (!pendingApplyRef.current) return;

    const { adjustments, targetRes } = pendingApplyRef.current;
    pendingApplyRef.current = null;

    inFlightCountRef.current += 1;

    executeApplyAdjustments(adjustments, true, targetRes).finally(() => {
      inFlightCountRef.current -= 1;
      if (pendingApplyRef.current) {
        requestAnimationFrame(() => flushPipeline());
      }
    });
  }, [executeApplyAdjustments]);

  const applyAdjustments = useCallback(
    (currentAdjustments: Adjustments, dragging: boolean = false, targetRes?: number) => {
      if (!selectedImage?.isReady) return;

      if (dragging) {
        pendingApplyRef.current = { adjustments: currentAdjustments, targetRes };
        flushPipeline();
      } else {
        pendingApplyRef.current = null;
        executeApplyAdjustments(currentAdjustments, false, targetRes);
      }
    },
    [selectedImage?.isReady, flushPipeline, executeApplyAdjustments],
  );

  // ============ BLITZRAW: one at a time, and the newest wins ============
  // This fired the render straight away, every time, with nothing counting how
  // many were already running. Each one is a GPU pass over the whole frame and
  // each spawns its own thread on the other side holding its own copy of the
  // image, so a rotation drag piled up 57 of them in nine seconds. They got
  // slower as they went, from 0.3s to 4.2s, because they were all fighting over
  // the same GPU, and the memory went with them. That is the freeze.
  //
  // The same shape as the pipeline `applyAdjustments` already uses: one in
  // flight, and a request arriving while one is running replaces whatever was
  // waiting rather than joining a queue. Everything sent is a whole state rather
  // than a change, so dropping a superseded one loses nothing.
  const uncroppedInFlightRef = useRef(false);
  const pendingUncroppedRef = useRef<Adjustments | null>(null);

  const flushUncroppedPreview = useCallback(() => {
    if (uncroppedInFlightRef.current) return;
    const next = pendingUncroppedRef.current;
    if (!next) return;
    pendingUncroppedRef.current = null;
    uncroppedInFlightRef.current = true;
    invoke(Invokes.GenerateUncroppedPreview, { jsAdjustments: next })
      .catch((err) => console.error('Failed to generate uncropped preview:', err))
      .finally(() => {
        uncroppedInFlightRef.current = false;
        if (pendingUncroppedRef.current) {
          flushUncroppedPreview();
        }
      });
  }, []);

  const generateUncroppedPreview = useCallback(
    (currentAdjustments: Adjustments) => {
      if (!selectedImage?.isReady) return;
      pendingUncroppedRef.current = currentAdjustments;
      flushUncroppedPreview();
    },
    [selectedImage?.isReady, flushUncroppedPreview],
  );
  // ========== BLITZRAW END: one at a time, and the newest wins ==========

  const calculateTargetRes = useCallback(() => {
    const baseTargetRes = appSettings?.editorPreviewResolution || 1920;
    if (!(appSettings?.enableZoomHifi ?? true) || displaySize.width === 0) {
      return baseTargetRes;
    }

    const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
    const sharpnessFactor = 1.25;
    const zoomMultiplier = appSettings?.highResZoomMultiplier || 1.0;
    const effectiveDpr = appSettings?.useFullDpiRendering ? dpr : 1;

    let targetRes = Math.max(displaySize.width, displaySize.height) * effectiveDpr * sharpnessFactor * zoomMultiplier;
    targetRes = Math.max(targetRes, 512);

    if (originalSize && originalSize.width > 0 && originalSize.height > 0) {
      const origMax = Math.max(originalSize.width, originalSize.height);
      targetRes = Math.min(targetRes, origMax);
      if (targetRes >= origMax * 0.8) {
        targetRes = origMax;
      }
    }

    if (originalSize && targetRes !== Math.max(originalSize.width, originalSize.height)) {
      targetRes = Math.ceil(targetRes / 256) * 256;
    }

    return Math.round(targetRes);
  }, [
    appSettings?.enableZoomHifi,
    appSettings?.editorPreviewResolution,
    appSettings?.highResZoomMultiplier,
    appSettings?.useFullDpiRendering,
    displaySize.width,
    displaySize.height,
    originalSize,
  ]);

  const requestHiFiZoom = useMemo(
    () =>
      debounce((currentAdjustments: Adjustments, targetRes: number) => {
        if (targetRes > currentResRef.current) {
          currentResRef.current = targetRes;
          applyAdjustments(currentAdjustments, false, targetRes);
        }
      }, 50),
    [applyAdjustments, currentResRef],
  );

  const requestHiFiOriginalZoom = useMemo(
    () =>
      debounce(async (currentAdjustments: Adjustments, targetRes: number) => {
        if (targetRes > currentOriginalResRef.current) {
          try {
            const base64Data: string = await invoke('generate_original_transformed_preview', {
              jsAdjustments: currentAdjustments,
              targetResolution: targetRes,
            });
            currentOriginalResRef.current = targetRes;
            setEditor({ transformedOriginalUrl: base64Data });
          } catch (e) {
            console.error('Failed to generate hi-fi original preview:', e);
          }
        }
      }, 200),
    [setEditor],
  );

  // BLITZRAW: `isReady` says the front end has something to draw, which on a
  // photo opened from the grid is true a second or two before the backend has
  // decoded anything. Asking then is answered with "No original image loaded",
  // the uncropped preview never arrives, and the crop rectangle never appears.
  // Nothing retried, because the thing being waited on was a ref and a ref
  // cannot wake an effect. Now it is store state, so this runs again the moment
  // the decode lands.
  // BLITZRAW: what this photo was last drawn as, so a change it cannot show is
  // not drawn again. Keyed to the photo, or the first sight of the next one
  // would be compared against the last one's state.
  const uncroppedDrawnRef = useRef<{ path: string; adjustments: Adjustments } | null>(null);
  useEffect(() => {
    if (!(activeView === 'editor' && isCropShowing && selectedImage?.isReady && isBackendReady)) {
      return;
    }
    // Moving the crop rectangle is the commonest thing to do in this panel and
    // it changes nothing about the picture underneath it, which is drawn without
    // the crop. See utils/cropPreview.ts.
    const drawn =
      uncroppedDrawnRef.current?.path === selectedImage.path
        ? uncroppedDrawnRef.current.adjustments
        : null;
    if (!uncroppedPreviewNeedsRedraw(drawn, adjustments)) {
      return;
    }
    uncroppedDrawnRef.current = { path: selectedImage.path, adjustments };
    generateUncroppedPreview(adjustments);
  }, [activeView, adjustments, isCropShowing, selectedImage?.isReady, isBackendReady, generateUncroppedPreview]);

  useEffect(() => {
    if (activeView === 'editor' && selectedImage?.isReady && displaySize.width > 0 && !isSliderDragging) {
      let baseRes = calculateTargetRes();
      if (originalSize.width > 0 && originalSize.height > 0) {
        const maxRes = Math.max(originalSize.width, originalSize.height);
        if (baseRes > maxRes) baseRes = maxRes;
      }
      const finalRes = Math.round(baseRes);

      if (finalRes > currentResRef.current) {
        requestHiFiZoom(adjustments, finalRes);
      }
    }
    return () => {
      requestHiFiZoom.cancel();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    activeView,
    displaySize.width,
    displaySize.height,
    calculateTargetRes,
    selectedImage?.isReady,
    isSliderDragging,
    requestHiFiZoom,
    originalSize,
  ]);

  useEffect(() => {
    if (!selectedImage?.isReady) return;

    if (dragIdleTimer.current) clearTimeout(dragIdleTimer.current);

    const targetRes = calculateTargetRes();
    const renderAdjustments = previewOverride ?? adjustments;

    if (activeView !== 'editor') {
      if (isSliderDragging) return;
    }

    if (isSliderDragging) {
      if (appSettings?.enableLivePreviews !== false) {
        applyAdjustments(renderAdjustments, true, targetRes);
      }
    } else {
      dragIdleTimer.current = setTimeout(() => {
        currentResRef.current = targetRes;

        applyAdjustments(renderAdjustments, false, targetRes);

        // A LUT being hovered is a preview of somebody else's look, not this
        // photo's state, so it is not saved. It still has to be reasoned about
        // below, because an adjustment changed during a hover is real.
        const previewing = !!previewOverride;

        // ============ BLITZRAW: name the one thing the user just did ============
        // The same name goes to the photo in the editor and to every photo the
        // change is sent to, so all of them can later be found again as one
        // action. Worked out here because this is the one place that sees both.
        //
        // Nothing is named on the first sight of a photo: with no earlier state
        // of its own to compare against, every value would read as a change
        // somebody had just made. See utils/actionId.ts.
        const seenBefore =
          lastSeenAdjustmentsRef.current?.path === selectedImage.path
            ? lastSeenAdjustmentsRef.current.adjustments
            : null;
        lastSeenAdjustmentsRef.current = { path: selectedImage.path, adjustments };
        const moved = seenBefore ? changedKeys(seenBefore, adjustments) : [];
        // A write that moved nothing belongs to no action.
        //
        // This effect re-runs on a dozen things that are not edits: the scopes
        // changing what they ask for, the view changing, a panel opening. Each
        // of those saves the photo again. `actionForChange` answers with the
        // action still open when nothing has moved, which is the right answer
        // for the question it was asked and the wrong name to put on this write:
        // the save would be filed under an action it was never part of.
        //
        // That is what blocked an undo. A photo opened after a nudge across a
        // selection had its next idle save stamped with the nudge's own name, so
        // the nudge appeared to have moved sixty adjustments on that one photo,
        // and stepping it back refused because the photo no longer looked like
        // what the nudge had left.
        //
        // And a state that arrived from an undo, a redo or a click in the
        // history list is not an edit either. It is the editor being told where
        // the photo already is, and the photo was written by whoever moved it.
        // Recording it would put a step of its own into the list of what I did,
        // and recording anything throws away everything waiting to be redone,
        // which is why Ctrl+Y did nothing after an undo. The same phantom is
        // why an undo sometimes needed pressing twice: the first press took
        // back the phantom rather than the edit.
        const cameFromHistory =
          useEditorStore.getState().historyMoveAt !== lastHistoryMoveRef.current;
        const actionId = moved.length > 0 && !cameFromHistory ? actionForChange(moved) : null;
        // ========== BLITZRAW END: name the one thing the user just did ==========

        if (!previewing) {
          // BLITZRAW: the save reports which numbers it moved this photo
          // between, and those two numbers are the whole of what the
          // application's list holds. The fan-out below reports the same for
          // the rest of the selection; one action name makes them one entry.
          // See utils/appActions.ts.
          const openPath = selectedImage.path;
          const inEditor = activeView === 'editor';
          const forSelection = multiSelectedPaths;
          const named = nameForKeys(moved);
          debouncedSave(openPath, adjustments, actionId, (photo) => {
            // BLITZRAW: the number this write was given, so the History panel
            // knows which step each of its rows really is.
            useEditorStore.getState().numberCurrentStep(photo.path, photo.to);
            if (cameFromHistory) {
              return;
            }
            recordAppAction({
              id: actionId,
              kind: 'adjustments',
              label: named,
              photos: [photo],
              selection: forSelection,
              openPath,
              inEditor,
            });
          });
        }

        // ============ BLITZRAW: one rule for the rest of the selection ============
        // This used to be five guards on four branches, each added after
        // something went out to photos it should not have. The whole rule is in
        // `autoSyncPlan` now, and it answers one of four ways. See
        // utils/autoSync.ts for what each means and why.
        //
        // The part that matters most: a decision NOT to send still moves the
        // reference forward. A reference that only moved on the way out is what
        // let a crop sit unrecorded and then ride along with a later, unrelated
        // change to two hundred photos.
        const resolvedTargets = selectionFor(EDIT_RULE, multiSelectedPaths);
        // Worked out above, before anything was saved or recorded, and only
        // spent here.
        lastHistoryMoveRef.current = useEditorStore.getState().historyMoveAt;

        const plan = autoSyncPlan({
          autoSyncOn: !!appSettings?.copyPasteSettings?.autoSync,
          inEditor: activeView === 'editor',
          openPath: selectedImage.path,
          resolvedTargets,
          fromHistoryMove: cameFromHistory,
          typing: isSliderTyping,
          previewing,
        });

        if (plan.kind === 'none') {
          return;
        }
        if (plan.kind === 'advance') {
          prevAdjustmentsRef.current = {
            path: selectedImage.path,
            adjustments,
            setBy: 'an edit that was not sent',
            setAt: Date.now(),
          };
          return;
        }

        // A change belongs to the selection it was made for. If the selection
        // has moved on since something was recorded, that something goes to the
        // photos it was aimed at before anything is recorded against the new
        // ones. It is never re-aimed.
        const waiting = pendingSyncRef.current;
        if (waiting && !sameTargets(waiting.paths, plan.paths)) {
          flushAutoSync.flush();
        }
        pendingSyncRef.current = {
          path: selectedImage.path,
          paths: plan.paths,
          adjustments,
          actionId,
        };

        // `hold` records but does not send. A number typed into a field arrives
        // one character at a time, so 4800 comes through as 4, then 48, then
        // 480, and writing every other selected photo for each of those left a
        // set of them sitting at 2000K. Recording it is what is new: the hold
        // used to keep nothing at all, so when the field closed the value was
        // aimed at whatever happened to be selected by then, and a field can
        // stay open for minutes.
        if (plan.kind === 'send') {
          flushAutoSync();
        }
        // ========== BLITZRAW END: one rule for the rest of the selection ==========
      }, 50);
    }

    return () => {
      if (dragIdleTimer.current) clearTimeout(dragIdleTimer.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    activeView,
    adjustments,
    previewOverride,
    selectedImage?.path,
    selectedImage?.isReady,
    isSliderDragging,
    isSliderTyping,
    multiSelectedPaths,
    appSettings?.enableLivePreviews,
    appSettings?.copyPasteSettings?.includedAdjustments,
    appSettings?.copyPasteSettings?.autoSync,
    isWaveformVisible,
    // What the scopes are asking for, not just whether any are showing.
    //
    // The request is read from a ref at the moment a render is asked for, so a
    // change to it with nothing else changing was never sent: adding a scope or
    // turning the vectorscope gain up altered the string and then waited for a
    // slider to be touched. The gain button appeared to do nothing at all.
    activeWaveformChannel,
    flushAutoSync,
  ]);

  useEffect(() => {
    setEditor({ transformedOriginalUrl: null });
    currentOriginalResRef.current = 0;
  }, [geometricAdjustmentsKey, selectedImage?.path, setEditor]);

  useEffect(() => {
    if (
      activeView === 'editor' &&
      showOriginal &&
      selectedImage?.isReady &&
      displaySize.width > 0 &&
      !isSliderDragging
    ) {
      let targetRes = calculateTargetRes();
      if (targetRes > currentOriginalResRef.current) {
        requestHiFiOriginalZoom(adjustments, targetRes);
      }
    }
    return () => {
      requestHiFiOriginalZoom.cancel();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    activeView,
    showOriginal,
    displaySize.width,
    displaySize.height,
    calculateTargetRes,
    selectedImage?.isReady,
    isSliderDragging,
    requestHiFiOriginalZoom,
    originalSize,
  ]);

  useEffect(() => {
    let isEffectActive = true;
    const generate = async () => {
      if (activeView === 'editor' && showOriginal && selectedImage?.path && !transformedOriginalUrl) {
        try {
          const targetRes = calculateTargetRes();
          const base64Data: string = await invoke('generate_original_transformed_preview', {
            jsAdjustments: adjustments,
            targetResolution: targetRes,
          });
          if (isEffectActive) {
            currentOriginalResRef.current = targetRes;
            setEditor({ transformedOriginalUrl: base64Data });
          }
        } catch (e) {
          if (isEffectActive) {
            console.error('Failed to generate original preview:', e);
            setEditor({ showOriginal: false });
          }
        }
      }
    };
    generate();
    return () => {
      isEffectActive = false;
    };
  }, [
    activeView,
    showOriginal,
    selectedImage?.path,
    adjustments,
    transformedOriginalUrl,
    calculateTargetRes,
    setEditor,
  ]);

  return {
    applyAdjustments,
    executeApplyAdjustments,
  };
}
