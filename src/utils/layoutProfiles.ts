import { Workspace } from './panelLayout';

export interface LayoutProfile {
  name: string;
  workspace: Workspace;
}

/**
 * Adding a named layout, or replacing the one that already has that name.
 *
 * In place rather than appended, so saving over a profile leaves the list in
 * the order it was built. A list that reshuffles itself when you overwrite an
 * entry is hard to use and hard to trust.
 */
export function withProfile(profiles: Array<LayoutProfile>, profile: LayoutProfile): Array<LayoutProfile> {
  const at = profiles.findIndex((existing) => existing.name === profile.name);
  if (at < 0) return [...profiles, profile];
  return profiles.map((existing, index) => (index === at ? profile : existing));
}

/** Removing one by name. Removing one that is not there is not an error. */
export function withoutProfile(profiles: Array<LayoutProfile>, name: string): Array<LayoutProfile> {
  return profiles.filter((profile) => profile.name !== name);
}

/**
 * Whether two arrangements are the same one.
 *
 * By value, so the "current" mark is earned rather than remembered: load a
 * profile and it is current, drag one panel and it is not. Remembering the
 * name instead would keep claiming a layout the screen no longer shows.
 *
 * Panel order inside a region counts, since that is the order of the tabs, and
 * so do the widths, since a layout is as much about size as arrangement.
 */
export function sameWorkspace(a: Workspace | null | undefined, b: Workspace | null | undefined): boolean {
  if (!a || !b) return false;
  const shape = (w: Workspace) => ({
    leftPanelWidth: w.leftPanelWidth,
    rightPanelWidth: w.rightPanelWidth,
    leftTopHeight: w.leftTopHeight,
    rightTopHeight: w.rightTopHeight,
    panelLayout: w.panelLayout,
    activePanels: w.activePanels,
    panelSwitcherPlacement: w.panelSwitcherPlacement,
  });
  return JSON.stringify(shape(a)) === JSON.stringify(shape(b));
}
