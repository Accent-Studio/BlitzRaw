use crate::app_settings::load_settings;
use crate::app_state::AppState;
use crate::file_management::parse_virtual_path;
use crate::formats::is_raw_file;
use crate::image_loader::load_base_image_from_bytes;
use crate::image_processing::apply_cpu_default_raw_processing;
use base64::{Engine as _, engine::general_purpose};
use image::{DynamicImage, GenericImageView, ImageFormat, Rgb, Rgb32FImage};
use rayon::prelude::*;
use std::cmp::Ordering;
use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

struct ProgressReporter<'a> {
    counter: &'a Arc<AtomicUsize>,
    total_work: usize,
    app_handle: &'a AppHandle,
}

const BLOCK_SIZE: usize = 8;
const BLOCK_AREA: usize = 64;
const MAX_GROUP_SIZE: usize = 16;
const STRIDE: usize = 6;
const SEARCH_WINDOW: usize = 19;
const FIXED_POINT_SCALE: f32 = 100_000.0;

#[derive(Clone, Copy)]
struct Bm3dParams {
    sigma: f32,
    hard_th_lambda: f32,
    max_dist_hard: f32,
    chroma_sigma_scale: f32,
}

impl Bm3dParams {
    fn from_intensity(i: f32) -> Self {
        let val = i.clamp(0.001, 1.0);
        Self {
            sigma: val * 80.0,
            hard_th_lambda: 2.0 + (val * 2.5),
            max_dist_hard: 3000.0 + (val * 20000.0),
            chroma_sigma_scale: 1.8,
        }
    }
}

#[tauri::command]
pub async fn apply_denoising(
    path: String,
    intensity: f32,
    method: String,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let (source_path, _) = parse_virtual_path(&path);
    let path_str = source_path.to_string_lossy().to_string();

    // BLITZRAW: SCUNet is a third method beside "ai" (NIND) and BM3D.
    let mut ai_session = None;
    if method == "ai" {
        ai_session = Some(
            crate::ai_processing::get_or_init_denoise_model(
                &app_handle,
                &state.ai_state,
                &state.ai_init_lock,
            )
            .await
            .map_err(|e| e.to_string())?,
        );
    } else if method == "scunet" {
        ai_session = Some(
            crate::ai_processing::get_or_init_scunet_model(
                &app_handle,
                &state.ai_state,
                &state.ai_init_lock,
            )
            .await
            .map_err(|e| e.to_string())?,
        );
    }

    let denoise_result_handle = state.denoise_result.clone();

    tokio::task::spawn_blocking(move || {
        match denoise_image(path_str, intensity, method, app_handle.clone(), ai_session) {
            Ok((image, _)) => {
                *denoise_result_handle.lock().unwrap() = Some(image);
            }
            Err(e) => {
                let _ = app_handle.emit("denoise-error", e);
            }
        }
    })
    .await
    .map_err(|e| format!("Denoising task failed: {}", e))
}

// ============ BLITZRAW: denoise only the bit I am looking at ============
/// One square of the picture, cleaned, at full resolution.
///
/// SCUNet costs about four seconds a megapixel, so a whole frame is a minute
/// and choosing a strength by trying one means a minute per try. A square of
/// 1024 is one megapixel and comes back in about a second, which turns the
/// question from "wait and see" into "drag the box".
///
/// Full resolution on purpose. A denoiser judged on a shrunk preview is judged
/// on noise the shrinking already removed.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DenoisePatch {
    /// Both as base64 PNG, ready for an `<img src>`.
    pub original: String,
    pub denoised: String,
    pub width: u32,
    pub height: u32,
    /// Where the square really landed, in full-resolution pixels. The asked-for
    /// centre gets pushed inside the frame near an edge, and the panel has to
    /// draw the box where it actually is rather than where it asked.
    pub x: u32,
    pub y: u32,
    pub full_width: u32,
    pub full_height: u32,
    pub took_ms: u64,
}

