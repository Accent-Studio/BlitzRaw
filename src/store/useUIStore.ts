import { create } from 'zustand';
import { ImageFile, Panel, UiVisibility, CullingSuggestions, PanelRegion } from '../components/ui/AppProperties';
import { leftPanelOnArriving } from '../utils/panelLayout';

export type SwitcherPlacement = 'bottom' | 'right' | 'left' | 'top';

export interface CollapsibleSectionsState {
  basic: boolean;
  color: boolean;
  curves: boolean;
  details: boolean;
  effects: boolean;
}

export interface ConfirmModalState {
  checkbox?: {
    label: string;
    checked: boolean;
    onChange(checked: boolean): void;
  };
  confirmText?: string;
  confirmVariant?: string;
  isOpen: boolean;
  message?: string;
  onConfirm?(): void;
  title?: string;
}

export interface CollageModalState {
  isOpen: boolean;
  sourceImages: Array<Pick<ImageFile, 'path'>>;
}

export interface PanoramaModalState {
  error: string | null;
  finalImageBase64: string | null;
  isOpen: boolean;
  isProcessing: boolean;
  progressMessage: string | null;
  stitchingSourcePaths: Array<string>;
}

export interface HdrModalState {
  error: string | null;
  finalImageBase64: string | null;
  isOpen: boolean;
  isProcessing: boolean;
  progressMessage: string | null;
  stitchingSourcePaths: Array<string>;
}

export interface DenoiseModalState {
  isOpen: boolean;
  isProcessing: boolean;
  previewBase64: string | null;
  originalBase64?: string | null;
  error: string | null;
  targetPaths: string[];
  progressMessage: string | null;
  isRaw: boolean;
}

export interface NegativeConversionModalState {
  isOpen: boolean;
  targetPaths: Array<string>;
}

export interface CullingModalState {
  isOpen: boolean;
  suggestions: CullingSuggestions | null;
  progress: { current: number; total: number; stage: string } | null;
  error: string | null;
  pathsToCull: Array<string>;
}

// ============ BLITZRAW: a tool is on when its own panel is on ============
/**
 * Whether a panel is the one its region is currently showing.
 *
 * The editor asks this to decide whether the crop, mask and AI tools are in
 * use. It used to ask the single global `activePanel` instead, which is
 * whatever `setActivePanel` was called with **last, for any region at all**.
 *
 * The two sidebars hold different things: history and the folder tree on the
 * left, adjustments and crop and masks on the right. Both are on screen at
 * once, so "the last panel activated anywhere" is not the same question as
 * "which tool is in use", and the difference is invisible until two regions
 * change in the same keystroke. Pressing R in the grid did exactly that: it
 * opened the photo and asked for Crop, then arriving in the editor put History
 * in the *left* column, which took the global with it. The Crop panel was still
 * there on the right, plainly visible, and the tool it drives was off.
 *
 * Asking the regions cannot go wrong that way. It also gets the floating window
 * right, where the global never did: a Crop panel dragged to the second screen
 * is still the crop tool.
 */
export function isPanelShowing(
  activePanels: Record<PanelRegion, Panel | null>,
  panel: Panel,
): boolean {
  return Object.values(activePanels).some((shown) => shown === panel);
}
// ========== BLITZRAW END: a tool is on when its own panel is on ==========

interface UIState {
  activeView: string;
  isFullScreen: boolean;
  isWindowFullScreen: boolean;
  isInstantTransition: boolean;
  isLayoutReady: boolean;
  uiVisibility: UiVisibility;
  isLibraryExportPanelVisible: boolean;
  isSettingsOpen: boolean;

  leftPanelWidth: number;
  rightPanelWidth: number;
  bottomPanelHeight: number;
  leftTopHeight: number;
  rightTopHeight: number;
  /** The split inside the floating window, when it holds two regions. */
  floatTopHeight: number;
  compactEditorPanelHeightOverride: number | null;

  panelLayout: Record<PanelRegion, Panel[]>;
  activePanels: Record<PanelRegion, Panel | null>;
  /**
   * Where each floating panel came from, so closing the window puts it back
   * where it was rather than at the end of whichever region is first.
   *
   * The panels themselves are in `panelLayout.floatTop` and `floatBottom` like
   * any others, and are saved with the workspace: the floating window comes
   * back on the next start holding what it held. This list is the only part
   * that is not saved, because a panel that has been floating since before a
   * restart has no sidebar position worth remembering, and the default one is
   * as good a guess as a stale one.
   */
  detachedPanels: Array<import('../utils/panelLayout').DetachedFrom>;
  /** The scopes the floating window is showing, written as the backend reads them. */
  detachedScopeChannels: Array<string> | null;
  activeLayoutDragItem: Panel | null;
  setLayoutDragItem: (panel: Panel | null) => void;
  movePanel: (panel: Panel, toRegion: PanelRegion) => void;
  movePanelToIndex: (panel: Panel, toRegion: PanelRegion, index: number) => void;
  setActivePanel: (region: PanelRegion, panel: Panel | null) => void;

