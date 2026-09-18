import { Panel, PanelRegion } from '../components/ui/AppProperties';

export type PanelLayout = Record<PanelRegion, Array<Panel>>;
export type ActivePanels = Record<PanelRegion, Panel | null>;

const REGIONS: Array<PanelRegion> = [
  'leftTop',
  'leftBottom',
  'rightTop',
  'rightBottom',
  'floatTop',
  'floatBottom',
];

/**
 * A saved workspace, merged over the current defaults.
 *
 * The layout is written to settings and read back whole, so a panel added after
 * a workspace was last saved is in the defaults and nowhere in the file, and
 * the file wins. It never appears, and no amount of looking at the default
 * layout explains why. That is exactly what happened to the Scopes and the
 * Navigator: both were added, both were correct, and neither could be seen by
 * anyone who had ever moved a panel.
 *
 * So: the saved arrangement is kept, because it is the user's, and any panel
 * the file has never heard of is appended to wherever the defaults put it.
 * Anything in the file that is no longer a panel is dropped, which is the same
 * problem from the other end and costs one line.
 *
 * The same shape as the accordion state, which is merged over its defaults for
 * the same reason: a section added later should open as it was meant to.
 */
export function reconcilePanelLayout(
  saved: Partial<PanelLayout> | null | undefined,
  defaults: PanelLayout,
): PanelLayout {
  const known = new Set<string>(Object.values(Panel));
  const out = {} as PanelLayout;
  const placed = new Set<Panel>();

  for (const region of REGIONS) {
    const kept: Array<Panel> = [];
    for (const panel of saved?.[region] ?? []) {
      // Unknown ids are panels that have been removed since the file was
      // written; a duplicate would put one panel in two places at once.
      if (!known.has(panel) || placed.has(panel)) continue;
      placed.add(panel);
      kept.push(panel);
    }
    out[region] = kept;
  }

  for (const region of REGIONS) {
    for (const panel of defaults[region] ?? []) {
      if (placed.has(panel)) continue;
      placed.add(panel);
      out[region].push(panel);
    }
  }

  return out;
}

/**
 * The active panel per region, made to agree with the layout it belongs to.
 *
 * A region can only show a panel it holds, and a region that holds anything
 * should be showing something: a tab strip with nothing selected reads as
 * broken rather than as empty. A region that holds nothing shows nothing.
 */
export function reconcileActivePanels(
  saved: Partial<ActivePanels> | null | undefined,
  layout: PanelLayout,
  defaults?: Partial<ActivePanels> | null,
): ActivePanels {
  const out = {} as ActivePanels;

  for (const region of REGIONS) {
    const panels = layout[region] ?? [];
    const usable = (panel: Panel | null | undefined) => (panel && panels.includes(panel) ? panel : null);
    // The default before the first panel in the region. Without that step,
    // restoring the defaults changed them: the left column ships showing the
    // folder tree, and falling straight to "first in the region" showed
    // metadata instead, because that is what happens to be listed first.
    out[region] = usable(saved?.[region]) ?? usable(defaults?.[region]) ?? panels[0] ?? null;
  }

  return out;
}

/** Everything about the arrangement of panels, as it is stored and restored. */
export interface Workspace {
  leftPanelWidth: number;
  rightPanelWidth: number;
  leftTopHeight: number;
  rightTopHeight: number;
  /** The split inside the floating window, when it holds two regions. */
  floatTopHeight: number;
  panelLayout: PanelLayout;
  activePanels: ActivePanels;
  panelSwitcherPlacement: Record<PanelRegion, string>;
  /**
   * BLITZRAW: where the two windows sit on the desk.
   *
   * Not part of the arrangement inside a window, which is everything above, and
   * deliberately not applied by `usableWorkspace`: this goes to the backend,
   * which is the only side that can move a window. A profile made before this
   * existed has none, and loading it leaves the windows where they are.
   */
  windows?: WindowPlaces | null;
}

