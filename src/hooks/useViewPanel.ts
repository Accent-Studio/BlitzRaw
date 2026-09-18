import { useEffect, useRef } from 'react';
import { Panel, PanelRegion } from '../components/ui/AppProperties';
import { useUIStore } from '../store/useUIStore';
import type { PanelLayout } from '../utils/panelLayout';

/**
 * Showing the panel that suits the view you just moved to.
 *
 * The grid and the editor want different things from the left column. In the
 * grid you are choosing what to work on, so the folder tree is what belongs
 * there. In the editor you are working on one photo, and its history is what
 * belongs there. Switching by hand every time is a click each way, several
 * hundred times a shoot.
 *
 * # Only when the view actually changes
 *
 * Not continuously, which is the difference between a helpful default and a
 * panel that fights you. Pick Metadata while in the editor and it stays picked
 * for as long as you are in the editor; it is only the move between grid and
 * editor that sets one. That is what "switch on entering, switch back on
 * leaving" means, and it leaves the last word with whoever clicked last.
 *
 * # It runs last, and that used to matter
 *
 * This fires on the change of view, so it is always after whatever caused the
 * change. Pressing R in the grid opens the photo and asks for the crop panel;
 * this then ran and set History. Both panels are correct and both are on
 * screen, one per sidebar, but the editor was deciding whether the crop tool
 * was in use by reading a single global that either sidebar could overwrite, so
 * the crop rectangle vanished. `isPanelShowing` in useUIStore asks the regions
 * instead, which makes the order of these two harmless.
 *
 * # Wherever the panel happens to be
 *
 * The region is found rather than assumed. Both panels start in the left
 * column, but they can be dragged anywhere, including into the floating window,
 * and the one thing this must not do is set the active panel of a region that
 * does not hold it, which would blank that region. A panel that is nowhere at
 * all, because it was dragged out and the layout no longer lists it, is left
 * alone.
 */

/** Which region holds a panel, or null if none does. */
export function regionHolding(layout: PanelLayout, panel: Panel): PanelRegion | null {
  for (const region of Object.keys(layout) as Array<PanelRegion>) {
    if (layout[region]?.includes(panel)) {
      return region;
    }
  }
  return null;
}

/** The panel a view wants shown, or null for a view with no opinion. */
export function panelForView(view: string): Panel | null {
  if (view === 'editor') return Panel.History;
  if (view === 'library') return Panel.FolderTree;
  return null;
}


export function useViewPanel() {
  const activeView = useUIStore((state) => state.activeView);
  const seen = useRef<string | null>(null);

  useEffect(() => {
    // The first render is not a change of view, it is arriving. Whatever the
    // saved workspace says was showing stays showing.
    if (seen.current === null) {
      seen.current = activeView;
      return;
    }
    if (seen.current === activeView) return;
    seen.current = activeView;

    const wanted = panelForView(activeView);
    if (!wanted) return;

    const { panelLayout, activePanels, setActivePanel } = useUIStore.getState();
    const region = regionHolding(panelLayout, wanted);
    if (!region) return;
    if (activePanels[region] === wanted) return;

    setActivePanel(region, wanted);
  }, [activeView]);
}