  panelSwitcherPlacement: Record<PanelRegion, SwitcherPlacement>;
  setPanelSwitcherPlacement: (region: PanelRegion, placement: SwitcherPlacement) => void;

  activePanel: Panel | null;
  renderedPanel: Panel | null;
  slideDirection: number;
  collapsibleSectionsState: CollapsibleSectionsState;

  isCreateFolderModalOpen: boolean;
  isRenameFolderModalOpen: boolean;
  isRenameFileModalOpen: boolean;
  renameTargetPaths: Array<string>;
  isImportModalOpen: boolean;
  isCopyPasteSettingsModalOpen: boolean;
  importTargetFolder: string | null;
  importSourcePaths: Array<string>;
  folderActionTarget: string | null;

  isCreateAlbumModalOpen: boolean;
  isCreateAlbumGroupModalOpen: boolean;
  isRenameAlbumModalOpen: boolean;
  albumActionTarget: string | null;

  confirmModalState: ConfirmModalState;
  panoramaModalState: PanoramaModalState;
  hdrModalState: HdrModalState;
  negativeModalState: NegativeConversionModalState;
  /** `mode` picks which detector runs; the dialog around it is the same. */
  autoStackModalState: { isOpen: boolean; targetPaths: Array<string>; mode?: 'brackets' | 'bursts' };
  /** True while a bulk HDR run owns the merge pipeline. The single-merge modal
   *  is driven by backend events, so it must stand aside while a queue is using
   *  the same events for its own progress. */
  isBulkHdrRunning: boolean;
  denoiseModalState: DenoiseModalState;
  cullingModalState: CullingModalState;
  collageModalState: CollageModalState;

  setUI: (updater: Partial<UIState> | ((state: UIState) => Partial<UIState>)) => void;
  setPanel: (panel: Panel | null) => void;
  /**
   * BLITZRAW: put the left side on the panel this view arrives at.
   *
   * Called on arriving in a view and once after a saved workspace is applied,
   * so opening the app in the grid does not land on the history of a photo you
   * are no longer looking at. See LEFT_PANEL_FOR_VIEW in utils/panelLayout.ts.
   */
  showLeftPanelForView: (view: string) => void;
  customEscapeHandler: (() => void) | null;
  setCustomEscapeHandler: (handler: (() => void) | null) => void;
  searchFocusRequest: number;
  requestSearchFocus: () => void;
}

/**
 * A layout with every region copied, whichever regions exist.
 *
 * `movePanel` used to build this by naming the four sidebar regions, so the two
 * floating ones were dropped the first time anything was dragged: the panels in
 * the floating window vanished from the layout, and the window with them.
 */
function copyLayout(layout: Record<PanelRegion, Panel[]>): Record<PanelRegion, Panel[]> {
  const out = {} as Record<PanelRegion, Panel[]>;
  for (const region of Object.keys(layout) as Array<PanelRegion>) {
    out[region] = [...layout[region]];
  }
  return out;
}

