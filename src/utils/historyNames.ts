import { Adjustments } from './adjustments';
import { sameValue } from './editHistory';

/**
 * Naming each step of an edit history.
 *
 * The history is kept as whole states rather than as changes, so what a step
 * *was* has to be worked out by comparing it against the one before it. That is
 * what this does, and the result is what both the clock in the editor toolbar
 * and the History panel show.
 *
 * Lifted out of the toolbar unchanged when the panel arrived, so there is one
 * set of names and not two that drift apart. It kept a cache of previous names
 * in a ref, which made sense inside a component that re-rendered constantly and
 * makes none out here; the callers memoise instead.
 *
 * A step that named itself keeps its own name. Only actions that would read
 * badly under comparison bother: resetting a photo moves about forty things at
 * once and would otherwise read as a list of forty things rather than as the one
 * thing that happened. See `editHistory.ts`.
 */

/**
 * BLITZRAW: the readable name of one adjustment.
 *
 * Lifted out of `describeSteps` unchanged so the application's own history can
 * name an action the same way the photo's history names a step. One set of
 * names, not two that drift apart.
 */
export function nameForKey(k: string): string {
    const special: Record<string, string> = {
      aiPatches: 'AI Patches',
      aspectRatio: 'Aspect Ratio',
      flipHorizontal: 'Flip Horizontal',
      flipVertical: 'Flip Vertical',
      orientationSteps: 'Rotation',
      lutPath: 'LUT',
      lutIntensity: 'LUT Intensity',
      lutData: 'LUT Data',
      lutName: 'LUT Name',
      lutSize: 'LUT Size',
      chromaticAberrationBlueYellow: 'Chromatic Aberration Blue/Yellow',
      chromaticAberrationRedCyan: 'Chromatic Aberration Red/Cyan',
      centré: 'Centré',
      lumaNoiseReduction: 'Luma Noise Reduction',
      colorNoiseReduction: 'Color Noise Reduction',
      lensMaker: 'Lens Maker',
      lensModel: 'Lens Model',
      lensDistortionAmount: 'Lens Distortion',
      lensVignetteAmount: 'Lens Vignette',
      lensTcaAmount: 'Lens TCA',
      lensDistortionEnabled: 'Enable Lens Distortion',
      lensTcaEnabled: 'Enable Lens TCA',
      lensVignetteEnabled: 'Enable Lens Vignette',
      transformDistortion: 'Transform Distortion',
      transformVertical: 'Transform Vertical',
      transformHorizontal: 'Transform Horizontal',
      transformRotate: 'Transform Rotate',
      transformAspect: 'Transform Aspect',
      transformScale: 'Transform Scale',
      transformXOffset: 'Transform X Offset',
      transformYOffset: 'Transform Y Offset',
      colorGrading: 'Color Grading',
      colorCalibration: 'Color Calibration',
      toneMapper: 'Tone Mapper',
      showClipping: 'Show Clipping',
      sectionVisibility: 'Section Visibility',
      flareAmount: 'Flare Amount',
      glowAmount: 'Glow Amount',
      halationAmount: 'Halation Amount',
      grainAmount: 'Grain Amount',
      grainRoughness: 'Grain Roughness',
      grainSize: 'Grain Size',
      vignetteAmount: 'Vignette Amount',
      vignetteFeather: 'Vignette Feather',
      vignetteMidpoint: 'Vignette Midpoint',
      vignetteRoundness: 'Vignette Roundness',
      dehaze: 'Dehaze',
      exposure: 'Exposure',
      blacks: 'Blacks',
      whites: 'Whites',
      shadows: 'Shadows',
      highlights: 'Highlights',
      contrast: 'Contrast',
      brightness: 'Brightness',
      clarity: 'Clarity',
      structure: 'Structure',
      sharpness: 'Sharpness',
      saturation: 'Saturation',
      temperature: 'Temperature',
      tint: 'Tint',
      vibrance: 'Vibrance',
      hsl: 'HSL',
      curves: 'Curves',
      crop: 'Crop',
      masks: 'Masks',
      rating: 'Rating',
    };
    if (special[k]) return special[k];
    return k.replace(/([A-Z])/g, ' $1').replace(/^./, (str) => str.toUpperCase());
}

