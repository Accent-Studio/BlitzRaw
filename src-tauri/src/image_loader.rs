use crate::Cursor;
use crate::app_settings::{AppSettings, load_settings};
use crate::app_state::{AppState, LoadedImage};
use crate::exif_processing;
use crate::file_management::{parse_virtual_path, read_file_mapped};
use crate::formats::is_raw_file;
use crate::image_processing::ImageMetadata;
use crate::image_processing::{
    apply_orientation, apply_srgb_to_linear, remove_raw_artifacts_and_enhance,
};
use crate::mask_generation::{MaskDefinition, SubMask, generate_mask_bitmap};
use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose};
use exif::{Reader as ExifReader, Tag};
use image::{DynamicImage, GenericImageView, ImageReader, imageops};
use rawler::Orientation;
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::panic;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Instant;

#[derive(serde::Serialize)]
pub struct LoadImageResult {
    pub width: u32,
    pub height: u32,
    pub metadata: ImageMetadata,
    pub exif: HashMap<String, String>,
    pub is_raw: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchMaskInfo {
    id: String,
    name: String,
    #[serde(default)]
    invert: bool,
    #[serde(default)]
    sub_masks: Vec<SubMask>,
}

fn srgb_to_linear_lut() -> &'static [f32; 256] {
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = [0.0f32; 256];
        for (i, v) in lut.iter_mut().enumerate() {
            let x = i as f32 / 255.0;
            *v = if x <= 0.04045 {
                x / 12.92
            } else {
                ((x + 0.055) / 1.055).powf(2.4)
            };
        }
        lut
    })
}

pub fn load_and_composite(
    base_image: &[u8],
    path: &str,
    adjustments: &Value,
    use_fast_raw_dev: bool,
    settings: &AppSettings,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
) -> Result<DynamicImage> {
    let base_image =
        load_base_image_from_bytes(base_image, path, use_fast_raw_dev, settings, cancel_token)?;
    composite_patches_on_image(&base_image, adjustments)
}

pub fn load_base_image_from_bytes(
    bytes: &[u8],
    path_for_ext_check: &str,
    use_fast_raw_dev: bool,
    settings: &AppSettings,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
) -> Result<DynamicImage> {
    let highlight_compression = settings.raw_highlight_compression.unwrap_or(2.5);
    let linear_mode = settings.linear_raw_mode.clone();
    let color_nr_setting = settings.raw_preprocessing_color_nr.unwrap_or(0.5);
    let color_nr_amount = if color_nr_setting <= 0.0 {
        0.0
    } else {
        let x = color_nr_setting.clamp(0.01, 1.0);
        (12.0 / x - 10.0).max(0.1)
    };
    let sharpening_amount = settings.raw_preprocessing_sharpening.unwrap_or(0.35);
    let apply_to_non_raws = settings.apply_preprocessing_to_non_raws.unwrap_or(false);

    crate::exif_processing::persist_exif_if_missing(
        Path::new(path_for_ext_check),
        path_for_ext_check,
        bytes,
    );

    if is_raw_file(path_for_ext_check) {
        match panic::catch_unwind(move || {
            // BLITZRAW: the develop now also reports the camera profile it
            // used, so the renderer can move the white balance later without
            // decoding the file again. See camera_profile.rs.
            crate::raw_processing::develop_raw_image_with_profile(
                bytes,
                use_fast_raw_dev,
                highlight_compression,
                linear_mode,
                cancel_token,
            )
        }) {
            Ok(Ok((mut image, profile))) => {
                // BLITZRAW: remember it against the path the render will use,
                // but never over a profile a merge recorded for itself.
                crate::camera_profile::remember_decoded(path_for_ext_check, profile);
                if !use_fast_raw_dev && (color_nr_amount > 0.0 || sharpening_amount > 0.0) {
                    let start = Instant::now();
                    remove_raw_artifacts_and_enhance(
                        &mut image,
                        color_nr_amount,
                        sharpening_amount,
                    );
                    let duration = start.elapsed();
                    log::info!(
                        "Raw enhancing for '{}' took {:?}",
                        path_for_ext_check,
                        duration
                    );
                }
                Ok(image)
            }
            Ok(Err(e)) => {
                let classified = classify_raw_develop_error(path_for_ext_check, e);

                if classified.to_string().contains("Load cancelled") {
                    return Err(classified);
                }

                log::warn!(
                    "Error developing RAW file '{}': {}",
                    path_for_ext_check,
                    classified
                );
                if let Some(preview) = safe_embedded_preview_fallback(bytes, path_for_ext_check) {
                    log::warn!(
                        "Using embedded preview fallback for '{}' ({}x{})",
                        path_for_ext_check,
                        preview.width(),
                        preview.height()
                    );

                    return Ok(linearize_embedded_preview(preview));
                }
                Err(classified)
            }
            Err(_) => {
                log::error!("Panic while processing RAW file: {}", path_for_ext_check);
                if let Some(preview) = safe_embedded_preview_fallback(bytes, path_for_ext_check) {
                    log::warn!(
                        "Using embedded preview fallback for '{}' after RAW decoder panic ({}x{})",
                        path_for_ext_check,
                        preview.width(),
                        preview.height()
                    );

                    return Ok(linearize_embedded_preview(preview));
                }
                Err(anyhow!(
                    "Failed to process RAW file: {}",
                    path_for_ext_check
                ))
            }
        }
    } else {
        let mut image = load_image_with_orientation(bytes, cancel_token)?;

        if apply_to_non_raws
            && !use_fast_raw_dev
            && (color_nr_amount > 0.0 || sharpening_amount > 0.0)
        {
            let start = Instant::now();
            remove_raw_artifacts_and_enhance(&mut image, color_nr_amount, sharpening_amount);
            let duration = start.elapsed();
            log::info!(
                "Enhancing non-RAW '{}' took {:?}",
                path_for_ext_check,
                duration
            );
        }

        Ok(image)
    }
}

fn classify_raw_develop_error(path: &str, err: anyhow::Error) -> anyhow::Error {
    let error_text = err.to_string();
    let lowered = error_text.to_ascii_lowercase();
    let unsupported_compression =
        lowered.contains("nef compression") && lowered.contains("not supported");

    if unsupported_compression {
        return anyhow!(
            "Unsupported RAW compression format for '{}'. Original error: {}",
            path,
            error_text
        );
    }

    err
}

