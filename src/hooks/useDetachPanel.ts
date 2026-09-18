import { useCallback, useEffect, useMemo, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, emit } from '@tauri-apps/api/event';
import { useShallow } from 'zustand/react/shallow';
import { toast } from 'react-toastify';
import { Panel, PanelRegion } from '../components/ui/AppProperties';
import { useUIStore } from '../store/useUIStore';
import { detachPanel, reattachPanel } from '../utils/panelLayout';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useProcessStore } from '../store/useProcessStore';
import { FLOATING_NAVIGATOR_EVENT } from '../utils/floatingNavigator';
import { FLOATING_SCOPES_EVENT } from '../utils/floatingScopes';
import { FLOATING_METADATA_EVENT } from '../utils/floatingMetadata';
import { FLOATING_REGIONS } from '../utils/floatingLayout';
import {
  anythingFloating,
  floatingArrangementOf,
  floatingPanels,
  sameArrangement,
  type FloatingArrangement,
} from '../utils/floatingLayout';

/**
 * Panels that can be sent to the floating window.
 *
 * Only ones that display. Anything that edits would have to send its changes
 * back across the gap in order, with a real risk of a slider fighting itself,
 * which is a different piece of work.
 */
export const DETACHABLE: Array<Panel> = [Panel.Scopes, Panel.Navigator, Panel.Metadata];

export function canDetach(panel: Panel | null | undefined): boolean {
  return !!panel && DETACHABLE.includes(panel);
}

/** Where a panel goes when it is dragged out and there is nothing floating yet. */
const FIRST_FLOATING_REGION: PanelRegion = 'floatTop';

/**
 * The main window's half of the floating window.
 *
 * One window holds all the floating panels, arranged in two rows with tab
 * strips, the same way a sidebar is. `floatTop` and `floatBottom` are ordinary
 * regions in the ordinary layout, so they are saved with the workspace and come
 * back on the next start; this hook is what opens a window to show them and
 * keeps the two copies of those two regions in step.
 *
 * Detaching is a drop rather than a button. Dragging a tab onto a region moves
 * it there already, so dragging it anywhere else meaning "out of the window
 * entirely" is the same gesture carried one step further, and there is nothing
 * extra to find. The floating window reads the same gesture the other way: a
 * tab dropped clear of both rows goes home.
 *
 * A detached panel leaves the sidebar. Living in two places at once was the
 * first thing that felt wrong: two copies of one scope, and no way to tell
 * which one the layout meant.
 */
