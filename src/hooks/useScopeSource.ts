import { useEffect, useMemo, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { Invokes } from '../components/ui/AppProperties';

/**
 * Keeps the scopes showing whatever picture is being looked at.
 *
 * # What was wrong with waiting
 *
 * Scopes arrived only once a raw had been decoded and rendered, which is about
 * a second and a half for a Z9 frame. So they were blank while a photo opened,
 * blank in the grid where nothing is decoded at all, and useless for the thing
 * they are best at: flicking along a strip to see which frames are a third of a
 * stop apart.
 *
 * Something of every photo is on disk long before that, though. A rendered
 * preview if one has been built, and a thumbnail for anything ever looked at,
 * both carrying the photo's own adjustments. Their scopes are the photo's
 * scopes to within the resampling, which is exact enough to compare frames and
 * was never going to be exact enough to grade from. The real ones replace them
 * the moment the decode finishes.
 *
 * # Which photo
 *
 * The same one the navigator draws: whatever the pointer is over, falling back
 * to what is being edited and then to what the library has selected. Hovering a
 * frame in the strip now moves the scopes with it, which is the whole reason
 * the navigator exists and the scopes were the missing half of it.
 *
 * # Which scopes win
 *
 * Three states, and the order matters:
 *
 * - Already showing the wanted photo: nothing to do.
 * - The editor's own scopes are for the wanted photo: use those, they are the
 *   accurate ones. This is what unhovering restores, so coming back from a
 *   hover does not leave preview-quality scopes on the photo being worked on.
 * - Otherwise: ask for the small picture's. A hundred and twenty milliseconds
 *   of settling first, so running the pointer along a strip of forty frames
 *   asks for one of them rather than forty.
 */
/**
 * `anyScopeShowing` says a Scopes panel is on screen somewhere: this window's
 * sidebar, or the floating window. Passed in rather than worked out here,
 * because the caller has already worked it out to decide what to ask the
 * backend for.
 */
export function useScopeSource(scopeRequest: string, anyScopeShowing: boolean) {
  const hoveredPath = useLibraryStore((state) => state.hoveredPath);
  const libraryActivePath = useLibraryStore((state) => state.libraryActivePath);
  const selectedPath = useEditorStore((state) => state.selectedImage?.path ?? null);
  const setEditor = useEditorStore((state) => state.setEditor);

  const wanted = hoveredPath ?? selectedPath ?? libraryActivePath ?? null;

  // The request only matters for the waveform, and a change of vectorscope gain
  // is not a reason to go and fetch a hovered photo again.
  const request = useMemo(() => scopeRequest || '', [scopeRequest]);
  const asked = useRef<string | null>(null);

  useEffect(() => {
    if (!wanted) return;
    // BLITZRAW: nothing to fill if nobody is looking at a scope.
    //
    // The histogram is shared with the curve editor, which draws it behind the
    // curve as the background of the photo being worked on. Fetching a hovered
    // photo's histogram into that field with no scope on screen anywhere
    // repainted that background with a different photo's picture, for no
    // benefit at all. An empty scope request still returns a histogram, so
    // asking was never free.
    if (!anyScopeShowing) return;

    const state = useEditorStore.getState();
    // BLITZRAW: and there has to actually be something there.
    //
    // Opening a photo clears the histogram and the waveform but leaves
    // `scopesPath` naming that photo, so this returned early on a pair that had
    // just been emptied and the panel stayed blank until the decode landed,
    // which is the wait this hook exists to remove.
    if (
      state.scopesPath === wanted &&
      state.histogram !== null &&
      asked.current === `${wanted}|${request}`
    ) {
      return;
    }

    const held = state.editorScopes;
    if (held?.path === wanted) {
      asked.current = `${wanted}|${request}`;
      setEditor({ histogram: held.histogram, waveform: held.waveform, scopesPath: wanted });
      return;
    }

    let alive = true;
    const timer = setTimeout(() => {
      asked.current = `${wanted}|${request}`;
      invoke(Invokes.ScopesFromSmallPicture, { path: wanted, scopes: request })
        .then((payload: any) => {
          if (!alive || !payload) return;
          // The pointer may have moved on while this was being read.
          const now = useLibraryStore.getState();
          const still =
            now.hoveredPath ??
            useEditorStore.getState().selectedImage?.path ??
            now.libraryActivePath ??
            null;
          if (still !== payload.path) return;
          setEditor({
            histogram: payload.histogram ?? null,
            waveform: payload.waveform ?? null,
            scopesPath: payload.path,
          });
        })
        .catch(() => {
          // A photo with neither a preview nor a thumbnail has no scopes to
          // show, which is not worth saying anything about.
        });
    }, 120);

    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [wanted, request, anyScopeShowing, setEditor]);
}
