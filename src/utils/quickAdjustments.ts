import { Adjustments, INITIAL_ADJUSTMENTS } from './adjustments';
import { KeybindDefinition } from './keyboardUtils';

/**
 * Nudging one adjustment from the keyboard.
 *
 * Dragging is fine for finding a look and useless for the last tenth of a
 * stop. These are the controls you reach for once you know what you want:
 * exposure by a tenth, white balance by fifty Kelvin, straightening by a tenth
 * of a degree. Every one moves by the same step its slider does, so the number
 * lands where it would have landed anyway.
 *
 * Three come as standard and any slider can be added to the list, which is why
 * an adjustment is described by a path rather than a case in a switch: the same
 * nudge has to work for a control nobody thought about in advance.
 *
 * The three that ship come with keys, so a new install can nudge from the first
 * minute: Ctrl with up and down for exposure, Ctrl with left and right for white
 * balance, Shift with left and right to straighten. Anything added from a slider
 * starts with none, since there is no way to guess what is free for it.
 *
 * The step each press moves can be changed in Settings. The override lives in
 * the settings as `quickAdjustmentSteps`, by id, and is applied here, so the
 * keyboard and the list in Settings always agree on it.
 */

export interface QuickAdjustment {
  /** Stable, and part of the keybind action name, so it must not change. */
  id: string;
  /** Dotted path into the adjustments object. */
  path: string;
  step: number;
  min: number;
  max: number;
  /** Translation key, for the three that ship. */
  labelKey?: string;
  /** Literal label, for one added from a slider. */
  label?: string;
  /** Translation key naming the step in Settings, for the three that ship. */
  stepLabelKey?: string;
  /** Keys it comes with, for the three that ship. */
  keys?: { up: Array<string>; down: Array<string> };
}

export const BUILT_IN_QUICK_ADJUSTMENTS: Array<QuickAdjustment> = [
  {
    id: 'exposure',
    path: 'exposure',
    step: 0.1,
    min: -5,
    max: 5,
    labelKey: 'settings.keybinds.actions.quick_exposure',
    stepLabelKey: 'settings.keybinds.quickSteps.exposure',
    keys: { up: ['ctrl', 'ArrowUp'], down: ['ctrl', 'ArrowDown'] },
  },
  {
    id: 'temperature',
    // Kelvin lives one level down, since it only means anything alongside its
    // tint. Only reachable on a file with a camera profile; on anything else
    // there is no Kelvin to move.
    path: 'whiteBalance.kelvin',
    step: 50,
    min: 1667,
    max: 50000,
    labelKey: 'settings.keybinds.actions.quick_temperature',
    stepLabelKey: 'settings.keybinds.quickSteps.temperature',
    keys: { up: ['ctrl', 'ArrowRight'], down: ['ctrl', 'ArrowLeft'] },
  },
  {
    id: 'rotation',
    path: 'rotation',
    step: 0.1,
    min: -45,
    max: 45,
    labelKey: 'settings.keybinds.actions.quick_rotation',
    stepLabelKey: 'settings.keybinds.quickSteps.rotation',
    keys: { up: ['shift', 'ArrowRight'], down: ['shift', 'ArrowLeft'] },
  },
];

/** The keybind action name for one direction of one adjustment. */
export function quickActionName(id: string, direction: 'up' | 'down'): string {
  return `quick_${id}_${direction}`;
}

/**
 * Whether a step typed in Settings can be used: more than nothing, and no more
 * than the whole range, since a step past that could only ever hit a limit.
 */
export function isUsableStep(item: QuickAdjustment, step: number): boolean {
  return Number.isFinite(step) && step > 0 && step <= item.max - item.min;
}

/**
 * Everything on the list, the three built in plus whatever was added, each
 * with the step chosen in Settings when there is a usable one.
 */
export function allQuickAdjustments(
  custom?: Array<QuickAdjustment> | null,
  steps?: { [id: string]: number } | null,
): Array<QuickAdjustment> {
  const extra = (custom ?? []).filter(
    (item) => item && item.id && !BUILT_IN_QUICK_ADJUSTMENTS.some((builtIn) => builtIn.id === item.id),
  );
  return [...BUILT_IN_QUICK_ADJUSTMENTS, ...extra].map((item) => {
    const chosen = steps?.[item.id];
    return typeof chosen === 'number' && isUsableStep(item, chosen) ? { ...item, step: chosen } : item;
  });
}

