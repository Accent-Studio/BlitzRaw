import { useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import { Invokes } from '../components/ui/AppProperties';
import { useLibraryStore } from '../store/useLibraryStore';
import { useUIStore } from '../store/useUIStore';
import { useProcessStore } from '../store/useProcessStore';
import { SelectedStack } from '../utils/imageStacking';

/**
 * Merging many bracket stacks in one run.
 *
 * The backend keeps a single merged image in memory: `merge_hdr` fills that
 * slot and `save_hdr` takes it. So merges must run strictly one after another.
 * Running two at once would have the second overwrite the first's result before
 * it reached disk, silently producing one file where two were expected.
 *
 * Stacks already merged are skipped by default, because `save_hdr` overwrites
 * without asking. That is harmless for one deliberate merge and not at all
 * harmless for a bulk run repeated over a folder.
 */

interface HdrOutputStatus {
  firstPath: string;
  exists: boolean;
  existingPath: string | null;
}

export interface BulkHdrSummary {
  merged: number;
  skipped: number;
  failed: number;
}

/**
 * Puts a merged result on top of the stack it came from, carrying over the
 * rating and colour label the bracket already had.
 *
 * A failure here leaves the merged file on disk and untouched, so it is
 * reported rather than treated as a failed merge.
 */
async function inheritStackPosition(savedPath: string, stack: SelectedStack): Promise<void> {
  await invoke('set_stack_leader', { newMemberPath: savedPath, siblingPath: stack.paths[0] });

  const { imageRatings, imageList } = useLibraryStore.getState();

  // Ratings propagate across a stack, so any member carries the same value.
  // Take the highest anyway, in case a stack was rated before that was true.
  const rating = Math.max(0, ...stack.paths.map((path) => imageRatings[path] ?? 0));
  if (rating > 0) {
    await invoke(Invokes.SetRatingForPaths, { paths: [savedPath], rating });
  }

  const colorTag = stack.paths
    .map((path) => imageList.find((image) => image.path === path))
    .flatMap((image) => image?.tags ?? [])
    .find((tag) => tag.startsWith('color:'));

  if (colorTag) {
    await invoke(Invokes.SetColorLabelForPaths, { paths: [savedPath], color: colorTag.substring(6) });
  }
}

export function useBulkHdr(refreshImageList?: () => Promise<void> | void) {
  const mergeStacks = useCallback(
    async (stacks: Array<SelectedStack>, options?: { overwrite?: boolean }): Promise<BulkHdrSummary> => {
      const summary: BulkHdrSummary = { merged: 0, skipped: 0, failed: 0 };

      const mergeable = stacks.filter((stack) => stack.paths.length >= 2);
      if (mergeable.length === 0) {
        toast.info('No stacks with two or more frames in the selection.');
        return summary;
      }

      let queue = mergeable;

      if (!options?.overwrite) {
        try {
          const statuses = await invoke<Array<HdrOutputStatus>>('hdr_outputs_present', {
            firstPaths: mergeable.map((stack) => stack.paths[0]),
          });
          const done = new Set(statuses.filter((s) => s.exists).map((s) => s.firstPath));
          queue = mergeable.filter((stack) => !done.has(stack.paths[0]));
          summary.skipped = mergeable.length - queue.length;
        } catch (err) {
          // A failed check should not stop the run; merging is still safe, it
          // just may overwrite. Say so rather than deciding silently.
          console.error('Could not check for existing HDR results', err);
        }
      }

      if (queue.length === 0) {
        toast.info(`All ${summary.skipped} stacks were already merged.`);
        return summary;
      }

      const toastId = toast.loading(`Merging HDR... 0/${queue.length}`);
      const failures: Array<string> = [];
      const savedPaths: Array<string> = [];

      // The single-merge modal is driven by the same backend events this queue
      // triggers, so without this it opens on every merge, offers to save a
      // result the queue is already saving, and its Merge button fires with
      // whatever paths it was last given.
      useUIStore.getState().setUI({ isBulkHdrRunning: true });

      try {
        for (let i = 0; i < queue.length; i++) {
          const stack = queue[i];
          toast.update(toastId, { render: `Merging HDR... ${i}/${queue.length}` });

          try {
            // Sequential by necessity, not by preference: see the note above.
            // Recorded so a bad merge can be traced to what actually went in.
            console.info('HDR merge inputs', stack.paths);
            // No preview: the queue waits on the command, and the listener
            // that would receive one drops it while a bulk run is going. See
            // the note in merge_hdr for what asking for it was costing.
            await invoke(Invokes.MergeHdr, { paths: stack.paths, withPreview: false });
            const savedPath = await invoke<string>(Invokes.SaveHdr, { firstPathStr: stack.paths[0] });

            // The merged frame joins the bracket it came from and goes on top, so
            // a merged stack reads as one finished photograph rather than leaving
            // the result loose beside the frames that made it.
            await inheritStackPosition(savedPath, stack);
            savedPaths.push(savedPath);
            summary.merged += 1;
          } catch (err) {
            summary.failed += 1;
            const name = stack.paths[0].split(/[\\/]/).pop() ?? stack.paths[0];
            failures.push(`${name}: ${err}`);
          }
        }
      } finally {
        useUIStore.getState().setUI({ isBulkHdrRunning: false });
      }

      const parts = [`${summary.merged} merged`];
      if (summary.skipped > 0) parts.push(`${summary.skipped} already done`);
      if (summary.failed > 0) parts.push(`${summary.failed} failed`);

      toast.update(toastId, {
        render: parts.join(', '),
        type: summary.failed > 0 ? 'warning' : 'success',
        isLoading: false,
        autoClose: 6000,
      });

      if (failures.length > 0) {
        console.error('HDR merge failures', failures);
        toast.error(failures[0], { autoClose: 10000 });
      }

      if (summary.merged > 0) {
        // Re-merging overwrites a file in place, so its path is unchanged and
        // the thumbnail store keeps serving the previous render. The backend
        // keys its cache on modification time and would produce a fresh one,
        // but is never asked for it. Dropping these entries makes it ask.
        useProcessStore.getState().setProcess((state: any) => {
          const thumbnails = { ...state.thumbnails };
          savedPaths.forEach((path) => delete thumbnails[path]);
          return { thumbnails };
        });
        await refreshImageList?.();
      }

      return summary;
    },
    [refreshImageList],
  );

  return { mergeStacks };
}
