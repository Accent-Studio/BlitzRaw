import React, { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { DndContext, DragOverlay, PointerSensor, useSensor, useSensors } from '@dnd-kit/core';
import { listen, emit } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import TitleBar from './TitleBar';
import SidePanelArea from '../components/panel/SidePanelArea';
import { PANEL_ICONS } from '../components/panel/PanelSwitcher';
import ScopesPanel from '../components/panel/right/ScopesPanel';
import NavigatorPanel from '../components/panel/right/NavigatorPanel';
import MetadataPanel from '../components/panel/right/MetadataPanel';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useProcessStore } from '../store/useProcessStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { useUIStore } from '../store/useUIStore';
import { FLOATING_NAVIGATOR_EVENT, type FloatingNavigatorFrame } from '../utils/floatingNavigator';
import { FLOATING_SCOPES_EVENT, type FloatingScopes } from '../utils/floatingScopes';
import { FLOATING_METADATA_EVENT, type FloatingMetadata } from '../utils/floatingMetadata';
import { applyTheme } from '../utils/themes';
import { scopeName, scopeToken } from '../utils/scopeRequest';
import {
  floatingArrangementOf,
  shouldReportArrangement,
  type FloatingArrangement,
} from '../utils/floatingLayout';
import { Invokes, Panel, type PanelRegion } from '../components/ui/AppProperties';
import { DisplayMode } from '../utils/adjustments';

/**
 * The floating window: a column of panels, arranged the way a sidebar is.
 *
 * # Why it is a sidebar and not a special case
 *
 * The first version put one panel in one window, and a second panel opened a
 * second window. What was wanted was a place to arrange several, in tabs and
 * rows, exactly as the sidebars do. So `floatTop` and `floatBottom` are regions
 * like any other and this renders `SidePanelArea` over them. The tab strips,
 * the drag and drop between the two rows, the switcher placement and the
 * vertical split are the same code running in a different window. There is one
 * layout manager in this application, not two.
 *
 * # The two windows have separate memories
 *
 * A window is its own webview with its own JavaScript heap, so this one cannot
 * see the main window's store; it has a store of its own with the same shape
 * and mostly empty. Only the floating regions are kept in step, and the main
 * window owns them: it is the one that saves the workspace, and one owner means
 * there is nothing to reconcile.
 *
 * So the traffic is two events carrying the same small object, the arrangement
 * of two regions:
 *
 * - `floating-layout` comes in, whenever the main window changes it or when
 *   this window says it has rendered.
 * - `floating-layout-changed` goes out, when a tab is dragged or switched here.
 *
 * Last message wins, which is safe because only one pair of hands is moving
 * tabs. `sameArrangement` stops the two bouncing off each other: an arrangement
 * that arrived from the other window is not sent back.
 *
 * # What else crosses
 *
 * The scopes. The backend only computes a scope somebody is looking at, and the
 * main window is the one that asks for the render, so it has to know what is
 * showing here. `detached-scopes-changed` carries the column, already written
 * the way the backend reads it, gain and all. Losing one costs a stale frame.
 *
 * # Why the window is dark rather than white
 *
 * Every colour resolves through an `--app-*` custom property and the stylesheet
 * gives none of them a value; they are set by an effect only the main window
 * runs. `applyTheme` is called in `main.tsx` before React starts, and again
 * here whenever the theme changes. See the note in `panel_window.rs` for how
 * long that took to find.
 */

/** Only panels that display. Anything that edits would have to send its changes back in order. */
export const FLOATABLE: Array<Panel> = [Panel.Scopes, Panel.Navigator, Panel.Metadata];

const FLOAT_REGIONS: Array<PanelRegion> = ['floatTop', 'floatBottom'];

function renderFloatingPanel(panel: Panel): React.ReactNode {
  switch (panel) {
    case Panel.Scopes:
      return <ScopesPanel />;
    case Panel.Navigator:
      return <NavigatorPanel />;
    case Panel.Metadata:
      return <MetadataPanel />;
    default:
      return (
        <div className="h-full w-full flex items-center justify-center text-text-secondary text-sm px-4 text-center">
          {panel} cannot be shown in a window of its own yet.
        </div>
      );
  }
}

/**
 * A crash in here shows what happened instead of an empty window.
 *
 * Styled inline on purpose. This is what is left when something has gone wrong,
 * and the thing most likely to have gone wrong is the part that gives the
 * classes their colours.
 */
class PanelErrorBoundary extends React.Component<{ children: React.ReactNode }, { message: string | null }> {
  state = { message: null as string | null };