#[tauri::command]
pub async fn denoise_preview_patch(
    path: String,
    centre_x: f32,
    centre_y: f32,
    size: u32,
    intensity: f32,
    method: String,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<DenoisePatch, String> {
    let (source_path, _) = parse_virtual_path(&path);
    let path_str = source_path.to_string_lossy().to_string();

    let session = match method.as_str() {
        "scunet" => Some(
            crate::ai_processing::get_or_init_scunet_model(
                &app_handle,
                &state.ai_state,
                &state.ai_init_lock,
            )
            .await
            .map_err(|e| e.to_string())?,
        ),
        "ai" => Some(
            crate::ai_processing::get_or_init_denoise_model(
                &app_handle,
                &state.ai_state,
                &state.ai_init_lock,
            )
            .await
            .map_err(|e| e.to_string())?,
        ),
        _ => None,
    };

    // The photo is usually open in the editor, so its decode is already in
    // hand. Decoding a 45 megapixel raw again would cost more than the denoise.
    let cached = state
        .decoded_image_cache
        .lock()
        .ok()
        .and_then(|mut c| c.get(&path_str))
        .map(|(img, _)| img);

    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let is_raw = is_raw_file(&path_str);

    tokio::task::spawn_blocking(move || {
        let base = match cached {
            Some(img) => img,
            None => {
                let bytes = fs::read(&path_str).map_err(|e| e.to_string())?;
                Arc::new(
                    load_base_image_from_bytes(&bytes, &path_str, false, &settings, None)
                        .map_err(|e| e.to_string())?,
                )
            }
        };

        let (full_width, full_height) = base.dimensions();
        let side = size.clamp(256, 2048).min(full_width).min(full_height);
        // Clamped so the square stays inside the frame however near the edge
        // the pointer goes.
        let x = ((centre_x.clamp(0.0, 1.0) * full_width as f32) as i64 - side as i64 / 2)
            .clamp(0, (full_width - side) as i64) as u32;
        let y = ((centre_y.clamp(0.0, 1.0) * full_height as f32) as i64 - side as i64 / 2)
            .clamp(0, (full_height - side) as i64) as u32;

        let patch = base.crop_imm(x, y, side, side);
        let patch_rgb = patch.to_rgb32f();

        let started = std::time::Instant::now();
        let cleaned = match method.as_str() {
            "scunet" => {
                let s = session.ok_or_else(|| "SCUNet session not provided".to_string())?;
                crate::ai_processing::run_scunet_denoise(&patch_rgb, intensity, &s, &app_handle)
                    .map_err(|e| e.to_string())?
            }
            "ai" => {
                let s = session.ok_or_else(|| "AI session not provided".to_string())?;
                crate::ai_processing::run_ai_denoise(&patch_rgb, intensity, &s, &app_handle)
                    .map_err(|e| e.to_string())?
            }
            _ => run_bm3d(&patch_rgb, intensity, &app_handle)?,
        };
        let took_ms = started.elapsed().as_millis() as u64;

        // Both halves get the same treatment, or the comparison is between a
        // denoiser and a tone curve.
        let encode = |mut img: DynamicImage| -> Result<String, String> {
            if is_raw {
                apply_cpu_default_raw_processing(&mut img);
            }
            let mut buf = Cursor::new(Vec::new());
            img.to_rgb8()
                .write_to(&mut buf, ImageFormat::Png)
                .map_err(|e| e.to_string())?;
            Ok(format!(
                "data:image/png;base64,{}",
                general_purpose::STANDARD.encode(buf.into_inner())
            ))
        };

        Ok(DenoisePatch {
            original: encode(patch)?,
            denoised: encode(cleaned)?,
            width: side,
            height: side,
            x,
            y,
            full_width,
            full_height,
            took_ms,
        })
    })
    .await
    .map_err(|e| format!("Preview task failed: {e}"))?
}
// ========== BLITZRAW END: denoise only the bit I am looking at ==========

#[tauri::command]
pub async fn batch_denoise_images(
    paths: Vec<String>,
    intensity: f32,
    method: String,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<String>, String> {
    // BLITZRAW: as above, SCUNet is a third method.
    let mut ai_session = None;
    if method == "ai" {
        ai_session = Some(
            crate::ai_processing::get_or_init_denoise_model(
                &app_handle,
                &state.ai_state,
                &state.ai_init_lock,
            )
            .await
            .map_err(|e| e.to_string())?,
        );
    } else if method == "scunet" {
        ai_session = Some(
            crate::ai_processing::get_or_init_scunet_model(
                &app_handle,
                &state.ai_state,
                &state.ai_init_lock,
            )
            .await
            .map_err(|e| e.to_string())?,
        );
    }

    tokio::task::spawn_blocking(move || {
        let mut results = Vec::new();

        for (i, path_str) in paths.iter().enumerate() {
            let _ = app_handle.emit(
                "denoise-batch-progress",
                serde_json::json!({
                    "current": i + 1,
                    "total": paths.len(),
                    "path": path_str
                }),
            );

            let (source_path, source_sidecar_path) =
                crate::file_management::parse_virtual_path(path_str);
            let real_path = source_path.to_string_lossy().to_string();

            match crate::denoising::denoise_image(
                real_path.clone(),
                intensity,
                method.clone(),
                app_handle.clone(),
                ai_session.clone(),
            ) {
                Ok((image, _)) => {
                    let is_raw = crate::formats::is_raw_file(&real_path);
                    let parent_dir = source_path.parent().unwrap_or(std::path::Path::new(""));
                    let stem = source_path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy();

                    let (output_filename, image_to_save) = if is_raw {
                        (
                            format!("{}_Denoised.tiff", stem),
                            // BLITZRAW: as in save_denoised_image above.
                            DynamicImage::ImageRgb16(to_viewable(image, true).to_rgb16()),
                        )
                    } else {
                        (
                            format!("{}_Denoised.png", stem),
                            DynamicImage::ImageRgb8(image.to_rgb8()),
                        )
                    };

                    let output_path = parent_dir.join(output_filename);
                    if let Err(e) = image_to_save.save(&output_path) {
                        let _ = app_handle.emit(
                            "denoise-error",
                            format!("Failed to save {}: {}", real_path, e),
                        );
                        continue;
                    }

                    let _ = crate::exif_processing::write_rrexif_sidecar(&real_path, &output_path);

                    if source_sidecar_path.exists()
                        && let Some(output_path_str) = output_path.to_str()
                    {
                        let (_, dest_sidecar_path) =
                            crate::file_management::parse_virtual_path(output_path_str);
                        if let Err(e) = std::fs::copy(&source_sidecar_path, &dest_sidecar_path) {
                            log::warn!("Failed to copy sidecar file for denoised image: {}", e);
                        }
                    }

                    results.push(output_path.to_string_lossy().to_string());
                }
                Err(e) => {
                    let _ = app_handle.emit(
                        "denoise-error",
                        format!("Failed to denoise {}: {}", real_path, e),
                    );
                }
            }
        }

        Ok(results)
    })
    .await
    .map_err(|e| format!("Batch denoising task failed: {}", e))?
}

// ============ BLITZRAW: save what the preview showed ============
// The denoiser hands back linear light, because that is what the pipeline
// works in. The preview then puts a curve on it so it looks like a photograph,
// and the file was written without one.
//
// Nothing in a TIFF says "this is linear", so everything that opens it assumes
// otherwise and linearises it a second time. That is the crushed, oversaturated
// look: shadows squeezed flat and channel ratios stretched, and no amount of
// pulling on the sliders brings it back, because the information is already
// gone by then.
//
// So the file gets the same curve the preview showed. What you approved is now
// what lands on disk.
fn to_viewable(image: DynamicImage, is_raw: bool) -> DynamicImage {
    if !is_raw {
        return image;
    }
    let mut out = image;
    apply_cpu_default_raw_processing(&mut out);
    out
}
// ========== BLITZRAW END: save what the preview showed ==========

#[tauri::command]
pub async fn save_denoised_image(
    original_path_str: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let denoised_image = state.denoise_result.lock().unwrap().take().ok_or_else(|| {
        "No denoised image found in memory. It might have already been saved or cleared."
            .to_string()
    })?;

    let is_raw = crate::formats::is_raw_file(&original_path_str);

    let (first_path, source_sidecar_path) =
        crate::file_management::parse_virtual_path(&original_path_str);
    let parent_dir = first_path
        .parent()
        .ok_or_else(|| "Could not determine parent directory.".to_string())?;
    let stem = first_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("denoised");

    let (output_filename, image_to_save): (String, DynamicImage) = if is_raw {
        let filename = format!("{}_Denoised.tiff", stem);
        (
            filename,
            // BLITZRAW: the same curve the preview showed, or the file is
            // linear and everything that opens it linearises it again.
            DynamicImage::ImageRgb16(to_viewable(denoised_image, true).to_rgb16()),
        )
    } else {
        let filename = format!("{}_Denoised.png", stem);
        (filename, DynamicImage::ImageRgb8(denoised_image.to_rgb8()))
    };

    let output_path = parent_dir.join(output_filename);

    image_to_save
        .save(&output_path)
        .map_err(|e| format!("Failed to save image: {}", e))?;

    let (real_path, _) = crate::file_management::parse_virtual_path(&original_path_str);
    let _ =
        crate::exif_processing::write_rrexif_sidecar(&real_path.to_string_lossy(), &output_path);

    if source_sidecar_path.exists()
        && let Some(output_path_str) = output_path.to_str()
    {
        let (_, dest_sidecar_path) = crate::file_management::parse_virtual_path(output_path_str);
        if let Err(e) = std::fs::copy(&source_sidecar_path, &dest_sidecar_path) {
            log::warn!("Failed to copy sidecar file for denoised image: {}", e);
        }
    }

    Ok(output_path.to_string_lossy().to_string())
}

fn run_bm3d(
    rgb_img: &Rgb32FImage,
    intensity: f32,
    app_handle: &AppHandle,
) -> Result<DynamicImage, String> {
    let (width, height) = rgb_img.dimensions();
    let params = Bm3dParams::from_intensity(intensity);
    let dct_tables = Arc::new(DctTables::new());

    let rgb_channels = split_channels(rgb_img);
    let (y, cb, cr) = rgb_to_ycbcr(&rgb_channels[0], &rgb_channels[1], &rgb_channels[2]);
    let original_y = y.clone();
    let channels = vec![y, cb, cr];

    let patches_x = (width as usize).saturating_sub(BLOCK_SIZE) / STRIDE + 1;
    let patches_y = (height as usize).saturating_sub(BLOCK_SIZE) / STRIDE + 1;
    let total_work_units = (patches_x * patches_y) * 2;
    let progress_counter = Arc::new(AtomicUsize::new(0));

    let _ = app_handle.emit("denoise-progress", "Processing (Step 1/2)...");

    let progress = ProgressReporter {
        counter: &progress_counter,
        total_work: total_work_units,
        app_handle,
    };
    let mut denoised_channels =
        bm3d_process_joint(&channels, width, height, &params, &dct_tables, &progress);

    {
        let _ = app_handle.emit("denoise-progress", "Blending detail...");
        let blurred_y = gaussian_blur_1ch(&original_y, width as usize, height as usize, 3.0);
        let detail_strength = (intensity * 0.5_f32).clamp(0.0_f32, 0.5_f32);
        let y_ch = &mut denoised_channels[0];
        for i in 0..y_ch.len() {
            let hf = original_y[i] - blurred_y[i];
            y_ch[i] = (y_ch[i] + detail_strength * hf).clamp(0.0, 255.0);
        }
    }

    let (r, g, b) = ycbcr_to_rgb(
        &denoised_channels[0],
        &denoised_channels[1],
        &denoised_channels[2],
    );

    let out_img_buffer = merge_channels(&[r, g, b], width, height);
    Ok(DynamicImage::ImageRgb32F(out_img_buffer))
}

fn denoise_image(
    path_str: String,
    intensity: f32,
    method: String,
    app_handle: AppHandle,
    ai_session: Option<Arc<Mutex<ort::session::Session>>>,
) -> Result<(DynamicImage, String), String> {
    let path = Path::new(&path_str);
    if !path.exists() {
        return Err("File not found".to_string());
    }

    let is_raw = is_raw_file(&path_str);
    let settings = load_settings(app_handle.clone()).unwrap_or_default();

    let _ = app_handle.emit("denoise-progress", "Loading image...");

    let file_bytes = fs::read(path).map_err(|e| e.to_string())?;
    let dynamic_img = load_base_image_from_bytes(&file_bytes, &path_str, false, &settings, None)
        .map_err(|e| e.to_string())?;

    let rgb_img_for_denoiser = dynamic_img.to_rgb32f();

    let out_dynamic = if method == "scunet" {
        // BLITZRAW
        let session_arc = ai_session.ok_or_else(|| "SCUNet session not provided".to_string())?;
        crate::ai_processing::run_scunet_denoise(
            &rgb_img_for_denoiser,
            intensity,
            &session_arc,
            &app_handle,
        )
        .map_err(|e| e.to_string())?
    } else if method == "ai" {
        let session_arc = ai_session.ok_or_else(|| "AI Session not provided".to_string())?;
        crate::ai_processing::run_ai_denoise(
            &rgb_img_for_denoiser,
            intensity,
            &session_arc,
            &app_handle,
        )
        .map_err(|e| e.to_string())?
    } else {
        run_bm3d(&rgb_img_for_denoiser, intensity, &app_handle)?
    };

    let _ = app_handle.emit("denoise-progress", "Finalizing data...");
    let _ = app_handle.emit("denoise-progress", "Generating previews...");

    let (width, height) = out_dynamic.dimensions();
    let (new_width, new_height) = if width > height {
        if width > 4000 {
            (4000, (4000.0 * height as f32 / width as f32).round() as u32)
        } else {
            (width, height)
        }
    } else {
        if height > 4000 {
            ((4000.0 * width as f32 / height as f32).round() as u32, 4000)
        } else {
            (width, height)
        }
    };

    let mut denoised_preview_source = out_dynamic.clone();

    if is_raw {
        apply_cpu_default_raw_processing(&mut denoised_preview_source);
    }

    let denoised_preview = if new_width != width {
        denoised_preview_source.resize(new_width, new_height, image::imageops::FilterType::Lanczos3)
    } else {
        denoised_preview_source
    };

    let mut buf_denoised = Cursor::new(Vec::new());
    denoised_preview
        .to_rgb8()
        .write_to(&mut buf_denoised, ImageFormat::Png)
        .map_err(|e| format!("Failed to encode preview: {}", e))?;
    let base64_str_denoised = general_purpose::STANDARD.encode(buf_denoised.get_ref());
    let data_url_denoised = format!("data:image/png;base64,{}", base64_str_denoised);

    let mut original_dynamic = DynamicImage::ImageRgb32F(rgb_img_for_denoiser);

    if is_raw {
        apply_cpu_default_raw_processing(&mut original_dynamic);
    }
    let original_preview = if new_width != width {
        original_dynamic.resize(new_width, new_height, image::imageops::FilterType::Lanczos3)
    } else {
        original_dynamic
    };

    let mut buf_orig = Cursor::new(Vec::new());
    original_preview
        .to_rgb8()
        .write_to(&mut buf_orig, ImageFormat::Png)
        .map_err(|e| format!("Failed to encode original preview: {}", e))?;
    let base64_str_orig = general_purpose::STANDARD.encode(buf_orig.get_ref());
    let data_url_orig = format!("data:image/png;base64,{}", base64_str_orig);

    let payload = serde_json::json!({
        "denoised": data_url_denoised,
        "original": data_url_orig
    });

    let _ = app_handle.emit("denoise-complete", &payload);

    Ok((out_dynamic, data_url_denoised))
}

fn rgb_to_ycbcr(r: &[f32], g: &[f32], b: &[f32]) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let n = r.len();
    let mut y = vec![0.0f32; n];
    let mut cb = vec![0.0f32; n];
    let mut cr = vec![0.0f32; n];
    for i in 0..n {
        let rv = r[i];
        let gv = g[i];
        let bv = b[i];
        y[i] = 0.299 * rv + 0.587 * gv + 0.114 * bv;
        cb[i] = -0.168736 * rv - 0.331264 * gv + 0.5 * bv + 128.0;
        cr[i] = 0.5 * rv - 0.418688 * gv - 0.081312 * bv + 128.0;
    }
    (y, cb, cr)
}

fn ycbcr_to_rgb(y: &[f32], cb: &[f32], cr: &[f32]) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let n = y.len();
    let mut r = vec![0.0f32; n];
    let mut g = vec![0.0f32; n];
    let mut b = vec![0.0f32; n];
    for i in 0..n {
        let yv = y[i];
        let cbv = cb[i] - 128.0;
        let crv = cr[i] - 128.0;
        r[i] = yv + 1.402 * crv;
        g[i] = yv - 0.344136 * cbv - 0.714136 * crv;
        b[i] = yv + 1.772 * cbv;
    }
    (r, g, b)
}

