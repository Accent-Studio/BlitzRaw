import { invoke } from '@tauri-apps/api/core';
import { Adjustments } from './adjustments';

/**
 * EXIF numbers arrive as strings, "14 mm" and the like, and the lens commands
 * take numbers. Passing the raw value straight through made the whole call
 * fail to deserialise, so autodetection found the lens and then came back with
 * no profile at all, which reads on screen as a detected lens whose correction
 * switches are greyed out.
 */
export const exifNumber = (value: unknown): number | null => {
  if (value === null || value === undefined) return null;
  const parsed = parseFloat(String(value));
  return Number.isNaN(parsed) ? null : parsed;
};

/**
 * Re-detecting the lens after adjustments arrive from somewhere else.
 *
 * A preset can carry `lensCorrectionMode: 'auto'` and the amounts to use, but
 * not a profile: the profile depends on the lens, focal length and aperture of
 * the photo being corrected, which is different for every photo the preset is
 * applied to. Copying one across would correct the wrong distortion.
 *
 * So the preset says "correct this lens automatically, by this much" and the
 * profile is looked up here, per photo. Without this the mode flips to auto and
 * nothing happens, because `lensDistortionParams` is only ever filled in on
 * save or bulk apply, never when a preset lands in the editor.
 *
 * Returns the adjustments unchanged when there is nothing to do, so callers can
 * apply it unconditionally.
 */
export async function resolveLensForImage(
  adjustments: Partial<Adjustments>,
  exif: Record<string, any> | null | undefined,
): Promise<Partial<Adjustments>> {
  if ((adjustments as any).lensCorrectionMode !== 'auto') {
    return adjustments;
  }

  const maker = exif?.Make || '';
  const model = exif?.LensModel || '';
  if (!model) {
    // Nothing to match on. Clear any profile that came in with the preset
    // rather than leaving another lens's correction applied to this photo.
    return { ...adjustments, lensMaker: null, lensModel: null, lensDistortionParams: null } as Partial<Adjustments>;
  }

  try {
    const detected: [string, string] | null = await invoke('autodetect_lens', { maker, model });
    if (!detected) {
      return { ...adjustments, lensMaker: null, lensModel: null, lensDistortionParams: null } as Partial<Adjustments>;
    }

    const [detectedMaker, detectedModel] = detected;
    const params = await invoke('get_lens_distortion_params', {
      maker: detectedMaker,
      model: detectedModel,
      focalLength: exifNumber(exif?.FocalLength),
      aperture: exifNumber(exif?.FNumber),
      distance: exifNumber(exif?.SubjectDistance),
    });

    return {
      ...adjustments,
      lensMaker: detectedMaker,
      lensModel: detectedModel,
      lensDistortionParams: params,
    } as Partial<Adjustments>;
  } catch (error) {
    console.error('Lens autodetection failed while applying a preset:', error);
    return adjustments;
  }
}