  static getDerivedStateFromError(error: unknown) {
    return { message: error instanceof Error ? error.message : String(error) };
  }

  componentDidCatch(error: unknown) {
    console.error('The floating window failed to render:', error);
  }

  render() {
    if (this.state.message) {
      return (
        <div
          style={{
            height: '100%',
            width: '100%',
            display: 'flex',
            flexDirection: 'column',
            alignItems: 'center',
            justifyContent: 'center',
            gap: '0.5rem',
            padding: '1rem',
            textAlign: 'center',
            background: '#1c1c1c',
            color: '#f0f0f0',
            fontFamily: 'system-ui, sans-serif',
          }}
        >
          <span style={{ fontWeight: 600 }}>These panels could not be drawn.</span>
          <span style={{ fontSize: '0.85rem', opacity: 0.75, wordBreak: 'break-all' }}>{this.state.message}</span>
        </div>
      );
    }
    return this.props.children;
  }
}

function FloatingWindowContents() {
  const setEditor = useEditorStore((state) => state.setEditor);
  const waveformChannels = useEditorStore((state) => state.waveformChannels);
  const vectorscopeGain = useEditorStore((state) => state.vectorscopeGain);
  const theme = useSettingsStore((state) => state.theme);
  const fontFamily = useSettingsStore((state) => state.appSettings?.fontFamily);
  const setUI = useUIStore((state) => state.setUI);
  const movePanel = useUIStore((state) => state.movePanel);
  const setLayoutDragItem = useUIStore((state) => state.setLayoutDragItem);
  const activeLayoutDragItem = useUIStore((state) => state.activeLayoutDragItem);
  const panelLayout = useUIStore((state) => state.panelLayout);
  const activePanels = useUIStore((state) => state.activePanels);
  const panelSwitcherPlacement = useUIStore((state) => state.panelSwitcherPlacement);
  const floatTopHeight = useUIStore((state) => state.floatTopHeight);

  const [settingsLoaded, setSettingsLoaded] = useState(false);
  const [width, setWidth] = useState(() => window.innerWidth);

  // The last arrangement that arrived from the main window. Anything equal to
  // it is not sent back, or the two windows correct each other forever.
  const fromMainRef = useRef<FloatingArrangement | null>(null);
  // Whether the main window has said what this one is showing yet. Until it
  // has, this window has nothing to report and must not report it. See the
  // note on the effect that sends.
  const [hasBeenTold, setHasBeenTold] = useState(false);

  useLayoutEffect(() => {
    applyTheme(theme, fontFamily || 'poppins');
  }, [theme, fontFamily]);

  // The switcher collapses in a narrow column, so the column has to know how
  // wide it is. In a sidebar that is the drag handle; here it is the window.
  useEffect(() => {
    const onResize = () => setWidth(window.innerWidth);
    window.addEventListener('resize', onResize);
    return () => window.removeEventListener('resize', onResize);
  }, []);

  // Settings carry the theme, the scope column and the arrangement this window
  // starts with, all read from disk rather than waited for. The main window
  // sends the arrangement again once this one says it has rendered, which
  // corrects anything changed since the file was written.
  useEffect(() => {
    invoke<any>(Invokes.LoadSettings)
      .then((settings) => {
        useSettingsStore.getState().setAppSettings(settings);
        if (settings?.waveformChannels?.length) {
          setEditor({ waveformChannels: settings.waveformChannels });
        }
        if (typeof settings?.vectorscopeGain === 'number') {
          setEditor({ vectorscopeGain: settings.vectorscopeGain });
        }
        if (Array.isArray(settings?.scopeHeights)) {
          setEditor({ scopeHeights: settings.scopeHeights });
        }
      })
      .catch((err) => console.error('Could not load settings in the floating window:', err))
      .finally(() => setSettingsLoaded(true));
  }, [setEditor]);

  // Only once the tree has committed, so a window that never got this far says
  // so in the log instead of looking the same either way. The main window
  // answers with the arrangement it wants shown.
  // Asked again until it is answered.
  //
  // Saying it once is a race. `listen` is a promise, so the main window's
  // listener may not be registered on the Rust side yet when this window
  // renders, and a lost answer leaves a window that never learns what it is
  // meant to be showing and can never report anything either. Asking again
  // costs a log line and nothing else, and it stops as soon as it is answered.
  useEffect(() => {
    if (hasBeenTold) return;
    let attempts = 0;
    const ask = () => {
      attempts += 1;
      invoke('panel_window_ready', { panel: 'floating' }).catch((err) =>
        console.error('Could not report that the floating window rendered:', err),
      );
      if (attempts >= 12) window.clearInterval(timer);
    };
    ask();
    const timer = window.setInterval(ask, 400);
    return () => window.clearInterval(timer);
  }, [hasBeenTold]);

  // BLITZRAW: the scopes come from the main window, not from the decode.
  //
  // This used to listen to `analytics-update`, which the backend emits only
  // after a full decode. That is exactly the wait the pointer-following scopes
  // exist to avoid, so with the panel floating the feature did not work at all:
  // the main window worked the answer out and had nowhere to put it.
  //
  // Not both. The main window already decides between the accurate pair it is
  // holding for the open photo and the quick one read from a hovered photo's
  // preview, and a decode arriving straight from the backend would land the
  // open photo's scopes on top of whatever the pointer had moved to. See
  // utils/floatingScopes.ts.
  useEffect(() => {
    const stop = listen<FloatingScopes>(FLOATING_SCOPES_EVENT, (event) => {
      setEditor({
        histogram: (event.payload?.histogram ?? null) as any,
        waveform: (event.payload?.waveform ?? null) as any,
        scopesPath: event.payload?.path ?? null,
      });
    });
    return () => {
      stop.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [setEditor]);

  // The Metadata panel's photo, from the main window, for the same reason as
  // the two above: this window has no library, so the stores the panel reads
  // are empty in it. Filled into those same stores rather than passed down, so
  // the panel is the one panel and not a second copy of it.
  useEffect(() => {
    const stop = listen<FloatingMetadata>(FLOATING_METADATA_EVENT, (event) => {
      const payload = event.payload;
      if (!payload) return;
      const path = (payload.selectedImage as any)?.path ?? null;

      setEditor({ selectedImage: payload.selectedImage as any });
      useLibraryStore.getState().setLibrary({
        multiSelectedPaths: payload.paths ?? [],
        imageRatings: path ? { [path]: payload.rating ?? 0 } : {},
        imageList: path ? ([{ path, tags: payload.tags ?? [] }] as any) : [],
      });
      if (path && payload.thumbnail) {
        useProcessStore.getState().setProcess((state: any) => ({
          thumbnails: { ...state.thumbnails, [path]: payload.thumbnail },
        }));
      }
    });
    return () => {
      stop.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [setEditor]);

  // The Navigator's frame, from the main window, which is the only place that
  // knows what the pointer is over. Filled into the same two stores the panel
  // already reads, so there is one Navigator and not a second one for here.
  useEffect(() => {
    const stop = listen<FloatingNavigatorFrame>(FLOATING_NAVIGATOR_EVENT, (event) => {
      const { path, thumbnail, hovering } = event.payload ?? { path: null, thumbnail: null, hovering: false };
      if (path && thumbnail) {
        useProcessStore.getState().setProcess((state: any) => ({
          thumbnails: { ...state.thumbnails, [path]: thumbnail },
        }));
      }
      // `hoveredPath` is what the panel says it is showing a hover of, and it
      // is also its first choice of frame, so it carries both facts.
      useLibraryStore.getState().setLibrary({
        hoveredPath: hovering ? path : null,
        libraryActivePath: path,
      } as any);
    });
    return () => {
      stop.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);

  useEffect(() => {
    const stop = listen<any>('floating-layout', (event) => {
      const arrangement = event.payload as FloatingArrangement | undefined;
      if (!arrangement?.panelLayout) return;
      fromMainRef.current = arrangement;
      setUI((state: any) => ({
        panelLayout: { ...state.panelLayout, ...arrangement.panelLayout },
        activePanels: { ...state.activePanels, ...arrangement.activePanels },
        panelSwitcherPlacement: { ...state.panelSwitcherPlacement, ...arrangement.panelSwitcherPlacement },
        floatTopHeight: arrangement.floatTopHeight ?? state.floatTopHeight,
      }));
      // Rendering the layout is the point at which this window has something
      // of its own to report, so reporting starts here and not before.
      setHasBeenTold(true);
    });
    return () => {
      stop.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [setUI]);

  // Anything rearranged here goes back to the main window, which owns the
  // layout and is the one that saves it.
  const arrangement = useMemo(
    () => floatingArrangementOf({ panelLayout, activePanels, panelSwitcherPlacement, floatTopHeight }),
    [panelLayout, activePanels, panelSwitcherPlacement, floatTopHeight],
  );
  useEffect(() => {
    // Not a word until the main window has said what this window is showing.
    //
    // This is the bug that lost panels. The store here starts with both
    // floating regions empty, because it is a fresh store in a fresh window.
    // The effect fired on that empty arrangement, the main window took it as a
    // report and emptied its own layout to match, and then closed the window
    // because nothing was floating any more. The panel was not put back either,
    // because by the time the window closed there was nothing left in the
    // layout to put back. In the log it is a window that renders and closes in
    // the same second.
    //
    // It worked the first time and not afterwards, which is what a race looks
    // like: whether the arrival beat the report decided the outcome.
    //
    // The rule that fixes it is the one this window should have followed all
    // along. It is a view of somebody else's layout. A view reports changes to
    // what it was given; it does not have an opinion before it is given
    // anything.
    if (!shouldReportArrangement(hasBeenTold, arrangement, fromMainRef.current)) return;
    fromMainRef.current = arrangement;
    emit('floating-layout-changed', arrangement).catch((err) =>
      console.error('Could not send the floating arrangement back:', err),
    );
  }, [arrangement, hasBeenTold]);

  // What the backend has to compute, which only the main window asks for.
  const showingScopes = FLOAT_REGIONS.some((region) => activePanels[region] === Panel.Scopes);
  useEffect(() => {
    if (!settingsLoaded) return;
    const column = showingScopes
      ? (waveformChannels ?? []).map((mode) =>
          scopeName(mode) === DisplayMode.Vectorscope
            ? scopeToken(DisplayMode.Vectorscope, vectorscopeGain ?? 1)
            : mode,
        )
      : [];
    emit('detached-scopes-changed', { channels: column }).catch((err) =>
      console.error('Could not tell the main window which scopes are showing:', err),
    );
  }, [showingScopes, waveformChannels, vectorscopeGain, settingsLoaded]);

  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 5 } }));

  const handleDragStart = useCallback(
    (event: any) => {
      if (event.active.data.current?.type === 'layout-tab') {
        setLayoutDragItem(event.active.data.current.panel as Panel);
      }
    },
    [setLayoutDragItem],
  );

  const handleDragEnd = useCallback(
    (event: any) => {
      setLayoutDragItem(null);
      if (event.active.data.current?.type !== 'layout-tab') return;
      const panel = event.active.data.current.panel as Panel;

      if (event.over?.data.current?.type === 'layout-region') {
        movePanel(panel, event.over.data.current.region as PanelRegion);
        return;
      }

      // Dropped clear of both rows. In the main window that gesture sends a
      // panel out here; here it is the same gesture meaning the same thing in
      // the other direction, so the panel goes home to the sidebar it left.
      emit('floating-panel-returned', { panel }).catch((err) =>
        console.error('Could not send the panel back to the sidebar:', err),
      );
    },
    [movePanel, setLayoutDragItem],
  );

  const ActiveDragIcon = activeLayoutDragItem ? PANEL_ICONS[activeLayoutDragItem] : null;
  const isEmpty = FLOAT_REGIONS.every((region) => (panelLayout[region] ?? []).length === 0);

  return (
    <div
      className="h-screen w-screen flex flex-col bg-bg-primary text-text-primary overflow-hidden"
      // A floor under the theme. If the custom properties are ever missing
      // again the window is dark and legible rather than a white rectangle
      // that says nothing about what went wrong.
      style={{ background: 'var(--app-bg-primary, #1c1c1c)', color: 'var(--app-text-primary, #f0f0f0)' }}
    >
      <TitleBar title="Panels" />
      <div className="flex-1 min-h-0 p-2">
        {isEmpty ? (
          <div className="h-full w-full flex items-center justify-center text-text-secondary text-sm text-center px-6">
            Drag a panel out of the main window to put it here.
          </div>
        ) : (
          <DndContext sensors={sensors} onDragStart={handleDragStart} onDragEnd={handleDragEnd}>
            <SidePanelArea
              side="float"
              width={width}
              topRegion="floatTop"
              bottomRegion="floatBottom"
              renderPanel={renderFloatingPanel}
              onWidthChange={() => {}}
              isResizing={false}
            />
            <DragOverlay dropAnimation={null}>
              {ActiveDragIcon ? (
                <div className="w-9 h-9 flex items-center justify-center rounded-lg bg-accent text-button-text shadow-lg">
                  <ActiveDragIcon size={18} />
                </div>
              ) : null}
            </DragOverlay>
          </DndContext>
        )}
      </div>
    </div>
  );
}

export default function FloatingPanels() {
  // The boundary is outside everything, title bar included. It was inside
  // before, which meant a throw in the title bar unmounted the tree above it
  // and the boundary never ran.
  return (
    <PanelErrorBoundary>
      <FloatingWindowContents />
    </PanelErrorBoundary>
  );
}