/**
 * BLITZRAW: the readable name of a set of adjustments that moved together.
 *
 * The same shortening the step list already uses: two names and an ellipsis,
 * because a list of forty is not a name.
 */
export function nameForKeys(keys: Array<string>): string {
  const named = keys.map(nameForKey);
  if (named.length === 0) return 'Edit';
  if (named.length > 2) return `${named.slice(0, 2).join(', ')}...`;
  return named.join(', ');
}

export function describeSteps(
  entries: Array<Adjustments> | null | undefined,
  labels?: Array<string | null> | null,
): Array<string> {
  if (!entries || entries.length === 0) return [];

  const formatKey = nameForKey;

  const newNames: Array<string> = [];

  for (let i = newNames.length; i < entries.length; i++) {
    if (i === 0) {
      newNames[i] = 'Initial State';
      continue;
    }

    const curr = entries[i];
    const prev = entries[i - 1];
    const changed: string[] = [];

    for (const key of Object.keys(curr)) {
      if (prev[key] === curr[key]) continue;
      // A reference test alone was enough while every state came from spreading
      // the one before it, because an untouched key kept its object. A state
      // rebuilt from the stored log does not: every key a step mentions is
      // parsed fresh, so it is a new object however identical its contents, and
      // each of those was being named as a change. A real history of a
      // multi-select edit read as "Lens Distortion Params" over and over.
      if (sameValue(prev[key], curr[key])) continue;

      if (key === 'masks') {
        const prevMasks = prev.masks || [];
        const currMasks = curr.masks || [];

        if (currMasks.length > prevMasks.length) changed.push('Added Mask');
        else if (currMasks.length < prevMasks.length) changed.push('Deleted Mask');
        else {
          currMasks.forEach((cMask: any) => {
            const pMask = prevMasks.find((m: any) => m.id === cMask.id);
            if (pMask) {
              if (pMask.opacity !== cMask.opacity) changed.push('Mask Opacity');
              if (pMask.invert !== cMask.invert) changed.push('Mask Invert');
              if (pMask.visible !== cMask.visible) changed.push('Mask Visibility');
              if (pMask.subMasks !== cMask.subMasks) changed.push('Mask Area / Brush');

              if (pMask.adjustments !== cMask.adjustments) {
                for (const adjKey of Object.keys(cMask.adjustments || {})) {
                  if (pMask.adjustments[adjKey] !== cMask.adjustments[adjKey]) {
                    changed.push(`Mask ${formatKey(adjKey)}`);
                  }
                }
              }
            }
          });
        }
      } else if (key === 'aiPatches') {
        const prevPatches = prev.aiPatches || [];
        const currPatches = curr.aiPatches || [];

        if (currPatches.length > prevPatches.length) changed.push('Added AI Patch');
        else if (currPatches.length < prevPatches.length) changed.push('Deleted AI Patch');
        else {
          currPatches.forEach((cPatch: any) => {
            const pPatch = prevPatches.find((p: any) => p.id === cPatch.id);
            if (pPatch) {
              if (pPatch.visible !== cPatch.visible) changed.push('AI Patch Visibility');
              if (pPatch.subMasks !== cPatch.subMasks) changed.push('AI Patch Area');
              if (pPatch.patchData !== cPatch.patchData || pPatch.prompt !== cPatch.prompt) {
                changed.push('AI Generation');
              }
            }
          });
        }
      } else {
        changed.push(formatKey(key));
      }
    }

    const uniqueChanged = Array.from(new Set(changed));

    if (uniqueChanged.length === 0) newNames[i] = 'Adjustment';
    else if (uniqueChanged.length > 2) newNames[i] = `${uniqueChanged.slice(0, 2).join(', ')}...`;
    else newNames[i] = uniqueChanged.join(', ');
  }


  // A step that named itself keeps its name. Resetting a photo moves forty
  // things at once, and comparing it against the state before reads as a
  // list of forty things rather than as the one thing that happened.
  return newNames.map((name, i) => labels?.[i] ?? name);
}