export function useDetachPanel() {
  const setUI = useUIStore((state) => state.setUI);

  const arrangementState = useUIStore(
    useShallow((state) => ({
      panelLayout: state.panelLayout,
      activePanels: state.activePanels,
      panelSwitcherPlacement: state.panelSwitcherPlacement,
      floatTopHeight: state.floatTopHeight,
    })),
  );

  // Rebuilt only when the floating half of the layout actually moves, so the
  // effect below is a comparison and not a message on every render.
  const arrangement = useMemo(() => floatingArrangementOf(arrangementState as any), [arrangementState]);
  // The last arrangement that came from the floating window. Sending it back
  // would be telling that window what it has just told us, and the two would
  // correct each other for as long as the application ran.
  const fromFloatingRef = useRef<FloatingArrangement | null>(null);
  const windowIsOpenRef = useRef(false);

  /** Opens the window if it is not already open. Answers whether it is now. */
  const ensureWindow = useCallback(async () => {
    try {
      await invoke('open_floating_window');
      windowIsOpenRef.current = true;
      return true;
    } catch (err) {
      toast.error(`${err}`);
      return false;
    }
  }, []);

  const detach = useCallback(
    async (panel: Panel) => {
      if (!canDetach(panel)) return false;

      // The window first. If it cannot be opened the layout is untouched,
      // rather than losing a panel to a window that never appeared.
      if (!(await ensureWindow())) return false;

      setUI((state: any) => {
        const { layout, from } = detachPanel(state.panelLayout, panel);
        if (!from) return {};

        // Into whichever floating row is already in use, so a second panel
        // joins the first as a tab rather than starting a row of its own.
        const target: PanelRegion = layout.floatBottom.length > 0 ? 'floatBottom' : FIRST_FLOATING_REGION;
        const floated = { ...layout, [target]: [...layout[target], panel] };

        const activePanels = { ...state.activePanels };
        if (activePanels[from.region] === panel) {
          activePanels[from.region] = floated[from.region][0] ?? null;
        }
        // A panel arriving in an empty row has to be the one showing, or the
        // row draws a tab strip with nothing under it.
        activePanels[target] = panel;

        return {
          panelLayout: floated,
          activePanels,
          detachedPanels: [...state.detachedPanels.filter((d: any) => d.panel !== panel), from],
        };
      });

      return true;
    },
    [ensureWindow, setUI],
  );

  /** Puts one panel back in the sidebar it came from. */
  const reattach = useCallback(
    (panel: Panel) =>
      setUI((state: any) => {
        const { layout, from: floatingFrom } = detachPanel(state.panelLayout, panel);
        if (!floatingFrom) return {};

        const remembered = state.detachedPanels.find((d: any) => d.panel === panel);
        // A panel that has been floating since before a restart has no
        // remembered sidebar position, so it goes back where the defaults put
        // it. A stale guess is no better than the shipped one.
        const home = remembered ?? { panel, region: 'rightTop' as PanelRegion, index: Number.MAX_SAFE_INTEGER };
        const back = reattachPanel(layout, home);

        const activePanels = { ...state.activePanels };
        if (activePanels[floatingFrom.region] === panel) {
          activePanels[floatingFrom.region] = back[floatingFrom.region][0] ?? null;
        }
        if (activePanels[home.region] == null) {
          activePanels[home.region] = panel;
        }

        return {
          panelLayout: back,
          activePanels,
          detachedPanels: state.detachedPanels.filter((d: any) => d.panel !== panel),
          ...(panel === Panel.Scopes ? { detachedScopeChannels: null } : {}),
        };
      }),
    [setUI],
  );

  // Everything comes home when the window closes, whether that was its close
  // button or the OS. Not when the application is closing: the backend does not
  // send this then, or an arrangement would be dismantled and saved on the way
  // out and the next start would have forgotten it.
  useEffect(() => {
    let active = true;
    const listeners = [
      listen<any>('panel-window-closed', () => {
        if (!active) return;
        windowIsOpenRef.current = false;
        for (const panel of floatingPanels(useUIStore.getState().panelLayout)) {
          reattach(panel);
        }
      }),
      listen<any>('floating-panel-returned', (event) => {
        if (!active) return;
        const panel = event.payload?.panel as Panel | undefined;
        if (panel) reattach(panel);
      }),
      listen<any>('floating-layout-changed', (event) => {
        if (!active) return;
        const incoming = event.payload as FloatingArrangement | undefined;
        if (!incoming?.panelLayout) return;
        fromFloatingRef.current = incoming;
        useUIStore.getState().setUI((state: any) => ({
          panelLayout: { ...state.panelLayout, ...incoming.panelLayout },
          activePanels: { ...state.activePanels, ...incoming.activePanels },
          panelSwitcherPlacement: { ...state.panelSwitcherPlacement, ...incoming.panelSwitcherPlacement },
          floatTopHeight: incoming.floatTopHeight ?? state.floatTopHeight,
        }));
      }),
      // The floating window says when it has rendered, and is answered with
      // the arrangement it should be showing. That is what makes a window
      // opened on startup, or reopened after a crash, show the right thing
      // without either side having to guess.
      listen<any>('panel-window-rendered', () => {
        if (!active) return;
        windowIsOpenRef.current = true;
        const state = useUIStore.getState();
        emit('floating-layout', floatingArrangementOf(state as any)).catch(() => {});
      }),
    ];

    return () => {
      active = false;
      for (const listener of listeners) {
        listener.then((unlisten) => unlisten()).catch(() => {});
      }
    };
  }, [reattach]);

  // Anything rearranged in the sidebar that touches a floating region goes out
  // to the window showing it.
  useEffect(() => {
    // Not gated on the window being open. An event with nobody listening costs
    // nothing, and believing the window is shut when it is not would stop the
    // two sides talking with no sign of why. The flag decides whether to open
    // or close a window, which is a decision worth being careful about; this is
    // not.
    if (sameArrangement(arrangement, fromFloatingRef.current)) return;
    fromFloatingRef.current = arrangement;
    emit('floating-layout', arrangement).catch(() => {});
  }, [arrangement]);

  // The front end can be reloaded without the window being closed, which
  // happens constantly under `npm run tauri dev`. Ask rather than assume, or
  // the first rearrangement after a reload is sent to a window this side
  // believes is not there.
  useEffect(() => {
    invoke<boolean>('floating_window_is_open')
      .then((open) => {
        // Only ever upgrades the answer. This resolves after a round trip, and
        // by then the layout may already have asked for a window and got one;
        // writing `false` over that would leave this side believing there is no
        // window while one is on screen.
        windowIsOpenRef.current = windowIsOpenRef.current || open;
      })
      .catch(() => {});
  }, []);

  // The window exists exactly when something is floating, and that is the only
  // rule. It covers three cases that would otherwise each need their own:
  // a saved workspace that had panels floating when the application last
  // closed, a named layout loaded from Settings that has them, and the last
  // panel being dragged home, which should leave no empty window behind.
  const floating = anythingFloating(arrangement.panelLayout);
  useEffect(() => {
    if (floating && !windowIsOpenRef.current) {
      void ensureWindow();
      return;
    }
    if (!floating && windowIsOpenRef.current) {
      windowIsOpenRef.current = false;
      invoke('close_floating_window').catch(() => {});
    }
  }, [floating, ensureWindow]);

  useFloatingNavigatorFeed();
  useFloatingScopesFeed();
  useFloatingMetadataFeed();

  return { detach, reattach };
}

