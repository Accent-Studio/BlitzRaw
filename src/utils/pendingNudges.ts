import { invoke } from '@tauri-apps/api/core';
import { Adjustments, INITIAL_ADJUSTMENTS } from './adjustments';
import { QuickAdjustment, applyQuickSteps } from './quickAdjustments';
import { useEditorStore } from '../store/useEditorStore';
import { debouncedSave, debouncedSetHistory } from '../hooks/useEditorActions';
import { Invokes } from '../components/ui/AppProperties';
import { actionForChange } from './currentAction';
import { recordAppAction, recordingFinished, recordingStarted } from './appHistory';
import { nameForKeys } from './historyNames';

/**
 * Presses that arrived while a photo was still opening.
 *
 * # What goes wrong without it
 *
 * Opening a photo is two waits, not one. The sidecar is read in a few
 * milliseconds; the raw is decoded in one to three seconds. A nudge pressed
 * anywhere in that window used to be handled by whichever piece of code
 * answered first, and the loser overwrote the winner:
 *
 * - Before the sidecar landed, the press was sent to the backend, which wrote
 *   the file. The sidecar read was already in flight and came back without the
 *   change. That stale value went into the store, and the auto-save then wrote
 *   it back over the file. The press showed on the thumbnail for a second or
 *   two and was then undone.
 * - After the sidecar landed but before the decode did, the press was refused
 *   outright, because the test for "can this photo be edited" was whether there
 *   was a decoded image to draw. Nothing happened at all until the decode
 *   finished.
 *
 * # The rule now
 *
 * A press is a press. It is counted here the moment it happens, and it is
 * counted as a **number of presses** rather than as a number: five presses is
 * five steps up from whatever this photo's own exposure turns out to be, which
 * is not knowable yet and does not need to be.
 *
 * When the photo's own values land, the count is spent on top of them. The
 * nudge is applied to the real value rather than to a guess, which is the only
 * version of this that is correct rather than merely quick. The preview and the
 * decoded raw will not look identical, but the press itself is never lost.
 *
 * Two things are kept per photo, and the difference matters:
 *
 * - `steps` is what has **not** reached the store yet, because the store was
 *   still holding the previous photo's numbers when the key went down. Spent
 *   when the sidecar lands, or handed to the backend if the photo is left
 *   first.
 * - `touched` says a nudge happened at all during this visit. It is what stops
 *   a background sidecar read from landing on top of an edit just made.
 *
 * The ledger is emptied when a photo's values land and when a photo is left.
 * Nothing in it outlives the opening of one photo.
 */

interface NudgeEntry {
  item: QuickAdjustment;
  /** Presses not yet reflected in the store. Signed; up is positive. */
  steps: number;
}

interface PathLedger {
  /** A nudge happened on this photo since it was opened. */
  touched: boolean;
  entries: Map<string, NudgeEntry>;
}

const ledgers = new Map<string, PathLedger>();

function ledgerFor(path: string): PathLedger {
  let ledger = ledgers.get(path);
  if (!ledger) {
    ledger = { touched: false, entries: new Map() };
    ledgers.set(path, ledger);
  }
  return ledger;
}

function unspentEntries(path: string): Array<NudgeEntry> {
  const ledger = ledgers.get(path);
  if (!ledger) {
    return [];
  }
  return [...ledger.entries.values()].filter((entry) => entry.steps !== 0);
}

/** Whether the store is holding this photo's own adjustments yet. */
export function adjustmentsAreLoadedFor(path: string): boolean {
  const store = useEditorStore.getState();
  return store.selectedImage?.path === path && store.adjustmentsPath === path;
}

/**
 * Counts one press against a photo.
 *
 * `applied` says the press has already gone into the store, which is only
 * possible once the store holds this photo's own numbers. An applied press
 * still marks the photo as touched, because a sidecar read still in flight
 * would otherwise land on top of it.
 */
export function recordNudge(
  path: string,
  item: QuickAdjustment,
  direction: 'up' | 'down',
  applied: boolean,
): void {
  const ledger = ledgerFor(path);
  ledger.touched = true;
  if (applied) {
    return;
  }
  const entry = ledger.entries.get(item.id);
  const delta = direction === 'up' ? 1 : -1;
  if (entry) {
    entry.steps += delta;
  } else {
    ledger.entries.set(item.id, { item, steps: delta });
  }
}

/** Whether this photo has been nudged since it was opened. */
export function wasNudged(path: string): boolean {
  return ledgers.get(path)?.touched ?? false;
}

/** Forgets everything held for a photo. */
export function clearPendingNudges(path: string): void {
  ledgers.delete(path);
}

/**
 * Moves the open photo by a number of presses, and writes the result.
 *
 * Shared by the key pressed just now and by the presses that were waiting for
 * the photo to open, because they are the same act and the awkward part is the
 * same for both.
 *
 * That awkward part is a nested setting the file has never carried. White
 * balance is the case: it is null until the camera's own as-shot value is
 * known, which is a property of the file rather than a constant, so there is
 * nothing to step from until the profile has been read. That read is
 * asynchronous, which is why this can finish after it has returned.
 *
 * `onCommit` is for the caller's own bookkeeping and runs once per write.
 *
 * Returns false only when there was nothing here to move.
 */
