import { useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import { Invokes } from '../components/ui/AppProperties';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useUIStore } from '../store/useUIStore';
import { useLibraryActions } from './useLibraryActions';
import { globalImageCache } from '../utils/ImageLRUCache';
import { normalizeLoadedAdjustments } from '../utils/adjustments';
import {
  commitRedo,
  commitUndo,
  recordAppAction,
  recordingSettled,
  redoTarget,
  undoTarget,
} from '../utils/appHistory';
import { debouncedSave } from './useEditorActions';

/**
 * BLITZRAW: one undo, wherever it is pressed from.
 *
 * A hook rather than a function inside the keyboard handler, because the editor
 * context menu has an Undo of its own and the two must not mean different
 * things. That split is exactly what this feature exists to remove.
 */
export function useAppUndo(
  handleBackToLibrary: () => void,
  // Absent for a jump inside the History panel, which never leaves the editor
  // and never changes the selection.
  prevAdjustmentsRef?: React.RefObject<any>,
) {
  const { handleUndoRating, handleRedoRating } = useLibraryActions();

  // ============ BLITZRAW: carrying out one undo, or one redo ============
  // Three steps, in this order, and the order matters.
  //
  // 1. Put the selection and the view back, so what happens next is visible and
  //    so the grid is showing the photos that are about to change.
  // 2. Tell each photo the entry touched to move its own bookmark to its own
  //    number. Nothing is written on top of anything: the step a photo leaves
  //    is still there, which is what makes the redo possible.
  // 3. Only then move the list, so a refused move does not lose the entry.
  //
  // The open photo is reloaded from its file afterwards rather than guessed at,
  // for the same reason the list holds no values: the file is the record.
  const walkAction = useCallback(
    async (redo: boolean) => {
      // BLITZRAW: a press that arrives before the write does.
      //
      // The editor's save waits 300 ms and then has to come back before the
      // entry exists, so pressing Ctrl+Z straight after an edit used to do
      // nothing and the edit had to be undone twice. The save is sent at once
      // and this waits for it. See utils/appHistory.ts.
      debouncedSave.flush();
      await recordingSettled();

      const entry = redo ? redoTarget() : undoTarget();
      if (!entry) {
        return;
      }

      if (entry.kind === 'ratings') {
        // Ratings are not part of a photo's edit history and never should be,
        // so their own stack is the only record there is. It moves in step with
        // this list because every rating change and every rating undo goes
        // through both.
        if (redo) {
          handleRedoRating();
          commitRedo();
        } else {
          handleUndoRating();
          commitUndo();
        }
        return;
      }

      const paths = entry.photos.map((photo) => photo.path);
      useLibraryStore.getState().setLibrary({ multiSelectedPaths: entry.selection });
      if (!entry.inEditor && useUIStore.getState().activeView === 'editor') {
        handleBackToLibrary();
      }

      try {
        const report: any = await invoke(Invokes.GoToSteps, {
          targets: entry.photos.map((photo) => ({
            path: photo.path,
            n: redo ? photo.to : photo.from,
          })),
        });

        paths.forEach((path) => globalImageCache.delete(path));

        const moved: Array<string> = report?.moved ?? [];
        const refused: Array<{ path: string; reason: string }> = report?.refused ?? [];
        console.info(
          `[undo] ${redo ? 'redo' : 'undo'} of ${entry.label}: ` +
            `${moved.length} moved, ${refused.length} refused`,
          refused.slice(0, 5),
        );

        if (moved.length === 0) {
          // Nothing can ever be done about this one: no photo can reach that
          // number any more. Staying on it would block everything older behind
          // it, so it is stepped over and the next press reaches the one before.
          toast.info(`Skipped ${entry.label}: those steps are no longer in the photos.`);
          if (redo) {
            commitRedo();
          } else {
            commitUndo();
          }
          return;
        }
        if (refused.length > 0) {
          toast.info(
            `${moved.length} of ${paths.length} photos moved. ` +
              `${refused.length} no longer have that step.`,
          );
        }

        const { selectedImage } = useEditorStore.getState();
        if (selectedImage && paths.includes(selectedImage.path)) {
          const meta: any = await invoke('load_metadata', { path: selectedImage.path });
          if (meta?.adjustments) {
            const restored = normalizeLoadedAdjustments(meta.adjustments);
            // The photo's own history panel is rebuilt from its file too, not
            // just its values, so the list moves when the picture moves.
            useEditorStore.getState().resetHistory(restored, meta.history ?? null);
            useEditorStore.getState().setEditor({
              adjustments: restored,
              // Stamped so auto-sync treats this as a state to measure from
              // rather than a change to spread. Every photo has already moved
              // itself; sending the open photo's values on top of that is
              // exactly the mistake this whole feature exists to end.
              historyMoveAt: Date.now(),
            });
            if (prevAdjustmentsRef) {
              prevAdjustmentsRef.current = {
                path: selectedImage.path,
                adjustments: restored,
                setBy: redo ? 'a redo' : 'an undo',
                setAt: Date.now(),
              };
            }
          }
        }
      } catch (err) {
        console.error('Failed to move a photo to a step:', err);
        toast.error(`Could not undo: ${err}`);
        return;
      }

      if (redo) {
        commitRedo();
      } else {
        commitUndo();
      }
    },
    [handleBackToLibrary, handleRedoRating, handleUndoRating, prevAdjustmentsRef],
  );
  // ========== BLITZRAW END: carrying out one undo, or one redo ==========



  // ============ BLITZRAW: clicking a step in the History panel ============
  // Not an undo. It is a thing I did, so it goes into the list of what I did
  // and Ctrl+Z takes it back: jump from step 12 to step 7, press Ctrl+Z, and
  // you are back on 12. That is the same mechanism an undo uses, which is why
  // every entry holds two numbers rather than one.
  //
  // Falls back to moving the editor alone when the steps have no numbers yet,
  // which is a photo whose history was made this session and never written.
  // Nothing is recorded then, because there would be nothing to point at.
  const jumpToStep = useCallback(
    async (index: number) => {
      const state = useEditorStore.getState();
      const path = state.selectedImage?.path;
      const from = state.historyNumbers?.[state.historyIndex] ?? -1;
      const to = state.historyNumbers?.[index] ?? -1;

      if (!path || from < 0 || to < 0 || from === to) {
        state.goToHistoryIndex(index);
        return;
      }

      try {
        const report: any = await invoke(Invokes.GoToSteps, {
          targets: [{ path, n: to }],
        });
        if ((report?.moved?.length ?? 0) === 0) {
          toast.error('That step is no longer in the photo.');
          return;
        }
        globalImageCache.delete(path);
        state.goToHistoryIndex(index);
        recordAppAction({
          id: `jump-${path}-${from}-${to}`,
          kind: 'adjustments',
          label: state.historyLabels?.[index] ?? 'History',
          photos: [{ path, from, to }],
          selection: useLibraryStore.getState().multiSelectedPaths,
          openPath: path,
          inEditor: true,
        });
        if (prevAdjustmentsRef) {
          prevAdjustmentsRef.current = {
            path,
            adjustments: useEditorStore.getState().adjustments,
            setBy: 'a click in the history list',
            setAt: Date.now(),
          };
        }
      } catch (err) {
        console.error('Failed to jump to a step:', err);
        toast.error(`Could not go to that step: ${err}`);
      }
    },
    [prevAdjustmentsRef],
  );
  // ========== BLITZRAW END: clicking a step in the History panel ==========

  return { walkAction, jumpToStep };
}