fn bm3d_process_joint(
    noisy_channels: &[Vec<f32>],
    width: u32,
    height: u32,
    params: &Bm3dParams,
    tables: &DctTables,
    progress: &ProgressReporter,
) -> Vec<Vec<f32>> {
    let basic_estimate = run_bm3d_step_joint(
        noisy_channels,
        noisy_channels,
        width,
        height,
        params,
        true,
        tables,
        progress,
    );

    run_bm3d_step_joint(
        noisy_channels,
        &basic_estimate,
        width,
        height,
        params,
        false,
        tables,
        progress,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_bm3d_step_joint(
    noisy: &[Vec<f32>],
    guide: &[Vec<f32>],
    width: u32,
    height: u32,
    params: &Bm3dParams,
    is_step_1: bool,
    tables: &DctTables,
    progress: &ProgressReporter,
) -> Vec<Vec<f32>> {
    let w = width as usize;
    let h = height as usize;
    let count = w * h;
    let num_channels = 3;

    let mut numerators = Vec::new();
    let mut denominators = Vec::new();
    for _ in 0..num_channels {
        numerators.push(Arc::new(AtomicAccumulator::new(count)));
        denominators.push(Arc::new(AtomicAccumulator::new(count)));
    }

    let mut ref_patches = Vec::with_capacity((w / STRIDE) * (h / STRIDE));
    for y in (0..h.saturating_sub(BLOCK_SIZE)).step_by(STRIDE) {
        for x in (0..w.saturating_sub(BLOCK_SIZE)).step_by(STRIDE) {
            ref_patches.push((x, y));
        }
    }

    ref_patches.par_iter().for_each(|&(rx, ry)| {
        let c = progress.counter.fetch_add(1, AtomicOrdering::Relaxed);
        if c.is_multiple_of(200) {
            let pct = (c as f32 / progress.total_work as f32) * 100.0;
            let step_str = if is_step_1 { "Step 1/2" } else { "Step 2/2" };
            let msg = format!("{} - {:.0}%", step_str, pct);
            let _ = progress.app_handle.emit("denoise-progress", msg);
        }

        let mut group_locs_buf = [(0, 0); MAX_GROUP_SIZE];
        let group_size =
            block_matching_joint(guide, w, h, rx, ry, is_step_1, params, &mut group_locs_buf);
        let group_locs = &group_locs_buf[0..group_size];

        for ch in 0..num_channels {
            let guide_ch = &guide[ch];
            let noisy_ch = &noisy[ch];

            let ch_sigma = if ch == 0 {
                params.sigma
            } else {
                params.sigma * params.chroma_sigma_scale
            };

            let mut guide_stack = build_3d_group(guide_ch, w, group_locs);
            let mut noisy_stack = if is_step_1 {
                guide_stack.clone()
            } else {
                build_3d_group(noisy_ch, w, group_locs)
            };

            transform_3d(&mut guide_stack, group_size, tables);
            if !is_step_1 {
                transform_3d(&mut noisy_stack, group_size, tables);
            }

            let weight;
            if is_step_1 {
                let threshold = params.hard_th_lambda * ch_sigma;
                let nonzero = hard_threshold(&mut guide_stack, threshold);
                weight = if nonzero > 0 {
                    1.0 / (nonzero as f32)
                } else {
                    1.0
                };
                noisy_stack = guide_stack;
            } else {
                weight = wiener_filter(&mut noisy_stack, &guide_stack, ch_sigma);
            }

            inverse_transform_3d(&mut noisy_stack, group_size, tables);

            let num_acc = &numerators[ch];
            let den_acc = &denominators[ch];

            for (k, &(lx, ly)) in group_locs.iter().enumerate() {
                let patch_offset = k * BLOCK_AREA;
                for dy in 0..BLOCK_SIZE {
                    let row_global = (ly + dy) * w + lx;
                    let row_patch = dy * BLOCK_SIZE;
                    for dx in 0..BLOCK_SIZE {
                        let idx = row_global + dx;
                        let val = noisy_stack[patch_offset + row_patch + dx];
                        let w_val = tables.kaiser[row_patch + dx] * weight;
                        num_acc.add(idx, val * w_val);
                        den_acc.add(idx, w_val);
                    }
                }
            }
        }
    });

    let mut results = Vec::new();
    for ch in 0..num_channels {
        let num_vec = numerators[ch].to_vec();
        let den_vec = denominators[ch].to_vec();
        let final_ch = num_vec
            .iter()
            .zip(den_vec.iter())
            .zip(noisy[ch].iter())
            .map(|((&n, &d), &orig)| if d > 1e-6 { n / d } else { orig })
            .collect();
        results.push(final_ch);
    }
    results
}

fn hard_threshold(stack: &mut [f32], th: f32) -> usize {
    let mut c = 0;
    for (i, x) in stack.iter_mut().enumerate() {
        if i == 0 {
            c += 1;
            continue;
        }

        if x.abs() < th {
            *x = 0.0;
        } else {
            c += 1;
        }
    }
    c
}

fn wiener_filter(noisy: &mut [f32], guide: &[f32], sigma: f32) -> f32 {
    let mut sum = 0.0;
    let s2 = sigma * sigma;
    for (i, (n, g)) in noisy.iter_mut().zip(guide).enumerate() {
        if i == 0 {
            sum += 1.0;
            continue;
        }

        let energy = g * g;
        let coef = energy / (energy + s2 + 1e-5);
        *n *= coef;
        sum += coef * coef;
    }
    if sum > 0.0 { 1.0 / sum } else { 1.0 }
}

#[derive(Clone, Copy)]
struct Match {
    dist: f32,
    x: u16,
    y: u16,
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn block_matching_joint(
    channels: &[Vec<f32>],
    w: usize,
    h: usize,
    rx: usize,
    ry: usize,
    is_step_1: bool,
    params: &Bm3dParams,
    out_buf: &mut [(usize, usize)],
) -> usize {
    const MAX_CANDIDATES: usize = 1024;
    let mut candidates: [Match; MAX_CANDIDATES] = [Match {
        dist: f32::MAX,
        x: 0,
        y: 0,
    }; MAX_CANDIDATES];
    let mut cand_count = 0;

    let threshold = if is_step_1 {
        params.max_dist_hard
    } else {
        params.max_dist_hard * 0.5
    };

    let mut ref_r = [0.0; 64];
    let mut ref_g = [0.0; 64];
    let mut ref_b = [0.0; 64];
    extract_patch(&channels[0], w, rx, ry, &mut ref_r);
    extract_patch(&channels[1], w, rx, ry, &mut ref_g);
    extract_patch(&channels[2], w, rx, ry, &mut ref_b);

    let half_sw = SEARCH_WINDOW / 2;
    let sx_start = rx.saturating_sub(half_sw);
    let sx_end = (rx + half_sw).min(w.saturating_sub(BLOCK_SIZE));
    let sy_start = ry.saturating_sub(half_sw);
    let sy_end = (ry + half_sw).min(h.saturating_sub(BLOCK_SIZE));

    candidates[0] = Match {
        dist: 0.0,
        x: rx as u16,
        y: ry as u16,
    };
    cand_count += 1;

    for y in sy_start..=sy_end {
        for x in sx_start..=sx_end {
            if x == rx && y == ry {
                continue;
            }
            let d_r = compute_ssd_flat(&channels[0], w, x, y, &ref_r, threshold);
            if d_r > threshold {
                continue;
            }
            let d_g = compute_ssd_flat(&channels[1], w, x, y, &ref_g, threshold - d_r);
            if d_r + d_g > threshold {
                continue;
            }
            let d_b = compute_ssd_flat(&channels[2], w, x, y, &ref_b, threshold - (d_r + d_g));
            let total_dist = d_r + d_g + d_b;

            if total_dist < threshold && cand_count < MAX_CANDIDATES {
                candidates[cand_count] = Match {
                    dist: total_dist,
                    x: x as u16,
                    y: y as u16,
                };
                cand_count += 1;
            }
        }
    }

    let valid_slice = &mut candidates[0..cand_count];
    valid_slice.sort_unstable_by(|a, b| a.dist.partial_cmp(&b.dist).unwrap_or(Ordering::Equal));

    let limit = MAX_GROUP_SIZE.min(cand_count);
    let p2_limit = prev_power_of_two(limit);

    for i in 0..p2_limit {
        out_buf[i] = (valid_slice[i].x as usize, valid_slice[i].y as usize);
    }
    p2_limit
}

#[inline(always)]
fn compute_ssd_flat(
    img: &[f32],
    w: usize,
    x: usize,
    y: usize,
    ref_patch: &[f32],
    stop_thr: f32,
) -> f32 {
    let mut dist = 0.0;
    for dy in 0..8 {
        let img_base = (y + dy) * w + x;
        let ref_base = dy * 8;
        for dx in 0..8 {
            let diff = img[img_base + dx] - ref_patch[ref_base + dx];
            dist += diff * diff;
        }
        if dist > stop_thr {
            return dist;
        }
    }
    dist / BLOCK_AREA as f32
}

#[inline(always)]
fn extract_patch(img: &[f32], w: usize, x: usize, y: usize, out: &mut [f32]) {
    for dy in 0..8 {
        let src_idx = (y + dy) * w + x;
        let dst_idx = dy * 8;
        out[dst_idx..dst_idx + 8].copy_from_slice(&img[src_idx..src_idx + 8]);
    }
}

fn build_3d_group(img: &[f32], w: usize, locs: &[(usize, usize)]) -> Vec<f32> {
    let mut stack = vec![0.0; locs.len() * 64];
    for (i, &(lx, ly)) in locs.iter().enumerate() {
        let offset = i * 64;
        extract_patch(img, w, lx, ly, &mut stack[offset..offset + 64]);
    }
    stack
}

struct DctTables {
    dct_coeff: [f32; 64],
    idct_coeff: [f32; 64],
    kaiser: Vec<f32>,
}

impl DctTables {
    fn new() -> Self {
        let mut dct_coeff = [0.0; 64];
        let mut idct_coeff = [0.0; 64];
        for k in 0..8 {
            for n in 0..8 {
                let c = k as f32 * std::f32::consts::PI / 8.0;
                let val = ((n as f32 + 0.5) * c).cos();
                let scale = if k == 0 { 0.35355339 } else { 0.5 };
                dct_coeff[k * 8 + n] = val * scale;
            }
        }
        for n in 0..8 {
            for k in 0..8 {
                let theta = (std::f32::consts::PI / 8.0) * (n as f32 + 0.5) * (k as f32);
                let scale = if k == 0 { 0.35355339 } else { 0.5 };
                idct_coeff[n * 8 + k] = scale * theta.cos();
            }
        }
        let mut kaiser = vec![0.0; 64];
        for y in 0..8 {
            for x in 0..8 {
                let wx = (std::f32::consts::PI * x as f32 / 7.0).sin();
                let wy = (std::f32::consts::PI * y as f32 / 7.0).sin();
                kaiser[y * 8 + x] = wx * wy;
            }
        }
        Self {
            dct_coeff,
            idct_coeff,
            kaiser,
        }
    }
}

struct AtomicAccumulator {
    data: Vec<AtomicI64>,
}

impl AtomicAccumulator {
    fn new(size: usize) -> Self {
        let mut data = Vec::with_capacity(size);
        for _ in 0..size {
            data.push(AtomicI64::new(0));
        }
        Self { data }
    }
    #[inline(always)]
    fn add(&self, index: usize, value: f32) {
        if index < self.data.len() {
            let fixed = (value * FIXED_POINT_SCALE) as i64;
            self.data[index].fetch_add(fixed, AtomicOrdering::Relaxed);
        }
    }
    fn to_vec(&self) -> Vec<f32> {
        self.data
            .iter()
            .map(|a| a.load(AtomicOrdering::Relaxed) as f32 / FIXED_POINT_SCALE)
            .collect()
    }
}

#[inline(always)]
fn transform_3d(stack: &mut [f32], group_size: usize, tables: &DctTables) {
    for i in 0..group_size {
        let offset = i * 64;
        dct_2d_8x8(&mut stack[offset..offset + 64], &tables.dct_coeff);
    }
    for i in 0..64 {
        let mut col = [0.0; MAX_GROUP_SIZE];
        for k in 0..group_size {
            col[k] = stack[k * 64 + i];
        }
        walsh_hadamard_1d(&mut col[0..group_size]);
        for k in 0..group_size {
            stack[k * 64 + i] = col[k];
        }
    }
}

#[inline(always)]
fn inverse_transform_3d(stack: &mut [f32], group_size: usize, tables: &DctTables) {
    for i in 0..64 {
        let mut col = [0.0; MAX_GROUP_SIZE];
        for k in 0..group_size {
            col[k] = stack[k * 64 + i];
        }
        walsh_hadamard_1d(&mut col[0..group_size]);
        for k in 0..group_size {
            stack[k * 64 + i] = col[k];
        }
    }
    for i in 0..group_size {
        let offset = i * 64;
        idct_2d_8x8(&mut stack[offset..offset + 64], &tables.idct_coeff);
    }
}

#[inline]
fn dct_2d_8x8(block: &mut [f32], coeffs: &[f32; 64]) {
    for i in 0..8 {
        dct_1d_8(&mut block[i * 8..(i + 1) * 8], coeffs);
    }
    transpose_8x8(block);
    for i in 0..8 {
        dct_1d_8(&mut block[i * 8..(i + 1) * 8], coeffs);
    }
    transpose_8x8(block);
}

#[inline]
fn idct_2d_8x8(block: &mut [f32], coeffs: &[f32; 64]) {
    transpose_8x8(block);
    for i in 0..8 {
        idct_1d_8(&mut block[i * 8..(i + 1) * 8], coeffs);
    }
    transpose_8x8(block);
    for i in 0..8 {
        idct_1d_8(&mut block[i * 8..(i + 1) * 8], coeffs);
    }
}

#[inline]
fn dct_1d_8(x: &mut [f32], coeffs: &[f32; 64]) {
    let mut tmp = [0.0; 8];
    tmp.copy_from_slice(x);
    for (k, x_k) in x[..8].iter_mut().enumerate() {
        let mut s = 0.0;
        let row_start = k * 8;
        for (n, &tmp_n) in tmp.iter().enumerate() {
            s += tmp_n * coeffs[row_start + n];
        }
        *x_k = s;
    }
}

#[inline]
fn idct_1d_8(x: &mut [f32], coeffs: &[f32; 64]) {
    let mut tmp = [0.0; 8];
    tmp.copy_from_slice(x);
    for (n, x_n) in x[..8].iter_mut().enumerate() {
        let mut s = 0.0;
        let row_start = n * 8;
        for (k, &tmp_k) in tmp.iter().enumerate() {
            s += tmp_k * coeffs[row_start + k];
        }
        *x_n = s;
    }
}

#[inline]
fn transpose_8x8(b: &mut [f32]) {
    for y in 0..8 {
        for x in (y + 1)..8 {
            b.swap(y * 8 + x, x * 8 + y);
        }
    }
}

#[inline]
fn walsh_hadamard_1d(data: &mut [f32]) {
    let n = data.len();
    let mut h = 1;
    while h < n {
        for i in (0..n).step_by(h * 2) {
            for j in i..i + h {
                let x = data[j];
                let y = data[j + h];
                data[j] = x + y;
                data[j + h] = x - y;
            }
        }
        h *= 2;
    }
    let scale = 1.0 / (n as f32).sqrt();
    for x in data {
        *x *= scale;
    }
}

fn split_channels(img: &Rgb32FImage) -> Vec<Vec<f32>> {
    let (w, h) = img.dimensions();
    let size = (w * h) as usize;
    let mut r = vec![0.0; size];
    let mut g = vec![0.0; size];
    let mut b = vec![0.0; size];
    for (i, p) in img.pixels().enumerate() {
        r[i] = p[0] * 255.0;
        g[i] = p[1] * 255.0;
        b[i] = p[2] * 255.0;
    }
    vec![r, g, b]
}

fn merge_channels(channels: &[Vec<f32>], w: u32, h: u32) -> Rgb32FImage {
    let mut img = Rgb32FImage::new(w, h);
    for (i, p) in img.pixels_mut().enumerate() {
        let r = channels[0][i].clamp(0.0, 255.0) / 255.0;
        let g = channels[1][i].clamp(0.0, 255.0) / 255.0;
        let b = channels[2][i].clamp(0.0, 255.0) / 255.0;
        *p = Rgb([r, g, b]);
    }
    img
}

fn prev_power_of_two(x: usize) -> usize {
    if x == 0 {
        return 0;
    }
    let mut p = 1;
    while p * 2 <= x {
        p *= 2;
    }
    p
}

fn gaussian_blur_1ch(data: &[f32], width: usize, height: usize, sigma: f32) -> Vec<f32> {
    let radius = (3.0 * sigma).ceil() as usize;
    let klen = 2 * radius + 1;
    let mut kernel = vec![0.0f32; klen];
    let two_s2 = 2.0 * sigma * sigma;
    for (i, kernel_val) in kernel.iter_mut().enumerate() {
        let k = i as f32 - radius as f32;
        *kernel_val = (-k * k / two_s2).exp();
    }
    let ksum: f32 = kernel.iter().sum();
    for k in &mut kernel {
        *k /= ksum;
    }

    let mut tmp = vec![0.0f32; width * height];
    for y in 0..height {
        let row_in = &data[y * width..(y + 1) * width];
        let row_out = &mut tmp[y * width..(y + 1) * width];
        for (x, out_val) in row_out.iter_mut().enumerate() {
            let mut val = 0.0f32;
            let mut wsum = 0.0f32;
            let x0 = x as isize - radius as isize;
            for (ki, &kernel_val) in kernel.iter().enumerate() {
                let kx = x0 + ki as isize;
                if kx >= 0 && kx < width as isize {
                    val += row_in[kx as usize] * kernel_val;
                    wsum += kernel_val;
                }
            }
            *out_val = val / wsum;
        }
    }

    let mut out = vec![0.0f32; width * height];
    for y in 0..height {
        let y0 = y as isize - radius as isize;
        for x in 0..width {
            let mut val = 0.0f32;
            let mut wsum = 0.0f32;
            for (ki, &kernel_val) in kernel.iter().enumerate() {
                let ky = y0 + ki as isize;
                if ky >= 0 && ky < height as isize {
                    val += tmp[ky as usize * width + x] * kernel_val;
                    wsum += kernel_val;
                }
            }
            out[y * width + x] = val / wsum;
        }
    }

    out
}

// ============ BLITZRAW: what the noise in a real frame measures ============
#[cfg(test)]
mod noise_probe {
    //! What noise measures at each of the three sizes the shader cleans at.
    //!
    //! The thresholds in `shader.wgsl` decide what counts as noise and what
    //! counts as detail. Guessing them is how you get a filter that scrubs the
    //! shadows flat and leaves the highlights dirty, which is exactly what the
    //! old one did. These print rather than assert: they are measurements, and
    //! they move with the camera, the lens and the ISO.
    //!
    //! ```text
    //! set RAPIDRAW_TEST_NOISE_RAW=D:\MyPhotos\<shoot>\RAW\_DSC0218.NEF
    //! cargo test --lib noise_probe -- --nocapture
    //! ```
    //!
    //! `RAPIDRAW_TEST_NOISE_PRESHARPEN` and `RAPIDRAW_TEST_NOISE_COLORNR`
    //! override the two Base preprocessing settings, so the same frame can be
    //! measured with them on and off.

    use super::gaussian_blur_1ch;
    use crate::app_settings::AppSettings;
    use crate::image_loader::load_base_image_from_bytes;

    /// The reach of each blur in pixels at a 1080 px short edge, and the cap.
    /// These have to stay in step with the `run_blur` calls in
    /// `gpu_processing.rs`, or the numbers below describe a filter nobody runs.
    const BASE_RADII: [f32; 3] = [2.0, 6.0, 16.0];
    const REACH_CAP: f32 = 64.0;

    /// How much of the crop to throw away before measuring. A blur near the
    /// edge of a crop reads clamped pixels, which are not noise.
    const MARGIN: usize = 70;

    const CROP: u32 = 1400;

    fn median_of(values: &mut [f32]) -> f32 {
        if values.is_empty() {
            return 0.0;
        }
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        values[values.len() / 2]
    }

    fn percentile(values: &[f32], p: f32) -> f32 {
        if values.is_empty() {
            return 0.0;
        }
        let idx = ((values.len() as f32 - 1.0) * p).round() as usize;
        values[idx.min(values.len() - 1)]
    }

    #[test]
    fn what_the_noise_measures_at_each_size() {
        let Ok(path) = std::env::var("RAPIDRAW_TEST_NOISE_RAW") else {
            eprintln!("RAPIDRAW_TEST_NOISE_RAW unset, skipping");
            return;
        };

        let mut settings = AppSettings::default();
        if let Ok(v) = std::env::var("RAPIDRAW_TEST_NOISE_PRESHARPEN") {
            settings.raw_preprocessing_sharpening = v.parse().ok();
        }
        if let Ok(v) = std::env::var("RAPIDRAW_TEST_NOISE_COLORNR") {
            settings.raw_preprocessing_color_nr = v.parse().ok();
        }

        let bytes = std::fs::read(&path).expect("could not read the file");
        let decoded = load_base_image_from_bytes(&bytes, &path, false, &settings, None)
            .expect("could not decode the file");

        let (full_w, full_h) = (decoded.width(), decoded.height());
        // The same resolution scale the shader uses, taken from the whole
        // frame, so the radii below are the ones an export would really run.
        let scale = (full_w.min(full_h) as f32) / 1080.0;

        let crop_w = CROP.min(full_w);
        let crop_h = CROP.min(full_h);
        let crop = decoded
            .crop_imm((full_w - crop_w) / 2, (full_h - crop_h) / 2, crop_w, crop_h)
            .to_rgb32f();
        let (w, h) = (crop.width() as usize, crop.height() as usize);

        println!();
        println!("file       {}", path);
        println!("full size  {} x {}", full_w, full_h);
        println!(
            "settings   pre-sharpen {:?}, base colour NR {:?}",
            settings.raw_preprocessing_sharpening, settings.raw_preprocessing_color_nr
        );
        println!("crop       {} x {} from the centre", w, h);
        println!("scale      {:.2} (short edge / 1080)", scale);

        let src = crop.as_raw();
        let n = w * h;
        let mut luma = vec![0.0f32; n];
        let mut ch_r = vec![0.0f32; n];
        let mut ch_b = vec![0.0f32; n];
        for i in 0..n {
            let (r, g, b) = (src[i * 3], src[i * 3 + 1], src[i * 3 + 2]);
            let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            luma[i] = y;
            ch_r[i] = r - y;
            ch_b[i] = b - y;
        }

        let radii: Vec<f32> = BASE_RADII
            .iter()
            .map(|b| (b * scale).ceil().clamp(1.0, REACH_CAP))
            .collect();
        println!(
            "radii      {:.0}, {:.0}, {:.0} px{}",
            radii[0],
            radii[1],
            radii[2],
            if radii[2] >= REACH_CAP {
                "  (the coarse one is at the cap)"
            } else {
                ""
            }
        );

        let blur_all = |data: &[f32]| -> Vec<Vec<f32>> {
            radii
                .iter()
                .map(|r| gaussian_blur_1ch(data, w, h, r / 2.0))
                .collect()
        };
        let by = blur_all(&luma);
        let br = blur_all(&ch_r);
        let bb = blur_all(&ch_b);

        // Brightness buckets, taken from the coarsest blur so that noise
        // cannot decide which bucket a pixel lands in.
        const EDGES: [f32; 6] = [0.0, 0.01, 0.04, 0.12, 0.35, 1.0e9];
        const NAMES: [&str; 5] = ["deep shadow", "shadow", "low mid", "mid", "highlight"];

        for (bi, band_name) in ["fine (centre - b1)", "mid (b1 - b2)", "coarse (b2 - b3)"]
            .iter()
            .enumerate()
        {
            println!();
            println!("  {}", band_name);
            println!(
                "  {:<12} {:>9} {:>10} {:>10} {:>10} {:>10}",
                "level", "pixels", "luma sig", "luma p99", "col sig", "col p99"
            );

            for b in 0..5 {
                let (lo, hi) = (EDGES[b], EDGES[b + 1]);
                let mut l_vals = Vec::new();
                let mut c_vals = Vec::new();

                for y in MARGIN..h.saturating_sub(MARGIN) {
                    for x in MARGIN..w.saturating_sub(MARGIN) {
                        let i = y * w + x;
                        let level = by[2][i];
                        if level < lo || level >= hi {
                            continue;
                        }
                        let (dy, dr, db) = match bi {
                            0 => (luma[i] - by[0][i], ch_r[i] - br[0][i], ch_b[i] - bb[0][i]),
                            1 => (
                                by[0][i] - by[1][i],
                                br[0][i] - br[1][i],
                                bb[0][i] - bb[1][i],
                            ),
                            _ => (
                                by[1][i] - by[2][i],
                                br[1][i] - br[2][i],
                                bb[1][i] - bb[2][i],
                            ),
                        };
                        l_vals.push(dy.abs());
                        c_vals.push(dr.abs().max(db.abs()));
                    }
                }

                if l_vals.len() < 500 {
                    continue;
                }
                let count = l_vals.len();
                // 1.4826 turns a median absolute deviation into a standard
                // deviation for gaussian noise. It is robust: real detail is a
                // minority of the pixels, so it does not drag the median.
                let l_sigma = median_of(&mut l_vals) * 1.4826;
                let c_sigma = median_of(&mut c_vals) * 1.4826;
                println!(
                    "  {:<12} {:>9} {:>10.5} {:>10.5} {:>10.5} {:>10.5}",
                    NAMES[b],
                    count,
                    l_sigma,
                    percentile(&l_vals, 0.99),
                    c_sigma,
                    percentile(&c_vals, 0.99),
                );
            }
        }

        println!();
        println!("  'sig' is the noise. 'p99' is what real detail looks like in");
        println!("  that band. A threshold has to sit between the two.");
        println!();
    }

    /// What SCUNet does to a real frame, and how long it takes.
    ///
    /// Runs the model the app would run, through the graphics card, on a crop
    /// of a real raw, and reports the noise before and against the detail
    /// before. That is the whole question: does it take the noise without
    /// taking the picture with it.
    ///
    /// ```text
    /// set RAPIDRAW_TEST_NOISE_RAW=D:\MyPhotos\<shoot>\RAW\_DSC0218.NEF
    /// cargo test --lib noise_probe::what_scunet -- --nocapture
    /// ```
    #[test]
    fn what_scunet_takes_and_how_long_it_takes_it() {
        let Ok(path) = std::env::var("RAPIDRAW_TEST_NOISE_RAW") else {
            eprintln!("RAPIDRAW_TEST_NOISE_RAW unset, skipping");
            return;
        };

        let resources = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources");
        let models =
            std::path::PathBuf::from(std::env::var("RAPIDRAW_TEST_MODELS_DIR").unwrap_or_default());
        let model_path = models.join("scunet_color_real_psnr.onnx");
        if !model_path.exists() {
            eprintln!("RAPIDRAW_TEST_MODELS_DIR must hold scunet_color_real_psnr.onnx, skipping");
            return;
        }
        unsafe {
            std::env::set_var("ORT_DYLIB_PATH", resources.join("onnxruntime.dll"));
            let existing = std::env::var("PATH").unwrap_or_default();
            std::env::set_var(
                "PATH",
                format!("{};{existing}", resources.to_string_lossy()),
            );
        }

        let bytes = std::fs::read(&path).expect("could not read the file");
        let decoded =
            load_base_image_from_bytes(&bytes, &path, false, &AppSettings::default(), None)
                .expect("could not decode the file");
        let (fw, fh) = (decoded.width(), decoded.height());
        let side = 1008u32.min(fw).min(fh); // two whole tiles of 504
        let before = decoded
            .crop_imm((fw - side) / 2, (fh - side) / 2, side, side)
            .to_rgb32f();

        let session = crate::ai_processing::scunet_session(&model_path)
            .expect("DirectML would not take SCUNet");
        for input in &session.inputs {
            println!("input      {} {:?}", input.name, input.input_type);
        }
        for output in &session.outputs {
            println!("output     {} {:?}", output.name, output.output_type);
        }

        let started = std::time::Instant::now();
        let out = crate::ai_processing::scunet_denoise_inner(
            &before,
            1.0,
            &std::sync::Mutex::new(session),
            &|_| {},
        )
        .expect("SCUNet failed");
        let elapsed = started.elapsed();
        let after = out.to_rgb32f();

        let (w, h) = (side as usize, side as usize);
        let luma = |img: &image::Rgb32FImage| -> Vec<f32> {
            let s = img.as_raw();
            (0..w * h)
                .map(|i| 0.2126 * s[i * 3] + 0.7152 * s[i * 3 + 1] + 0.0722 * s[i * 3 + 2])
                .collect()
        };
        let (l0, l1) = (luma(&before), luma(&after));
        let b0 = gaussian_blur_1ch(&l0, w, h, 4.0);
        let b1 = gaussian_blur_1ch(&l1, w, h, 4.0);

        let mut noise0 = Vec::new();
        let mut noise1 = Vec::new();
        let mut kept = Vec::new();
        for y in MARGIN..h - MARGIN {
            for x in MARGIN..w - MARGIN {
                let i = y * w + x;
                let (d0, d1) = (l0[i] - b0[i], l1[i] - b1[i]);
                noise0.push(d0.abs());
                noise1.push(d1.abs());
                // Only where the detail is far above the noise, so what is
                // measured here is the picture rather than the grain.
                if d0.abs() > 0.045 {
                    kept.push(d1.abs() / d0.abs());
                }
            }
        }
        let med = |v: &mut Vec<f32>| median_of(v);

        println!();
        println!("crop       {side} x {side} from a {fw} x {fh} frame");
        println!("time       {:?} on the graphics card", elapsed);
        println!(
            "fine noise {:.5} before, {:.5} after  ({:.0}% gone)",
            med(&mut noise0.clone()) * 1.4826,
            med(&mut noise1.clone()) * 1.4826,
            (1.0 - med(&mut noise1) / med(&mut noise0).max(1e-9)) * 100.0
        );
        kept.sort_by(|a, c| a.partial_cmp(c).unwrap_or(std::cmp::Ordering::Equal));
        println!(
            "detail     {:.0}% kept at the median, over {} coefficients",
            percentile(&kept, 0.50) * 100.0,
            kept.len()
        );
        println!();
    }

    /// How much of a known noise each band ends up holding.
    ///
    /// The measurement above cannot tell blotch noise from a real gradient at
    /// the coarse sizes, because both are smooth and large. This one can: the
    /// noise is made here, so its size is known exactly, and there is no
    /// picture underneath to confuse it.
    ///
    /// Three kinds, because sensor noise is not one thing. Straight from the
    /// photosite it is white. Demosaic then spreads each photosite's error
    /// across its neighbours, which pushes energy down into the wider bands.
    /// That spreading is what a blotch is.
    #[test]
    fn how_much_of_a_known_noise_each_band_holds() {
        const W: usize = 1024;
        const RADII: [f32; 3] = [8.0, 23.0, 60.0];
        const MARGIN: usize = 70;
        const INPUT_SIGMA: f32 = 0.01;

        // A plain xorshift, so the numbers are the same on every machine and
        // every run. A measurement nobody can repeat is not a measurement.
        let mut state: u32 = 0x9E3779B9;
        let mut next = || -> f32 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state as f32 / u32::MAX as f32) - 0.5
        };
        // Twelve uniforms summed is gaussian enough, and has variance 1.
        let mut white = vec![0.0f32; W * W];
        for v in white.iter_mut() {
            let mut acc = 0.0;
            for _ in 0..12 {
                acc += next();
            }
            *v = acc * INPUT_SIGMA;
        }

        let renormalise = |mut data: Vec<f32>| -> Vec<f32> {
            let mean: f32 = data.iter().sum::<f32>() / data.len() as f32;
            let var: f32 =
                data.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / data.len() as f32;
            let k = INPUT_SIGMA / var.sqrt().max(1e-9);
            for v in data.iter_mut() {
                *v = (*v - mean) * k;
            }
            data
        };

        let cases: [(&str, Vec<f32>); 3] = [
            ("white (straight off the sensor)", white.clone()),
            (
                "lightly spread (demosaic)",
                renormalise(gaussian_blur_1ch(&white, W, W, 0.8)),
            ),
            (
                "blotchy (what a high ISO frame looks like)",
                renormalise(gaussian_blur_1ch(&white, W, W, 4.0)),
            ),
        ];

        println!();
        println!("  input noise sigma {:.4} in every case", INPUT_SIGMA);
        println!("  blur radii {:?} px", RADII);
        println!();
        println!(
            "  {:<44} {:>10} {:>10} {:>10}",
            "noise kind", "fine", "mid", "coarse"
        );

        for (name, data) in cases.iter() {
            let blurs: Vec<Vec<f32>> = RADII
                .iter()
                .map(|r| gaussian_blur_1ch(data, W, W, r / 2.0))
                .collect();

            let band_sigma = |pick: &dyn Fn(usize) -> f32| -> f32 {
                let mut acc = 0.0f64;
                let mut count = 0usize;
                for y in MARGIN..W - MARGIN {
                    for x in MARGIN..W - MARGIN {
                        let d = pick(y * W + x) as f64;
                        acc += d * d;
                        count += 1;
                    }
                }
                (acc / count.max(1) as f64).sqrt() as f32
            };

            let fine = band_sigma(&|i| data[i] - blurs[0][i]);
            let mid = band_sigma(&|i| blurs[0][i] - blurs[1][i]);
            let coarse = band_sigma(&|i| blurs[1][i] - blurs[2][i]);

            println!(
                "  {:<44} {:>10.5} {:>10.5} {:>10.5}",
                name, fine, mid, coarse
            );
            println!(
                "  {:<44} {:>9.1}% {:>9.1}% {:>9.1}%",
                "  as a share of the input",
                fine / INPUT_SIGMA * 100.0,
                mid / INPUT_SIGMA * 100.0,
                coarse / INPUT_SIGMA * 100.0
            );
        }

        println!();
        println!("  A threshold for a band is a few times the number on its row.");
        println!();
    }
}
// ========== BLITZRAW END: what the noise in a real frame measures ==========
