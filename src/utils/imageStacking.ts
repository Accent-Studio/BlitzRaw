import { ImageFile } from '../components/ui/AppProperties';

/**
 * Collapsing user-confirmed stacks down to one visible frame.
 *
 * A stack holds several different captures, typically an exposure bracket.
 * That is a separate idea from `group_id`, which holds several files of one
 * capture, and the two nest: a three-shot bracket shot RAW+JPEG is three
 * groups inside one stack. Grouping runs first, so by the time this sees the
 * list each capture is already down to a single file.
 *
 * Which frame is left showing is `resolveLeaders`: an explicit leader, then a
 * derived result, then the first in the list's current order. For a bracket in
 * capture order that last one is the metered exposure, which is the one that
 * looks like the scene rather than the darkest or brightest frame.
 */

export interface StackInfo {
  /** How many frames the stack holds, including the visible one. */
  count: number;
  /** Whether the user has opened it. */
  isExpanded: boolean;
  /**
   * Where each frame sits in the stack, one-based, by path.
   *
   * Taken from the display list rather than from the file order, so the badge
   * counts down the screen the way the frames are actually drawn. Only the
   * leader is in here while the stack is closed, since it is the only frame
   * shown.
   */
  positions: Map<string, number>;
}

export interface StackingResult {
  displayList: Array<ImageFile>;
  /** Stack id to what the badge should show. Only for collapsed representatives. */
  stackInfo: Map<string, StackInfo>;
}

/**
 * Which frame leads each stack, in the order the caller handed them over.
 *
 * Three rules, in order. An explicit leader wins: that is a merge putting its
 * result on top of the bracket it came from. Failing that a derived output
 * leads, because a merged stack's deliverable is the merge and the frames
 * underneath are working files. Failing both, the first member in the order
 * given, which for a bracket in capture order is the metered exposure.
 *
 * The middle rule is not cosmetic, and it exists because of two real faults it
 * covers between them.
 *
 * The recorded leader lives in `.blitzraw-stacks.json` by file name, and
 * nothing prunes that file when a member is deleted outside the app. A merge
 * removed from disk leaves the record naming a file that is not there, so no
 * frame comes back carrying `is_stack_leader` and the stack quietly falls back
 * to its first ingredient. Every `leaderOnly` action then acts on an unedited
 * source frame instead of the merge: export writes the raw, delete removes the
 * wrong file.
 *
 * It also settles a disagreement between this function's two callers. The
 * display resolves leaders over the sorted list while actions resolve them
 * over `imageList` in directory order, and those two orders are not the same:
 * `localeCompare` puts `_DSC3479_Hdr.tiff` before `_DSC3479.dng` while the
 * directory puts it after. Naming the derived output outright makes both
 * agree by construction rather than by an accident of collation.
 */
export function resolveLeaders(images: Array<ImageFile>): Map<string, string> {
  const first = new Map<string, string>();
  const derived = new Map<string, string>();
  const explicit = new Map<string, string>();

  for (const image of images) {
    const id = image.stack_id;
    if (!id) continue;
    if (!first.has(id)) first.set(id, image.path);
    if (!derived.has(id) && isMergedOutput(image.path)) derived.set(id, image.path);
    if (image.is_stack_leader) explicit.set(id, image.path);
  }

  const leaders = new Map<string, string>();
  for (const [id, firstMember] of first) {
    leaders.set(id, explicit.get(id) ?? derived.get(id) ?? firstMember);
  }

  return leaders;
}