/** The two keybind rows for one adjustment, one each way. */
export function quickKeybindDefinitionsFor(item: QuickAdjustment): Array<KeybindDefinition> {
  return [
    {
      action: quickActionName(item.id, 'up'),
      description: item.labelKey ? `${item.labelKey}_up` : `+ ${item.label ?? item.id}`,
      defaultCombo: item.keys?.up ?? [],
      section: 'quick' as const,
    },
    {
      action: quickActionName(item.id, 'down'),
      description: item.labelKey ? `${item.labelKey}_down` : `- ${item.label ?? item.id}`,
      defaultCombo: item.keys?.down ?? [],
      section: 'quick' as const,
    },
  ];
}

/**
 * Two keybind rows per adjustment, one each way, so they appear in Settings
 * beside everything else rather than in a list of their own.
 */
export function quickKeybindDefinitions(custom?: Array<QuickAdjustment> | null): Array<KeybindDefinition> {
  return allQuickAdjustments(custom).flatMap(quickKeybindDefinitionsFor);
}

function readPath(source: any, path: string): unknown {
  return path.split('.').reduce((value, key) => (value == null ? undefined : value[key]), source);
}

function writePath(source: any, path: string, value: number): any {
  const [head, ...rest] = path.split('.');
  if (rest.length === 0) {
    return { ...source, [head]: value };
  }
  return { ...source, [head]: writePath(source?.[head] ?? {}, rest.join('.'), value) };
}

/**
 * The adjustments after one press, or null when the press means nothing here.
 *
 * Null rather than an unchanged copy so the caller can leave the event alone
 * and let it fall through, instead of swallowing a key that did nothing.
 */
export function applyQuickAdjustment(
  adjustments: Adjustments,
  item: QuickAdjustment,
  direction: 'up' | 'down',
): Partial<Adjustments> | null {
  const current = readPath(adjustments, item.path);

  // A nested target that does not exist yet cannot be nudged from nothing:
  // white balance is null until the file's own as-shot value is known, and
  // guessing a starting point would silently re-white-balance the photo.
  if (typeof current !== 'number' || !Number.isFinite(current)) {
    if (item.path.includes('.')) {
      return null;
    }
    const fallback = INITIAL_ADJUSTMENTS[item.path as keyof Adjustments];
    if (typeof fallback !== 'number') {
      return null;
    }
    return applyStep(adjustments, item, direction, fallback);
  }

  return applyStep(adjustments, item, direction, current);
}

function applyStep(
  adjustments: Adjustments,
  item: QuickAdjustment,
  direction: 'up' | 'down',
  from: number,
): Partial<Adjustments> | null {
  const delta = direction === 'up' ? item.step : -item.step;
  const next = Math.min(item.max, Math.max(item.min, from + delta));

  // Rounded to the step, so repeated presses stay on the same grid rather than
  // drifting onto values the slider could never produce. Clamped again after
  // rounding, since a limit that is not a multiple of the step can be stepped
  // past on the way to the nearest grid line.
  const rounded = Number((Math.round(next / item.step) * item.step).toFixed(4));
  const snapped = Math.min(item.max, Math.max(item.min, rounded));
  if (snapped === from) {
    return null;
  }

  return writePath(adjustments, item.path, snapped);
}

/**
 * The adjustments after `steps` presses in one direction, or null if none of
 * them moved anything.
 *
 * Not one press multiplied by five. A press is a step onto a grid with limits
 * at both ends, so five of them stop where five of them would have stopped,
 * which is the only reason it is worth holding on to a count of presses rather
 * than to a number.
 *
 * Negative counts down. Zero is nothing, and so is a count that ran into a
 * limit on its first press.
 */
export function applyQuickSteps(
  adjustments: Adjustments,
  item: QuickAdjustment,
  steps: number,
): Partial<Adjustments> | null {
  if (!Number.isFinite(steps) || steps === 0) {
    return null;
  }
  const direction = steps > 0 ? 'up' : 'down';
  let current: any = adjustments;
  let moved = false;
  for (let index = 0; index < Math.abs(steps); index += 1) {
    const patch = applyQuickAdjustment(current, item, direction);
    // A limit, or a nested setting this file has never carried. Either way the
    // presses after it would not have moved anything either.
    if (!patch) {
      break;
    }
    current = { ...current, ...patch };
    moved = true;
  }
  return moved ? current : null;
}
