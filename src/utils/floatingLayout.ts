import { Panel, PanelRegion } from '../components/ui/AppProperties';
import type { ActivePanels, PanelLayout } from './panelLayout';

/**
 * The part of the layout that crosses between the two windows.
 *
 * The main window owns the layout, because it is the one that saves the
 * workspace and one owner means there is nothing to reconcile. The floating
 * window is a view of two of its regions, and this is what it is sent and what
 * it sends back: the two regions, which panel each is showing, where each tab
 * strip sits, and the split between them. Nothing else. The sidebar regions
 * stay in the main window, where a second window has no business rearranging
 * them.
 */

export const FLOATING_REGIONS: Array<PanelRegion> = ['floatTop', 'floatBottom'];

export interface FloatingArrangement {
  panelLayout: Partial<PanelLayout>;
  activePanels: Partial<ActivePanels>;
  panelSwitcherPlacement: Record<string, string>;
  floatTopHeight: number;
}

/** The floating half of a full layout, ready to send. */
export function floatingArrangementOf(state: {
  panelLayout: PanelLayout;
  activePanels: ActivePanels;
  panelSwitcherPlacement: Record<string, string>;
  floatTopHeight: number;
}): FloatingArrangement {
  const panelLayout: Partial<PanelLayout> = {};
  const activePanels: Partial<ActivePanels> = {};
  const panelSwitcherPlacement: Record<string, string> = {};

  for (const region of FLOATING_REGIONS) {
    panelLayout[region] = [...(state.panelLayout[region] ?? [])];
    activePanels[region] = state.activePanels[region] ?? null;
    panelSwitcherPlacement[region] = state.panelSwitcherPlacement[region] ?? 'bottom';
  }

  return { panelLayout, activePanels, panelSwitcherPlacement, floatTopHeight: state.floatTopHeight };
}

/**
 * Whether two arrangements say the same thing.
 *
 * This is what stops the windows correcting each other forever. Each one sends
 * what it has whenever it changes, and each one applies what it is sent, so
 * without a comparison an arrival would look like a change and be sent
 * straight back. Compared by value rather than by identity, because the two
 * sides are different objects by definition: one of them arrived over an event
 * and was rebuilt from JSON.
 */
export function sameArrangement(a: FloatingArrangement | null, b: FloatingArrangement | null): boolean {
  if (!a || !b) return a === b;
  if (a.floatTopHeight !== b.floatTopHeight) return false;

  for (const region of FLOATING_REGIONS) {
    const left = a.panelLayout[region] ?? [];
    const right = b.panelLayout[region] ?? [];
    if (left.length !== right.length) return false;
    for (let i = 0; i < left.length; i += 1) {
      if (left[i] !== right[i]) return false;
    }
    if ((a.activePanels[region] ?? null) !== (b.activePanels[region] ?? null)) return false;
    if ((a.panelSwitcherPlacement[region] ?? 'bottom') !== (b.panelSwitcherPlacement[region] ?? 'bottom')) {
      return false;
    }
  }

  return true;
}

/**
 * Whether the floating window should report the arrangement it is showing.
 *
 * This is the rule that lost panels. The floating window's store starts with
 * both floating regions empty, because it is a fresh store in a fresh window.
 * It reported that, the main window took the report at face value and emptied
 * its own layout to match, and then closed the window because nothing was
 * floating any more. The panel was not put back either: by the time the window
 * closed there was nothing left in the layout to put back. In the log it reads
 * as a window that renders and closes inside the same second.
 *
 * It worked the first time and not afterwards, which is what a race looks like.
 * Whether the main window's answer arrived before the empty report went out
 * decided the outcome, and nothing made that ordering certain.
 *
 * The rule is the one a view should always have followed: it reports changes to
 * what it was given, and has no opinion before it is given anything.
 */
export function shouldReportArrangement(
  hasBeenTold: boolean,
  current: FloatingArrangement,
  lastKnown: FloatingArrangement | null,
): boolean {
  if (!hasBeenTold) return false;
  return !sameArrangement(current, lastKnown);
}

/** Whether anything is floating at all, which is what decides if the window opens. */
export function anythingFloating(layout: Partial<PanelLayout> | null | undefined): boolean {
  return FLOATING_REGIONS.some((region) => (layout?.[region]?.length ?? 0) > 0);
}

/** Every panel currently in the floating window, top row first. */
export function floatingPanels(layout: Partial<PanelLayout> | null | undefined): Array<Panel> {
  return FLOATING_REGIONS.flatMap((region) => layout?.[region] ?? []);
}