export function buildStacks(images: Array<ImageFile>, expandedStacks: Array<string>): StackingResult {
  const expanded = new Set(expandedStacks);
  const counts = new Map<string, number>();

  for (const image of images) {
    if (!image.stack_id) continue;
    counts.set(image.stack_id, (counts.get(image.stack_id) ?? 0) + 1);
  }

  const leaders = resolveLeaders(images);

  const stackInfo = new Map<string, StackInfo>();
  const displayList: Array<ImageFile> = [];

  for (const image of images) {
    const id = image.stack_id;
    if (!id) {
      displayList.push(image);
      continue;
    }

    const count = counts.get(id) ?? 1;

    // A stack whose other members were filtered away is not a stack any more.
    if (count < 2) {
      displayList.push(image);
      continue;
    }

    if (leaders.get(id) === image.path) {
      stackInfo.set(id, { count, isExpanded: expanded.has(id), positions: new Map() });
      displayList.push(image);
      continue;
    }

    if (expanded.has(id)) {
      displayList.push(image);
    }
  }

  // Numbered in one pass over what will be drawn, since a named leader is not
  // necessarily the first of its stack in file order and the badge has to agree
  // with the eye rather than with the directory.
  for (const image of displayList) {
    if (!image.stack_id) continue;
    const info = stackInfo.get(image.stack_id);
    if (!info) continue;
    info.positions.set(image.path, info.positions.size + 1);
  }

  return { displayList, stackInfo };
}

/**
 * Suffixes the backend gives a file it derived from several others.
 *
 * `save_hdr` writes `_Hdr`, `panorama_stitching` writes `_Pano` and the
 * collage writes `_Collage`. Only `_Hdr` used to be listed here, which meant a
 * panorama was treated as an ordinary capture: its stack looked leaderless,
 * and the panorama itself was not excluded from being fed back into a merge,
 * which is the whole reason this check exists.
 */
const DERIVED_SUFFIXES = ['_hdr', '_pano', '_collage'];

/** Whether a path is something the app produced by combining other frames. */
export function isMergedOutput(path: string): boolean {
  const name = path.split('?')[0].split(/[\/]/).pop() ?? '';
  const stem = name.replace(/\.[^.]+$/, '').toLowerCase();
  return DERIVED_SUFFIXES.some((suffix) => stem.endsWith(suffix));
}

export interface SelectedStack {
  stackId: string;
  /** Members in display order. The first names any output derived from it. */
  paths: Array<string>;
}

/**
 * The full stacks touched by a selection, one entry each.
 *
 * Selecting a single collapsed frame is enough to pull in its whole bracket,
 * which is the point: the other frames are hidden underneath it.
 *
 * Where a capture exists as several files, only one is returned. Handing an
 * HDR merge both a NEF and its JPEG would feed it the same exposure twice, so
 * the RAW wins and a DNG wins over that, matching how the library already
 * picks a representative.
 */
export function stacksForSelection(images: Array<ImageFile>, selectedPaths: Array<string>): Array<SelectedStack> {
  const selected = new Set(selectedPaths);
  const stackIds = new Set<string>();

  for (const image of images) {
    if (image.stack_id && selected.has(image.path)) {
      stackIds.add(image.stack_id);
    }
  }

  const byStack = new Map<string, Array<ImageFile>>();
  for (const image of images) {
    if (!image.stack_id || !stackIds.has(image.stack_id) || image.is_virtual_copy) continue;
    // A merged result joins the stack it came from, so without this a second
    // merge would take the first one's output as an input and blend a finished
    // HDR back over its own source frames.
    if (isMergedOutput(image.path)) continue;
    const bucket = byStack.get(image.stack_id);
    if (bucket) bucket.push(image);
    else byStack.set(image.stack_id, [image]);
  }

  const rank = (image: ImageFile): number => {
    const ext = image.path.split('?')[0].split('.').pop()?.toLowerCase();
    if (ext === 'dng') return 0;
    return image.is_raw ? 1 : 2;
  };

  const result: Array<SelectedStack> = [];

  for (const [stackId, members] of byStack) {
    // One file per capture. Without a group id a file stands alone, so key on
    // the path to keep it.
    const bestPerCapture = new Map<string, ImageFile>();
    for (const member of members) {
      const key = member.group_id ?? member.path;
      const current = bestPerCapture.get(key);
      if (!current || rank(member) < rank(current)) {
        bestPerCapture.set(key, member);
      }
    }

    // Preserve the order the members appeared in, not map insertion order.
    const chosen = new Set([...bestPerCapture.values()].map((m) => m.path));
    const paths = members.filter((m) => chosen.has(m.path)).map((m) => m.path);

    if (paths.length >= 2) {
      result.push({ stackId, paths });
    }
  }

  return result;
}