/** One window's visible rectangle, in physical pixels. */
export interface WindowPlace {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Both windows, as `window_places.rs` reports and accepts them. */
export interface WindowPlaces {
  main: WindowPlace | null;
  mainMaximized: boolean;
  panel: WindowPlace | null;
}

/**
 * A stored workspace, made safe to apply.
 *
 * Everything a saved layout can be wrong about in one place: panels that no
 * longer exist, panels that did not exist yet, an active panel in a region that
 * no longer holds it, and widths of zero from a file written while a panel was
 * collapsed. The last one matters because a workspace with a zero width cannot
 * be recovered from inside the app: the panel it hides is the one holding the
 * control that would fix it.
 */
export function usableWorkspace(saved: Partial<Workspace> | null | undefined, defaults: Workspace): Workspace {
  const layout = reconcilePanelLayout(saved?.panelLayout, defaults.panelLayout);
  const width = (value: unknown, fallback: number) =>
    typeof value === 'number' && value >= 200 ? value : fallback;
  const height = (value: unknown, fallback: number) =>
    typeof value === 'number' && value >= 150 ? value : fallback;

  return {
    leftPanelWidth: width(saved?.leftPanelWidth, defaults.leftPanelWidth),
    rightPanelWidth: width(saved?.rightPanelWidth, defaults.rightPanelWidth),
    leftTopHeight: height(saved?.leftTopHeight, defaults.leftTopHeight),
    rightTopHeight: height(saved?.rightTopHeight, defaults.rightTopHeight),
    floatTopHeight: height(saved?.floatTopHeight, defaults.floatTopHeight),
    panelLayout: layout,
    activePanels: reconcileActivePanels(saved?.activePanels, layout, defaults.activePanels),
    panelSwitcherPlacement: saved?.panelSwitcherPlacement ?? defaults.panelSwitcherPlacement,
    // Carried through untouched. Nothing here can check a rectangle against
    // monitors it cannot see; the backend fits it to a real screen when it
    // applies it. See `fit_place_to_area` in panel_window.rs.
    windows: saved?.windows ?? null,
  };
}


// ============ BLITZRAW: the left side follows the view ============
/**
 * Which panel the left side shows in each view.
 *
 * The grid is for choosing a photo, so the left side is the folder tree. Full
 * view is for working on one, so it is that photo's history. Nothing else is
 * ever what you want on arriving in either.
 *
 * The saved workspace used to decide this, so opening the app landed you in the
 * grid with whatever was open when you closed, which was usually the history of
 * a photo you were no longer looking at.
 *
 * Applied on arriving in a view and nowhere else. Switching the left panel by
 * hand while you are already in a view is left alone, because that is a
 * deliberate act; it is only the arrival that has an obvious right answer.
 *
 * A view with no entry here, such as the community browser, is left alone.
 */
export const LEFT_PANEL_FOR_VIEW: Record<string, Panel> = {
  library: Panel.FolderTree,
  editor: Panel.History,
};

/** The left region a panel lives in, or null when it is not on the left. */
export function leftRegionHolding(layout: PanelLayout, panel: Panel): PanelRegion | null {
  for (const region of ['leftTop', 'leftBottom'] as Array<PanelRegion>) {
    if (layout[region]?.includes(panel)) {
      return region;
    }
  }
  return null;
}

/**
 * The left region and panel a view should arrive at, or null when there is
 * nothing to do: an unknown view, or a panel the user has dragged somewhere
 * that is not the left side at all, including into the floating window.
 */
export function leftPanelOnArriving(
  layout: PanelLayout,
  view: string,
): { region: PanelRegion; panel: Panel } | null {
  const panel = LEFT_PANEL_FOR_VIEW[view];
  if (!panel) {
    return null;
  }
  const region = leftRegionHolding(layout, panel);
  return region ? { region, panel } : null;
}
// ========== BLITZRAW END: the left side follows the view ==========

/** Where a panel was before it was sent to a window of its own. */
export interface DetachedFrom {
  panel: Panel;
  region: PanelRegion;
  index: number;
}

/**
 * Taking a panel out of the layout, remembering where it was.
 *
 * A detached panel leaves the sidebar rather than living in two places at
 * once. Its position is kept so closing the window puts it back where it came
 * from, not at the end of whichever region happens to be first.
 */
export function detachPanel(
  layout: PanelLayout,
  panel: Panel,
): { layout: PanelLayout; from: DetachedFrom | null } {
  for (const region of REGIONS) {
    const index = layout[region].indexOf(panel);
    if (index < 0) continue;
    return {
      layout: { ...layout, [region]: layout[region].filter((p) => p !== panel) },
      from: { panel, region, index },
    };
  }
  return { layout, from: null };
}

/**
 * Putting one back, at the position it left from.
 *
 * The region may have changed while the panel was away, so the index is a
 * preference rather than a promise: past the end simply means last.
 */
export function reattachPanel(layout: PanelLayout, from: DetachedFrom): PanelLayout {
  if (layout[from.region].includes(from.panel)) return layout;
  const next = [...layout[from.region]];
  next.splice(Math.min(from.index, next.length), 0, from.panel);
  return { ...layout, [from.region]: next };
}