fn largest_tiff_jpeg_preview(buf: &[u8]) -> Option<DynamicImage> {
    let le = match buf.get(..4)? {
        [0x49, 0x49, 0x2A, 0x00] => true,
        [0x4D, 0x4D, 0x00, 0x2A] => false,
        _ => return None,
    };
    let rd16 = |o: usize| -> Option<u64> {
        let b: [u8; 2] = buf.get(o..o + 2)?.try_into().ok()?;
        Some(if le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        } as u64)
    };
    let rd32 = |o: usize| -> Option<u64> {
        let b: [u8; 4] = buf.get(o..o + 4)?.try_into().ok()?;
        Some(if le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        } as u64)
    };

    let mut candidates: Vec<(u64, u64)> = Vec::new();
    let mut queue: Vec<u64> = vec![rd32(4)?];
    let mut seen = HashMap::new();

    while let Some(ifd) = queue.pop() {
        if seen.insert(ifd, ()).is_some() || seen.len() > 64 {
            continue;
        }
        let Some(n) = rd16(ifd as usize) else {
            continue;
        };

        let mut compression: u64 = 0;
        let mut strip: Option<(u64, u64)> = None;
        let mut old_jpeg: Option<(u64, u64)> = None;

        for i in 0..n {
            let e = ifd as usize + 2 + (i as usize) * 12;
            let (Some(tag), Some(count), Some(val)) = (rd16(e), rd32(e + 4), rd32(e + 8)) else {
                continue;
            };
            match tag {
                259 => compression = val,
                273 if count == 1 => strip = Some((val, strip.map_or(0, |s| s.1))),
                279 if count == 1 => strip = strip.map(|s| (s.0, val)).or(Some((0, val))),
                513 => old_jpeg = Some((val, old_jpeg.map_or(0, |s| s.1))),
                514 => old_jpeg = old_jpeg.map(|s| (s.0, val)).or(Some((0, val))),
                330 => {
                    if count == 1 {
                        queue.push(val);
                    } else {
                        for j in 0..count.min(8) {
                            if let Some(p) = rd32(val as usize + (j as usize) * 4) {
                                queue.push(p);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if matches!(compression, 6 | 7)
            && let Some(s) = strip
        {
            candidates.push(s);
        }
        if let Some(oj) = old_jpeg {
            candidates.push(oj);
        }
        if let Some(next) = rd32(ifd as usize + 2 + (n as usize) * 12)
            && next != 0
        {
            queue.push(next);
        }
    }

    candidates.sort_by_key(|&(_, len)| std::cmp::Reverse(len));

    for (off, len) in candidates {
        if let Some(bytes) = buf.get(off as usize..(off + len) as usize)
            && let Ok(img) = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg)
        {
            return Some(img);
        }
    }

    None
}

fn embedded_preview_fallback(bytes: &[u8], path: &str) -> Option<DynamicImage> {
    let img = match largest_tiff_jpeg_preview(bytes) {
        Some(img) => img,
        None => rawler::analyze::extract_preview_pixels(
            path,
            &rawler::decoders::RawDecodeParams::default(),
        )
        .ok()?,
    };

    let orientation = ExifReader::new()
        .read_from_container(&mut Cursor::new(bytes))
        .ok()
        .and_then(|exif| {
            exif.get_field(Tag::Orientation, exif::In::PRIMARY)?
                .value
                .get_uint(0)
        });

    Some(match orientation {
        Some(o) if o > 1 => apply_orientation(img, Orientation::from_u16(o as u16)),
        _ => img,
    })
}

fn safe_embedded_preview_fallback(bytes: &[u8], path: &str) -> Option<DynamicImage> {
    match panic::catch_unwind(panic::AssertUnwindSafe(|| {
        embedded_preview_fallback(bytes, path)
    })) {
        Ok(preview) => preview,
        Err(_) => {
            log::warn!("Embedded RAW preview extraction panicked for '{}'", path);
            None
        }
    }
}

fn linearize_embedded_preview(preview: DynamicImage) -> DynamicImage {
    let preview = DynamicImage::ImageRgb32F(preview.to_rgb32f());
    let mut linear_preview = apply_srgb_to_linear(preview).into_rgb32f();
    for pixel in linear_preview.pixels_mut() {
        pixel[0] *= 0.4;
        pixel[1] *= 0.4;
        pixel[2] *= 0.4;
    }
    DynamicImage::ImageRgb32F(linear_preview)
}

pub fn load_image_with_orientation(
    bytes: &[u8],
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
) -> Result<DynamicImage> {
    let check_cancel = || -> Result<()> {
        if let Some((tracker, generation)) = &cancel_token
            && tracker.load(Ordering::SeqCst) != *generation
        {
            return Err(anyhow!("Load cancelled"));
        }
        Ok(())
    };

    let cursor = Cursor::new(bytes);
    let mut reader = ImageReader::new(cursor.clone())
        .with_guessed_format()
        .context("Failed to guess image format")?;

    reader.no_limits();

    check_cancel()?;

    let image = reader.decode().context("Failed to decode image")?;
    check_cancel()?;

    let oriented_image = {
        let exif_reader = ExifReader::new();
        if let Ok(exif) = exif_reader.read_from_container(&mut cursor.clone()) {
            if let Some(orientation) = exif
                .get_field(Tag::Orientation, exif::In::PRIMARY)
                .and_then(|f| f.value.get_uint(0))
            {
                check_cancel()?;
                apply_orientation(image, Orientation::from_u16(orientation as u16))
            } else {
                image
            }
        } else {
            image
        }
    };

    Ok(DynamicImage::ImageRgb32F(oriented_image.to_rgb32f()))
}

pub fn composite_patches_on_image(
    base_image: &DynamicImage,
    current_adjustments: &Value,
) -> Result<DynamicImage> {
    composite_patches_on_image_scaled(base_image, current_adjustments, 1.0)
}

// ============ BLITZRAW: a patch has to know what it was drawn on ============
/// Composites the healing and cleanup patches onto a base that may be smaller
/// than the one they were drawn on.
///
/// A patch records `offsetX`, `offsetY`, `width` and `height` in the pixel
/// coordinates of the image that was open when it was made, which is the full
/// decode. Thumbnails and previews are built from a **fast RAW develop at half
/// or quarter size**, and this pasted the patch at its recorded coordinates
/// regardless. On a half-size decode a spot 12% across the frame was drawn 24%
/// across it and covered four times the area. That is the healed sensor dust
/// that came back after a restart enlarged and halfway to the middle, and it
/// corrected itself on opening the photo because the editor decodes at full
/// size, where the scale is 1 and there was never anything wrong.
///
/// `patch_scale` is the size of `base_image` relative to the full decode. The
/// caller knows it and the patch cannot: nothing stored in a patch says what it
/// was drawn against, and adding a field would leave every patch already on
/// disk still wrong.
pub fn composite_patches_on_image_scaled(
    base_image: &DynamicImage,
    current_adjustments: &Value,
    patch_scale: f32,
) -> Result<DynamicImage> {
    // Only ever a downscale. Anything else is a bug in the caller, and falling
    // back to the recorded size repeats the old wrongness rather than adding a
    // new one.
    let patch_scale = if patch_scale.is_finite() && patch_scale > 0.0 && patch_scale <= 1.0 {
        patch_scale
    } else {
        1.0
    };
    let to_base = |value: u32| -> u32 { ((value as f32) * patch_scale).round() as u32 };
    let to_base_size = |value: u32| -> u32 { to_base(value).max(1) };

    let patches_val = match current_adjustments.get("aiPatches") {
        Some(val) => val,
        None => return Ok(base_image.clone()),
    };

    let patches_arr = match patches_val.as_array() {
        Some(arr) if !arr.is_empty() => arr,
        _ => return Ok(base_image.clone()),
    };

    let visible_patches: Vec<&Value> = patches_arr
        .par_iter()
        .filter(|patch_obj| {
            let is_visible = patch_obj
                .get("visible")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if !is_visible {
                return false;
            }
            patch_obj
                .get("patchData")
                .and_then(|data| data.get("color"))
                .and_then(|color| color.as_str())
                .is_some_and(|s| !s.is_empty())
        })
        .collect();

    if visible_patches.is_empty() {
        return Ok(base_image.clone());
    }

    let (base_w, base_h) = base_image.dimensions();

    struct DecodedPatch {
        offset_x: Option<u32>,
        offset_y: Option<u32>,
        mask: image::GrayImage,
        color: image::RgbImage,
        is_srgb_encoded: bool,
    }

    let decoded_patches: Result<Vec<DecodedPatch>> = visible_patches
        .par_iter()
        .map(|patch_obj| {
            let patch_data = patch_obj.get("patchData").context("Missing patchData")?;
            // BLITZRAW: recorded against the full decode, so they are brought
            // into the base image's own coordinates before anything uses them.
            let offset_x = patch_data
                .get("offsetX")
                .and_then(|v| v.as_u64())
                .map(|v| to_base(v as u32));
            let offset_y = patch_data
                .get("offsetY")
                .and_then(|v| v.as_u64())
                .map(|v| to_base(v as u32));
            let is_cropped = offset_x.is_some() && offset_y.is_some();

            let is_srgb_encoded = patch_data
                .get("isSrgbEncoded")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let mask_bitmap = if let Some(mask_b64) = patch_data
                .get("mask")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                let mask_bytes = general_purpose::STANDARD.decode(mask_b64)?;
                let mask_img = image::load_from_memory(&mask_bytes)?.to_luma8();
                if !is_cropped && (mask_img.width() != base_w || mask_img.height() != base_h) {
                    imageops::resize(&mask_img, base_w, base_h, imageops::FilterType::Lanczos3)
                } else if is_cropped && patch_scale != 1.0 {
                    // BLITZRAW: the patch is a small picture measured in
                    // full-decode pixels. On a smaller base it has to shrink
                    // with the rest of the photo, or it covers the wrong area
                    // at the wrong size.
                    imageops::resize(
                        &mask_img,
                        to_base_size(mask_img.width()),
                        to_base_size(mask_img.height()),
                        imageops::FilterType::Lanczos3,
                    )
                } else {
                    mask_img
                }
            } else {
                let patch_info: PatchMaskInfo = serde_json::from_value((*patch_obj).clone())
                    .context("Failed to deserialize patch info for mask generation")?;

                let mask_def = MaskDefinition {
                    id: patch_info.id,
                    name: patch_info.name,
                    visible: true,
                    invert: patch_info.invert,
                    opacity: 100.0,
                    adjustments: Value::Null,
                    sub_masks: patch_info.sub_masks,
                };

                let orientation_steps = current_adjustments
                    .get("orientationSteps")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u8;
                let (trans_w, trans_h) = if orientation_steps % 2 == 1 {
                    (base_h, base_w)
                } else {
                    (base_w, base_h)
                };

                let mut gen_mask =
                    generate_mask_bitmap(&mask_def, trans_w, trans_h, 1.0, (0.0, 0.0), None)
                        .context("Failed to generate mask from sub_masks for compositing")?;

                gen_mask =
                    crate::image_processing::inverse_transform_mask(gen_mask, current_adjustments);

                if let (Some(ox), Some(oy)) = (offset_x, offset_y) {
                    // This mask was generated at the base image's own size, so
                    // the window cut out of it is in base pixels as well.
                    let w = patch_data
                        .get("width")
                        .and_then(|v| v.as_u64())
                        .map(|v| to_base_size(v as u32))
                        .unwrap_or(base_w);
                    let h = patch_data
                        .get("height")
                        .and_then(|v| v.as_u64())
                        .map(|v| to_base_size(v as u32))
                        .unwrap_or(base_h);
                    let crop_w = w.min(base_w.saturating_sub(ox));
                    let crop_h = h.min(base_h.saturating_sub(oy));
                    gen_mask = imageops::crop_imm(&gen_mask, ox, oy, crop_w, crop_h).to_image();
                }
                gen_mask
            };

            let color_b64 = patch_data
                .get("color")
                .and_then(|v| v.as_str())
                .context("Missing color data")?;
            let color_bytes = general_purpose::STANDARD.decode(color_b64)?;
            let color_image_u8 = image::load_from_memory(&color_bytes)?.to_rgb8();

            let (patch_w, patch_h) = color_image_u8.dimensions();
            let final_color = if !is_cropped && (base_w != patch_w || base_h != patch_h) {
                imageops::resize(
                    &color_image_u8,
                    base_w,
                    base_h,
                    imageops::FilterType::Lanczos3,
                )
            } else if is_cropped && patch_scale != 1.0 {
                // Resized to match the mask exactly rather than to its own
                // rounded size, since the two are read pixel for pixel below.
                imageops::resize(
                    &color_image_u8,
                    mask_bitmap.width(),
                    mask_bitmap.height(),
                    imageops::FilterType::Lanczos3,
                )
            } else {
                color_image_u8
            };

            Ok(DecodedPatch {
                offset_x,
                offset_y,
                mask: mask_bitmap,
                color: final_color,
                is_srgb_encoded,
            })
        })
        .collect();

    let decoded_patches = decoded_patches?;

    let mut composited_image = base_image.clone();
    let lut = srgb_to_linear_lut();

    let get_color = |patch: &DecodedPatch, r: u8, g: u8, b: u8| -> (f32, f32, f32) {
        if patch.is_srgb_encoded {
            (lut[r as usize], lut[g as usize], lut[b as usize])
        } else {
            (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
        }
    };

    match &mut composited_image {
        DynamicImage::ImageRgb32F(img_buf) => {
            for patch in decoded_patches {
                if let (Some(ox), Some(oy)) = (patch.offset_x, patch.offset_y) {
                    let max_x = (ox + patch.mask.width()).min(base_w);
                    let max_y = (oy + patch.mask.height()).min(base_h);

                    for y in oy..max_y {
                        let py = y - oy;
                        for x in ox..max_x {
                            let px = x - ox;
                            let mask_value = patch.mask.get_pixel(px, py)[0];
                            if mask_value > 0 {
                                let patch_pixel = patch.color.get_pixel(px, py);
                                let (pr, pg, pb) = get_color(
                                    &patch,
                                    patch_pixel[0],
                                    patch_pixel[1],
                                    patch_pixel[2],
                                );

                                let alpha = mask_value as f32 / 255.0;
                                let one_minus_alpha = 1.0 - alpha;

                                let base_px = img_buf.get_pixel_mut(x, y);
                                base_px[0] = pr * alpha + base_px[0] * one_minus_alpha;
                                base_px[1] = pg * alpha + base_px[1] * one_minus_alpha;
                                base_px[2] = pb * alpha + base_px[2] * one_minus_alpha;
                            }
                        }
                    }
                } else {
                    img_buf
                        .par_chunks_mut((base_w * 3) as usize)
                        .enumerate()
                        .for_each(|(y, row)| {
                            for x in 0..base_w as usize {
                                let mask_value = patch.mask.get_pixel(x as u32, y as u32)[0];
                                if mask_value > 0 {
                                    let patch_pixel = patch.color.get_pixel(x as u32, y as u32);
                                    let (pr, pg, pb) = get_color(
                                        &patch,
                                        patch_pixel[0],
                                        patch_pixel[1],
                                        patch_pixel[2],
                                    );

                                    let alpha = mask_value as f32 / 255.0;
                                    let one_minus_alpha = 1.0 - alpha;

                                    row[x * 3] = pr * alpha + row[x * 3] * one_minus_alpha;
                                    row[x * 3 + 1] = pg * alpha + row[x * 3 + 1] * one_minus_alpha;
                                    row[x * 3 + 2] = pb * alpha + row[x * 3 + 2] * one_minus_alpha;
                                }
                            }
                        });
                }
            }
        }
        DynamicImage::ImageRgba32F(img_buf) => {
            for patch in decoded_patches {
                if let (Some(ox), Some(oy)) = (patch.offset_x, patch.offset_y) {
                    let max_x = (ox + patch.mask.width()).min(base_w);
                    let max_y = (oy + patch.mask.height()).min(base_h);

                    for y in oy..max_y {
                        let py = y - oy;
                        for x in ox..max_x {
                            let px = x - ox;
                            let mask_value = patch.mask.get_pixel(px, py)[0];
                            if mask_value > 0 {
                                let patch_pixel = patch.color.get_pixel(px, py);
                                let (pr, pg, pb) = get_color(
                                    &patch,
                                    patch_pixel[0],
                                    patch_pixel[1],
                                    patch_pixel[2],
                                );

                                let alpha = mask_value as f32 / 255.0;
                                let one_minus_alpha = 1.0 - alpha;

                                let base_px = img_buf.get_pixel_mut(x, y);
                                base_px[0] = pr * alpha + base_px[0] * one_minus_alpha;
                                base_px[1] = pg * alpha + base_px[1] * one_minus_alpha;
                                base_px[2] = pb * alpha + base_px[2] * one_minus_alpha;
                            }
                        }
                    }
                } else {
                    img_buf
                        .par_chunks_mut((base_w * 4) as usize)
                        .enumerate()
                        .for_each(|(y, row)| {
                            for x in 0..base_w as usize {
                                let mask_value = patch.mask.get_pixel(x as u32, y as u32)[0];
                                if mask_value > 0 {
                                    let patch_pixel = patch.color.get_pixel(x as u32, y as u32);
                                    let (pr, pg, pb) = get_color(
                                        &patch,
                                        patch_pixel[0],
                                        patch_pixel[1],
                                        patch_pixel[2],
                                    );

                                    let alpha = mask_value as f32 / 255.0;
                                    let one_minus_alpha = 1.0 - alpha;

                                    row[x * 4] = pr * alpha + row[x * 4] * one_minus_alpha;
                                    row[x * 4 + 1] = pg * alpha + row[x * 4 + 1] * one_minus_alpha;
                                    row[x * 4 + 2] = pb * alpha + row[x * 4 + 2] * one_minus_alpha;
                                }
                            }
                        });
                }
            }
        }
        _ => {
            let mut rgba32_img = composited_image.to_rgba32f();
            for patch in decoded_patches {
                if let (Some(ox), Some(oy)) = (patch.offset_x, patch.offset_y) {
                    let max_x = (ox + patch.mask.width()).min(base_w);
                    let max_y = (oy + patch.mask.height()).min(base_h);
                    for y in oy..max_y {
                        let py = y - oy;
                        for x in ox..max_x {
                            let px = x - ox;
                            let mask_val = patch.mask.get_pixel(px, py)[0];
                            if mask_val > 0 {
                                let patch_px = patch.color.get_pixel(px, py);
                                let (pr, pg, pb) =
                                    get_color(&patch, patch_px[0], patch_px[1], patch_px[2]);

                                let alpha = mask_val as f32 / 255.0;
                                let one_minus_alpha = 1.0 - alpha;
                                let base_px = rgba32_img.get_pixel_mut(x, y);
                                base_px[0] = pr * alpha + base_px[0] * one_minus_alpha;
                                base_px[1] = pg * alpha + base_px[1] * one_minus_alpha;
                                base_px[2] = pb * alpha + base_px[2] * one_minus_alpha;
                            }
                        }
                    }
                } else {
                    for y in 0..base_h {
                        for x in 0..base_w {
                            let mask_val = patch.mask.get_pixel(x, y)[0];
                            if mask_val > 0 {
                                let patch_px = patch.color.get_pixel(x, y);
                                let (pr, pg, pb) =
                                    get_color(&patch, patch_px[0], patch_px[1], patch_px[2]);

                                let alpha = mask_val as f32 / 255.0;
                                let one_minus_alpha = 1.0 - alpha;
                                let base_px = rgba32_img.get_pixel_mut(x, y);
                                base_px[0] = pr * alpha + base_px[0] * one_minus_alpha;
                                base_px[1] = pg * alpha + base_px[1] * one_minus_alpha;
                                base_px[2] = pb * alpha + base_px[2] * one_minus_alpha;
                            }
                        }
                    }
                }
            }
            composited_image = DynamicImage::ImageRgba32F(rgba32_img);
        }
    }

    Ok(composited_image)
}

#[tauri::command]
pub fn is_image_cached(path: String, state: tauri::State<'_, AppState>) -> bool {
    let (source_path, _) = parse_virtual_path(&path);
    let source_path_str = source_path.to_string_lossy().to_string();
    state
        .decoded_image_cache
        .lock()
        .unwrap()
        .get(&source_path_str)
        .is_some()
}

#[tauri::command]
pub async fn load_image(
    path: String,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<LoadImageResult, String> {
    let my_generation = state.load_image_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let generation_tracker = state.load_image_generation.clone();
    let cancel_token = Some((generation_tracker.clone(), my_generation));

    {
        *state.original_image.lock().unwrap() = None;
        *state.cached_preview.lock().unwrap() = None;
        *state.gpu_image_cache.lock().unwrap() = None;
        *state.full_warped_cache.lock().unwrap() = None;
        *state.full_transformed_cache.lock().unwrap() = None;

        state.mask_cache.lock().unwrap().clear();
        state.patch_cache.lock().unwrap().clear();
        state.geometry_cache.lock().unwrap().clear();

        *state.denoise_result.lock().unwrap() = None;
        *state.hdr_result.lock().unwrap() = None;
        *state.panorama_result.lock().unwrap() = None;
    }

    let (source_path, sidecar_path) = parse_virtual_path(&path);
    let source_path_str = source_path.to_string_lossy().to_string();

    let metadata: ImageMetadata = crate::exif_processing::load_sidecar(&sidecar_path);

    let settings = load_settings(app_handle.clone()).unwrap_or_default();

    let path_clone = source_path_str.clone();

    let cached_data = state
        .decoded_image_cache
        .lock()
        .unwrap()
        .get(&source_path_str);

    let (pristine_arc, exif_data) = if let Some((cached_img, cached_exif)) = cached_data {
        (cached_img, cached_exif)
    } else {
        if crate::file_management::is_cloud_placeholder(&source_path) {
            return Err(format!(
                "'{}' is stored in iCloud and hasn't been downloaded yet. Download it in Finder, then try again.",
                source_path_str
            ));
        }

        // ============ BLITZRAW: one photo decodes at a time ============
        // Wait for whatever is decoding now, and only then ask whether this
        // request is still wanted.
        //
        // Deciding before the wait would decide too early: at the moment a
        // request arrives it is always the newest one. Running through five
        // photos puts five requests here, and by the time the second reaches
        // the front the fifth has arrived behind it. So the second, third and
        // fourth turn around at this line having read nothing from disk and
        // demosaiced nothing, and the photo actually on screen is the only one
        // that costs anything.
        //
        // This is also why the check is not a cancellation. A demosaic already
        // running cannot be stopped; rawler offers no way in, and the cheapest
        // decode is the one that never starts.
        let _decode_slot = state.editor_decode_slot.clone().lock_owned().await;
        if state.load_image_generation.load(Ordering::SeqCst) != my_generation {
            return Err("Load cancelled".to_string());
        }

        /// Clears the flag however the load ends, including on an early return.
        struct DecodeBusy(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for DecodeBusy {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        state.editor_decode_busy.store(true, Ordering::SeqCst);
        let _busy = DecodeBusy(state.editor_decode_busy.clone());
        // ========== BLITZRAW END: one photo decodes at a time ==========

        let (pristine_img, exif_data_loaded) = tokio::task::spawn_blocking(move || {
            if generation_tracker.load(Ordering::SeqCst) != my_generation {
                return Err("Load cancelled".to_string());
            }

            let result: Result<(DynamicImage, HashMap<String, String>), String> =
                (|| match read_file_mapped(Path::new(&path_clone)) {
                    Ok(mmap) => {
                        if generation_tracker.load(Ordering::SeqCst) != my_generation {
                            return Err("Load cancelled".to_string());
                        }

                        let img = load_base_image_from_bytes(
                            &mmap,
                            &path_clone,
                            false,
                            &settings,
                            cancel_token.clone(),
                        )
                        .map_err(|e| e.to_string())?;
                        let exif = exif_processing::read_exif_data(&path_clone, &mmap);
                        Ok((img, exif))
                    }
                    Err(e) => {
                        log::warn!(
                            "Failed to memory-map file '{}': {}. Falling back to standard read.",
                            path_clone,
                            e
                        );
                        let bytes = fs::read(&path_clone).map_err(|io_err| {
                            format!("Fallback read failed for {}: {}", path_clone, io_err)
                        })?;

                        if generation_tracker.load(Ordering::SeqCst) != my_generation {
                            return Err("Load cancelled".to_string());
                        }

                        let img = load_base_image_from_bytes(
                            &bytes,
                            &path_clone,
                            false,
                            &settings,
                            cancel_token.clone(),
                        )
                        .map_err(|e| e.to_string())?;
                        let exif = exif_processing::read_exif_data(&path_clone, &bytes);
                        Ok((img, exif))
                    }
                })();
            result
        })
        .await
        .map_err(|e| e.to_string())??;

        let arc_img = Arc::new(pristine_img);

        state.decoded_image_cache.lock().unwrap().insert(
            source_path_str.clone(),
            arc_img.clone(),
            exif_data_loaded.clone(),
        );

        (arc_img, exif_data_loaded)
    };

    if state.load_image_generation.load(Ordering::SeqCst) != my_generation {
        return Err("Load cancelled".to_string());
    }

    let is_raw = is_raw_file(&source_path_str);

    if state.load_image_generation.load(Ordering::SeqCst) != my_generation {
        return Err("Load cancelled".to_string());
    }

    let (orig_width, orig_height) = pristine_arc.dimensions();

    *state.original_image.lock().unwrap() = Some(LoadedImage {
        path,
        image: pristine_arc,
        is_raw,
    });

    Ok(LoadImageResult {
        width: orig_width,
        height: orig_height,
        metadata,
        exif: exif_data,
        is_raw,
    })
}

#[cfg(test)]
mod hdr_dng_probe {
    use super::*;
    use rawler::dng::writer::DngWriter;
    use rawler::dng::{DNG_VERSION_V1_6, DngCompression};
    use rawler::tags::DngTag;

    /// The whole thing on a real merge: write it as we now write merges, then
    /// open it as the app will have to.
    #[test]
    fn report_real_merge_round_trip() {
        let Ok(path) = std::env::var("RAPIDRAW_TEST_HDR_TIFF") else {
            eprintln!("RAPIDRAW_TEST_HDR_TIFF unset, skipping");
            return;
        };
        let on_disk = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

        let mut reader = ImageReader::open(&path)
            .expect("open")
            .with_guessed_format()
            .expect("format");
        reader.no_limits();
        let original = reader.decode().expect("decode");
        eprintln!(
            "merge: {}x{}, {:.0} MB as the TIFF",
            original.width(),
            original.height(),
            on_disk as f64 / 1e6
        );

        let out = std::env::temp_dir().join("blitzraw-real-merge.dng");
        let start = std::time::Instant::now();
        crate::hdr_dng::write_linear_jxl_dng(&out, &original, crate::hdr_dng::DEFAULT_DISTANCE)
            .expect("write");
        let written = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        eprintln!(
            "written in {:?} -> {:.1} MB, {:.0}x smaller",
            start.elapsed(),
            written as f64 / 1e6,
            on_disk as f64 / written.max(1) as f64
        );

        let settings = AppSettings::default();
        let bytes = std::fs::read(&out).expect("read");
        let start = std::time::Instant::now();
        let decoded =
            load_base_image_from_bytes(&bytes, &out.to_string_lossy(), false, &settings, None)
                .expect("our loader should open what we wrote");
        eprintln!(
            "opened in {:?} -> {}x{} {:?}",
            start.elapsed(),
            decoded.width(),
            decoded.height(),
            decoded.color()
        );
        assert_eq!(
            (decoded.width(), decoded.height()),
            (original.width(), original.height()),
            "full size, not the thumbnail the loader falls back to"
        );

        // The two sides are in different spaces on purpose. A merge is
        // written display-referred, which is what the TIFF holds, and the DNG
        // says so with a linearization table, so what comes back out of the
        // loader is linear. Putting the curve back on is what makes the
        // comparison mean anything, and it is also the check: skip it and
        // every sample reads as a huge error, which is precisely what the file
        // used to do to the screen.
        let to_srgb = |x: f32| -> f32 {
            let x = x.clamp(0.0, 1.0);
            if x <= 0.0031308 {
                x * 12.92
            } else {
                1.055 * x.powf(1.0 / 2.4) - 0.055
            }
        };
        let a = original.to_rgb32f();
        let b = decoded.to_rgb32f();
        let mut worst = 0f32;
        let mut sum_sq = 0f64;
        // Signed and per channel, which is what tells a systematic shift from
        // codec noise. A colour matrix that is not quite the inverse of the
        // one the pipeline applies shows up as a bias here and as nothing at
        // all in a mean of absolute values.
        let mut bias = [0f64; 3];
        let mut counts = [0f64; 3];
        for (i, (x, y)) in a.as_raw().iter().zip(b.as_raw().iter()).enumerate() {
            let d = (*x - to_srgb(*y)) * 255.0;
            worst = worst.max(d.abs());
            sum_sq += (d as f64) * (d as f64);
            bias[i % 3] += d as f64;
            counts[i % 3] += 1.0;
        }
        eprintln!(
            "pixels, in display units out of 255: rms {:.2}, worst {worst:.1}",
            (sum_sq / a.as_raw().len() as f64).sqrt()
        );
        eprintln!(
            "mean signed error per channel: r {:+.3}  g {:+.3}  b {:+.3}",
            bias[0] / counts[0],
            bias[1] / counts[1],
            bias[2] / counts[2]
        );
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_file(out.with_extension("dng.rrdata"));
    }

    /// Where the time goes between pressing Merge and having a file.
    ///
    /// A bulk run over 32 brackets took about seventeen minutes, against the
    /// 2.3 seconds an encode was measured at, so the encode is plainly not the
    /// story. This times the two things `save_hdr` and `merge_hdr` do to a
    /// finished merge, using a real one as the stand-in for the merged buffer.
    /// The decodes that come before are the other half and are measured by
    /// `report_preview_render_cost`.
    #[test]
    fn report_merge_save_cost() {
        let Ok(path) = std::env::var("RAPIDRAW_TEST_HDR_TIFF") else {
            eprintln!("RAPIDRAW_TEST_HDR_TIFF unset, skipping");
            return;
        };
        let mut reader = ImageReader::open(&path)
            .expect("open")
            .with_guessed_format()
            .expect("format");
        reader.no_limits();
        let merged = reader.decode().expect("decode");
        eprintln!("merged buffer: {}x{}", merged.width(), merged.height());

        // What merge_hdr does before the dialog can show anything: a
        // full-resolution PNG of the merge, base64'd into an event payload.
        let start = std::time::Instant::now();
        let rgb8 = merged.to_rgb8();
        let to_rgb8 = start.elapsed();
        let start = std::time::Instant::now();
        let mut buf = std::io::Cursor::new(Vec::new());
        rgb8.write_to(&mut buf, image::ImageFormat::Png)
            .expect("png");
        let png = start.elapsed();
        let start = std::time::Instant::now();
        let encoded =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, buf.get_ref());
        let b64 = start.elapsed();
        eprintln!(
            "preview: to_rgb8 {to_rgb8:?} + png {png:?} ({:.0} MB) + base64 {b64:?} ({:.0} MB of payload)",
            buf.get_ref().len() as f64 / 1e6,
            encoded.len() as f64 / 1e6
        );
        drop(rgb8);
        drop(encoded);

        let write_once = |label: &str| {
            let out = std::env::temp_dir().join("blitzraw-merge-save-cost.dng");
            let start = std::time::Instant::now();
            crate::hdr_dng::write_linear_jxl_dng(&out, &merged, crate::hdr_dng::DEFAULT_DISTANCE)
                .expect("write");
            eprintln!("dng write {label}: {:?}", start.elapsed());
            let _ = std::fs::remove_file(&out);
        };

        // The same call twice, with several hundred megabytes held across the
        // second one, to test whether memory pressure explains why one merge in
        // three in a bulk run takes twice as long as the rest.
        //
        // It does not: 3.48s against 3.44s. The hypothesis came from seeing
        // this write take 12.8s once, in a run that happened to hold about a
        // gigabyte of measurement buffers, and that turned out to be the
        // machine being busy rather than anything about the memory. Kept
        // because a negative result nobody wrote down gets re-derived.
        write_once("with nothing else held");

        // The one new cost of warming both caches from the merge in hand. The
        // rest of that work replaces two decodes of the file just written.
        let start = std::time::Instant::now();
        let linear = crate::hdr_dng::as_decoded(merged.clone(), std::path::Path::new("a.dng"));
        eprintln!(
            "merge into the space a decode gives back: {:?}",
            start.elapsed()
        );
        drop(linear);

        // Whether the thumbnail is worth building from the 16-bit buffer the
        // writer makes anyway rather than from the full float image. It is not:
        // the 16-bit route measures slower, 725ms against 653ms, before even
        // counting the 349ms to make the copy. The float image is already the
        // cheaper source.
        let start = std::time::Instant::now();
        let from_float = merged
            .resize(256, 256, image::imageops::FilterType::Triangle)
            .to_rgb8();
        let thumb_from_float = start.elapsed();
        let start = std::time::Instant::now();
        let rgb16 = merged.to_rgb16();
        let to_rgb16 = start.elapsed();
        let start = std::time::Instant::now();
        let from_int = DynamicImage::ImageRgb16(rgb16)
            .resize(256, 256, image::imageops::FilterType::Triangle)
            .to_rgb8();
        let thumb_from_int = start.elapsed();
        let worst = from_float
            .as_raw()
            .iter()
            .zip(from_int.as_raw().iter())
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap_or(0);
        eprintln!(
            "thumbnail: {thumb_from_float:?} from the float image, {thumb_from_int:?} from a              16-bit copy ({to_rgb16:?} to make), worst channel difference {worst}"
        );

        // Held, so the allocator cannot hand the pages back before the retry.
        let ballast = merged.to_rgb16();
        write_once("with a 272 MB buffer held");
        eprintln!("(ballast was {} samples)", ballast.as_raw().len());
    }

    /// Why a Lightroom JPEG XL HDR DNG comes back as its 256x171 thumbnail.
    ///
    /// The loader swallows the reason: a failed develop falls back to the
    /// embedded preview and only logs, and a panic inside the decoder does the
    /// same. This calls the layers underneath one at a time so the first one to
    /// fail says so.
    #[test]
    fn diagnose_lightroom_hdr_dng_read() {
        let Ok(path) = std::env::var("RAPIDRAW_TEST_LR_HDR_DNG") else {
            eprintln!("RAPIDRAW_TEST_LR_HDR_DNG unset, skipping");
            return;
        };
        let bytes = std::fs::read(&path).expect("read");
        eprintln!("file: {} ({:.1} MB)", path, bytes.len() as f64 / 1e6);

        let source = rawler::rawsource::RawSource::new_from_slice(&bytes);

        let decoder = match rawler::get_decoder(&source) {
            Ok(d) => {
                eprintln!("1. get_decoder: ok");
                d
            }
            Err(e) => {
                eprintln!("1. get_decoder FAILED: {e:?}");
                return;
            }
        };

        match decoder.raw_metadata(&source, &rawler::decoders::RawDecodeParams::default()) {
            Ok(_) => eprintln!("2. raw_metadata: ok"),
            Err(e) => eprintln!("2. raw_metadata FAILED: {e:?}"),
        }

        // The one that matters. Wrapped, because the loader's other fallback
        // path exists for a decoder that panics rather than returns.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            decoder.raw_image(
                &source,
                &rawler::decoders::RawDecodeParams::default(),
                false,
            )
        }));
        match result {
            Ok(Ok(raw)) => {
                eprintln!(
                    "3. raw_image: ok -> {}x{}, cpp {}, bps {}",
                    raw.width, raw.height, raw.cpp, raw.bps
                );
                eprintln!(
                    "   whitelevel {:?}  blacklevel {:?}",
                    raw.whitelevel, raw.blacklevel
                );
                eprintln!(
                    "   crop_area {:?}  active_area {:?}",
                    raw.crop_area, raw.active_area
                );
            }
            Ok(Err(e)) => eprintln!("3. raw_image FAILED: {e:?}"),
            Err(_) => eprintln!("3. raw_image PANICKED"),
        }

        // And the whole of our own path, for the record.
        let settings = AppSettings::default();
        match load_base_image_from_bytes(&bytes, &path, false, &settings, None) {
            Ok(img) => eprintln!(
                "4. our loader: {}x{} ({:?})",
                img.width(),
                img.height(),
                img.color()
            ),
            Err(e) => eprintln!("4. our loader FAILED: {e}"),
        }
    }

    /// The number that decides whether a JPEG XL merge is usable day to day:
    /// what it costs to open one, against what a TIFF costs today.
    ///
    /// Opening a merge is not the 1.42s a Z9 raw takes. A merge is already
    /// demosaiced, so the current cost is a file read and a TIFF parse. What
    /// has to be compared is that against a JPEG XL decode plus the much
    /// smaller read, and the read is not nothing: 545 MB off a disk is real
    /// time that 24 MB is not.
    #[test]
    fn report_merge_open_cost() {
        let Ok(path) = std::env::var("RAPIDRAW_TEST_HDR_TIFF") else {
            eprintln!("RAPIDRAW_TEST_HDR_TIFF unset, skipping");
            return;
        };
        let on_disk = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

        // What the app does today, timed as a whole: bytes off the disk, then
        // the TIFF decoded into the buffer the pipeline works on.
        let start = std::time::Instant::now();
        let tiff_bytes = std::fs::read(&path).expect("read tiff");
        let read_time = start.elapsed();
        let start = std::time::Instant::now();
        let mut reader = image::ImageReader::new(std::io::Cursor::new(&tiff_bytes))
            .with_guessed_format()
            .expect("format");
        reader.no_limits();
        let img = reader.decode().expect("decode tiff");
        let parse_time = start.elapsed();
        eprintln!(
            "tiff today: {:.0} MB, read {read_time:?} + parse {parse_time:?} = {:?}",
            on_disk as f64 / 1e6,
            read_time + parse_time
        );

        let rgb16 = img.to_rgb16();
        drop(img);
        drop(tiff_bytes);
        let (w, h) = (rgb16.width(), rgb16.height());
        let pixels: Vec<u8> = bytemuck::cast_slice(rgb16.as_raw().as_slice()).to_vec();
        drop(rgb16);

        for distance in [0.1f32, 0.15, 0.25] {
            let encoded = match jxl_encoder::LossyConfig::new(distance).encode(
                &pixels,
                w,
                h,
                jxl_encoder::PixelLayout::Rgb16,
            ) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("distance {distance}: encode failed: {e}");
                    continue;
                }
            };

            // Decoded twice: the first pass warms whatever the decoder sets up
            // once, and the second is what opening the photo would actually
            // cost the second time round.
            let mut best = std::time::Duration::from_secs(9999);
            for _ in 0..2 {
                let start = std::time::Instant::now();
                let image = jxl_oxide::JxlImage::builder()
                    .read(encoded.as_slice())
                    .expect("read jxl");
                let frame = image.render_frame(0).expect("render");
                let mut stream = frame.stream_no_alpha();
                let mut decoded = vec![0u16; (w as usize) * (h as usize) * 3];
                for row in decoded.chunks_mut(w as usize * 3) {
                    stream.write_to_buffer(row);
                }
                best = best.min(start.elapsed());
            }
            eprintln!(
                "jxl distance {distance}: {:.1} MB, decode {best:?}  ({:.1}x the whole TIFF open today)",
                encoded.len() as f64 / 1e6,
                best.as_secs_f64() / (read_time + parse_time).as_secs_f64(),
            );
        }
    }

    /// Whether a lossy merge survives the tone curve, which is the only place
    /// the damage would ever be seen.
    ///
    /// The premise this was first written on was wrong, and the correction is
    /// worth keeping because it is the same mistake that made merges open too
    /// bright. A merge is **not** scene-linear: `merge_hdr` finishes with
    /// `apply_linear_to_srgb`, so what is stored is already display-referred,
    /// measured at a median of 0.468 on a real merge where scene-linear would
    /// be 0.186. That happens to be exactly what butteraugli's model wants, so
    /// the bits already land where the eye is and the numbers below were fine;
    /// what was wrong was the reason given for them, and the extra curve this
    /// applied on top of one that was already there.
    #[test]
    fn report_jpegxl_roundtrip_quality() {
        let Ok(path) = std::env::var("RAPIDRAW_TEST_HDR_TIFF") else {
            eprintln!("RAPIDRAW_TEST_HDR_TIFF unset, skipping");
            return;
        };

        let mut reader = ImageReader::open(&path)
            .expect("open")
            .with_guessed_format()
            .expect("format");
        reader.no_limits();
        let img = reader.decode().expect("decode");
        let rgb16 = img.to_rgb16();
        drop(img);
        let (w, h) = (rgb16.width(), rgb16.height());
        let original: Vec<u16> = rgb16.as_raw().clone();
        drop(rgb16);
        let pixels: Vec<u8> = bytemuck::cast_slice(original.as_slice()).to_vec();

        // The stored samples are display-referred already, so this is a scale
        // and nothing more. It used to raise them to 1/2.4 on the way, which
        // curved a curve and overstated every shadow error it reported.
        let to_display = |v: u16| -> f32 { v as f32 / 65535.0 * 255.0 };

        // Stored through the same curve, and inverted on the way back. This is
        // the point of the comparison: linear samples give the encoder uniform
        // absolute precision when what matters is uniform relative precision,
        // so every bit it spends near white is a bit it did not spend near
        // black. Half-float, which is what Adobe stores, buys the same thing a
        // different way.
        let curved: Vec<u16> = original
            .iter()
            .map(|v| (((*v as f32 / 65535.0).powf(1.0 / 2.4)) * 65535.0).round() as u16)
            .collect();
        let curved_bytes: Vec<u8> = bytemuck::cast_slice(curved.as_slice()).to_vec();
        drop(curved);

        for (stored_curve, distance) in [(false, 0.1f32), (false, 0.25), (false, 0.5), (false, 1.0)]
        {
            let source = if stored_curve { &curved_bytes } else { &pixels };
            let encoded = match jxl_encoder::LossyConfig::new(distance).encode(
                source,
                w,
                h,
                jxl_encoder::PixelLayout::Rgb16,
            ) {
                Ok(data) => data,
                Err(e) => {
                    eprintln!("distance {distance}: encode failed: {e}");
                    continue;
                }
            };

            let image = match jxl_oxide::JxlImage::builder().read(encoded.as_slice()) {
                Ok(i) => i,
                Err(e) => {
                    eprintln!("distance {distance}: read back failed: {e:?}");
                    continue;
                }
            };
            let frame = image.render_frame(0).expect("render");
            let mut stream = frame.stream_no_alpha();
            let mut decoded = vec![0u16; original.len()];
            for row in decoded.chunks_mut(w as usize * 3) {
                stream.write_to_buffer(row);
            }
            if stored_curve {
                // Back to linear, so both sides of the comparison mean the
                // same thing and the curve below is applied once, not twice.
                for v in decoded.iter_mut() {
                    *v = (((*v as f32 / 65535.0).powf(2.4)) * 65535.0).round() as u16;
                }
            }

            let mut worst = 0.0f32;
            let mut worst_shadow = 0.0f32;
            let mut sum_sq = 0.0f64;
            let mut over_one = 0u64;
            let mut over_two = 0u64;
            let mut shadow_samples = 0u64;
            // How many samples are badly wrong, not just how wrong the single
            // worst one is. One ringing pixel at a window frame is invisible;
            // a hundred thousand of them is a halo.
            let mut buckets = [0u64; 4]; // >=5, >=10, >=25, >=50 display levels

            for (a, b) in original.iter().zip(decoded.iter()) {
                let da = to_display(*a);
                let db = to_display(*b);
                let err = (da - db).abs();
                worst = worst.max(err);
                sum_sq += (err as f64) * (err as f64);
                if err >= 1.0 {
                    over_one += 1;
                }
                if err >= 2.0 {
                    over_two += 1;
                }
                if err >= 5.0 {
                    buckets[0] += 1;
                    if err >= 10.0 {
                        buckets[1] += 1;
                    }
                    if err >= 25.0 {
                        buckets[2] += 1;
                    }
                    if err >= 50.0 {
                        buckets[3] += 1;
                    }
                }
                // The bottom two percent of the linear range, which the curve
                // stretches over roughly a fifth of the visible one.
                if (*a as f32 / 65535.0) < 0.02 {
                    shadow_samples += 1;
                    worst_shadow = worst_shadow.max(err);
                }
            }

            let n = original.len() as f64;
            eprintln!(
                "{} distance {distance}: {:.1} MB  |  in display units out of 255: rms {:.3}, worst {:.1}, worst in shadows {:.1}",
                if stored_curve {
                    "curve-stored"
                } else {
                    "linear-stored"
                },
                encoded.len() as f64 / 1e6,
                (sum_sq / n).sqrt(),
                worst,
                worst_shadow,
            );
            eprintln!(
                "               off by >=1: {:.3}%  >=2: {:.4}%  (shadow samples were {:.2}% of the frame)",
                100.0 * over_one as f64 / n,
                100.0 * over_two as f64 / n,
                100.0 * shadow_samples as f64 / n,
            );
            eprintln!(
                "               badly off: >=5 levels {} samples ({:.5}%), >=10 {}, >=25 {}, >=50 {}",
                buckets[0],
                100.0 * buckets[0] as f64 / n,
                buckets[1],
                buckets[2],
                buckets[3],
            );
        }
    }

    /// What Lightroom's own HDR merges are, and whether we can open one.
    ///
    /// Point RAPIDRAW_TEST_LR_HDR_DNG at one of them.
    #[test]
    fn report_lightroom_hdr_dng() {
        let Ok(path) = std::env::var("RAPIDRAW_TEST_LR_HDR_DNG") else {
            eprintln!("RAPIDRAW_TEST_LR_HDR_DNG unset, skipping");
            return;
        };
        let on_disk = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        eprintln!("lightroom hdr: {} ({:.1} MB)", path, on_disk as f64 / 1e6);

        let settings = AppSettings::default();
        let bytes = std::fs::read(&path).expect("read");
        let start = std::time::Instant::now();
        match load_base_image_from_bytes(&bytes, &path, false, &settings, None) {
            Ok(img) => eprintln!(
                "  our loader opens it in {:?} -> {}x{} {:?}",
                start.elapsed(),
                img.width(),
                img.height(),
                img.color()
            ),
            Err(e) => eprintln!(
                "  our loader CANNOT open it after {:?}: {e}",
                start.elapsed()
            ),
        }
    }

    /// What JPEG XL would do to one of our merges, measured on the pixels
    /// rather than argued about. This is the codec Lightroom uses inside its
    /// HDR DNGs, and it is already a dependency here for decoding.
    #[test]
    fn report_merge_as_jpegxl() {
        let Ok(path) = std::env::var("RAPIDRAW_TEST_HDR_TIFF") else {
            eprintln!("RAPIDRAW_TEST_HDR_TIFF unset, skipping");
            return;
        };
        let on_disk = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

        let mut reader = ImageReader::open(&path)
            .expect("open")
            .with_guessed_format()
            .expect("format");
        reader.no_limits();
        let img = reader.decode().expect("decode");
        let rgb16 = img.to_rgb16();
        drop(img);
        let (w, h) = (rgb16.width(), rgb16.height());

        // The encoder takes bytes; the layout says how to read them back.
        let pixels: Vec<u8> = bytemuck::cast_slice(rgb16.as_raw().as_slice()).to_vec();
        drop(rgb16);
        eprintln!(
            "merge: {}x{}, {:.0} MB of 16 bit pixels, {:.0} MB as the TIFF on disk",
            w,
            h,
            pixels.len() as f64 / 1e6,
            on_disk as f64 / 1e6
        );

        for distance in [0.0f32, 0.5, 1.0, 2.0] {
            let start = std::time::Instant::now();
            let encoded = if distance == 0.0 {
                jxl_encoder::LosslessConfig::new().encode(
                    &pixels,
                    w,
                    h,
                    jxl_encoder::PixelLayout::Rgb16,
                )
            } else {
                jxl_encoder::LossyConfig::new(distance).encode(
                    &pixels,
                    w,
                    h,
                    jxl_encoder::PixelLayout::Rgb16,
                )
            };
            match encoded {
                Ok(data) => eprintln!(
                    "  jxl {}: {:?} -> {:.1} MB, {:.1}x smaller than the TIFF, {:.3} bytes per sample",
                    if distance == 0.0 {
                        "lossless".to_string()
                    } else {
                        format!("distance {distance}")
                    },
                    start.elapsed(),
                    data.len() as f64 / 1e6,
                    on_disk as f64 / data.len().max(1) as f64,
                    data.len() as f64 / (w as f64 * h as f64 * 3.0),
                ),
                Err(e) => eprintln!(
                    "  jxl distance {distance}: failed after {:?}: {e}",
                    start.elapsed()
                ),
            }
        }
    }

    /// What a merge would cost as a linear DNG instead of the uncompressed
    /// 32-bit float TIFF it is written as today.
    ///
    /// A report, not an assertion. Point RAPIDRAW_TEST_HDR_TIFF at one of the
    /// merges and it prints what each option weighs and whether the result can
    /// be read back, which is the only question that decides whether the format
    /// is usable at all.
    #[test]
    fn report_merge_as_dng() {
        let Ok(path) = std::env::var("RAPIDRAW_TEST_HDR_TIFF") else {
            eprintln!("RAPIDRAW_TEST_HDR_TIFF unset, skipping");
            return;
        };
        let on_disk = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        eprintln!(
            "input: {} ({:.0} MB as an uncompressed 32-bit float TIFF)",
            path,
            on_disk as f64 / 1e6
        );

        // The same no_limits the loader uses: the image crate refuses a
        // half gigabyte allocation by default, which is most of the point.
        let start = std::time::Instant::now();
        let mut reader = ImageReader::open(&path)
            .expect("open the merge")
            .with_guessed_format()
            .expect("guess the format");
        reader.no_limits();
        let img = reader.decode().expect("decode the merge");
        eprintln!(
            "read back: {:?} -> {}x{} {:?}",
            start.elapsed(),
            img.width(),
            img.height(),
            img.color()
        );

        // The merge is histogram-stretched to 0..1 before it is written, so
        // sixteen bits across that range is the same picture. This is the step
        // that halves it before anything is compressed.
        let start = std::time::Instant::now();
        let rgb16 = img.to_rgb16();
        let (w, h) = (rgb16.width() as usize, rgb16.height() as usize);
        eprintln!(
            "to 16 bit: {:?} -> {:.0} MB of pixel data",
            start.elapsed(),
            (w * h * 3 * 2) as f64 / 1e6
        );

        for (name, compression, predictor) in [
            ("uncompressed", DngCompression::Uncompressed, 1u8),
            ("lossless p1", DngCompression::Lossless, 1u8),
            ("lossless p2", DngCompression::Lossless, 2u8),
        ] {
            let out =
                std::env::temp_dir().join(format!("blitzraw-probe-{}.dng", name.replace(' ', "-")));
            let start = std::time::Instant::now();
            {
                let file = std::fs::File::create(&out).expect("create");
                let mut buf = std::io::BufWriter::new(file);
                let mut dng = DngWriter::new(&mut buf, DNG_VERSION_V1_6).expect("writer");
                {
                    let mut frame = dng.subframe(0);
                    frame
                        .rgb_image_u16(rgb16.as_raw().as_slice(), w, h, compression, predictor)
                        .expect("write pixels");
                    frame.ifd_mut().add_tag(DngTag::BlackLevel, [0u16, 0, 0]);
                    frame
                        .ifd_mut()
                        .add_tag(DngTag::WhiteLevel, [u16::MAX, u16::MAX, u16::MAX]);
                    frame.finalize().expect("finalize");
                }
                dng.close().expect("close");
            }
            let written = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
            eprintln!(
                "dng {name}: wrote in {:?} -> {:.0} MB, {:.1}x smaller than the TIFF",
                start.elapsed(),
                written as f64 / 1e6,
                on_disk as f64 / written.max(1) as f64,
            );

            // The question that decides everything: can the app open what it
            // just wrote? A format we cannot read back is worse than a large one.
            let settings = AppSettings::default();
            let bytes = std::fs::read(&out).expect("read back");
            let start = std::time::Instant::now();
            match load_base_image_from_bytes(&bytes, &out.to_string_lossy(), false, &settings, None)
            {
                Ok(decoded) => eprintln!(
                    "dng {name}: reads back in {:?} -> {}x{} {:?}",
                    start.elapsed(),
                    decoded.width(),
                    decoded.height(),
                    decoded.color()
                ),
                Err(e) => eprintln!(
                    "dng {name}: WILL NOT READ BACK after {:?}: {e}",
                    start.elapsed()
                ),
            }
            let _ = std::fs::remove_file(&out);
        }
    }
}

