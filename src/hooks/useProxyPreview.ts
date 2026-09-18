import { useCallback, useEffect, useMemo, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useEditorStore } from '../store/useEditorStore';
import { Invokes } from '../components/ui/AppProperties';
import { Nudge, nudgeBetween } from '../utils/proxyPreview';

/**
 * Moves the large picture while the raw is still decoding.
 *
 * # Why this is safe to get wrong
 *
 * The canvas draws `finalPreviewUrl || cachedPreviewUrl || thumbnailUrl`, and
 * this writes to the middle one. So the real render always wins the moment it
 * exists, and there is no ordering to get right, no job id to compare and no
 * race to lose: this cannot reach the field that beats it. The hand-over is the
 * fallback chain doing what it already did.
 *
 * # When it runs
 *
 * Only in the window where nothing renders at all today: the photo is open, its
 * own adjustments have landed from its sidecar, the raw has not finished
 * decoding, and something has moved since it opened.
 *
 * # Why it is not settled first
 *
 * It was, for eighty milliseconds, and that was wrong. A key held down repeats
 * faster than that, so every press pushed the settle out again and the picture
 * only moved once the key was released. The whole point is that it moves while
 * you are pressing.
 *
 * So: fire at once, and never more than one at a time. A press arriving while a
 * render is in the air replaces whatever was waiting rather than queueing
 * behind it, and goes as soon as the current one lands. That gives the fastest
 * first picture and then one render per render, at about seventy milliseconds
 * each, with the newest state always the one being drawn. Queueing them instead
 * would show every intermediate value late, which is the same fault in a
 * different shape.
 */
export function useProxyPreview() {
  const selectedImage = useEditorStore((state) => state.selectedImage);
  const adjustments = useEditorStore((state) => state.adjustments);
  const adjustmentsPath = useEditorStore((state) => state.adjustmentsPath);
  const adjustmentsAtOpen = useEditorStore((state) => state.adjustmentsAtOpen);
  const setEditor = useEditorStore((state) => state.setEditor);

  const path = selectedImage?.path ?? null;
  const isReady = selectedImage?.isReady ?? false;

  const nudge = useMemo(
    () => nudgeBetween(adjustmentsAtOpen, adjustments),
    [adjustmentsAtOpen, adjustments],
  );

  const inFlight = useRef(false);
  const waiting = useRef<{ path: string; nudge: Nudge } | null>(null);
  /** Whether a stand-in was ever rendered, so there is something to let go of. */
  const rendered = useRef(false);

  const drain = useCallback(() => {
    if (inFlight.current) {
      return;
    }
    const next = waiting.current;
    waiting.current = null;
    if (!next) {
      return;
    }

    // The photo may have been left, or its decode may have landed, since this
    // was put down.
    const before = useEditorStore.getState();
    if (before.selectedImage?.path !== next.path || before.selectedImage?.isReady) {
      return;
    }

    inFlight.current = true;
    rendered.current = true;
    invoke<string | null>(Invokes.RenderNudgedPreview, { path: next.path, nudge: next.nudge })
      .then((url) => {
        if (!url) {
          return;
        }
        const now = useEditorStore.getState();
        if (now.selectedImage?.path !== next.path || now.selectedImage?.isReady) {
          return;
        }
        setEditor({ cachedPreviewUrl: url });
      })
      .catch(() => {
        // A photo with no preview and no current thumbnail has no picture to
        // nudge. Showing the un-nudged one is the right answer and is already
        // what is on screen.
      })
      .finally(() => {
        inFlight.current = false;
        // A press that arrived while that was rendering.
        drain();
      });
  }, [setEditor]);

  useEffect(() => {
    // The real thing is here or on its way; nothing to stand in for.
    if (!path || isReady) {
      return;
    }
    // The store is still holding the previous photo's numbers, so a difference
    // measured against them would not be this photo's nudge. See
    // `adjustmentsPath`.
    if (adjustmentsPath !== path) {
      return;
    }
    if (!nudge) {
      return;
    }

    waiting.current = { path, nudge };
    drain();
  }, [path, isReady, adjustmentsPath, nudge, drain]);

  // Once the decode has landed, the stand-in it was nudging is megabytes held
  // against nothing.
  useEffect(() => {
    if (!isReady || !rendered.current) {
      return;
    }
    waiting.current = null;
    rendered.current = false;
    invoke(Invokes.ForgetNudgedSource).catch(() => {});
  }, [isReady, path]);
}