/**
 * Which stacks are open after toggling one of them.
 *
 * Pure, and separate from the hook that calls it, because which stacks are open
 * is one piece of state for the whole library and three places reach for it:
 * the badge in the grid, the badge in the filmstrip, and the button in the
 * bottom bar. The grid and the strip carried the same ten lines twice before
 * this, and this project has already paid once for two copies of one rule
 * drifting apart.
 */
export function toggledStack(expanded: Array<string>, stackId: string): Array<string> {
  return expanded.includes(stackId) ? expanded.filter((id) => id !== stackId) : [...expanded, stackId];
}

/**
 * The same for all of them at once: open every stack given, unless they are
 * already all open, in which case close them.
 *
 * One gesture rather than two buttons, matching what the badge on a single
 * stack already does. Closing empties the list rather than subtracting these
 * ids, because a stack that has left the screen, when the folder changed or a
 * filter hid it, would otherwise stay open with nothing left to close it.
 */
export function toggledAllStacks(expanded: Array<string>, stackIds: Array<string>): Array<string> {
  return stackIds.some((id) => !expanded.includes(id)) ? [...stackIds] : [];
}

/**
 * The stacks a selection touches, by id.
 *
 * Open ones as well as closed ones, which is what separates this from
 * `resolveSelection`: opening and closing a stack, and taking one apart, are
 * about the stack itself rather than about the photographs in it, so an open
 * stack is still a stack to them.
 */
export function stackIdsForSelection(images: Array<ImageFile>, paths: Array<string>): Array<string> {
  const selected = new Set(paths);
  const ids: Array<string> = [];
  const seen = new Set<string>();

  for (const image of images) {
    const id = image.stack_id;
    if (!id || seen.has(id) || !selected.has(image.path)) continue;
    seen.add(id);
    ids.push(id);
  }

  return ids;
}

/**
 * What a selection means when one of the things selected is a collapsed stack.
 *
 * Two kinds of stack want opposite things, so every action states which it
 * wants rather than there being one answer. A **merged** stack holds a result
 * the app derived, an HDR or a panorama, with the frames it was built from
 * underneath: the result is the deliverable and its ingredients are working
 * files. A **peer** stack is frames that all stand on their own, a bracket, a
 * burst, or a manual grouping, with no one of them more finished than the rest.
 *
 * Editing an HDR should not push those edits onto its brackets, where they
 * would mean nothing. Editing one frame of a burst usually should reach the
 * others. Same gesture, opposite intent, so the rule travels with the action.
 *
 * An **open** stack is not a stack for these purposes. Opening one is how you
 * say you want to work on its frames individually, so nothing expands and
 * nothing collapses.
 *
 * Every action names the rule it takes, and `selection.ts` is where those
 * rules are spelled out. That file is the table.
 */

export type StackKind = 'merged' | 'peer';

/** What an action wants from a collapsed stack. */
export type StackRule = 'fullStack' | 'leaderOnly';

export interface SelectionRule {
  merged: StackRule;
  peer: StackRule;
}

/** Both kinds behave the same. */
export const BOTH = (rule: StackRule): SelectionRule => ({ merged: rule, peer: rule });

/**
 * The rule almost every editing action takes: leave a merged result alone,
 * reach the whole of a peer stack.
 */
export const EDIT_RULE: SelectionRule = { merged: 'leaderOnly', peer: 'fullStack' };

interface StackFacts {
  kind: StackKind;
  leader: string;
  members: Array<string>;
}

/**
 * What each stack is and who leads it.
 *
 * The leader is whichever frame shows on top: named explicitly when a merge
 * put its result there, otherwise the first member in display order. It says
 * nothing about which kind the stack is, deliberately. Promoting a frame of a
 * burst changes the face of the stack, not what the stack is, so a future
 * where the best-rated frame rises to the top cannot silently turn a burst
 * into something that stops taking batch edits.
 */