// ================= BLITZRAW: preview cache sizing =================

#[cfg(test)]
mod preview_cost_tests {
    use super::*;

    /// What opening one photo costs, and what a cached preview of it would
    /// cost, so the cache can be aimed at the expensive half rather than the
    /// obvious one. Reports rather than asserts: these are the numbers the
    /// design was chosen from, and they move with the hardware.
    #[test]
    fn report_open_and_preview_cost() {
        let Ok(path) = std::env::var("RAPIDRAW_TEST_DNG") else {
            eprintln!("RAPIDRAW_TEST_DNG unset, skipping");
            return;
        };
        let settings = AppSettings::default();
        let bytes = std::fs::read(&path).expect("read test file");
        eprintln!(
            "file: {} ({:.1} MB on disk)",
            path,
            bytes.len() as f64 / 1e6
        );

        let start = std::time::Instant::now();
        let full =
            load_base_image_from_bytes(&bytes, &path, false, &settings, None).expect("decode");
        let decode = start.elapsed();
        let (w, h) = full.dimensions();
        let bytes_per_px: usize = match &full {
            DynamicImage::ImageRgb32F(_) => 12,
            DynamicImage::ImageRgba32F(_) => 16,
            DynamicImage::ImageRgb16(_) => 6,
            DynamicImage::ImageRgb8(_) => 3,
            _ => 0,
        };
        eprintln!(
            "full decode: {decode:?} -> {w}x{h}, {bytes_per_px} bytes/px, {:.0} MB held in RAM",
            (w as f64 * h as f64 * bytes_per_px as f64) / 1e6,
        );

        let start = std::time::Instant::now();
        let fast = load_base_image_from_bytes(&bytes, &path, true, &settings, None);
        match &fast {
            Ok(img) => eprintln!(
                "fast decode: {:?} -> {}x{}",
                start.elapsed(),
                img.width(),
                img.height()
            ),
            Err(e) => eprintln!("fast decode failed after {:?}: {e}", start.elapsed()),
        }

        for target in [1280u32, 1920, 2560] {
            let start = std::time::Instant::now();
            let small = crate::image_processing::downscale_f32_image(&full, target, target);
            let downscale = start.elapsed();
            for quality in [80u8, 90] {
                let start = std::time::Instant::now();
                let mut buf = std::io::Cursor::new(Vec::new());
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality)
                    .encode_image(&small.to_rgb8())
                    .expect("encode");
                let encoded = buf.into_inner();
                eprintln!(
                    "preview {target}px q{quality}: downscale {downscale:?} + encode {:?} -> {}x{}, {:.0} KB on disk ({:.1} GB for 1377 files)",
                    start.elapsed(),
                    small.width(),
                    small.height(),
                    encoded.len() as f64 / 1e3,
                    (encoded.len() as f64 * 1377.0) / 1e9,
                );
            }
        }
    }
}