export const useUIStore = create<UIState>((set, get) => ({
  activeView: 'library',
  isFullScreen: false,
  isWindowFullScreen: false,
  isInstantTransition: false,
  isLayoutReady: false,
  uiVisibility: { filmstrip: true, leftPanel: true, rightPanel: true },
  isLibraryExportPanelVisible: false,
  isSettingsOpen: false,

  leftPanelWidth: 350,
  rightPanelWidth: 350,
  bottomPanelHeight: 144,
  leftTopHeight: 450,
  rightTopHeight: 450,
  floatTopHeight: 400,
  compactEditorPanelHeightOverride: null,

  panelLayout: {
    leftTop: [Panel.Metadata, Panel.FolderTree, Panel.History, Panel.Export],
    leftBottom: [],
    // Both new panels start in the switcher beside the others, so they can be
    // found. Dragging either into a bottom region is what gets them visible at
    // the same time as the sliders, which is the arrangement they are for.
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
  detachedPanels: [],
  detachedScopeChannels: null,
  activeLayoutDragItem: null,

  panelSwitcherPlacement: {
    leftTop: 'bottom',
    leftBottom: 'bottom',
    rightTop: 'right',
    rightBottom: 'right',
    floatTop: 'bottom',
    floatBottom: 'bottom',
  },
  setPanelSwitcherPlacement: (region, placement) =>
    set((state) => ({
      panelSwitcherPlacement: { ...state.panelSwitcherPlacement, [region]: placement },
    })),

  activePanel: Panel.Adjustments,
  renderedPanel: Panel.Adjustments,
  slideDirection: 1,
  // BLITZRAW: colorCorrection open by default, because it is the one every
  // photo needs; the mixer and grading closed, because most photos do not.
  collapsibleSectionsState: {
    basic: true,
    colorCorrection: true,
    curves: true,
    colorMixer: false,
    color: false,
    details: false,
    effects: false,
  },

  isCreateFolderModalOpen: false,
  isRenameFolderModalOpen: false,
  isRenameFileModalOpen: false,
  renameTargetPaths: [],
  isImportModalOpen: false,
  isCopyPasteSettingsModalOpen: false,
  importTargetFolder: null,
  importSourcePaths: [],
  folderActionTarget: null,
  isCreateAlbumModalOpen: false,
  isCreateAlbumGroupModalOpen: false,
  isRenameAlbumModalOpen: false,
  albumActionTarget: null,

  confirmModalState: { isOpen: false },
  panoramaModalState: {
    error: null,
    finalImageBase64: null,
    isOpen: false,
    isProcessing: false,
    progressMessage: '',
    stitchingSourcePaths: [],
  },
  hdrModalState: {
    error: null,
    finalImageBase64: null,
    isOpen: false,
    isProcessing: false,
    progressMessage: '',
    stitchingSourcePaths: [],
  },
  negativeModalState: { isOpen: false, targetPaths: [] },
  autoStackModalState: { isOpen: false, targetPaths: [], mode: 'brackets' },
  isBulkHdrRunning: false,
  denoiseModalState: {
    isOpen: false,
    isProcessing: false,
    previewBase64: null,
    error: null,
    targetPaths: [],
    progressMessage: null,
    isRaw: false,
  },
  cullingModalState: { isOpen: false, suggestions: null, progress: null, error: null, pathsToCull: [] },
  collageModalState: { isOpen: false, sourceImages: [] },

  setUI: (updater) => set((state) => (typeof updater === 'function' ? updater(state) : updater)),

  setLayoutDragItem: (panel) => set({ activeLayoutDragItem: panel }),

  movePanel: (panel, toRegion) =>
    set((state) => {
      // Copied from whatever regions there are rather than named one by one.
      // Listing them meant that adding the two floating regions silently
      // dropped them from the layout on the next drag, and the type checker
      // was the only thing that noticed.
      const layout = copyLayout(state.panelLayout);
      const active = { ...state.activePanels };

      let fromRegion: PanelRegion | null = null;
      (Object.keys(layout) as PanelRegion[]).forEach((r) => {
        if (layout[r].includes(panel)) {
          fromRegion = r;
          layout[r] = layout[r].filter((p) => p !== panel);
        }
      });

      if (!layout[toRegion].includes(panel)) layout[toRegion].push(panel);

      if (fromRegion && active[fromRegion] === panel) {
        active[fromRegion] = layout[fromRegion].length > 0 ? layout[fromRegion][0] : null;
      }

      active[toRegion] = panel;

      return {
        panelLayout: layout,
        activePanels: active,
        activeLayoutDragItem: null,
        activePanel: panel,
        renderedPanel: panel,
      };
    }),

  movePanelToIndex: (panel, toRegion, index) =>
    set((state) => {
      // Copied from whatever regions there are rather than named one by one.
      // Listing them meant that adding the two floating regions silently
      // dropped them from the layout on the next drag, and the type checker
      // was the only thing that noticed.
      const layout = copyLayout(state.panelLayout);
      const active = { ...state.activePanels };

      let fromRegion: PanelRegion | null = null;
      (Object.keys(layout) as PanelRegion[]).forEach((r) => {
        if (layout[r].includes(panel)) {
          fromRegion = r;
          layout[r] = layout[r].filter((p) => p !== panel);
        }
      });

      const clampedIndex = Math.max(0, Math.min(index, layout[toRegion].length));
      layout[toRegion].splice(clampedIndex, 0, panel);

      if (fromRegion && active[fromRegion] === panel) {
        active[fromRegion] = layout[fromRegion].length > 0 ? layout[fromRegion][0] : null;
      }
      active[toRegion] = panel;

      return {
        panelLayout: layout,
        activePanels: active,
        activeLayoutDragItem: null,
        activePanel: panel,
        renderedPanel: panel,
      };
    }),

  setActivePanel: (region, panel) =>
    set((state) => {
      if (!panel) return state;
      const updates: Partial<UIState> = {
        activePanels: { ...state.activePanels, [region]: panel },
        activePanel: panel,
        renderedPanel: panel,
      };
      return updates;
    }),

  showLeftPanelForView: (view) =>
    set((state) => {
      const wanted = leftPanelOnArriving(state.panelLayout, view);
      if (!wanted || state.activePanels[wanted.region] === wanted.panel) {
        return state;
      }
      return { activePanels: { ...state.activePanels, [wanted.region]: wanted.panel } };
    }),

  setPanel: (panelId) => {
    const state = get();
    if (!panelId) return;

    let targetRegion: PanelRegion | null = null;
    for (const region of Object.keys(state.panelLayout) as PanelRegion[]) {
      if (state.panelLayout[region].includes(panelId)) {
        targetRegion = region;
        break;
      }
    }
    if (targetRegion) state.setActivePanel(targetRegion, panelId);
  },

  customEscapeHandler: null,
  setCustomEscapeHandler: (handler) => set({ customEscapeHandler: handler }),
  searchFocusRequest: 0,
  requestSearchFocus: () => set((state) => ({ searchFocusRequest: state.searchFocusRequest + 1 })),
}));
