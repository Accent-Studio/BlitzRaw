import { useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useUIStore } from '../store/useUIStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { Workspace, WindowPlaces, usableWorkspace } from '../utils/panelLayout';
import { currentWorkspaceDefaults } from '../utils/workspaceDefaults';
import { sameWorkspace, withProfile, withoutProfile } from '../utils/layoutProfiles';

export interface LayoutProfile {
  name: string;
  workspace: Workspace;
}

/**
 * Keeping, restoring and resetting arrangements of panels, by name.
 *
 * The layout already autosaves, which is right for not losing a nudge and
 * wrong for anything deliberate. These are the deliberate part: name an
 * arrangement, come back to it, and get out of one that has gone wrong.
 *
 * The first version of this kept exactly one unnamed layout, and it did not
 * survive a restart. Two reasons, and only one of them was the interface: the
 * backend's settings struct had no field for it, so every save dropped it
 * silently on the way to disk, while the copy in memory made it look saved
 * until the app closed.
 *
 * "Reset" matters most. A layout can be dragged into a state that hides the
 * control that would undo it, and without this the way back is editing
 * settings.json by hand.
 *
 * # Where the windows are is part of the layout
 *
 * It was not, and that made two profiles for two screen arrangements identical:
 * a profile described which panel sat in which column and how wide the columns
 * were, and said nothing about the windows those columns were in. Loading
 * either one correctly did nothing, which reads as broken and was not.
 *
 * The windows are the backend's to move, so a profile carries the rectangles
 * and the backend applies them, fitting each to a monitor that exists. See
 * `window_places.rs`. Saving and loading are therefore asynchronous now, which
 * is why both return promises.
 */
export function useWorkspace() {
  const setUI = useUIStore((state) => state.setUI);
  const profiles = useSettingsStore(
    (state) => (state.appSettings?.layoutProfiles as Array<LayoutProfile> | undefined) ?? [],
  );

  /**
   * The arrangement inside the windows, which is all this can see.
   *
   * Deliberately without the window places: this is also what the "current"
   * mark is compared against, and a window moved by one pixel must not stop a
   * profile reading as the one on screen.
   */
  const currentWorkspace = useCallback((): Workspace => {
    const ui = useUIStore.getState();
    return {
      leftPanelWidth: ui.leftPanelWidth,
      rightPanelWidth: ui.rightPanelWidth,
      leftTopHeight: ui.leftTopHeight,
      rightTopHeight: ui.rightTopHeight,
      floatTopHeight: ui.floatTopHeight,
      panelLayout: ui.panelLayout,
      activePanels: ui.activePanels,
      panelSwitcherPlacement: ui.panelSwitcherPlacement,
    };
  }, []);

  const writeProfiles = useCallback((next: Array<LayoutProfile>) => {
    const { appSettings, handleSettingsChange } = useSettingsStore.getState();
    if (!appSettings) return false;
    handleSettingsChange({ ...appSettings, layoutProfiles: next } as any);
    return true;
  }, []);

  /** Keeps the arrangement on screen under a name, replacing one of that name. */
  const saveProfile = useCallback(
    async (name: string) => {
      const trimmed = name.trim();
      if (!trimmed) return false;

      // Where the windows are is part of what is being kept. A failure here is
      // not a reason to lose the rest: a profile with no window places is what
      // every profile was until today, and it loads perfectly well.
      const windows = await invoke<WindowPlaces>('get_window_places').catch(() => null);

      return writeProfiles(
        withProfile(profiles, { name: trimmed, workspace: { ...currentWorkspace(), windows } }),
      );
    },
    [profiles, currentWorkspace, writeProfiles],
  );

  const loadProfile = useCallback(
    async (name: string) => {
      const found = profiles.find((profile) => profile.name === name);
      if (!found) return false;

      const workspace = usableWorkspace(found.workspace, currentWorkspaceDefaults());
      setUI(workspace as any);

      // Second, and only if the profile has any. A profile saved before window
      // places existed leaves the windows exactly where they are, which is the
      // right thing for it to do.
      if (workspace.windows) {
        await invoke('apply_window_places', { places: workspace.windows }).catch((error) =>
          console.error('Could not move the windows for this profile:', error),
        );
      }
      return true;
    },
    [profiles, setUI],
  );

  const deleteProfile = useCallback(
    (name: string) => writeProfiles(withoutProfile(profiles, name)),
    [profiles, writeProfiles],
  );

  /** Back to what the app ships with. The way out of a layout gone wrong. */
  const resetWorkspace = useCallback(() => {
    setUI(currentWorkspaceDefaults() as any);
  }, [setUI]);

  /**
   * Which kept layout the screen currently matches, if any.
   *
   * Compared by value rather than remembered by name, so dragging a panel
   * after loading a profile stops it reading as current, which is the only
   * honest answer.
   */
  const currentProfileName = useCallback((): string | null => {
    const now = currentWorkspace();
    return profiles.find((profile) => sameWorkspace(profile.workspace, now))?.name ?? null;
  }, [profiles, currentWorkspace]);

  return { profiles, saveProfile, loadProfile, deleteProfile, resetWorkspace, currentProfileName };
}