/**
 * Sends the Navigator the one frame it is pointing at, while it is floating.
 *
 * The panel reads `hoveredPath` and the thumbnails, and both of those are
 * filled by the grid and the filmstrip, neither of which exists in the floating
 * window. So it came up empty there. The alternative to sending this was giving
 * the second window a library, which is a great deal more than the panel needs:
 * it wants one path and one picture, and the picture is usually a short
 * `asset://` URL to a file the backend has already written.
 *
 * Nothing is sent unless the Navigator is actually showing over there, or every
 * frame the pointer crossed would be a message with nobody to read it.
 */
/**
 * Sends the Scopes panel the pair the main window has decided on, while it is
 * floating.
 *
 * The whole point of `useScopeSource` is that the scopes follow the pointer and
 * come from a preview rather than from a decode. It runs in the main window,
 * because that is where the pointer is, and it writes into the main window's
 * store. With the panel floating, the answer was computed and then discarded,
 * and the panel over there sat on a direct decode listener instead. See
 * utils/floatingScopes.ts.
 *
 * Sent only while the panel is actually over there, or every frame the pointer
 * crossed would be a message with nobody to read it.
 */
function useFloatingScopesFeed() {
  const showing = useUIStore((state) =>
    FLOATING_REGIONS.some((region) => state.activePanels[region] === Panel.Scopes),
  );
  const histogram = useEditorStore((state) => state.histogram);
  const waveform = useEditorStore((state) => state.waveform);
  const scopesPath = useEditorStore((state) => state.scopesPath);

  useEffect(() => {
    if (!showing) return;
    emit(FLOATING_SCOPES_EVENT, { path: scopesPath, histogram, waveform }).catch(() => {});
  }, [showing, scopesPath, histogram, waveform]);
}

/**
 * Sends the Metadata panel the one photo's worth of facts it reads, while it is
 * floating.
 *
 * More than the Navigator needs, because the panel draws EXIF, stars, tags and
 * a thumbnail, and all four live in stores the floating window does not fill.
 * Still one photo's worth: what crosses is a map of short strings.
 *
 * `paths` is the main window's selection, so a tag added over there is applied
 * to the photos actually selected here. See utils/floatingMetadata.ts.
 */
function useFloatingMetadataFeed() {
  const showing = useUIStore((state) =>
    FLOATING_REGIONS.some((region) => state.activePanels[region] === Panel.Metadata),
  );
  const selectedImage = useEditorStore((state) => state.selectedImage);
  const multiSelectedPaths = useLibraryStore((state) => state.multiSelectedPaths);
  const imageRatings = useLibraryStore((state) => state.imageRatings);
  const imageList = useLibraryStore((state) => state.imageList);

  const path = selectedImage?.path ?? null;
  const thumbnail = useProcessStore((state) => (path ? state.thumbnails[path] : undefined));
  const rating = path ? (imageRatings?.[path] ?? 0) : 0;
  const tags = useMemo(
    () => (path ? (imageList.find((image: any) => image.path === path)?.tags ?? []) : []),
    [imageList, path],
  );

  // The selection as one string, so an array rebuilt by an unrelated library
  // write does not send the same facts again.
  const pathsKey = useMemo(() => multiSelectedPaths.join('\n'), [multiSelectedPaths]);

  useEffect(() => {
    if (!showing) return;
    emit(FLOATING_METADATA_EVENT, {
      selectedImage: selectedImage ?? null,
      paths: multiSelectedPaths,
      rating,
      tags,
      thumbnail: thumbnail ?? null,
    }).catch(() => {});
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [showing, selectedImage, pathsKey, rating, tags, thumbnail]);
}

function useFloatingNavigatorFeed() {
  const showing = useUIStore((state) =>
    FLOATING_REGIONS.some((region) => state.activePanels[region] === Panel.Navigator),
  );
  const hoveredPath = useLibraryStore((state) => state.hoveredPath);
  const libraryActivePath = useLibraryStore((state) => state.libraryActivePath);
  const selectedPath = useEditorStore((state) => state.selectedImage?.path);

  // The same fallback the panel itself uses, so the two never disagree about
  // which frame is meant: the pointer, then the open photo, then the selection.
  const path = hoveredPath ?? selectedPath ?? libraryActivePath ?? null;
  const thumbnail = useProcessStore((state) => (path ? state.thumbnails[path] : undefined));

  useEffect(() => {
    if (!showing) return;
    emit(FLOATING_NAVIGATOR_EVENT, {
      path,
      thumbnail: thumbnail ?? null,
      hovering: !!hoveredPath,
    }).catch(() => {});
  }, [showing, path, thumbnail, hoveredPath]);
}
