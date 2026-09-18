import { useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { Invokes } from '../components/ui/AppProperties';
import { INITIAL_ADJUSTMENTS, normalizeLoadedAdjustments } from '../utils/adjustments';
import { settlePendingNudges } from '../utils/pendingNudges';

export function useImageLoader(cachedEditStateRef: React.RefObject<any>, prevAdjustmentsRef: React.RefObject<any>) {
  const selectedImage = useEditorStore((s) => s.selectedImage);
  const adjustments = useEditorStore((s) => s.adjustments);
  const histogram = useEditorStore((s) => s.histogram);
  const waveform = useEditorStore((s) => s.waveform);
  const finalPreviewUrl = useEditorStore((s) => s.finalPreviewUrl);
  const uncroppedAdjustedPreviewUrl = useEditorStore((s) => s.uncroppedAdjustedPreviewUrl);
  const originalSize = useEditorStore((s) => s.originalSize);
  const previewSize = useEditorStore((s) => s.previewSize);
  const hasRenderedFirstFrame = useEditorStore((s) => s.hasRenderedFirstFrame);

  const setEditor = useEditorStore((s) => s.setEditor);
  const resetHistory = useEditorStore((s) => s.resetHistory);
  const setLibrary = useLibraryStore((s) => s.setLibrary);
  const appSettings = useSettingsStore((s) => s.appSettings);

  const isWgpuActive = appSettings?.useWgpuRenderer !== false && selectedImage?.isReady && hasRenderedFirstFrame;

  // BLITZRAW: the preview this photo has on disk, if any. Asked for on every
  // change of photo and cleared first, so a preview is never left showing
  // against the wrong file, and dropped silently when the answer arrives after
  // the user has moved on.
  useEffect(() => {
    const path = selectedImage?.path;
    setEditor({ cachedPreviewUrl: null });
    if (!path) {
      return;
    }
    let isEffectActive = true;
    invoke<string | null>(Invokes.CachedPreviewForPath, { path })
      .then((url) => {
        if (!isEffectActive || !url) return;
        if (useEditorStore.getState().selectedImage?.path !== path) return;
        // BLITZRAW: something better may already be showing. This is cleared to
        // null on every change of photo, so anything in it now was put there by
        // the proxy render, which is this same preview with the nudge already
        // on it. Replacing it would undo the nudge on screen and then let the
        // real render put it back, which reads as a flicker.
        if (useEditorStore.getState().cachedPreviewUrl !== null) return;
        setEditor({ cachedPreviewUrl: url });
      })
      .catch((err) => console.error('Failed to read the cached preview:', err));
    return () => {
      isEffectActive = false;
    };
  }, [selectedImage?.path, setEditor]);

  useEffect(() => {
    if (selectedImage && !selectedImage.isReady && selectedImage.path) {
      let isEffectActive = true;

      const loadMetadataEarly = async () => {
        try {
          useEditorStore.getState().patchesSentToBackend.clear();
          await invoke('clear_session_caches').catch((e) => console.warn('Cache clear failed:', e));

          const metadata: any = await invoke(Invokes.LoadMetadata, { path: selectedImage.path });
          if (!isEffectActive) return;

          let initialAdjusts;
          if (metadata.adjustments && !metadata.adjustments.is_null) {
            initialAdjusts = normalizeLoadedAdjustments(metadata.adjustments);
          } else {
            initialAdjusts = { ...INITIAL_ADJUSTMENTS };
          }

          setEditor({ adjustments: initialAdjusts });
          // BLITZRAW: the sidecar's own log, which outlives the session and
          // holds what happened to this photo while it was closed.
          resetHistory(initialAdjusts, metadata.history ?? null);
          // BLITZRAW: arrow to the next photo and nudge exposure straight away
          // and the key goes down before this read comes back. Those presses
          // are spent here, on the values that just landed, rather than on the
          // previous photo's. See pendingNudges.ts.
          const spent = settlePendingNudges(selectedImage.path);
          if (spent) {
            // BLITZRAW: what auto-sync measures the next change against, moved
            // past the presses that were just spent. Without this, a photo
            // opened twice in one session can leave that reference holding its
            // own older values, and auto-sync then reads the nudge as a change
            // to fan out across the rest of the selection. Every other file in
            // it has already been nudged from its own value, file by file, so
            // that would replace each of them with this one's number.
            //
            // Only when something was actually spent. Setting it on every open
            // would change when auto-sync fans out at all, which is not what
            // this is for.
            prevAdjustmentsRef.current = {
              path: selectedImage.path,
              adjustments: useEditorStore.getState().adjustments,
              setBy: 'presses spent when a photo finished opening',
              setAt: Date.now(),
            };
          }
        } catch (err) {
          console.error('Failed to load metadata early:', err);
        }
      };

      const loadFullImageData = async () => {
        try {
          const loadImageResult: any = await invoke(Invokes.LoadImage, { path: selectedImage.path });
          if (!isEffectActive) return;

          const { width, height } = loadImageResult;
          setEditor({ originalSize: { width, height } });

          if (appSettings?.editorPreviewResolution) {
            const maxSize = appSettings.editorPreviewResolution;
            const aspectRatio = width / height;

            if (width > height) {
              const pWidth = Math.min(width, maxSize);
              const pHeight = Math.round(pWidth / aspectRatio);
              setEditor({ previewSize: { width: pWidth, height: pHeight } });
            } else {
              const pHeight = Math.min(height, maxSize);
              const pWidth = Math.round(pHeight * aspectRatio);
              setEditor({ previewSize: { width: pWidth, height: pHeight } });
            }
          } else {
            setEditor({ previewSize: { width: 0, height: 0 } });
          }

          setEditor((state) => {
            if (state.selectedImage && state.selectedImage.path === selectedImage.path) {
              return {
                selectedImage: {
                  ...state.selectedImage,
                  exif: loadImageResult.exif,
                  height: loadImageResult.height,
                  isRaw: loadImageResult.is_raw,
                  isReady: true,
                  metadata: loadImageResult.metadata,
                  originalUrl: null,
                  width: loadImageResult.width,
                },
              };
            }
            return state;
          });

          setEditor((state) => {
            if (!state.adjustments.aspectRatio && !state.adjustments.crop) {
              return {
                adjustments: { ...state.adjustments, aspectRatio: loadImageResult.width / loadImageResult.height },
              };
            }
            return state;
          });
        } catch (err) {
          if (isEffectActive) {
            console.error('Failed to load image:', err);
            toast.error(`Failed to load image: ${err}`);
            setEditor({ selectedImage: null });
          }
        } finally {
          if (isEffectActive) {
            setLibrary({ isViewLoading: false });
          }
        }
      };

      const loadAll = async () => {
        await loadMetadataEarly();
        if (isEffectActive) {
          await loadFullImageData();
        }
      };

      loadAll();

      return () => {
        isEffectActive = false;
      };
    }
  }, [
    selectedImage?.path,
    selectedImage?.isReady,
    appSettings?.editorPreviewResolution,
    resetHistory,
    setEditor,
    setLibrary,
    prevAdjustmentsRef,
  ]);

  useEffect(() => {
    if (selectedImage?.path && selectedImage.isReady && (finalPreviewUrl || isWgpuActive)) {
      cachedEditStateRef.current = {
        adjustments,
        histogram,
        waveform,
        finalPreviewUrl,
        uncroppedPreviewUrl: uncroppedAdjustedPreviewUrl,
        selectedImage,
        originalSize,
        previewSize,
      };
    } else {
      cachedEditStateRef.current = null;
    }
  }, [
    selectedImage,
    adjustments,
    histogram,
    waveform,
    finalPreviewUrl,
    uncroppedAdjustedPreviewUrl,
    originalSize,
    previewSize,
    isWgpuActive,
    cachedEditStateRef,
  ]);
}
