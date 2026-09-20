import { useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useShallow } from 'zustand/react/shallow';
import { useSettingsStore } from '../store/useSettingsStore';
import { useUIStore } from '../store/useUIStore';
import { usableWorkspace } from '../utils/panelLayout';
import { usableStepLimit } from '../utils/editHistory';
import { currentWorkspaceDefaults } from '../utils/workspaceDefaults';
import { useLibraryStore } from '../store/useLibraryStore';
import { useEditorStore } from '../store/useEditorStore';
import { useProcessStore } from '../store/useProcessStore';
import { DEFAULT_THEME_ID, applyTheme } from '../utils/themes';
import { COPYABLE_ADJUSTMENT_KEYS } from '../utils/adjustments';
import {
  FilterCriteria,
  Invokes,
  LibraryViewMode,
  RawStatus,
  EditedStatus,
  Theme,
  LEGACY_THUMBNAIL_SIZES,
  THUMBNAIL_SIZE_DEFAULT,
  THUMBNAIL_SIZE_DEFAULT_ANDROID,
  THUMBNAIL_SIZE_MAX,
  THUMBNAIL_SIZE_MIN,
  ThumbnailAspectRatio,
} from '../components/ui/AppProperties';
import { useTranslation } from 'react-i18next';

interface UseAppInitializationProps {
  preloadedDataRef: React.RefObject<any>;
  thumbnailSize: number;
  setThumbnailSize: (size: number) => void;
  thumbnailAspectRatio: ThumbnailAspectRatio;
  setThumbnailAspectRatio: (ratio: ThumbnailAspectRatio) => void;
  libraryViewMode: LibraryViewMode;
  setLibraryViewMode: (mode: LibraryViewMode) => void;
}

const getDefaultLanguage = (i18nInstance: any): string => {
  const browserLang = navigator.language || (navigator as any).userLanguage || 'en';
  const shortLang = browserLang.split('-')[0].toLowerCase();
  const supportedLanguages = Object.keys(i18nInstance.options.resources || {});
  const fallbackLang =
    typeof i18nInstance.options.fallbackLng === 'string'
      ? i18nInstance.options.fallbackLng
      : i18nInstance.options.fallbackLng?.[0] || 'en';

  return supportedLanguages.includes(browserLang)
    ? browserLang
    : supportedLanguages.includes(shortLang)
      ? shortLang
      : fallbackLang;
};

