import { useCallback, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
  Album as AlbumIcon,
  Aperture,
  Briefcase,
  Camera,
  Car,
  Check,
  ChevronDown,
  ChevronUp,
  ChevronsDownUp,
  ChevronsUpDown,
  ClipboardPaste,
  Combine,
  Copy,
  CopyPlus,
  Edit,
  FileDown,
  FileEdit,
  FileInput,
  Film,
  Folder,
  FolderInput,
  FolderPlus,
  Gauge,
  Grip,
  Group,
  Heart,
  Home,
  ImageDown,
  ImageOff,
  Images,
  Layers,
  LayoutTemplate,
  Map,
  Mountain,
  Palette,
  Pin,
  PinOff,
  Plane,
  Redo,
  RefreshCw,
  RotateCcw,
  SquaresUnite,
  Star,
  Sun,
  Tag,
  Trash2,
  Undo,
  Ungroup,
  User,
  Users,
  X,
  Zap,
} from 'lucide-react';
import { toast } from 'react-toastify';
import { useTranslation } from 'react-i18next';
import { useContextMenu } from '../context/ContextMenuContext';
import { useDngConversion } from './useDngConversion';
import { useBulkHdr } from './useBulkHdr';
import { stackIdsForSelection, stacksForSelection, BOTH, EDIT_RULE } from '../utils/imageStacking';
import { selectionFor } from '../utils/selection';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useProcessStore } from '../store/useProcessStore';
import { useUIStore } from '../store/useUIStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { Invokes, Option, OPTION_SEPARATOR, Panel, AlbumItem, Album, AlbumGroup } from '../components/ui/AppProperties';
import { Color, COLOR_LABELS, INITIAL_ADJUSTMENTS, normalizeLoadedAdjustments } from '../utils/adjustments';
import TaggingSubMenu from '../context/TaggingSubMenu';
import { useEditorActions } from './useEditorActions';
import { useLibraryActions } from './useLibraryActions';
import { globalImageCache } from '../utils/ImageLRUCache';
import { useAppUndo } from './useAppUndo';
import { actionForChange } from '../utils/currentAction';
import { recordAppAction, recordingFinished, recordingStarted } from '../utils/appHistory';
import { redoTarget, undoTarget } from '../utils/appHistory';

export interface UseAppContextMenusProps {
  handleImageSelect: (path: string) => void;
  handleBackToLibrary: () => void;
  handleRenameFiles: (paths: string[]) => void;
  handleImportClick: (path: string) => void;
  handleLibraryRefresh: () => Promise<void>;
  refreshAllFolderTrees: () => Promise<void>;
  refreshImageList: () => Promise<void>;
  executeDelete: (paths: string[], options: any) => Promise<void>;
  handleTogglePinFolder: (path: string) => Promise<void>;
  /** BLITZRAW: so Undo here means the same as Ctrl+Z. See useAppUndo. */
  prevAdjustmentsRef: React.RefObject<any>;
}

