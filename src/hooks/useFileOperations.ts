import { useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { toast } from 'react-toastify';
import { useLibraryStore } from '../store/useLibraryStore';
import { useEditorStore } from '../store/useEditorStore';
import { useUIStore } from '../store/useUIStore';
import { useProcessStore } from '../store/useProcessStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { Invokes } from '../components/ui/AppProperties';
import { BOTH, EDIT_RULE } from '../utils/imageStacking';
import { selectionFor } from '../utils/selection';
import { Status } from '../components/ui/ExportImportProperties';

export function useFileOperations(
  refreshImageList: () => Promise<void>,
  refreshAllFolderTrees: () => Promise<void>,
  handleImageSelect: (path: string) => void,
  handleBackToLibrary: () => void,
  sortedImageList: any[],
) {
  const getParentDir = (filePath: string): string => {
    const separator = filePath.includes('/') ? '/' : '\\';
    const lastSeparatorIndex = filePath.lastIndexOf(separator);
    if (lastSeparatorIndex === -1) return '';
    return filePath.substring(0, lastSeparatorIndex);
  };

  const executeDelete = useCallback(
    async (pathsToDelete: Array<string>, options = { includeAssociated: false }) => {
      if (!pathsToDelete || pathsToDelete.length === 0) return;

      const { libraryActivePath, setLibrary } = useLibraryStore.getState();
      const { selectedImage } = useEditorStore.getState();
      const { activeView } = useUIStore.getState(); // <-- Check active view

      // Determine the active path based on the current view
      const activePath = selectedImage && activeView === 'editor' ? selectedImage.path : libraryActivePath;
      let nextImagePath: string | null = null;

      if (activePath) {
        const physicalPath = activePath.split('?vc=')[0];
        const isActiveImageDeleted = pathsToDelete.some((p) => p === activePath || p === physicalPath);

        if (isActiveImageDeleted) {
          const currentIndex = sortedImageList.findIndex((img) => img.path === activePath);
          if (currentIndex !== -1) {
            const nextCandidate = sortedImageList
              .slice(currentIndex + 1)
              .find((img) => !pathsToDelete.includes(img.path));

            if (nextCandidate) {
              nextImagePath = nextCandidate.path;
            } else {
              const prevCandidate = sortedImageList
                .slice(0, currentIndex)
                .reverse()
                .find((img) => !pathsToDelete.includes(img.path));

              if (prevCandidate) {
                nextImagePath = prevCandidate.path;
              }
            }
          }
        } else {
          nextImagePath = activePath;
        }
      }

      try {
        const command = options.includeAssociated ? 'delete_files_with_associated' : 'delete_files_from_disk';
        await invoke(command, { paths: pathsToDelete });
        await refreshImageList();

        if (selectedImage && activeView === 'editor') {
          const physicalPath = selectedImage.path.split('?vc=')[0];
          const isFileBeingEditedDeleted = pathsToDelete.some((p) => p === selectedImage.path || p === physicalPath);

          if (isFileBeingEditedDeleted) {
            if (nextImagePath) {
              handleImageSelect(nextImagePath);
            } else {
              handleBackToLibrary();
            }
          }
        } else {
          if (nextImagePath) {
            setLibrary({ multiSelectedPaths: [nextImagePath], libraryActivePath: nextImagePath });
          } else {
            setLibrary({ multiSelectedPaths: [], libraryActivePath: null });
          }

          if (selectedImage) {
            const physicalPath = selectedImage.path.split('?vc=')[0];
            if (pathsToDelete.some((p) => p === selectedImage.path || p === physicalPath)) {
              useEditorStore.getState().setEditor({ selectedImage: null });
            }
          }
        }
      } catch (err) {
        console.error('Failed to delete files:', err);
        toast.error(`Failed to delete files: ${err}`);
      }
    },
    [refreshImageList, handleBackToLibrary, sortedImageList, handleImageSelect],
  );

  const handleDeleteSelected = useCallback(() => {
    const { multiSelectedPaths, imageList } = useLibraryStore.getState();
    const { setUI } = useUIStore.getState();

    // Deleting a collapsed stack deletes the stack. Both kinds: leaving the
    // brackets behind after removing the HDR they made is not a tidier result,
    // it is three orphans. This used to take the one visible frame.
    const pathsToDelete = selectionFor(BOTH('fullStack'), multiSelectedPaths);
    if (pathsToDelete.length === 0) {
      return;
    }

    const isSingle = pathsToDelete.length === 1;

    const selectionHasVirtualCopies =
      isSingle &&
      !pathsToDelete[0].includes('?vc=') &&
      imageList.some((image) => image.path.startsWith(`${pathsToDelete[0]}?vc=`));

    // The count goes in the title. It used to sit mid-sentence in the body at
    // body weight, which is not where anyone looks before clicking Delete, and
    // it matters more now: a collapsed stack is one click and several files.
    const count = pathsToDelete.length;
    let modalTitle = count === 1 ? 'Delete 1 image?' : `Delete ${count} images?`;
    let modalMessage = '';
    let confirmText = 'Delete';

    // Say so when the number is larger than what was clicked, rather than
    // leaving the reader to work out where the extra files came from.
    const clicked = multiSelectedPaths.length;
    const stackNote =
      count > clicked
        ? ` This includes every frame of ${clicked === 1 ? 'the stack you selected' : 'the stacks you selected'}.`
        : '';

    if (selectionHasVirtualCopies) {
      modalTitle = 'Delete this image and all its virtual copies?';
      modalMessage = `This cannot be undone.`;
      confirmText = 'Delete All';
    } else if (isSingle) {
      modalMessage = `This cannot be undone. Right-click for more options, such as deleting associated files.`;
      confirmText = 'Delete Selected Only';
    } else {
      modalMessage = `This cannot be undone.${stackNote} Right-click for more options, such as deleting associated files.`;
      confirmText = 'Delete Selected Only';
    }

    setUI({
      confirmModalState: {
        confirmText,
        confirmVariant: 'destructive',
        isOpen: true,
        message: modalMessage,
        onConfirm: () => executeDelete(pathsToDelete, { includeAssociated: false }),
        title: modalTitle,
      },
    });
  }, [executeDelete]);

  const handleCreateFolder = useCallback(
    async (folderName: string) => {
      const { folderActionTarget } = useUIStore.getState();

      if (folderName && folderName.trim() !== '' && folderActionTarget) {
        try {
          await invoke(Invokes.CreateFolder, { path: `${folderActionTarget}/${folderName.trim()}` });
          await refreshAllFolderTrees();
        } catch (err) {
          toast.error(`Failed to create folder: ${err}`);
        }
      }
    },
    [refreshAllFolderTrees],
  );

  const handleRenameFolder = useCallback(
    async (newName: string) => {
      const { folderActionTarget } = useUIStore.getState();
      const { rootPaths, currentFolderPath, setLibrary } = useLibraryStore.getState();
      const { appSettings, handleSettingsChange } = useSettingsStore.getState();

      if (newName && newName.trim() !== '' && folderActionTarget) {
        try {
          const oldPath = folderActionTarget;
          const trimmedNewName = newName.trim();

          await invoke(Invokes.RenameFolder, { path: oldPath, newName: trimmedNewName });

          const parentDir = getParentDir(oldPath);
          const separator = oldPath.includes('/') ? '/' : '\\';
          const newPath = parentDir ? `${parentDir}${separator}${trimmedNewName}` : trimmedNewName;

          const newAppSettings = { ...appSettings } as any;
          let settingsChanged = false;

          if (rootPaths.includes(oldPath)) {
            const newRoots = rootPaths.map((r) => (r === oldPath ? newPath : r));
            setLibrary({ rootPaths: newRoots });
            newAppSettings.rootFolders = newRoots;
            settingsChanged = true;
          }
          if (currentFolderPath?.startsWith(oldPath)) {
            const newCurrentPath = currentFolderPath.replace(oldPath, newPath);
            setLibrary({ currentFolderPath: newCurrentPath });
          }

          const currentPins = appSettings?.pinnedFolders || [];
          if (currentPins.includes(oldPath)) {
            const newPins = currentPins
              .map((p: string) => (p === oldPath ? newPath : p))
              .sort((a: string, b: string) => a.localeCompare(b));
            newAppSettings.pinnedFolders = newPins;
            settingsChanged = true;
          }

          if (settingsChanged) {
            handleSettingsChange(newAppSettings);
          }

          await refreshAllFolderTrees();
        } catch (err) {
          toast.error(`Failed to rename folder: ${err}`);
        }
      }
    },
    [refreshAllFolderTrees],
  );

  const handleSaveRename = useCallback(
    async (nameTemplate: string) => {
      const { renameTargetPaths, setUI } = useUIStore.getState();
      const { selectedImage } = useEditorStore.getState();
      const { libraryActivePath, setLibrary } = useLibraryStore.getState();

      if (renameTargetPaths.length > 0 && nameTemplate) {
        try {
          const newPaths: Array<string> = await invoke(Invokes.RenameFiles, {
            nameTemplate,
            paths: renameTargetPaths,
          });

          await refreshImageList();

          if (selectedImage && renameTargetPaths.includes(selectedImage.path)) {
            const oldPathIndex = renameTargetPaths.indexOf(selectedImage.path);
            if (newPaths[oldPathIndex]) {
              handleImageSelect(newPaths[oldPathIndex]);
            } else {
              handleBackToLibrary();
            }
          }

          if (libraryActivePath && renameTargetPaths.includes(libraryActivePath)) {
            const oldPathIndex = renameTargetPaths.indexOf(libraryActivePath);
            if (newPaths[oldPathIndex]) {
              setLibrary({ libraryActivePath: newPaths[oldPathIndex] });
            } else {
              setLibrary({ libraryActivePath: null });
            }
          }

          setLibrary({ multiSelectedPaths: newPaths });
        } catch (err) {
          toast.error(`Failed to rename files: ${err}`);
        }
      }
      setUI({ renameTargetPaths: [] });
    },
    [refreshImageList, handleImageSelect, handleBackToLibrary],
  );

  const handleRenameFiles = useCallback((paths: Array<string>) => {
    // A merged result is renamed on its own, since it is the deliverable and
    // its ingredients are working files. A peer stack renames together.
    paths = selectionFor(EDIT_RULE, paths);
    if (paths && paths.length > 0) {
      useUIStore.getState().setUI({ renameTargetPaths: paths, isRenameFileModalOpen: true });
    }
  }, []);

  const startImportFiles = useCallback(async (sourcePaths: string[], destinationFolder: string, settings: any) => {
    if (sourcePaths.length === 0 || !destinationFolder) return;

    try {
      await invoke(Invokes.ImportFiles, { destinationFolder, settings, sourcePaths });
    } catch (err) {
      console.error('Failed to start import:', err);
      useProcessStore
        .getState()
        .setImportState({ status: Status.Error, errorMessage: `Failed to start import: ${err}` });
    }
  }, []);

  const handleStartImport = useCallback(
    async (settings: any) => {
      const { importTargetFolder, importSourcePaths } = useUIStore.getState();
      if (!importTargetFolder) return;
      await startImportFiles(importSourcePaths, importTargetFolder, settings);
    },
    [startImportFiles],
  );

  const handleImportClick = useCallback(
    async (targetPath: string) => {
      const { supportedTypes, osPlatform } = useSettingsStore.getState();
      const { setUI } = useUIStore.getState();
      const isAndroid = osPlatform === 'android';

      try {
        const nonRaw = supportedTypes?.nonRaw || [];
        const raw = supportedTypes?.raw || [];

        const expandExtensions = (exts: string[]) => {
          return Array.from(new Set(exts.flatMap((ext) => [ext.toLowerCase(), ext.toUpperCase()])));
        };

        const processedNonRaw = expandExtensions(nonRaw);
        const processedRaw = expandExtensions(raw);
        const allImageExtensions = [...processedNonRaw, ...processedRaw];

        const typeFilters = isAndroid
          ? []
          : [
              { name: 'All Supported Images', extensions: allImageExtensions },
              { name: 'RAW Images', extensions: processedRaw },
              { name: 'Standard Images (JPEG, PNG, etc.)', extensions: processedNonRaw },
              { name: 'All Files', extensions: ['*'] },
            ];

        const selected = await open({
          filters: typeFilters,
          multiple: true,
          title: 'Select files to import',
        });

        if (Array.isArray(selected) && selected.length > 0) {
          const invalidExtensions = new Set<string>();
          const allowedExtensions = new Set(allImageExtensions.map((e) => e.toLowerCase()));

          const resolvedFiles = await Promise.all(
            selected.map(async (path) => {
              if (isAndroid) {
                try {
                  return await invoke<string>('resolve_android_content_uri_name', { uriStr: path });
                } catch (e) {
                  console.error('Failed to resolve URI:', e);
                  return path;
                }
              }
              return path;
            }),
          );

          const validFiles = selected.filter((originalPath, index) => {
            const resolvedName = resolvedFiles[index];
            const ext = resolvedName.split('.').pop()?.toLowerCase() || 'unknown';

            if (!allowedExtensions.has(ext)) {
              invalidExtensions.add(`.${ext}`);
              return false;
            }
            return true;
          });

          if (invalidExtensions.size > 0) {
            const extList = Array.from(invalidExtensions).join(', ');
            toast.error(`Unsupported file format(s) detected: ${extList}`);
            return;
          }

          if (isAndroid) {
            const DEFAULT_IMPORT_SETTINGS = {
              filenameTemplate: '{original_filename}',
              organizeByDate: false,
              dateFolderFormat: 'YYYY/MM-DD',
              deleteAfterImport: false,
            };
            await startImportFiles(validFiles, targetPath, DEFAULT_IMPORT_SETTINGS);
            return;
          }

          setUI({ importSourcePaths: validFiles, importTargetFolder: targetPath, isImportModalOpen: true });
        }
      } catch (err) {
        console.error('Failed to open file dialog for import:', err);
      }
    },
    [startImportFiles],
  );

  const handlePasteFiles = useCallback(
    async (mode = 'copy') => {
      const { copiedFilePaths, setProcess } = useProcessStore.getState();
      const { currentFolderPath, setLibrary } = useLibraryStore.getState();

      if (copiedFilePaths.length === 0 || !currentFolderPath) return;

      try {
        if (mode === 'copy') {
          await invoke(Invokes.CopyFiles, { sourcePaths: copiedFilePaths, destinationFolder: currentFolderPath });
        } else {
          await invoke(Invokes.MoveFiles, { sourcePaths: copiedFilePaths, destinationFolder: currentFolderPath });
          setProcess({ copiedFilePaths: [] });
          setLibrary({ multiSelectedPaths: [] });
          await refreshAllFolderTrees();
        }
        await refreshImageList();
      } catch (err) {
        toast.error(`Failed to ${mode} files: ${err}`);
      }
    },
    [refreshImageList, refreshAllFolderTrees],
  );

  return {
    executeDelete,
    handleDeleteSelected,
    handleCreateFolder,
    handleRenameFolder,
    handleSaveRename,
    handleRenameFiles,
    handleStartImport,
    startImportFiles,
    handleImportClick,
    handlePasteFiles,
  };
}