export const useAppInitialization = ({
  preloadedDataRef,
  thumbnailSize,
  setThumbnailSize,
  thumbnailAspectRatio,
  setThumbnailAspectRatio,
  libraryViewMode,
  setLibraryViewMode,
}: UseAppInitializationProps) => {
  const isInitialMount = useRef(true);
  const { i18n } = useTranslation();

  const {
    appSettings,
    theme,
    osPlatform,
    setAppSettings,
    setTheme,
    setSupportedTypes,
    initPlatform,
    handleSettingsChange,
  } = useSettingsStore(
    useShallow((state) => ({
      appSettings: state.appSettings,
      theme: state.theme,
      osPlatform: state.osPlatform,
      setAppSettings: state.setAppSettings,
      setTheme: state.setTheme,
      setSupportedTypes: state.setSupportedTypes,
      initPlatform: state.initPlatform,
      handleSettingsChange: state.handleSettingsChange,
    })),
  );

  const { uiVisibility, setUI } = useUIStore(
    useShallow((state) => ({
      uiVisibility: state.uiVisibility,
      setUI: state.setUI,
    })),
  );

  const workspaceProps = useUIStore(
    useShallow((state) => ({
      leftPanelWidth: state.leftPanelWidth,
      rightPanelWidth: state.rightPanelWidth,
      leftTopHeight: state.leftTopHeight,
      rightTopHeight: state.rightTopHeight,
      floatTopHeight: state.floatTopHeight,
      panelLayout: state.panelLayout,
      activePanels: state.activePanels,
      panelSwitcherPlacement: state.panelSwitcherPlacement,
    })),
  );

  const {
    sortCriteria,
    filterCriteria,
    currentFolderPath,
    expandedFolders,
    activeAlbumId,
    expandedAlbumGroups,
    setSortCriteria,
    setFilterCriteria,
    setLibrary,
  } = useLibraryStore(
    useShallow((state) => ({
      sortCriteria: state.sortCriteria,
      filterCriteria: state.filterCriteria,
      currentFolderPath: state.currentFolderPath,
      expandedFolders: state.expandedFolders,
      activeAlbumId: state.activeAlbumId,
      expandedAlbumGroups: state.expandedAlbumGroups,
      setSortCriteria: state.setSortCriteria,
      setFilterCriteria: state.setFilterCriteria,
      setLibrary: state.setLibrary,
    })),
  );

  const { setEditor } = useEditorStore(
    useShallow((state) => ({
      setEditor: state.setEditor,
    })),
  );

  const isAndroid = osPlatform === 'android';
  const defaultThumbnailSize = isAndroid ? THUMBNAIL_SIZE_DEFAULT_ANDROID : THUMBNAIL_SIZE_DEFAULT;
  const defaultLibraryViewMode = isAndroid ? LibraryViewMode.Recursive : LibraryViewMode.Flat;
  const prevImageCountsNeed = useRef<boolean | undefined>(undefined);

  useEffect(() => {
    initPlatform();
  }, [initPlatform]);

  useEffect(() => {
    invoke(Invokes.GetSupportedFileTypes)
      .then((types: any) => setSupportedTypes(types))
      .catch((err) => console.error('Failed to load supported file types:', err));
  }, [setSupportedTypes]);

  useEffect(() => {
    invoke(Invokes.LoadSettings)
      .then(async (settings: any) => {
        if (
          !settings.copyPasteSettings ||
          !settings.copyPasteSettings.includedAdjustments ||
          settings.copyPasteSettings.includedAdjustments.length === 0
        ) {
          settings.copyPasteSettings = { mode: 'merge', includedAdjustments: COPYABLE_ADJUSTMENT_KEYS };
        }

        if (!settings.language) {
          settings.language = getDefaultLanguage(i18n);
          handleSettingsChange(settings);
        }

        // legacy
        const savedRawStatus = settings?.filterCriteria?.rawStatus as string | undefined;
        if (savedRawStatus === 'groupVariants' || savedRawStatus === 'rawOverNonRaw') {
          const legacyPref = settings?.groupPreferredType === 'jpeg' ? 'jpeg' : 'raw';
          settings.grouping = legacyPref;
          settings.filterCriteria = { ...settings.filterCriteria, rawStatus: 'all' };
          handleSettingsChange(settings);
        }

        setAppSettings(settings);
        i18n.changeLanguage(settings.language);

        if (settings?.sortCriteria) setSortCriteria(settings.sortCriteria);

        if (settings?.filterCriteria) {
          setFilterCriteria((prev: FilterCriteria) => ({
            ...prev,
            ...settings.filterCriteria,
            rawStatus: settings.filterCriteria.rawStatus || RawStatus.All,
            editedStatus: settings.filterCriteria.editedStatus || EditedStatus.All,
            colors: settings.filterCriteria.colors || [],
          }));
        }

        if (settings?.theme) setTheme(settings.theme);

        if (settings?.uiVisibility) {
          setUI((state) => ({ uiVisibility: { ...state.uiVisibility, ...settings.uiVisibility } }));
        }

        // Merged over the defaults rather than replacing them, so a section
        // added later opens as it was meant to instead of as missing.
        if (settings?.openAdjustmentSections) {
          setUI((state) => ({
            collapsibleSectionsState: {
              ...state.collapsibleSectionsState,
              ...settings.openAdjustmentSections,
            },
          }));
        }

        if (settings?.workspace) {
          // Merged over the defaults rather than replacing them. A saved
          // workspace is a complete layout, so a panel added since it was
          // written is in the defaults, absent from the file, and invisible
          // once the file is applied. See reconcilePanelLayout.
          setUI(usableWorkspace(settings.workspace, currentWorkspaceDefaults()) as any);
          // BLITZRAW: and then the left side, by the rule rather than by what
          // was open at the last close. This has to be after the line above,
          // which carries a saved `activePanels` with it. Without it the app
          // opens in the grid showing the history of a photo you are no longer
          // looking at, which is what it used to do.
          useUIStore.getState().showLeftPanelForView(useUIStore.getState().activeView);
        }

        if (settings?.isWaveformVisible !== undefined) setEditor({ isWaveformVisible: settings.isWaveformVisible });
        if (settings?.activeWaveformChannel) setEditor({ activeWaveformChannel: settings.activeWaveformChannel });
        // A settings file written before the column existed carries one
        // channel; it becomes a column of one rather than an empty panel.
        setEditor({
          waveformChannels: settings?.waveformChannels?.length
            ? settings.waveformChannels
            : [settings?.activeWaveformChannel || 'luma'],
        });
        if (typeof settings?.waveformHeight === 'number') setEditor({ waveformHeight: settings.waveformHeight });
        if (typeof settings?.vectorscopeGain === 'number') setEditor({ vectorscopeGain: settings.vectorscopeGain });
        if (Array.isArray(settings?.scopeHeights)) setEditor({ scopeHeights: settings.scopeHeights });
        if (typeof settings?.historyStepLimit === 'number')
          setEditor({ historyStepLimit: usableStepLimit(settings.historyStepLimit) });

        setLibraryViewMode(settings?.libraryViewMode ?? defaultLibraryViewMode);
        // A settings file written before the slider carries a name rather than
        // a number, so it is read once and then only the number is kept.
        const savedThumbnailPx =
          typeof settings?.thumbnailSizePx === 'number'
            ? settings.thumbnailSizePx
            : LEGACY_THUMBNAIL_SIZES[settings?.thumbnailSize as string];
        setThumbnailSize(
          savedThumbnailPx
            ? Math.min(THUMBNAIL_SIZE_MAX, Math.max(THUMBNAIL_SIZE_MIN, savedThumbnailPx))
            : defaultThumbnailSize,
        );
        if (settings?.thumbnailAspectRatio) setThumbnailAspectRatio(settings.thumbnailAspectRatio);

        if (settings?.pinnedFolders && settings.pinnedFolders.length > 0) {
          try {
            const trees = await invoke(Invokes.GetPinnedFolderTrees, {
              paths: settings.pinnedFolders,
              expandedFolders: settings.lastFolderState?.expandedFolders || [],
              showImageCounts: settings.enableFolderImageCounts || settings.folderTreeSort?.key === 'imageCount',
            });
            setLibrary({ pinnedFolderTrees: trees });
          } catch (err) {
            console.error('Failed to load pinned folder trees:', err);
          }
        }

        const rootFolders = settings.rootFolders?.length
          ? settings.rootFolders
          : settings.lastRootPath
            ? [settings.lastRootPath]
            : [];

        if (!isAndroid && rootFolders.length > 0) {
          const currentPath = settings.lastFolderState?.currentFolderPath || rootFolders[0];
          const isAlbum = currentPath.startsWith('Album: ');
          const command =
            settings.libraryViewMode === LibraryViewMode.Recursive
              ? Invokes.ListImagesRecursive
              : Invokes.ListImagesInDir;

          preloadedDataRef.current = {
            rootPaths: rootFolders,
            currentPath: currentPath,
            trees: invoke(Invokes.GetPinnedFolderTrees, {
              paths: rootFolders,
              expandedFolders: settings.lastFolderState?.expandedFolders ?? rootFolders,
              showImageCounts: settings.enableFolderImageCounts || settings.folderTreeSort?.key === 'imageCount',
            }),
            images: isAlbum ? undefined : invoke(command, { path: currentPath }),
          };
        }

        if (settings?.lastFolderState) {
          setLibrary({
            expandedFolders: new Set(settings.lastFolderState.expandedFolders || []),
            expandedAlbumGroups: new Set(settings.lastFolderState.expandedAlbumGroups || []),
          });
        }

        invoke('frontend_ready')
          .then((launch: any) => {
            if (launch?.editSession) {
              useProcessStore.getState().setProcess({ externalEditSession: launch.editSession });
            } else if (launch?.openWithFile) {
              useProcessStore.getState().setProcess({ initialFileToOpen: launch.openWithFile });
            }
          })
          .catch((e) => console.error('Failed to notify backend of readiness:', e));
      })
      .catch((err) => {
        console.error('Failed to load settings:', err);
        setAppSettings({
          lastRootPath: null,
          theme: DEFAULT_THEME_ID as Theme,
          thumbnailSizePx: defaultThumbnailSize,
          libraryViewMode: defaultLibraryViewMode,
        });
      })
      .finally(() => {
        isInitialMount.current = false;
      });
  }, [
    isAndroid,
    setAppSettings,
    setTheme,
    setUI,
    defaultLibraryViewMode,
    defaultThumbnailSize,
    setSortCriteria,
    setFilterCriteria,
    setEditor,
    setLibrary,
    preloadedDataRef,
    setLibraryViewMode,
    setThumbnailSize,
    setThumbnailAspectRatio,
  ]);

  useEffect(() => {
    if (isInitialMount.current || !appSettings) return;

    const currentWorkspaceStr = JSON.stringify(appSettings.workspace || {});
    const newWorkspaceStr = JSON.stringify(workspaceProps);

    if (currentWorkspaceStr !== newWorkspaceStr) {
      const timeoutId = setTimeout(() => {
        handleSettingsChange({ ...appSettings, workspace: workspaceProps });
      }, 500);

      return () => clearTimeout(timeoutId);
    }
  }, [workspaceProps, appSettings, handleSettingsChange]);

  useEffect(() => {
    if (isInitialMount.current || !appSettings) return;
    if (JSON.stringify(appSettings.uiVisibility) !== JSON.stringify(uiVisibility)) {
      handleSettingsChange({ ...appSettings, uiVisibility });
    }
  }, [uiVisibility, appSettings, handleSettingsChange]);

  useEffect(() => {
    if (isInitialMount.current || !appSettings) return;
    if (appSettings.thumbnailSizePx !== thumbnailSize) {
      handleSettingsChange({ ...appSettings, thumbnailSizePx: thumbnailSize });
    }
  }, [thumbnailSize, appSettings, handleSettingsChange]);

  useEffect(() => {
    if (isInitialMount.current || !appSettings) return;
    if (appSettings.thumbnailAspectRatio !== thumbnailAspectRatio) {
      handleSettingsChange({ ...appSettings, thumbnailAspectRatio });
    }
  }, [thumbnailAspectRatio, appSettings, handleSettingsChange]);

  useEffect(() => {
    if (isInitialMount.current || !appSettings) return;
    if (appSettings.libraryViewMode !== libraryViewMode) {
      handleSettingsChange({ ...appSettings, libraryViewMode });
    }
  }, [libraryViewMode, appSettings, handleSettingsChange]);

  useEffect(() => {
    if (isInitialMount.current || !appSettings) return;
    if (JSON.stringify(appSettings.sortCriteria) !== JSON.stringify(sortCriteria)) {
      handleSettingsChange({ ...appSettings, sortCriteria });
    }
  }, [sortCriteria, appSettings, handleSettingsChange]);

  useEffect(() => {
    if (isInitialMount.current || !appSettings) return;
    if (JSON.stringify(appSettings.filterCriteria) !== JSON.stringify(filterCriteria)) {
      handleSettingsChange({ ...appSettings, filterCriteria });
    }
  }, [filterCriteria, appSettings, handleSettingsChange]);

  useEffect(() => {
    if (isInitialMount.current || !appSettings) return;
    if (appSettings.language && appSettings.language !== i18n.language) {
      i18n.changeLanguage(appSettings.language);
    }
  }, [appSettings?.language, i18n.language]);

  useEffect(() => {
    if (isInitialMount.current || !appSettings) return;
    if (!currentFolderPath && !activeAlbumId) return;

    const currentExpanded = Array.from(expandedFolders);
    const currentExpandedAlbums = Array.from(expandedAlbumGroups);

    const prevFolderState = appSettings.lastFolderState || {
      currentFolderPath: null,
      expandedFolders: [],
      activeAlbumId: null,
      expandedAlbumGroups: [],
    };

    const pathChanged = prevFolderState.currentFolderPath !== currentFolderPath;
    const expandedChanged = JSON.stringify(prevFolderState.expandedFolders || []) !== JSON.stringify(currentExpanded);
    const albumChanged = prevFolderState.activeAlbumId !== activeAlbumId;
    const albumExpandedChanged =
      JSON.stringify(prevFolderState.expandedAlbumGroups || []) !== JSON.stringify(currentExpandedAlbums);

    if (pathChanged || expandedChanged || albumChanged || albumExpandedChanged) {
      handleSettingsChange({
        ...appSettings,
        lastFolderState: {
          currentFolderPath,
          expandedFolders: currentExpanded,
          activeAlbumId,
          expandedAlbumGroups: currentExpandedAlbums,
          // BLITZRAW: carried through, because this write replaces the whole
          // object and would otherwise drop the remembered photo every time a
          // folder was opened or a tree branch was twirled.
          activePath:
            useLibraryStore.getState().libraryActivePath ?? (prevFolderState as any).activePath ?? null,
        },
      });
    }
  }, [currentFolderPath, expandedFolders, activeAlbumId, expandedAlbumGroups, appSettings, handleSettingsChange]);

  // ============ BLITZRAW: the photo I was on when I closed the app ============
  // Kept apart from the folder write above, and put off for a moment, because
  // walking a shoot with the arrow keys changes the current photo many times a
  // second and the settings file is rewritten whole on every save. Writing on
  // each press would put the entire file through the disk hundreds of times in
  // one cull.
  //
  // The write reads the newest settings rather than the ones this effect closed
  // over. Both this and the folder write touch `lastFolderState`, and either
  // can be the later of the two.
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | null = null;

    const write = (path: string) => {
      const { appSettings: latest, handleSettingsChange: save } = useSettingsStore.getState();
      if (!latest) return;
      if ((latest.lastFolderState?.activePath ?? null) === path) return;
      save({
        ...latest,
        lastFolderState: {
          ...(latest.lastFolderState || {}),
          activePath: path,
        },
      } as any);
    };

    const unsubscribe = useLibraryStore.subscribe((state, prev) => {
      if (state.libraryActivePath === prev.libraryActivePath) return;
      const path = state.libraryActivePath;
      // Leaving a folder clears the current photo before the next folder has
      // one. Forgetting on that would mean every folder change threw away the
      // answer, and reopening the app would land at the top of the list.
      if (!path) return;
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        write(path);
      }, 1500);
    });

    return () => {
      if (timer) clearTimeout(timer);
      unsubscribe();
    };
  }, []);
  // ========== BLITZRAW END: the photo I was on when I closed the app ==========

  useEffect(() => {
    if (!appSettings) return;

    const needsImageCounts = Boolean(
      appSettings.enableFolderImageCounts || appSettings.folderTreeSort?.key === 'imageCount',
    );

    if (prevImageCountsNeed.current === undefined) {
      prevImageCountsNeed.current = needsImageCounts;
      return;
    }

    if (prevImageCountsNeed.current !== needsImageCounts) {
      prevImageCountsNeed.current = needsImageCounts;

      const rootFolders = appSettings.rootFolders?.length
        ? appSettings.rootFolders
        : appSettings.lastRootPath
          ? [appSettings.lastRootPath]
          : [];
      const pinnedFolders = appSettings.pinnedFolders || [];

      const currentExpanded = Array.from(useLibraryStore.getState().expandedFolders);

      setLibrary({ isTreeLoading: true });

      const promises = [];

      if (pinnedFolders.length > 0) {
        promises.push(
          invoke(Invokes.GetPinnedFolderTrees, {
            paths: pinnedFolders,
            expandedFolders: currentExpanded,
            showImageCounts: needsImageCounts,
          }).then((trees: any) => ({ type: 'pinned', trees })),
        );
      }

      if (rootFolders.length > 0) {
        promises.push(
          invoke(Invokes.GetPinnedFolderTrees, {
            paths: rootFolders,
            expandedFolders: currentExpanded,
            showImageCounts: needsImageCounts,
          }).then((trees: any) => ({ type: 'root', trees })),
        );
      }

      Promise.all(promises)
        .then((results) => {
          useLibraryStore.getState().setLibrary((_state) => {
            const updates: any = { isTreeLoading: false };
            results.forEach((res) => {
              if (res.type === 'pinned') updates.pinnedFolderTrees = res.trees;
              if (res.type === 'root') updates.folderTrees = res.trees;
            });
            return updates;
          });
        })
        .catch((err) => {
          console.error('Failed to re-fetch trees for image counts:', err);
          setLibrary({ isTreeLoading: false });
        });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [appSettings?.enableFolderImageCounts, appSettings?.folderTreeSort?.key]);

  // The body of this moved to `applyTheme`, unchanged, so that a detached
  // panel window can paint itself with the same one call. See themes.ts.
  useEffect(() => {
    applyTheme(theme, appSettings?.fontFamily || 'poppins');
  }, [theme, appSettings?.fontFamily]);
};
