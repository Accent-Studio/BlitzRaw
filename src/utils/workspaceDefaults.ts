import { Panel, PanelRegion } from '../components/ui/AppProperties';
import { Workspace } from './panelLayout';

/**
 * The layout the app ships with.
 *
 * Kept apart from the store so "reset to default" means one thing, and so a
 * saved layout can be checked against it without reaching for a store whose
 * current values are the very thing being replaced.
 */
export const DEFAULT_WORKSPACE: Workspace = {
  leftPanelWidth: 350,
  rightPanelWidth: 350,
  leftTopHeight: 450,
  rightTopHeight: 450,
  floatTopHeight: 400,
  panelLayout: {
    leftTop: [Panel.Metadata, Panel.FolderTree, Panel.History, Panel.Export],
    leftBottom: [],
    rightTop: [
      Panel.Adjustments,
      Panel.Crop,
      Panel.Masks,
      Panel.Ai,
      Panel.Presets,
      Panel.Scopes,
      Panel.Navigator,
    ],
    rightBottom: [],
    // Nothing floats by default. A panel gets here by being dragged out, and
    // stays here across restarts, which is why these are part of the saved
    // workspace rather than something the window remembers for itself.
    floatTop: [],
    floatBottom: [],
  },
  activePanels: {
    leftTop: Panel.FolderTree,
    leftBottom: null,
    rightTop: Panel.Adjustments,
    rightBottom: null,
    floatTop: null,
    floatBottom: null,
  },
  panelSwitcherPlacement: {
    leftTop: 'bottom',
    leftBottom: 'bottom',
    rightTop: 'right',
    rightBottom: 'right',
    floatTop: 'bottom',
    floatBottom: 'bottom',
  } as Record<PanelRegion, string>,
};

/** The same, for anything that wants the shipped values rather than the live ones. */
export function currentWorkspaceDefaults(): Workspace {
  return {
    ...DEFAULT_WORKSPACE,
    panelLayout: {
      leftTop: [...DEFAULT_WORKSPACE.panelLayout.leftTop],
      leftBottom: [...DEFAULT_WORKSPACE.panelLayout.leftBottom],
      rightTop: [...DEFAULT_WORKSPACE.panelLayout.rightTop],
      rightBottom: [...DEFAULT_WORKSPACE.panelLayout.rightBottom],
      floatTop: [...DEFAULT_WORKSPACE.panelLayout.floatTop],
      floatBottom: [...DEFAULT_WORKSPACE.panelLayout.floatBottom],
    },
    activePanels: { ...DEFAULT_WORKSPACE.activePanels },
    panelSwitcherPlacement: { ...DEFAULT_WORKSPACE.panelSwitcherPlacement },
  };
}