// =============== BLITZRAW END: preview cache sizing ===============

// ============ BLITZRAW: a patch has to know what it was drawn on ============
#[cfg(test)]
mod patch_scale_tests {
    use super::*;
    use image::{GrayImage, Luma, Rgb, Rgb32FImage, RgbImage};

    fn as_png_base64_gray(img: &GrayImage) -> String {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(img.clone())
            .write_to(&mut bytes, image::ImageFormat::Png)
            .expect("encode mask");
        general_purpose::STANDARD.encode(bytes.into_inner())
    }

    fn as_png_base64_rgb(img: &RgbImage) -> String {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img.clone())
            .write_to(&mut bytes, image::ImageFormat::Png)
            .expect("encode colour");
        general_purpose::STANDARD.encode(bytes.into_inner())
    }

    /// One opaque white square, `size` across, recorded as sitting at
    /// (`at`, `at`) in a full-size decode.
    fn white_square_patch(at: u32, size: u32) -> Value {
        let mut mask = GrayImage::new(size, size);
        for pixel in mask.pixels_mut() {
            *pixel = Luma([255]);
        }
        let mut color = RgbImage::new(size, size);
        for pixel in color.pixels_mut() {
            *pixel = Rgb([255, 255, 255]);
        }

        serde_json::json!({
            "aiPatches": [{
                "id": "test-patch",
                "name": "Heal",
                "visible": true,
                "invert": false,
                "subMasks": [],
                "patchData": {
                    "offsetX": at,
                    "offsetY": at,
                    "width": size,
                    "height": size,
                    "mask": as_png_base64_gray(&mask),
                    "color": as_png_base64_rgb(&color),
                    "isSrgbEncoded": false,
                }
            }]
        })
    }

    fn black_canvas(side: u32) -> DynamicImage {
        DynamicImage::ImageRgb32F(Rgb32FImage::new(side, side))
    }

    /// The bounding box of everything that is not still black, or `None` when
    /// the patch never landed anywhere.
    fn painted_bounds(img: &DynamicImage) -> Option<(u32, u32, u32, u32)> {
        let buf = img.as_rgb32f().expect("linear image");
        let mut bounds: Option<(u32, u32, u32, u32)> = None;
        for (x, y, pixel) in buf.enumerate_pixels() {
            if pixel[0] > 0.5 {
                bounds = Some(match bounds {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
        bounds
    }

    #[test]
    fn at_full_size_the_patch_lands_where_it_was_recorded() {
        let adjustments = white_square_patch(40, 20);
        let out = composite_patches_on_image(&black_canvas(100), &adjustments).expect("composite");

        assert_eq!(painted_bounds(&out), Some((40, 40, 59, 59)));
    }

    /// The bug. A half-size decode used to take the recorded coordinates
    /// literally, so the patch sat twice as far in and covered four times the
    /// area: healed sensor dust reappearing in the middle of a thumbnail.
    #[test]
    fn on_a_half_size_decode_the_patch_shrinks_and_moves_with_the_photo() {
        let adjustments = white_square_patch(40, 20);
        let out = composite_patches_on_image_scaled(&black_canvas(50), &adjustments, 0.5)
            .expect("composite");

        assert_eq!(painted_bounds(&out), Some((20, 20, 29, 29)));
    }

    #[test]
    fn a_quarter_size_decode_scales_the_same_way() {
        let adjustments = white_square_patch(40, 20);
        let out = composite_patches_on_image_scaled(&black_canvas(25), &adjustments, 0.25)
            .expect("composite");

        assert_eq!(painted_bounds(&out), Some((10, 10, 14, 14)));
    }

    /// Told nothing about the scale, it behaves exactly as it always did. Every
    /// caller that decodes at full size goes through this.
    #[test]
    fn the_unscaled_call_is_unchanged() {
        let adjustments = white_square_patch(10, 8);
        let scaled =
            composite_patches_on_image_scaled(&black_canvas(64), &adjustments, 1.0).expect("a");
        let plain = composite_patches_on_image(&black_canvas(64), &adjustments).expect("b");

        assert_eq!(painted_bounds(&scaled), painted_bounds(&plain));
        assert_eq!(painted_bounds(&plain), Some((10, 10, 17, 17)));
    }

    /// A scale that cannot be right is ignored rather than acted on, because
    /// repeating the old placement is a known quantity and inventing a new one
    /// is not.
    #[test]
    fn a_nonsense_scale_is_refused() {
        let adjustments = white_square_patch(40, 20);
        for scale in [0.0, -1.0, 4.0, f32::NAN] {
            let out = composite_patches_on_image_scaled(&black_canvas(100), &adjustments, scale)
                .expect("composite");
            assert_eq!(
                painted_bounds(&out),
                Some((40, 40, 59, 59)),
                "scale {scale}"
            );
        }
    }

    /// A patch smaller than the scale would allow still has to draw something.
    /// Rounding it away would make a small heal vanish from every thumbnail.
    #[test]
    fn a_tiny_patch_survives_a_heavy_downscale() {
        let adjustments = white_square_patch(40, 2);
        let out = composite_patches_on_image_scaled(&black_canvas(25), &adjustments, 0.25)
            .expect("composite");

        assert!(painted_bounds(&out).is_some(), "the patch disappeared");
    }
}
// ========== BLITZRAW END: a patch has to know what it was drawn on ==========