export function useAppContextMenus(props: UseAppContextMenusProps) {
  const { t } = useTranslation();
  // BLITZRAW: one undo, wherever it is pressed from. This menu used to walk the
  // open photo's own history while Ctrl+Z walked the list of what was done, so
  // the same word meant two different things.
  const { walkAction } = useAppUndo(props.handleBackToLibrary, props.prevAdjustmentsRef);
  const { showContextMenu } = useContextMenu();
  const { convertPaths: convertPathsToDng } = useDngConversion(props.handleLibraryRefresh);
  const { mergeStacks } = useBulkHdr(props.handleLibraryRefresh);

  const { handleAutoAdjustments, handleResetAdjustments, handleCopyAdjustments, handlePasteAdjustments } =
    useEditorActions();
  const { handleRate, handleAdjustRating, handleSetColorLabel, handleTagsChanged } = useLibraryActions();

  const albumIcons = useMemo(
    () => [
      { label: t('contextMenus.albumIcons.default'), value: undefined, icon: Folder },
      { label: t('contextMenus.albumIcons.travel'), value: 'plane', icon: Plane },
      { label: t('contextMenus.albumIcons.nature'), value: 'mountain', icon: Mountain },
      { label: t('contextMenus.albumIcons.summer'), value: 'sun', icon: Sun },
      { label: t('contextMenus.albumIcons.photography'), value: 'camera', icon: Camera },
      { label: t('contextMenus.albumIcons.locations'), value: 'map', icon: Map },
      { label: t('contextMenus.albumIcons.favorites'), value: 'heart', icon: Heart },
      { label: t('contextMenus.albumIcons.featured'), value: 'star', icon: Star },
      { label: t('contextMenus.albumIcons.people'), value: 'users', icon: Users },
      { label: t('contextMenus.albumIcons.person'), value: 'user', icon: User },
      { label: t('contextMenus.albumIcons.automotive'), value: 'car', icon: Car },
      { label: t('contextMenus.albumIcons.portfolio'), value: 'briefcase', icon: Briefcase },
    ],
    [t],
  );

  const getCommonTags = useCallback((paths: string[]): { tag: string; isUser: boolean }[] => {
    const { imageList } = useLibraryStore.getState();
    if (paths.length === 0) return [];
    const imageFiles = imageList.filter((img) => paths.includes(img.path));
    if (imageFiles.length === 0) return [];

    const allTagsSets = imageFiles.map((img) => {
      const tagsWithPrefix = (img.tags || []).filter((t: string) => !t.startsWith('color:'));
      return new Set(tagsWithPrefix);
    });

    if (allTagsSets.length === 0) return [];

    const commonTagsWithPrefix = allTagsSets.reduce((intersection, currentSet) => {
      return new Set([...intersection].filter((tag) => currentSet.has(tag)));
    });

    return Array.from(commonTagsWithPrefix)
      .map((tag: string) => ({
        tag: tag.startsWith('user:') ? tag.substring(5) : tag,
        isUser: tag.startsWith('user:'),
      }))
      .sort((a, b) => a.tag.localeCompare(b.tag));
  }, []);

  const buildAddToAlbumMenu = useCallback(
    (items: AlbumItem[], pathsToAdd: string[]): Option[] => {
      return items.map((item) => {
        const customIconDef = item.icon ? albumIcons.find((i) => i.value === item.icon) : null;
        const ResolvedIcon = customIconDef?.icon || (item.type === 'group' ? Folder : AlbumIcon);

        if (item.type === 'group') {
          return {
            label: item.name,
            icon: ResolvedIcon,
            submenu:
              (item as AlbumGroup).children.length > 0
                ? buildAddToAlbumMenu((item as AlbumGroup).children, pathsToAdd)
                : [{ label: t('contextMenus.album.emptyGroup'), disabled: true }],
          };
        } else {
          return {
            label: item.name,
            icon: ResolvedIcon,
            onClick: () => {
              invoke(Invokes.AddToAlbum, { albumId: item.id, paths: pathsToAdd })
                .then(() => {
                  console.log(`Added image(s) to ${item.name}`);
                  invoke(Invokes.GetAlbums).then((res: any) =>
                    useLibraryStore.getState().setLibrary({ albumTree: res }),
                  );
                })
                .catch((err) => toast.error(t('contextMenus.toasts.failedAddToAlbum', { err })));
            },
          };
        }
      });
    },
    [albumIcons, t],
  );

  const handleEditorContextMenu = useCallback(
    (event: any) => {
      event.preventDefault();
      event.stopPropagation();

      const { selectedImage, copiedAdjustments, setEditor } =
        useEditorStore.getState();
      const { appSettings } = useSettingsStore.getState();
      const { setPanel, setUI } = useUIStore.getState();

      if (!selectedImage) return;

      // BLITZRAW: what the list of what I did offers, not what this one photo
      // offers. The menu used to be greyed out on a photo with no history of its
      // own even when there was plenty to undo.
      const canUndo = undoTarget() !== null;
      const canRedo = redoTarget() !== null;
      const commonTags = getCommonTags([selectedImage.path]);

      const options: Array<Option> = [
        {
          label: t('contextMenus.editor.exportImage'),
          icon: FileInput,
          onClick: () => setPanel(Panel.Export),
        },
        { type: OPTION_SEPARATOR },
        {
          label: t('contextMenus.editor.undo'),
          icon: Undo,
          onClick: () => void walkAction(false),
          disabled: !canUndo,
        },
        {
          label: t('contextMenus.editor.redo'),
          icon: Redo,
          onClick: () => void walkAction(true),
          disabled: !canRedo,
        },
        { type: OPTION_SEPARATOR },
        {
          label: t('contextMenus.editor.copyAdjustments'),
          icon: Copy,
          onClick: () => handleCopyAdjustments(),
        },
        {
          label: t('contextMenus.editor.pasteAdjustments'),
          icon: ClipboardPaste,
          onClick: () => handlePasteAdjustments(),
          disabled: copiedAdjustments === null,
        },
        {
          label: t('contextMenus.editor.productivity'),
          icon: Gauge,
          submenu: [
            {
              label: t('contextMenus.editor.autoAdjust'),
              icon: Aperture,
              onClick: handleAutoAdjustments,
              disabled: !selectedImage?.isReady,
            },
            {
              label: t('contextMenus.editor.denoise'),
              icon: Grip,
              onClick: () => {
                setUI({
                  denoiseModalState: {
                    isOpen: true,
                    isProcessing: false,
                    previewBase64: null,
                    error: null,
                    targetPaths: [selectedImage.path],
                    progressMessage: null,
                    isRaw: selectedImage?.isRaw || false,
                  },
                });
              },
            },
            {
              label: t('contextMenus.editor.convertNegative'),
              icon: Film,
              onClick: () => {
                if (selectedImage) {
                  setUI({ negativeModalState: { isOpen: true, targetPaths: [selectedImage.path] } });
                }
              },
            },
            { disabled: true, icon: SquaresUnite, label: t('contextMenus.editor.stitchPanorama') },
            { disabled: true, icon: Images, label: t('contextMenus.editor.mergeHdr') },
            {
              icon: LayoutTemplate,
              label: t('contextMenus.editor.frameImage'),
              onClick: () => {
                setUI({ collageModalState: { isOpen: true, sourceImages: [selectedImage] } });
              },
            },
            { label: t('contextMenus.editor.cullImage'), icon: Users, disabled: true },
          ],
        },
        { type: OPTION_SEPARATOR },
        {
          label: t('contextMenus.editor.rating'),
          icon: Star,
          submenu: [
            ...[0, 1, 2, 3, 4, 5].map((rating: number) => ({
              label:
                rating === 0
                  ? t('contextMenus.editor.noRating')
                  : t('contextMenus.editor.ratingLabel', { count: rating }),
              onClick: () => handleRate(rating),
            })),
            // BLITZRAW: relative, so a mixed selection keeps its order instead
            // of being flattened to one number. See handleAdjustRating.
            { type: OPTION_SEPARATOR },
            {
              label: t('contextMenus.editor.increaseRating'),
              icon: ChevronUp,
              onClick: () => handleAdjustRating(1),
            },
            {
              label: t('contextMenus.editor.decreaseRating'),
              icon: ChevronDown,
              onClick: () => handleAdjustRating(-1),
            },
          ],
        },
        {
          label: t('contextMenus.editor.colorLabel'),
          icon: Palette,
          submenu: [
            { label: t('contextMenus.editor.noLabel'), onClick: () => handleSetColorLabel(null) },
            ...COLOR_LABELS.map((label: Color) => ({
              label: t(`contextMenus.colors.${label.name}`),
              color: label.color,
              onClick: () => handleSetColorLabel(label.name),
            })),
          ],
        },
        {
          label: t('contextMenus.editor.tagging'),
          icon: Tag,
          submenu: [
            {
              customComponent: TaggingSubMenu,
              customProps: {
                paths: [selectedImage.path],
                initialTags: commonTags,
                onTagsChanged: handleTagsChanged,
                appSettings,
              },
            },
          ],
        },
        { type: OPTION_SEPARATOR },
        {
          label: t('contextMenus.editor.resetAdjustments'),
          icon: RotateCcw,
          submenu: [
            { label: t('contextMenus.editor.cancel'), icon: X, onClick: () => {} },
            {
              label: t('contextMenus.editor.confirmReset'),
              icon: Check,
              isDestructive: true,
              onClick: () => {
                const originalAspectRatio =
                  selectedImage.width && selectedImage.height ? selectedImage.width / selectedImage.height : null;
                // A step, not a fresh start. Resetting a photo is a thing you
                // did to it, and being able to walk back out of it is the
                // point of having a history at all.
                const reset = { ...INITIAL_ADJUSTMENTS, aspectRatio: originalAspectRatio, aiPatches: [] };
                useEditorStore.getState().pushHistory(reset, 'Reset');
                setEditor({ adjustments: reset });
              },
            },
          ],
        },
      ];
      showContextMenu(event.clientX, event.clientY, options);
    },
    [
      getCommonTags,
      handleCopyAdjustments,
      handlePasteAdjustments,
      handleAutoAdjustments,
      handleRate,
      handleAdjustRating,
      handleSetColorLabel,
      handleTagsChanged,
      showContextMenu,
      t,
    ],
  );

  const handleThumbnailContextMenu = useCallback(
    (event: any, path: string, forceSingleSelection: boolean = false) => {
      event.preventDefault();
      event.stopPropagation();

      const { selectedImage, copiedAdjustments, setEditor } = useEditorStore.getState();
      const {
        multiSelectedPaths,
        imageList,
        libraryActivePath,
        albumTree,
        activeAlbumId,
        expandedStacks,
        setLibrary,
      } = useLibraryStore.getState();
      const { appSettings } = useSettingsStore.getState();
      const { activeView, setUI, setPanel } = useUIStore.getState();
      const { setProcess } = useProcessStore.getState();

      const isTargetInSelection = multiSelectedPaths.includes(path);
      let finalSelection: string[];

      if (forceSingleSelection) {
        finalSelection = [path];
      } else if (!isTargetInSelection) {
        finalSelection = [path];
        setLibrary({ multiSelectedPaths: [path] });
        if (!selectedImage) {
          setLibrary({ libraryActivePath: path });
        }
      } else {
        finalSelection = multiSelectedPaths;
      }

      // Three views of the same click, because a collapsed stack means
      // different things to different verbs. See "BlitzRaw — Stack Selection".
      //
      //   edit     a merged result stays alone, a peer stack opens up
      //   whole    every frame, whichever kind of stack it is
      //   leader   one file per stack, whichever kind
      //
      // finalSelection itself is never rewritten: it is still what was clicked,
      // and anything comparing against it keeps working.
      const editSelection = selectionFor(EDIT_RULE, finalSelection);
      const wholeSelection = selectionFor(BOTH('fullStack'), finalSelection);
      const leaderSelection = selectionFor(BOTH('leaderOnly'), finalSelection);

      const commonTags = getCommonTags(finalSelection);
      const selectedStacks = stacksForSelection(imageList, finalSelection);

      // Stacking verbs act on the stack rather than on the photographs, so they
      // count open stacks too and take every frame of the ones they touch.
      const touchedStackIds = stackIdsForSelection(imageList, finalSelection);
      const openStacks = touchedStackIds.filter((id) => (expandedStacks ?? []).includes(id));
      const closedStacks = touchedStackIds.filter((id) => !(expandedStacks ?? []).includes(id));

      const selectionCount = finalSelection.length;
      const isSingleSelection = selectionCount === 1;
      const isEditingThisImage = activeView === 'editor' && selectedImage?.path === path;
      const deleteLabel = t('contextMenus.thumbnail.deleteImage', { count: selectionCount });
      const exportLabel = t('contextMenus.thumbnail.exportImage', { count: selectionCount });

      const selectionHasVirtualCopies =
        isSingleSelection &&
        !finalSelection[0].includes('?vc=') &&
        imageList.some((image) => image.path.startsWith(`${finalSelection[0]}?vc=`));

      const hasAssociatedFiles = finalSelection.some((selectedPath) => {
        const image = imageList.find((img) => img.path === selectedPath);
        if (image?.group_id != null) return true;

        const getBasePath = (p: string) => {
          const qMark = p.indexOf('?');
          const clean = qMark === -1 ? p : p.substring(0, qMark);
          const dot = clean.lastIndexOf('.');
          return dot === -1 ? clean : clean.substring(0, dot);
        };
        const basePath = getBasePath(selectedPath);

        return imageList.some((img) => img.path !== selectedPath && getBasePath(img.path) === basePath);
      });

      let deleteSubmenu;
      if (selectionHasVirtualCopies) {
        deleteSubmenu = [
          { label: t('contextMenus.editor.cancel'), icon: X, onClick: () => {} },
          {
            label: t('contextMenus.thumbnail.confirmDeleteVc'),
            icon: Check,
            isDestructive: true,
            onClick: () => props.executeDelete(wholeSelection, { includeAssociated: false }),
          },
        ];
      } else if (hasAssociatedFiles) {
        deleteSubmenu = [
          { label: t('contextMenus.editor.cancel'), icon: X, onClick: () => {} },
          {
            label: t('contextMenus.thumbnail.deleteSelected'),
            icon: Check,
            isDestructive: true,
            onClick: () => props.executeDelete(wholeSelection, { includeAssociated: false }),
          },
          {
            label: t('contextMenus.thumbnail.deleteAssociated'),
            icon: Check,
            isDestructive: true,
            onClick: () => props.executeDelete(wholeSelection, { includeAssociated: true }),
          },
        ];
      } else {
        deleteSubmenu = [
          { label: t('contextMenus.editor.cancel'), icon: X, onClick: () => {} },
          {
            label: t('contextMenus.thumbnail.confirmDelete'),
            icon: Check,
            isDestructive: true,
            onClick: () => props.executeDelete(wholeSelection, { includeAssociated: false }),
          },
        ];
      }

      const pasteLabel = t('contextMenus.thumbnail.pasteAdjustments', { count: selectionCount });
      const resetLabel = t('contextMenus.thumbnail.resetAdjustments', { count: selectionCount });
      const copyLabel = t('contextMenus.thumbnail.copyImage', { count: selectionCount });
      const autoAdjustLabel = t('contextMenus.thumbnail.autoAdjust', { count: selectionCount });
      const renameLabel = t('contextMenus.thumbnail.renameImage', { count: selectionCount });
      const cullLabel = t('contextMenus.thumbnail.cullImage', { count: selectionCount });
      const collageLabel = t('contextMenus.thumbnail.collage', { count: selectionCount });
      const stitchLabel = t('contextMenus.editor.stitchPanorama');
      const conversionLabel = t('contextMenus.thumbnail.convertNegative', { count: selectionCount });
      const denoiseLabel = t('contextMenus.thumbnail.denoise', { count: selectionCount });
      const mergeLabel = t('contextMenus.editor.mergeHdr');
      const openStacksLabel = t('contextMenus.thumbnail.openStacks', { count: closedStacks.length });
      const closeStacksLabel = t('contextMenus.thumbnail.closeStacks', { count: openStacks.length });
      const stackLabel = t('contextMenus.thumbnail.stackImages', { count: wholeSelection.length });
      const unstackLabel = t('contextMenus.thumbnail.unstack', { count: touchedStackIds.length });

      // Both copy actions used to take finalSelection[0] and act on one file.
      // A peer stack copies whole, so they loop now; a merged result still
      // copies alone, which the edit rule already resolves to a single path.
      const handleCreateVirtualCopy = async (sourcePaths: Array<string>) => {
        try {
          for (const sourcePath of sourcePaths) {
            await invoke(Invokes.CreateVirtualCopy, {
              sourceVirtualPath: sourcePath,
              targetAlbumId: activeAlbumId || null,
            });
          }

          if (activeAlbumId) {
            const sortedTree = await invoke<AlbumItem[]>(Invokes.GetAlbums);
            setLibrary({ albumTree: sortedTree });
          }
          await props.refreshImageList();
        } catch (err) {
          toast.error(t('contextMenus.toasts.failedCreateVirtualCopy', { err }));
        }
      };

      const handleApplyAutoAdjustmentsToSelection = () => {
        if (editSelection.length === 0) return;
        editSelection.forEach((p) => globalImageCache.delete(p));

        // BLITZRAW: one deliberate event, so it opens an action of its own that
        // nothing joins. The hard rule: anything that writes a step into a photo
        // writes an entry here.
        const autoAction = actionForChange(['auto'], true);
        recordingStarted();
        invoke(Invokes.ApplyAutoAdjustmentsToPaths, {
          paths: editSelection,
          skipHistoryFor: selectedImage?.path ?? null,
        })
          .then(async (photos: any) => {
            recordAppAction({
              id: autoAction,
              kind: 'adjustments',
              label: 'Auto adjustments',
              photos: photos ?? [],
              selection: editSelection,
              openPath: selectedImage?.path ?? null,
              inEditor: !!selectedImage,
            });
            if (selectedImage && finalSelection.includes(selectedImage.path)) {
              const metadata: any = await invoke(Invokes.LoadMetadata, { path: selectedImage.path });
              if (metadata.adjustments && !metadata.adjustments.is_null) {
                const normalized = normalizeLoadedAdjustments(metadata.adjustments);
                setEditor({ adjustments: normalized });
                // Something was applied across the selection and this photo is
                // one of them. A step, so trying a preset and disliking it
                // leaves the way back intact.
                useEditorStore.getState().pushHistory(normalized, 'Auto adjustments');
              }
            }
            if (libraryActivePath && finalSelection.includes(libraryActivePath)) {
              const metadata: any = await invoke(Invokes.LoadMetadata, { path: libraryActivePath });
              if (metadata.adjustments && !metadata.adjustments.is_null) {
                const normalized = normalizeLoadedAdjustments(metadata.adjustments);
                setLibrary({ libraryActiveAdjustments: normalized });
              }
            }
          })
          .catch((err) => {
            console.error('Failed to apply auto adjustments to paths:', err);
            toast.error(t('contextMenus.toasts.failedApplyAuto', { err }));
          })
          .finally(recordingFinished);
      };

      const onExportClick = () => {
        if (selectedImage) {
          if (selectedImage.path !== path) {
            props.handleImageSelect(path);
          }
          setLibrary({ multiSelectedPaths: finalSelection });
          setPanel(Panel.Export);
        } else {
          setLibrary({ multiSelectedPaths: finalSelection });
          setUI({ isLibraryExportPanelVisible: true });
        }
      };

      const handleRemoveFromAlbum = async () => {
        if (!activeAlbumId) return;
        const newTree = JSON.parse(JSON.stringify(albumTree));

        const removeImages = (nodes: AlbumItem[]): boolean => {
          for (const n of nodes) {
            if (n.id === activeAlbumId && n.type === 'album') {
              (n as Album).images = (n as Album).images.filter((p) => !leaderSelection.includes(p));
              return true;
            } else if (n.type === 'group') {
              if (removeImages(n.children)) return true;
            }
          }
          return false;
        };

        if (removeImages(newTree)) {
          try {
            await invoke(Invokes.SaveAlbums, { tree: newTree });
            const sortedTree = await invoke<AlbumItem[]>(Invokes.GetAlbums);
            setLibrary({ albumTree: sortedTree });

            const albumObj = sortedTree.reduce((acc: any, cur: any) => {
              const find = (n: any): any =>
                n.id === activeAlbumId
                  ? n
                  : n.type === 'group'
                    ? n.children.reduce((a: any, c: any) => a || find(c), null)
                    : null;
              return acc || find(cur);
            }, null) as Album;

            if (albumObj) {
              setLibrary({ imageList: imageList.filter((i) => albumObj.images.includes(i.path)) });
            }
          } catch (e) {
            toast.error(t('contextMenus.toasts.failedRemoveImages', { err: e }));
          }
        }
      };

      const options = [
        ...(!isEditingThisImage
          ? [
              {
                disabled: !isSingleSelection,
                icon: Edit,
                label: t('contextMenus.editor.editImage'),
                onClick: () => props.handleImageSelect(finalSelection[0]),
              },
              { icon: FileInput, label: exportLabel, onClick: onExportClick },
              { type: OPTION_SEPARATOR },
            ]
          : [{ icon: FileInput, label: exportLabel, onClick: onExportClick }, { type: OPTION_SEPARATOR }]),
        {
          disabled: !isSingleSelection,
          icon: Copy,
          label: t('contextMenus.editor.copyAdjustments'),
          onClick: () => handleCopyAdjustments(finalSelection[0]),
        },
        {
          disabled: copiedAdjustments === null,
          icon: ClipboardPaste,
          label: pasteLabel,
          onClick: () => handlePasteAdjustments(editSelection),
        },
        // BLITZRAW: three submenus where there was one. Productivity had grown
        // to twelve entries covering three unrelated jobs, so stacking and
        // merging moved out to their own. See "BlitzRaw — Stack Selection" for
        // which selection each verb reads.
        //
        // Opening, closing, stacking and unstacking act on the stack itself
        // rather than on the photographs in it, so they count open stacks as
        // well as closed ones and take every frame of the ones they touch.
        {
          label: t('contextMenus.thumbnail.stacksMenu'),
          icon: Layers,
          submenu: [
            {
              label: openStacksLabel,
              icon: ChevronsUpDown,
              disabled: closedStacks.length === 0,
              onClick: () =>
                setLibrary({ expandedStacks: [...(expandedStacks ?? []), ...closedStacks] }),
            },
            {
              label: closeStacksLabel,
              icon: ChevronsDownUp,
              disabled: openStacks.length === 0,
              onClick: () =>
                setLibrary({
                  expandedStacks: (expandedStacks ?? []).filter((id: string) => !openStacks.includes(id)),
                }),
            },
            { type: OPTION_SEPARATOR },
            {
              label: stackLabel,
              icon: Group,
              // Two frames is the smallest thing worth stacking. set_stacks
              // replaces any stack that already held one of these, so stacking
              // across existing stacks merges them rather than conflicting.
              disabled: wholeSelection.length < 2,
              onClick: async () => {
                try {
                  await invoke('set_stacks', { stacks: [wholeSelection] });
                  await props.refreshImageList();
                  toast.success(t('notifications.stacked', { count: wholeSelection.length }));
                } catch (err) {
                  toast.error(`${err}`);
                }
              },
            },
            {
              label: unstackLabel,
              icon: Ungroup,
              disabled: touchedStackIds.length === 0,
              onClick: async () => {
                try {
                  const removed = await invoke<number>('clear_stacks', { paths: wholeSelection });
                  await props.refreshImageList();
                  toast.success(t('notifications.unstacked', { count: removed }));
                } catch (err) {
                  toast.error(`${err}`);
                }
              },
            },
            { type: OPTION_SEPARATOR },
            {
              label: t('contextMenus.thumbnail.autoStack', { count: selectionCount }),
              icon: Layers,
              // A bracket is at least three frames, so a smaller selection has
              // nothing to find.
              disabled: selectionCount < 3,
              onClick: () => {
                setUI({
                  autoStackModalState: { isOpen: true, targetPaths: finalSelection, mode: 'brackets' },
                });
              },
            },
            {
              label: t('contextMenus.thumbnail.autoStackBursts'),
              icon: Zap,
              // Two frames is a burst, unlike a bracket which needs three.
              disabled: selectionCount < 2,
              onClick: () => {
                setUI({
                  autoStackModalState: { isOpen: true, targetPaths: finalSelection, mode: 'bursts' },
                });
              },
            },
          ],
        },
        {
          label: t('contextMenus.thumbnail.mergeMenu'),
          icon: Combine,
          submenu: [
            {
              label: t('contextMenus.thumbnail.mergeStackedHdr', { count: selectedStacks.length }),
              icon: Combine,
              // Selecting one collapsed frame is enough; its bracket comes with it.
              disabled: selectedStacks.length === 0,
              onClick: () => {
                // A long unattended run that writes a file per stack is worth
                // confirming once, with the actual counts, rather than starting
                // the moment the menu is clicked.
                const frames = selectedStacks.reduce((total, stack) => total + stack.paths.length, 0);
                const { appSettings, handleSettingsChange } = useSettingsStore.getState();

                // Alignment belongs on this dialog rather than in settings: it
                // is a per-shoot decision, tripod or handheld, and the answer is
                // remembered so a tripod shooter sets it once.
                const setAlign = (checked: boolean) => {
                  handleSettingsChange({ ...appSettings!, hdrAutoAlign: checked });
                  setUI((state: any) => ({
                    confirmModalState: {
                      ...state.confirmModalState,
                      checkbox: { ...state.confirmModalState.checkbox, checked },
                    },
                  }));
                };

                setUI({
                  confirmModalState: {
                    isOpen: true,
                    title: t('modals.bulkHdr.title'),
                    message: t('modals.bulkHdr.message', { stacks: selectedStacks.length, frames }),
                    confirmText: t('modals.bulkHdr.confirm'),
                    checkbox: {
                      label: t('modals.bulkHdr.autoAlign'),
                      checked: appSettings?.hdrAutoAlign ?? false,
                      onChange: setAlign,
                    },
                    onConfirm: () => mergeStacks(selectedStacks),
                  },
                });
              },
            },
            {
              disabled: selectionCount < 2 || selectionCount > 9,
              icon: Images,
              label: mergeLabel,
              onClick: () => {
                setUI({
                  hdrModalState: {
                    error: null,
                    finalImageBase64: null,
                    isOpen: true,
                    isProcessing: false,
                    progressMessage: null,
                    stitchingSourcePaths: editSelection,
                  },
                });
              },
            },
            {
              disabled: selectionCount < 2 || selectionCount > 30,
              icon: SquaresUnite,
              label: stitchLabel,
              onClick: () => {
                setUI({
                  panoramaModalState: {
                    error: null,
                    finalImageBase64: null,
                    isOpen: true,
                    isProcessing: false,
                    progressMessage: null,
                    stitchingSourcePaths: editSelection,
                  },
                });
              },
            },
            {
              icon: LayoutTemplate,
              label: collageLabel,
              onClick: () => {
                const imagesForCollage = imageList.filter((img) => editSelection.includes(img.path));
                setUI({ collageModalState: { isOpen: true, sourceImages: imagesForCollage } });
              },
              disabled: selectionCount === 0 || selectionCount > 9,
            },
          ],
        },
        {
          label: t('contextMenus.editor.productivity'),
          icon: Gauge,
          submenu: [
            { label: autoAdjustLabel, icon: Aperture, onClick: handleApplyAutoAdjustmentsToSelection },
            {
              label: denoiseLabel,
              icon: Grip,
              disabled: finalSelection.length === 0,
              onClick: () => {
                setUI({
                  denoiseModalState: {
                    isOpen: true,
                    isProcessing: false,
                    previewBase64: null,
                    error: null,
                    targetPaths: editSelection,
                    progressMessage: null,
                    isRaw: selectedImage?.isRaw || false,
                  },
                });
              },
            },
            {
              label: conversionLabel,
              icon: Film,
              disabled: selectionCount === 0,
              onClick: () => {
                setUI({ negativeModalState: { isOpen: true, targetPaths: editSelection } });
              },
            },
            {
              label: t('contextMenus.thumbnail.convertToDng', { count: selectionCount }),
              icon: FileDown,
              disabled: selectionCount === 0,
              onClick: () => convertPathsToDng(editSelection),
            },
            // BLITZRAW: previews on disk, so opening a photo large does not
            // wait on a decode it has already paid for once. Same rule as the
            // rest of this submenu: a merged stack keeps its result, a peer
            // stack opens up, since a bracket's frames are working files and a
            // burst's frames are photographs.
            {
              label: t('contextMenus.thumbnail.buildPreviews', { count: selectionCount }),
              icon: ImageDown,
              disabled: selectionCount === 0,
              onClick: () => {
                invoke(Invokes.BuildPreviewsForPaths, { paths: editSelection })
                  .then((result: any) => {
                    if (result.failed > 0) {
                      toast.warning(
                        t('notifications.previewsFailed', {
                          failed: result.failed,
                          reason: result.first_error ?? '',
                        }),
                      );
                      return;
                    }
                    toast.success(
                      result.skipped > 0
                        ? t('notifications.previewsBuiltWithSkips', {
                            built: result.built,
                            skipped: result.skipped,
                          })
                        : t('notifications.previewsBuilt', { built: result.built }),
                    );
                  })
                  .catch((err) => toast.error(`${err}`));
              },
            },
            {
              label: t('contextMenus.thumbnail.discardPreviews'),
              icon: ImageOff,
              disabled: selectionCount === 0,
              onClick: () => {
                invoke(Invokes.DiscardPreviewsForPaths, { paths: editSelection })
                  .then((count: any) => toast.success(t('notifications.previewsDiscarded', { count })))
                  .catch((err) => toast.error(`${err}`));
              },
            },
            {
              label: cullLabel,
              icon: Users,
              onClick: () =>
                setUI({
                  cullingModalState: {
                    isOpen: true,
                    progress: null,
                    suggestions: null,
                    error: null,
                    pathsToCull: editSelection,
                  },
                }),
              disabled: selectionCount < 2,
            },
          ],
        },
        { type: OPTION_SEPARATOR },
        {
          label: copyLabel,
          icon: Copy,
          onClick: () => {
            setProcess({ copiedFilePaths: editSelection, isCopied: true });
          },
        },
        {
          icon: CopyPlus,
          label: t('contextMenus.thumbnail.duplicateImage'),
          disabled: !isSingleSelection,
          submenu: [
            {
              label: t('contextMenus.thumbnail.physicalCopy'),
              icon: Copy,
              onClick: async () => {
                try {
                  for (const path of editSelection) {
                    await invoke(Invokes.DuplicateFile, {
                      path,
                      targetAlbumId: activeAlbumId || null,
                    });
                  }
                  if (activeAlbumId) {
                    const sortedTree = await invoke<AlbumItem[]>(Invokes.GetAlbums);
                    setLibrary({ albumTree: sortedTree });
                  }
                  await props.refreshImageList();
                } catch (err) {
                  console.error('Failed to duplicate file:', err);
                  toast.error(t('contextMenus.toasts.failedDuplicate', { err }));
                }
              },
            },
            {
              label: t('contextMenus.thumbnail.virtualCopy'),
              icon: CopyPlus,
              onClick: () => handleCreateVirtualCopy(editSelection),
            },
          ],
        },
        { icon: FileEdit, label: renameLabel, onClick: () => props.handleRenameFiles(finalSelection) },
        { type: OPTION_SEPARATOR },
        {
          icon: Star,
          label: t('contextMenus.editor.rating'),
          submenu: [
            ...[0, 1, 2, 3, 4, 5].map((rating: number) => ({
              label:
                rating === 0
                  ? t('contextMenus.editor.noRating')
                  : t('contextMenus.editor.ratingLabel', { count: rating }),
              onClick: () => handleRate(rating, finalSelection),
            })),
            // BLITZRAW: relative, so promoting a mixed selection moves every
            // photo up from where it was rather than levelling them.
            { type: OPTION_SEPARATOR },
            {
              label: t('contextMenus.editor.increaseRating'),
              icon: ChevronUp,
              onClick: () => handleAdjustRating(1, finalSelection),
            },
            {
              label: t('contextMenus.editor.decreaseRating'),
              icon: ChevronDown,
              onClick: () => handleAdjustRating(-1, finalSelection),
            },
          ],
        },
        {
          label: t('contextMenus.editor.colorLabel'),
          icon: Palette,
          submenu: [
            { label: t('contextMenus.editor.noLabel'), onClick: () => handleSetColorLabel(null, finalSelection) },
            ...COLOR_LABELS.map((label: Color) => ({
              label: t(`contextMenus.colors.${label.name}`),
              color: label.color,
              onClick: () => handleSetColorLabel(label.name, finalSelection),
            })),
          ],
        },
        {
          label: t('contextMenus.editor.tagging'),
          icon: Tag,
          submenu: [
            {
              customComponent: TaggingSubMenu,
              customProps: {
                paths: finalSelection,
                initialTags: commonTags,
                onTagsChanged: handleTagsChanged,
                appSettings,
              },
            },
          ],
        },
        { type: OPTION_SEPARATOR },
        {
          label: t('contextMenus.thumbnail.addToAlbum'),
          icon: FolderPlus,
          submenu:
            albumTree.length > 0
              ? buildAddToAlbumMenu(albumTree, leaderSelection)
              : [{ label: t('contextMenus.thumbnail.noAlbums'), disabled: true }],
        },
        ...(activeAlbumId
          ? [
              {
                label: t('contextMenus.thumbnail.removeFromAlbum', { count: selectionCount }),
                icon: Trash2,
                isDestructive: true,
                onClick: handleRemoveFromAlbum,
              },
            ]
          : []),
        { type: OPTION_SEPARATOR },
        {
          disabled: !isSingleSelection,
          icon: Folder,
          label: t('contextMenus.thumbnail.showExplorer'),
          onClick: () => {
            invoke(Invokes.ShowInFinder, { path: finalSelection[0] }).catch((err) =>
              toast.error(t('contextMenus.toasts.couldNotShowExplorer', { err })),
            );
          },
        },
        {
          label: resetLabel,
          icon: RotateCcw,
          submenu: [
            { label: t('contextMenus.editor.cancel'), icon: X, onClick: () => {} },
            {
              label: t('contextMenus.editor.confirmReset'),
              icon: Check,
              isDestructive: true,
              onClick: () => handleResetAdjustments(editSelection),
            },
          ],
        },
        {
          label: deleteLabel,
          icon: Trash2,
          isDestructive: true,
          submenu: deleteSubmenu,
        },
      ];
      showContextMenu(event.clientX, event.clientY, options);
    },
    [
      getCommonTags,
      buildAddToAlbumMenu,
      handleCopyAdjustments,
      handlePasteAdjustments,
      handleRate,
      handleAdjustRating,
      handleSetColorLabel,
      handleTagsChanged,
      handleResetAdjustments,
      showContextMenu,
      props,
      t,
    ],
  );

  const handleFolderTreeContextMenu = useCallback(
    (event: any, path: string | null, isCurrentlyPinned?: boolean) => {
      event.preventDefault();
      event.stopPropagation();

      if (!path) {
        showContextMenu(event.clientX, event.clientY, [
          {
            icon: RefreshCw,
            label: t('contextMenus.folders.refresh'),
            onClick: () => props.refreshAllFolderTrees(),
          },
        ]);
        return;
      }

      const { rootPaths, currentFolderPath, folderTrees, setLibrary } = useLibraryStore.getState();
      const { copiedFilePaths, setProcess } = useProcessStore.getState();
      const { appSettings, handleSettingsChange } = useSettingsStore.getState();
      const { setUI } = useUIStore.getState();
      const targetPath = path;
      const isRoot = rootPaths.includes(targetPath);
      const numCopied = copiedFilePaths.length;
      const copyPastedLabel = t('contextMenus.folders.copyHere', { count: numCopied });
      const movePastedLabel = t('contextMenus.folders.moveHere', { count: numCopied });

      const pinOption = isCurrentlyPinned
        ? {
            icon: PinOff,
            label: t('contextMenus.folders.unpin'),
            onClick: () => props.handleTogglePinFolder(targetPath),
          }
        : { icon: Pin, label: t('contextMenus.folders.pin'), onClick: () => props.handleTogglePinFolder(targetPath) };

      const options = [
        ...(isRoot
          ? [
              {
                icon: Trash2,
                label: t('contextMenus.folders.removeRoot'),
                isDestructive: true,
                onClick: () => {
                  const newRoots = rootPaths.filter((r: string) => r !== targetPath);
                  const newFolderTrees = folderTrees.filter((t: any) => t.path !== targetPath);

                  const isCurrentInTarget =
                    currentFolderPath === targetPath ||
                    currentFolderPath?.startsWith(targetPath + '/') ||
                    currentFolderPath?.startsWith(targetPath + '\\');

                  const updates: any = {
                    rootPaths: newRoots,
                    folderTrees: newFolderTrees,
                  };

                  if (isCurrentInTarget) {
                    updates.currentFolderPath = null;
                    updates.imageList = [];
                    updates.libraryActivePath = null;
                    updates.multiSelectedPaths = [];
                    updates.selectionAnchorPath = null;
                    props.handleBackToLibrary();
                  }

                  setLibrary(updates);

                  const { appSettings, handleSettingsChange } = useSettingsStore.getState();
                  if (appSettings) {
                    const newSettings = { ...appSettings, rootFolders: newRoots } as any;
                    if (newRoots.length === 0) {
                      newSettings.lastRootPath = null;
                      newSettings.lastFolderState = null;
                    } else if (newSettings.lastRootPath === targetPath) {
                      newSettings.lastRootPath = newRoots[0];
                    }

                    if (isCurrentInTarget) {
                      newSettings.lastFolderState = null;
                    }

                    handleSettingsChange(newSettings);
                  }
                },
              },
              { type: OPTION_SEPARATOR },
            ]
          : []),
        pinOption,
        { type: OPTION_SEPARATOR },
        {
          icon: FolderPlus,
          label: t('contextMenus.folders.newFolder'),
          onClick: () => {
            setUI({ folderActionTarget: targetPath, isCreateFolderModalOpen: true });
          },
        },
        {
          disabled: isRoot,
          icon: FileEdit,
          label: t('contextMenus.folders.renameFolder'),
          onClick: () => {
            setUI({ folderActionTarget: targetPath, isRenameFolderModalOpen: true });
          },
        },
        {
          label: t('contextMenus.folders.changeIcon'),
          icon: Palette,
          submenu: albumIcons.map((iconDef) => ({
            label: iconDef.label,
            icon: iconDef.icon,
            onClick: () => {
              if (appSettings) {
                const currentIcons = appSettings.folderIcons || {};
                const newIcons = { ...currentIcons };

                if (iconDef.value) {
                  newIcons[targetPath] = iconDef.value;
                } else {
                  delete newIcons[targetPath];
                }

                handleSettingsChange({ ...appSettings, folderIcons: newIcons });
              }
            },
          })),
        },
        { type: OPTION_SEPARATOR },
        {
          disabled: copiedFilePaths.length === 0,
          icon: ClipboardPaste,
          label: t('contextMenus.folders.paste'),
          submenu: [
            {
              label: copyPastedLabel,
              onClick: async () => {
                try {
                  await invoke(Invokes.CopyFiles, { sourcePaths: copiedFilePaths, destinationFolder: targetPath });
                  if (targetPath === currentFolderPath) props.handleLibraryRefresh();
                } catch (err) {
                  toast.error(t('contextMenus.toasts.failedCopy', { err }));
                }
              },
            },
            {
              label: movePastedLabel,
              onClick: async () => {
                try {
                  await invoke(Invokes.MoveFiles, { sourcePaths: copiedFilePaths, destinationFolder: targetPath });
                  setProcess({ copiedFilePaths: [] });
                  setLibrary({ multiSelectedPaths: [] });
                  props.refreshAllFolderTrees();
                  props.handleLibraryRefresh();
                } catch (err) {
                  toast.error(t('contextMenus.toasts.failedMove', { err }));
                }
              },
            },
          ],
        },
        {
          icon: FolderInput,
          label: t('contextMenus.folders.importImages'),
          onClick: () => props.handleImportClick(targetPath),
        },
        { type: OPTION_SEPARATOR },
        {
          icon: Folder,
          label: t('contextMenus.folders.showExplorer'),
          onClick: () =>
            invoke(Invokes.ShowInFinder, { path: targetPath }).catch((err) =>
              toast.error(t('contextMenus.toasts.couldNotShowFolder', { err })),
            ),
        },
        {
          icon: RefreshCw,
          label: t('contextMenus.folders.refresh'),
          onClick: () => props.refreshAllFolderTrees(),
        },
        {
          disabled: isRoot,
          icon: Trash2,
          isDestructive: true,
          label: t('contextMenus.folders.deleteFolder'),
          submenu: [
            { label: t('contextMenus.editor.cancel'), icon: X, onClick: () => {} },
            {
              label: t('contextMenus.folders.confirm'),
              icon: Check,
              isDestructive: true,
              onClick: async () => {
                try {
                  await invoke(Invokes.DeleteFolder, { path: targetPath });

                  const isCurrentInTarget =
                    currentFolderPath === targetPath ||
                    currentFolderPath?.startsWith(targetPath + '/') ||
                    currentFolderPath?.startsWith(targetPath + '\\');

                  if (isCurrentInTarget) {
                    props.handleBackToLibrary();
                    setLibrary({
                      currentFolderPath: null,
                      imageList: [],
                      libraryActivePath: null,
                      multiSelectedPaths: [],
                      selectionAnchorPath: null,
                    });

                    const { appSettings, handleSettingsChange } = useSettingsStore.getState();
                    if (appSettings) {
                      handleSettingsChange({ ...appSettings, lastFolderState: null } as any);
                    }
                  }

                  props.refreshAllFolderTrees();
                } catch (err) {
                  toast.error(t('contextMenus.toasts.failedDeleteFolder', { err }));
                }
              },
            },
          ],
        },
      ];
      showContextMenu(event.clientX, event.clientY, options);
    },
    [props, showContextMenu, albumIcons, t],
  );

  const handleAlbumTreeContextMenu = useCallback(
    (event: any, item: AlbumItem | null) => {
      event.preventDefault();
      event.stopPropagation();

      const { setUI } = useUIStore.getState();
      const { albumTree, setLibrary } = useLibraryStore.getState();

      const findParentId = (
        nodes: AlbumItem[],
        childId: string,
        parentId: string | null = null,
      ): string | null | undefined => {
        for (const n of nodes) {
          if (n.id === childId) return parentId;
          if (n.type === 'group') {
            const found = findParentId((n as AlbumGroup).children, childId, n.id);
            if (found !== undefined) return found;
          }
        }
        return undefined;
      };

      const currentParentId = item ? findParentId(albumTree, item.id) : undefined;

      const handleMove = (targetId: string | null) => {
        if (!item) return;
        const newTree = structuredClone(albumTree);
        let extractedItem: AlbumItem | null = null;

        const removeAndGet = (nodes: AlbumItem[], id: string): AlbumItem | null => {
          for (let i = 0; i < nodes.length; i++) {
            if (nodes[i].id === id) return nodes.splice(i, 1)[0];
            if (nodes[i].type === 'group') {
              const res = removeAndGet((nodes[i] as AlbumGroup).children, id);
              if (res) return res;
            }
          }
          return null;
        };

        extractedItem = removeAndGet(newTree, item.id);
        if (!extractedItem) return;

        if (!targetId) {
          newTree.push(extractedItem);
        } else {
          let inserted = false;

          const insert = (nodes: AlbumItem[]) => {
            for (const n of nodes) {
              if (n.id === targetId && n.type === 'group') {
                n.children.push(extractedItem!);
                inserted = true;
                return;
              } else if (n.type === 'group') {
                insert(n.children);
                if (inserted) return;
              }
            }
          };

          insert(newTree);

          if (!inserted) {
            toast.error(t('contextMenus.toasts.failedMoveInvalid'));
            return;
          }
        }

        invoke(Invokes.SaveAlbums, { tree: newTree })
          .then(() => invoke(Invokes.GetAlbums))
          .then((sortedTree: any) => setLibrary({ albumTree: sortedTree }))
          .catch((err) => toast.error(t('contextMenus.toasts.failedMoveError', { err })));
      };

      const buildMoveSubmenu = (nodes: AlbumItem[]): Option[] => {
        let opts: Option[] = [];
        nodes.forEach((n) => {
          if (n.type === 'group' && n.id !== item?.id) {
            const isCurrentParent = n.id === currentParentId;
            const subOpts = buildMoveSubmenu((n as AlbumGroup).children);

            const customIconDef = n.icon ? albumIcons.find((i) => i.value === n.icon) : null;
            const ResolvedIcon = customIconDef?.icon || Folder;

            if (subOpts.length > 0) {
              opts.push({
                label: n.name,
                icon: ResolvedIcon,
                submenu: [
                  {
                    label: isCurrentParent ? t('contextMenus.albums.alreadyHere') : t('contextMenus.albums.moveHere'),
                    icon: Check,
                    disabled: isCurrentParent,
                    onClick: isCurrentParent ? undefined : () => handleMove(n.id),
                  },
                  { type: OPTION_SEPARATOR },
                  ...subOpts,
                ],
              });
            } else {
              opts.push({
                label: isCurrentParent ? `${n.name} (Current)` : n.name,
                icon: ResolvedIcon,
                disabled: isCurrentParent,
                onClick: isCurrentParent ? undefined : () => handleMove(n.id),
              });
            }
          }
        });
        return opts;
      };

      const moveOptions = buildMoveSubmenu(albumTree);
      const isAtRoot = currentParentId === null;
      const isMoveDisabled = moveOptions.length === 0 && isAtRoot;

      const options: Option[] = [
        {
          label: t('contextMenus.albums.newAlbum'),
          icon: Images,
          onClick: () => setUI({ albumActionTarget: item?.id || null, isCreateAlbumModalOpen: true }),
        },
        {
          label: t('contextMenus.albums.newGroup'),
          icon: FolderPlus,
          onClick: () => setUI({ albumActionTarget: item?.id || null, isCreateAlbumGroupModalOpen: true }),
        },
        ...(item
          ? [
              { type: OPTION_SEPARATOR },
              {
                label:
                  item.type === 'group' ? t('contextMenus.albums.renameGroup') : t('contextMenus.albums.renameAlbum'),
                icon: FileEdit,
                onClick: () => setUI({ albumActionTarget: item.id, isRenameAlbumModalOpen: true }),
              },
              {
                label: t('contextMenus.folders.changeIcon'),
                icon: Palette,
                submenu: albumIcons.map((iconDef) => ({
                  label: iconDef.label,
                  icon: iconDef.icon,
                  onClick: () => {
                    const newTree = structuredClone(albumTree);
                    const updateIcon = (nodes: AlbumItem[]) => {
                      for (const n of nodes) {
                        if (n.id === item.id) {
                          n.icon = iconDef.value;
                          return true;
                        }
                        if (n.type === 'group' && updateIcon((n as AlbumGroup).children)) return true;
                      }
                      return false;
                    };

                    if (updateIcon(newTree)) {
                      invoke(Invokes.SaveAlbums, { tree: newTree })
                        .then(() => invoke(Invokes.GetAlbums))
                        .then((sorted: any) => setLibrary({ albumTree: sorted }))
                        .catch((err) => toast.error(t('contextMenus.toasts.failedChangeIcon', { err })));
                    }
                  },
                })),
              },
              {
                label: t('contextMenus.albums.moveTo'),
                icon: FolderInput,
                disabled: isMoveDisabled,
                submenu: isMoveDisabled
                  ? []
                  : [
                      {
                        label: isAtRoot ? t('contextMenus.albums.alreadyAtRoot') : t('contextMenus.albums.rootDir'),
                        icon: Home,
                        disabled: isAtRoot,
                        onClick: isAtRoot ? undefined : () => handleMove(null),
                      },
                      ...(moveOptions.length > 0 ? [{ type: OPTION_SEPARATOR }, ...moveOptions] : []),
                    ],
              },
              { type: OPTION_SEPARATOR },
              {
                label:
                  item.type === 'group' ? t('contextMenus.albums.deleteGroup') : t('contextMenus.albums.deleteAlbum'),
                icon: Trash2,
                isDestructive: true,
                submenu: [
                  { label: t('contextMenus.editor.cancel'), icon: X, onClick: () => {} },
                  {
                    label:
                      item.type === 'album'
                        ? t('contextMenus.albums.confirmDeleteAlbum')
                        : (item as AlbumGroup).children.length > 0
                          ? t('contextMenus.albums.confirmDeleteGroupNested')
                          : t('contextMenus.albums.confirmDeleteGroupEmpty'),
                    icon: Check,
                    isDestructive: true,
                    onClick: () => {
                      const newTree = structuredClone(albumTree);
                      const del = (nodes: AlbumItem[]) => {
                        const idx = nodes.findIndex((n) => n.id === item.id);
                        if (idx !== -1) nodes.splice(idx, 1);
                        else
                          nodes.forEach((n) => {
                            if (n.type === 'group') del((n as AlbumGroup).children);
                          });
                      };
                      del(newTree);
                      invoke(Invokes.SaveAlbums, { tree: newTree })
                        .then(() => invoke(Invokes.GetAlbums))
                        .then((sorted: any) => setLibrary({ albumTree: sorted }))
                        .catch((err) => toast.error(t('contextMenus.toasts.failedDelete', { err })));
                    },
                  },
                ],
              },
            ]
          : []),
      ];

      showContextMenu(event.clientX, event.clientY, options);
    },
    [showContextMenu, albumIcons, t],
  );

  const handleMainLibraryContextMenu = useCallback(
    (event: any) => {
      event.preventDefault();
      event.stopPropagation();

      const { copiedFilePaths, setProcess } = useProcessStore.getState();
      const { currentFolderPath, activeAlbumId, setLibrary } = useLibraryStore.getState();

      const numCopied = copiedFilePaths.length;
      const copyPastedLabel = t('contextMenus.folders.copyHere', { count: numCopied });
      const movePastedLabel = t('contextMenus.folders.moveHere', { count: numCopied });
      const addCopiedToAlbumLabel = t('contextMenus.library.addCopiedToAlbum', { count: numCopied });

      const isAlbumView = !!activeAlbumId;

      const pasteOption = isAlbumView
        ? {
            label: addCopiedToAlbumLabel,
            icon: ClipboardPaste,
            disabled: copiedFilePaths.length === 0,
            onClick: async () => {
              try {
                await invoke(Invokes.AddToAlbum, { albumId: activeAlbumId, paths: copiedFilePaths });
                console.log(`Added ${numCopied} image(s) to album`);
                const updatedTree = await invoke<AlbumItem[]>(Invokes.GetAlbums);
                setLibrary({ albumTree: updatedTree });
                await props.refreshImageList();
              } catch (err) {
                toast.error(t('contextMenus.toasts.failedAddToAlbum', { err }));
              }
            },
          }
        : {
            label: t('contextMenus.folders.paste'),
            icon: ClipboardPaste,
            disabled: copiedFilePaths.length === 0,
            submenu: [
              {
                label: copyPastedLabel,
                onClick: async () => {
                  try {
                    await invoke(Invokes.CopyFiles, {
                      sourcePaths: copiedFilePaths,
                      destinationFolder: currentFolderPath,
                    });
                    props.handleLibraryRefresh();
                  } catch (err) {
                    toast.error(t('contextMenus.toasts.failedCopy', { err }));
                  }
                },
              },
              {
                label: movePastedLabel,
                onClick: async () => {
                  try {
                    await invoke(Invokes.MoveFiles, {
                      sourcePaths: copiedFilePaths,
                      destinationFolder: currentFolderPath,
                    });
                    setProcess({ copiedFilePaths: [] });
                    setLibrary({ multiSelectedPaths: [] });
                    props.refreshAllFolderTrees();
                    props.handleLibraryRefresh();
                  } catch (err) {
                    toast.error(t('contextMenus.toasts.failedMove', { err }));
                  }
                },
              },
            ],
          };

      const options = [
        { label: t('contextMenus.library.refreshView'), icon: RefreshCw, onClick: props.handleLibraryRefresh },
        { type: OPTION_SEPARATOR },
        pasteOption,
        {
          icon: FolderInput,
          label: t('contextMenus.folders.importImages'),
          onClick: () => props.handleImportClick(currentFolderPath as string),
          disabled: !currentFolderPath || isAlbumView,
        },
      ];

      showContextMenu(event.clientX, event.clientY, options);
    },
    [props, showContextMenu, t],
  );

  return {
    handleEditorContextMenu,
    handleThumbnailContextMenu,
    handleFolderTreeContextMenu,
    handleAlbumTreeContextMenu,
    handleMainLibraryContextMenu,
  };
}