export function applyNudgeSteps(
  path: string,
  item: QuickAdjustment,
  steps: number,
  onCommit?: (merged: Adjustments) => void,
): boolean {
  if (!adjustmentsAreLoadedFor(path) || steps === 0) {
    return false;
  }

  const commit = (base: Adjustments, patch: Partial<Adjustments>) => {
    const merged = { ...base, ...patch } as Adjustments;
    useEditorStore.getState().setEditor({ adjustments: merged });
    // The same rate-limited push every slider uses, so a run of presses on one
    // adjustment stays one step in the history rather than becoming twenty.
    debouncedSetHistory(merged, path);
    // Saved from here rather than left to the render pipeline, which writes
    // nothing until there is a decoded image to draw. On a raw that is seconds
    // away, and moving on to the next photo before then would lose the press.
    debouncedSave(path, merged);
    onCommit?.(merged);
  };

  const current = useEditorStore.getState().adjustments;
  const patch = applyQuickSteps(current, item, steps);
  if (patch) {
    commit(current, patch);
    return true;
  }

  // Null is either a limit, or the nested setting above. Only the second is
  // worth another round trip.
  if (!item.path.startsWith('whiteBalance.')) {
    return false;
  }

  invoke<any>('get_white_balance_info', { path })
    .then((info) => {
      if (!info?.hasProfile || !adjustmentsAreLoadedFor(path)) {
        return;
      }
      const editor = useEditorStore.getState();
      if (editor.adjustments.whiteBalance) {
        return;
      }
      const seeded = {
        ...editor.adjustments,
        whiteBalance: { kelvin: info.asShotKelvin, tint: info.asShotTint },
      } as Adjustments;
      const seededPatch = applyQuickSteps(seeded, item, steps);
      if (seededPatch) {
        commit(seeded, seededPatch);
      }
    })
    .catch((err) => console.error('Failed to read as-shot white balance for a nudge:', err));
  return true;
}

/**
 * Spends the waiting presses on the photo's own values, now that they are here.
 *
 * Called by every load that fills the store with a photo's real adjustments,
 * immediately after it has done so. The store is read rather than passed in, so
 * whatever the load decided to put there is what gets nudged.
 *
 * The result is written the way a slider writes one: into the store, into the
 * history, and into the sidecar. It is an ordinary edit that happened to be
 * made early, so it undoes like one.
 */
export function settlePendingNudges(path: string): boolean {
  const waiting = unspentEntries(path);
  clearPendingNudges(path);
  if (waiting.length === 0) {
    return false;
  }
  // The photo was left while its sidecar was being read. Whoever left it has
  // already handed these to the backend; see `flushPendingNudges`.
  if (!adjustmentsAreLoadedFor(path)) {
    return false;
  }

  let moved = false;
  for (const entry of waiting) {
    if (applyNudgeSteps(path, entry.item, entry.steps)) {
      moved = true;
    }
  }
  return moved;
}

/**
 * Hands the waiting presses to the backend, because the photo is being left.
 *
 * The front end has nothing to apply them to: the sidecar read never came back,
 * so there is no starting value in the store and there is not going to be one.
 * The backend does its own read, modify and write per file, which is what is
 * needed here and is the same path the rest of a selection already takes.
 *
 * One call per press, in order. The backend takes the size of a step from the
 * size of the delta it is given and rounds onto that grid, so three presses
 * sent as a single delta of 0.3 would land the file on a multiple of 0.3
 * instead of a multiple of 0.1. Sequential rather than at once, because each
 * call reads the file it is about to write.
 */
export function flushPendingNudges(path: string | null | undefined): void {
  if (!path) {
    return;
  }
  const waiting = unspentEntries(path);
  clearPendingNudges(path);
  if (waiting.length === 0) {
    return;
  }

  const presses: Array<{ item: QuickAdjustment; delta: number }> = [];
  for (const entry of waiting) {
    const delta = entry.steps > 0 ? entry.item.step : -entry.item.step;
    for (let index = 0; index < Math.abs(entry.steps); index += 1) {
      presses.push({ item: entry.item, delta });
    }
  }

  presses
    .reduce(
      (chain, press) =>
        chain.then(() => {
          // BLITZRAW: presses on one setting, spent back to back, are one thing
          // the user did. The joining rule groups them without this having to
          // know how many there were. See utils/actionId.ts.
          const pressAction = actionForChange([press.item.path.split('.')[0]]);
          recordingStarted();
          return invoke(Invokes.NudgeAdjustmentsForPaths, {
            paths: [path],
            path: press.item.path,
            delta: press.delta,
            min: press.item.min,
            max: press.item.max,
            fallback: fallbackFor(press.item),
            historyAction: pressAction,
          }).then((photos: any) => {
            // BLITZRAW: a press spent after a decode is a thing I did, so it
            // goes into the list of what I did like any other. The hard rule:
            // anything that writes a step into a photo writes an entry there.
            recordAppAction({
              id: pressAction,
              kind: 'adjustments',
              label: nameForKeys([press.item.path.split('.')[0]]),
              photos: photos ?? [],
              selection: [path],
              openPath: path,
              inEditor: true,
            });
          }).finally(recordingFinished);
        }),
      Promise.resolve(),
    )
    .catch((err) => console.error('Failed to hand a waiting nudge to the backend:', err));
}

/**
 * Where a file that has never had this setting touched should start.
 *
 * White balance and anything else nested is left out on purpose: absent means
 * as-shot, which the backend reads from the camera profile rather than from a
 * constant.
 */
function fallbackFor(item: QuickAdjustment): number | null {
  if (item.path.includes('.')) {
    return null;
  }
  const value = (INITIAL_ADJUSTMENTS as any)[item.path];
  return typeof value === 'number' ? value : null;
}