function readStacks(images: Array<ImageFile>): Map<string, StackFacts> {
  const stacks = new Map<string, StackFacts>();
  const leaders = resolveLeaders(images);

  for (const image of images) {
    const id = image.stack_id;
    if (!id) continue;

    const existing = stacks.get(id);
    if (!existing) {
      stacks.set(id, {
        kind: isMergedOutput(image.path) ? 'merged' : 'peer',
        leader: leaders.get(id)!,
        members: [image.path],
      });
      continue;
    }

    existing.members.push(image.path);
    if (isMergedOutput(image.path)) {
      existing.kind = 'merged';
    }
  }

  return stacks;
}

/** Stacks that count: still two frames or more, and not opened by the user. */
function collapsedStacks(images: Array<ImageFile>, expandedStacks: Array<string>): Map<string, StackFacts> {
  const expanded = new Set(expandedStacks);
  const stacks = readStacks(images);

  for (const [id, facts] of [...stacks]) {
    // A stack filtered down to one frame is not a stack any more, which is the
    // same rule buildStacks applies when deciding what to draw.
    if (expanded.has(id) || facts.members.length < 2) {
      stacks.delete(id);
    }
  }

  return stacks;
}

/**
 * The paths an action should actually touch, given what the user clicked.
 *
 * Selection itself is never rewritten. `multiSelectedPaths` stays exactly what
 * was clicked, so anything that counts it or compares against it keeps working,
 * and each action resolves its own view of that selection here.
 */
export function resolveSelection(
  images: Array<ImageFile>,
  expandedStacks: Array<string>,
  paths: Array<string>,
  rule: SelectionRule,
): Array<string> {
  const stacks = collapsedStacks(images, expandedStacks);
  if (stacks.size === 0) {
    return paths;
  }

  const stackOfPath = new Map<string, string>();
  for (const [id, facts] of stacks) {
    for (const member of facts.members) {
      stackOfPath.set(member, id);
    }
  }

  const out: Array<string> = [];
  const seen = new Set<string>();
  const push = (path: string) => {
    if (!seen.has(path)) {
      seen.add(path);
      out.push(path);
    }
  };

  for (const path of paths) {
    const id = stackOfPath.get(path);
    if (!id) {
      push(path);
      continue;
    }

    const facts = stacks.get(id)!;
    if (rule[facts.kind] === 'fullStack') {
      facts.members.forEach(push);
    } else {
      // The leader rather than the path itself. Collapsing a stack while one
      // of its hidden frames is selected would otherwise act on a frame that
      // is not on screen.
      push(facts.leader);
    }
  }

  return out;
}

export interface SelectionSummary {
  /** Every file the selection stands for, counting whole collapsed stacks. */
  files: number;
  /** Of those, how many sit inside a collapsed stack. */
  stacked: number;
  /** How many collapsed stacks are represented. */
  stacks: number;
  /** Files selected on their own, including frames of an opened stack. */
  individual: number;
  /**
   * How many things were actually picked, a collapsed stack counting once.
   *
   * Not the same question as `files`, and both are worth reporting. This is
   * what you chose; `files` is what a delete would take, because deleting a
   * collapsed stack takes the whole bracket.
   */
  clicked: number;
}

/**
 * What to tell the user they have selected.
 *
 * Counts whole collapsed stacks rather than the one frame on screen, because
 * that is what most actions are about to touch, and a bracket quietly counting
 * as one file is how you delete three by accident.
 */
export function summariseSelection(
  images: Array<ImageFile>,
  expandedStacks: Array<string>,
  paths: Array<string>,
): SelectionSummary {
  const stacks = collapsedStacks(images, expandedStacks);
  const stackOfPath = new Map<string, string>();
  for (const [id, facts] of stacks) {
    for (const member of facts.members) {
      stackOfPath.set(member, id);
    }
  }

  const touched = new Set<string>();
  let individual = 0;

  for (const path of paths) {
    const id = stackOfPath.get(path);
    if (id) {
      touched.add(id);
    } else {
      individual += 1;
    }
  }

  let stacked = 0;
  for (const id of touched) {
    stacked += stacks.get(id)!.members.length;
  }

  return { files: stacked + individual, stacked, stacks: touched.size, individual, clicked: touched.size + individual };
}
