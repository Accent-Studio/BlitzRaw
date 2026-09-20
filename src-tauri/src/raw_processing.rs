use crate::image_processing::apply_orientation;
use anyhow::{Result, anyhow};
use image::{DynamicImage, ImageBuffer, Rgba};
use rawler::{
    decoders::{Orientation, RawDecodeParams},
    imgop::develop::{DemosaicAlgorithm, Intermediate, ProcessingStep, RawDevelop},
    rawimage::{RawImage, RawImageData, RawPhotometricInterpretation},
    rawsource::RawSource,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// Upstream's entry point, kept so its callers and any future merge still find
/// the name they expect. Nothing in the fork calls it any more: our own callers
/// need the profile as well, which `develop_raw_image_with_profile` returns.
#[allow(dead_code)]
pub fn develop_raw_image(
    file_bytes: &[u8],
    fast_demosaic: bool,
    highlight_compression: f32,
    linear_mode: String,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
) -> Result<DynamicImage> {
    develop_raw_image_with_profile(
        file_bytes,
        fast_demosaic,
        highlight_compression,
        linear_mode,
        cancel_token,
    )
    .map(|(image, _)| image)
}

// ===================== BLITZRAW: camera-calibrated colour =====================
/// Develops a RAW file and hands back the camera profile it was developed with.
///
/// The image alone is not enough any more. Its colour is now the as-shot render
/// of a specific calibration, and moving the white balance later means knowing
/// which one, so the caller gets both. `develop_raw_image` stays for callers
/// that only want pixels.
pub fn develop_raw_image_with_profile(
    file_bytes: &[u8],
    fast_demosaic: bool,
    highlight_compression: f32,
    linear_mode: String,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
) -> Result<(DynamicImage, Option<crate::camera_profile::CameraProfile>)> {
    let (developed_image, orientation, profile) = develop_internal(
        file_bytes,
        fast_demosaic,
        highlight_compression,
        linear_mode,
        cancel_token,
    )?;
    Ok((apply_orientation(developed_image, orientation), profile))
}
// =================== BLITZRAW END: camera-calibrated colour ===================

fn is_linear_raw_format(raw_image: &RawImage) -> bool {
    matches!(
        raw_image.photometric,
        RawPhotometricInterpretation::LinearRaw
    )
}

// ===================== BLITZRAW: camera-calibrated colour =====================

/// Scales each sensel by the multiplier for the colour its filter passes.
///
/// Doing this before demosaic rather than after is the whole reason it lives
/// here. A demosaic interpolates between neighbouring sensels, and on an
/// unbalanced mosaic those neighbours sit at very different levels: on a Z9
/// under tungsten the red channel runs at roughly half the green. Interpolating
/// across that step and only correcting afterwards smears the imbalance into
/// edge colour, which is the classic false colour fringing on high-contrast
/// detail. Balancing first gives the demosaic a mosaic whose neighbours already
/// agree, which is what every algorithm it might use assumes.
///
/// Expects data already through `apply_scaling`, so black level is gone and the
/// values are a plain linear ratio.
fn apply_white_balance_to_mosaic(raw_image: &mut RawImage, multipliers: [f32; 3]) {
    use rawler::cfa::CFAColor;

    let width = raw_image.width;

    match &raw_image.photometric {
        RawPhotometricInterpretation::Cfa(config) => {
            // A lookup per row keeps `color_at` out of the inner loop; the
            // pattern repeats along a row, so one row's worth is enough.
            let cfa = config.cfa.clone();
            let RawImageData::Float(data) = &mut raw_image.data else {
                // apply_scaling always leaves floats behind. If that ever
                // changes, skipping is safer than reinterpreting the bytes.
                log::warn!("Expected float data after scaling; skipping mosaic white balance");
                return;
            };

            let period = cfa.width.max(1);
            let mut row_scales = vec![1.0f32; period.max(1) * cfa.height.max(1)];
            for row in 0..cfa.height.max(1) {
                for col in 0..period {
                    row_scales[row * period + col] = match cfa.cfa_color_at(row, col) {
                        CFAColor::RED => multipliers[0],
                        CFAColor::GREEN => multipliers[1],
                        CFAColor::BLUE => multipliers[2],
                        // A four-colour or unusual filter is outside what the
                        // three multipliers describe, so leave it untouched.
                        _ => 1.0,
                    };
                }
            }

            let pattern_height = cfa.height.max(1);
            data.chunks_exact_mut(width)
                .enumerate()
                .for_each(|(row, line)| {
                    let base = (row % pattern_height) * period;
                    for (col, value) in line.iter_mut().enumerate() {
                        *value *= row_scales[base + (col % period)];
                    }
                });
        }
        RawPhotometricInterpretation::LinearRaw => {
            // Already demosaiced, so there is no pattern to follow: every pixel
            // carries all three channels.
            let components = raw_image.cpp;
            if components < 3 {
                return;
            }
            let RawImageData::Float(data) = &mut raw_image.data else {
                log::warn!("Expected float data after scaling; skipping linear white balance");
                return;
            };
            data.chunks_exact_mut(components).for_each(|pixel| {
                pixel[0] *= multipliers[0];
                pixel[1] *= multipliers[1];
                pixel[2] *= multipliers[2];
            });
        }
        RawPhotometricInterpretation::BlackIsZero => {}
    }
}

// =================== BLITZRAW END: camera-calibrated colour ===================

#[inline]
fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(3.0)
    }
}

fn develop_internal(
    file_bytes: &[u8],
    fast_demosaic: bool,
    highlight_compression: f32,
    linear_mode: String,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
) -> Result<(
    DynamicImage,
    Orientation,
    Option<crate::camera_profile::CameraProfile>,
)> {
    let check_cancel = || -> Result<()> {
        if let Some((tracker, generation)) = &cancel_token
            && tracker.load(Ordering::SeqCst) != *generation
        {
            return Err(anyhow!("Load cancelled"));
        }
        Ok(())
    };

    check_cancel()?;

    let source = RawSource::new_from_slice(file_bytes);
    let decoder = rawler::get_decoder(&source)?;

    check_cancel()?;
    let mut raw_image: RawImage = decoder.raw_image(&source, &RawDecodeParams::default(), false)?;

    let metadata = decoder.raw_metadata(&source, &RawDecodeParams::default())?;
    let orientation = metadata
        .exif
        .orientation
        .map(Orientation::from_u16)
        .unwrap_or(Orientation::Normal);

    let is_linear_format = is_linear_raw_format(&raw_image);

    let (apply_ungamma, apply_calibration) = match linear_mode.as_str() {
        "gamma" => (true, true),
        "skip_calib" => (false, false),
        "gamma_skip_calib" => (true, false),
        _ => (false, true),
    };

    let original_white_level = raw_image
        .whitelevel
        .0
        .first()
        .cloned()
        .unwrap_or(u16::MAX as u32) as f32;
    let original_black_level = raw_image
        .blacklevel
        .levels
        .first()
        .map(|r| r.as_f32())
        .unwrap_or(0.0);

    for level in raw_image.whitelevel.0.iter_mut() {
        *level = u32::MAX;
    }

    let mut developer = RawDevelop::default();

    if is_linear_format {
        developer.steps.retain(|&step| {
            step != ProcessingStep::SRgb
                && step != ProcessingStep::Demosaic
                && (apply_calibration || step != ProcessingStep::Calibrate)
        });
    } else if fast_demosaic {
        developer.demosaic_algorithm = DemosaicAlgorithm::Speed;
        developer.steps.retain(|&step| step != ProcessingStep::SRgb);
    } else {
        developer.steps.retain(|&step| step != ProcessingStep::SRgb);
    }

    raw_image.wb_coeffs =
        crate::multi_exposure::neutralize_wb_if_multiexposure(raw_image.wb_coeffs, file_bytes);

    // ===================== BLITZRAW: camera-calibrated colour =====================
    // Everything to the matching END marker replaces rawler's ProcessingStep::
    // Calibrate. This is a real divergence from upstream, not an addition, and
    // it is the reason the temperature slider can read in Kelvin.
    //
    // rawler's Calibrate does three things we cannot live with. It picks the
    // D65 colour matrix and ignores the second calibration illuminant, so no
    // interpolation by temperature happens and the same Kelvin means different
    // colour on different files. It row-normalises that matrix, which is the
    // old dcraw trick of folding an implicit white balance into the matrix
    // itself, on top of the real one. And it ignores ForwardMatrix entirely,
    // which is the tag Adobe fits specifically to hold the neutral axis exact.
    // It also clips negatives before we ever see them, throwing away every
    // colour outside sRGB.
    //
    // In its place: white balance multipliers on the mosaic *before* demosaic,
    // where they belong, then a single camera-to-sRGB matrix built from the
    // real calibration for the white the camera chose. See camera_profile.rs.
    //
    // Falls back to rawler's own path whenever a file has no usable profile,
    // so nothing that worked before stops working.
    let profile = crate::camera_profile::CameraProfile::from_raw(&raw_image, file_bytes);
    let calibration = profile.as_ref().and_then(|p| {
        // The "skip calibration" linear modes exist to diagnose colour
        // problems by turning the conversion off. Honour them.
        if !apply_calibration {
            return None;
        }
        p.balanced_camera_to_linear_srgb_as_shot()
            .map(|matrix| (matrix, p.as_shot_multipliers()))
    });

    if let Some((_, multipliers)) = &calibration {
        // Black level first: white balance is a ratio between channels, and
        // scaling a pedestal along with the signal would skew every shadow.
        // apply_scaling does that subtraction, so it has to run here rather
        // than inside develop_intermediate, and Rescale comes back out below.
        raw_image.apply_scaling()?;
        apply_white_balance_to_mosaic(&mut raw_image, *multipliers);

        developer.steps.retain(|&step| {
            step != ProcessingStep::Rescale
                && step != ProcessingStep::WhiteBalance
                && step != ProcessingStep::Calibrate
        });
    }
    // =================== BLITZRAW END: camera-calibrated colour ===================

    check_cancel()?;
    let mut developed_intermediate = developer.develop_intermediate(&raw_image)?;

    drop(raw_image);

    let denominator = (original_white_level - original_black_level).max(1.0);
    let rescale_factor = (u32::MAX as f32 - original_black_level) / denominator;

    let safe_highlight_compression = highlight_compression.max(1.01);

    let clamp_limit = if fast_demosaic {
        1.0
    } else {
        safe_highlight_compression
    };

    check_cancel()?;

    match &mut developed_intermediate {
        Intermediate::Monochrome(pixels) => {
            pixels.data.iter_mut().for_each(|p| {
                let mut linear_val = *p * rescale_factor;
                if is_linear_format && apply_ungamma {
                    linear_val = srgb_to_linear(linear_val.clamp(0.0, 1.0));
                }
                *p = linear_val.clamp(0.0, clamp_limit);
            });
        }
        Intermediate::ThreeColor(pixels) => {
            // BLITZRAW: the camera-to-sRGB matrix, or None when we fell back to
            // rawler's own calibration and the pixels are already in sRGB.
            let camera_matrix = calibration.as_ref().map(|(matrix, _)| *matrix);

            pixels.data.iter_mut().for_each(|p| {
                let mut r = (p[0] * rescale_factor).max(0.0);
                let mut g = (p[1] * rescale_factor).max(0.0);
                let mut b = (p[2] * rescale_factor).max(0.0);

                if is_linear_format && apply_ungamma {
                    r = srgb_to_linear(r.clamp(0.0, 1.0));
                    g = srgb_to_linear(g.clamp(0.0, 1.0));
                    b = srgb_to_linear(b.clamp(0.0, 1.0));
                }

                // BLITZRAW: camera RGB to linear sRGB. Placed here so the
                // highlight compression below still sees display-space values,
                // exactly as it did when rawler produced them.
                if let Some(m) = &camera_matrix {
                    let converted = crate::camera_profile::mat_apply(m, [r, g, b]);
                    r = converted[0].max(0.0);
                    g = converted[1].max(0.0);
                    b = converted[2].max(0.0);
                }

                let max_c = r.max(g).max(b);

                let (final_r, final_g, final_b) = if max_c > 1.0 {
                    let min_c = r.min(g).min(b);
                    let compression_factor =
                        (1.0 - (max_c - 1.0) / (safe_highlight_compression - 1.0)).clamp(0.0, 1.0);
                    let compressed_r = min_c + (r - min_c) * compression_factor;
                    let compressed_g = min_c + (g - min_c) * compression_factor;
                    let compressed_b = min_c + (b - min_c) * compression_factor;
                    let compressed_max = compressed_r.max(compressed_g).max(compressed_b);

                    if compressed_max > 1e-6 {
                        let rescale = max_c / compressed_max;
                        (
                            compressed_r * rescale,
                            compressed_g * rescale,
                            compressed_b * rescale,
                        )
                    } else {
                        (max_c, max_c, max_c)
                    }
                } else {
                    (r, g, b)
                };

                p[0] = final_r.clamp(0.0, clamp_limit);
                p[1] = final_g.clamp(0.0, clamp_limit);
                p[2] = final_b.clamp(0.0, clamp_limit);
            });
        }
        Intermediate::FourColor(pixels) => {
            pixels.data.iter_mut().for_each(|p| {
                p.iter_mut().for_each(|c| {
                    let mut linear_val = *c * rescale_factor;
                    if is_linear_format && apply_ungamma {
                        linear_val = srgb_to_linear(linear_val.clamp(0.0, 1.0));
                    }
                    *c = linear_val.clamp(0.0, clamp_limit);
                });
            });
        }
    }

    let (width, height) = {
        let dim = developed_intermediate.dim();
        (dim.w as u32, dim.h as u32)
    };

    check_cancel()?;

    let dynamic_image = match developed_intermediate {
        Intermediate::ThreeColor(pixels) => {
            let buffer = ImageBuffer::<Rgba<f32>, _>::from_fn(width, height, |x, y| {
                let p = pixels.data[(y * width + x) as usize];
                Rgba([p[0], p[1], p[2], 1.0])
            });
            DynamicImage::ImageRgba32F(buffer)
        }
        Intermediate::Monochrome(pixels) => {
            let buffer = ImageBuffer::<Rgba<f32>, _>::from_fn(width, height, |x, y| {
                let p = pixels.data[(y * width + x) as usize];
                Rgba([p, p, p, 1.0])
            });
            DynamicImage::ImageRgba32F(buffer)
        }
        _ => {
            return Err(anyhow!("Unsupported intermediate format for conversion"));
        }
    };

    // BLITZRAW: the profile travels with the image; see develop_raw_image_with_profile.
    Ok((dynamic_image, orientation, profile))
}

pub fn get_fast_demosaic_scale_factor(
    file_bytes: &[u8],
    decoded_width: u32,
    decoded_height: u32,
) -> f32 {
    let source = RawSource::new_from_slice(file_bytes);
    if let Ok(decoder) = rawler::get_decoder(&source)
        && let Ok(raw_img) = decoder.raw_image(&source, &RawDecodeParams::default(), true)
    {
        let max_orig = (raw_img.width as f32).max(raw_img.height as f32);
        let max_comp = (decoded_width as f32).max(decoded_height as f32);
        if max_orig > 0.0 {
            let ratio = max_comp / max_orig;
            if ratio > 0.1 && ratio < 0.35 {
                return 0.25;
            } else if (0.35..0.75).contains(&ratio) {
                return 0.5;
            }
        }
    }
    1.0
}

// ===================== BLITZRAW: camera-calibrated colour =====================
#[cfg(test)]
mod blitzraw_tests {
    use crate::camera_profile::{CameraProfile, mat_apply};

    fn sample_dng() -> Option<std::path::PathBuf> {
        let path = std::path::PathBuf::from(std::env::var("RAPIDRAW_TEST_DNG").ok()?);
        if path.is_dir() {
            eprintln!("RAPIDRAW_TEST_DNG is a folder, skipping");
            return None;
        }
        Some(path)
    }

    fn profile_of(bytes: &[u8]) -> CameraProfile {
        crate::camera_profile::profile_from_bytes(bytes).expect("the file should carry a profile")
    }

    /// Develops a real file and reports what came out.
    ///
    /// The unit tests in camera_profile prove the arithmetic. This proves the
    /// pipeline actually runs it: that white balance reaches the mosaic, that
    /// the matrix reaches the pixels, and that the result lands in the range
    /// the rest of the pipeline expects rather than, say, a thousand times too
    /// dark. Skips silently when `RAPIDRAW_TEST_DNG` is unset.
    #[test]
    fn develops_a_real_file_into_sensible_linear_srgb() {
        let Some(path) = sample_dng() else { return };

        let bytes = std::fs::read(&path).expect("read file");
        // Fast demosaic: a quarter of the pixels, identical colour arithmetic.
        let (image, profile) =
            super::develop_raw_image_with_profile(&bytes, true, 2.5, String::new(), None)
                .expect("develop");
        let profile = profile.expect("the file should carry a camera profile");

        let buffer = image.as_rgba32f().expect("RAW develops to 32-bit float");
        let mut sums = [0.0f64; 3];
        let mut peak = 0.0f32;
        let mut counted = 0usize;

        for pixel in buffer.pixels() {
            for c in 0..3 {
                assert!(pixel[c].is_finite(), "developed a non-finite pixel");
                sums[c] += pixel[c] as f64;
                peak = peak.max(pixel[c]);
            }
            counted += 1;
        }
        assert!(counted > 0, "developed an empty image");

        let mean = [
            sums[0] / counted as f64,
            sums[1] / counted as f64,
            sums[2] / counted as f64,
        ];
        let (kelvin, tint) = profile.as_shot_temp_tint();

        eprintln!("{}", path.display());
        eprintln!("  {}x{}", image.width(), image.height());
        eprintln!("  as-shot {kelvin:.0}K tint {tint:.1}");
        eprintln!("  multipliers {:?}", profile.as_shot_multipliers());
        eprintln!("  mean rgb {mean:.4?}");
        eprintln!("  peak {peak:.4}");

        // A photograph is neither black nor entirely blown out. Outside this
        // means a scaling mistake, which is the failure a matrix change causes
        // most easily and shows least obviously.
        let overall = (mean[0] + mean[1] + mean[2]) / 3.0;
        assert!(
            (0.005..0.9).contains(&overall),
            "mean level {overall:.4} is not a plausible exposure"
        );
        assert!(peak <= 2.6, "peak {peak} exceeds the compression ceiling");

        // Once balanced, the channels should sit close together. A factor of
        // three apart would mean the white balance never took effect.
        let low = mean[0].min(mean[1]).min(mean[2]);
        let high = mean[0].max(mean[1]).max(mean[2]);
        assert!(
            high / low.max(1e-9) < 3.0,
            "channels are {:.2}x apart, so white balance did not take effect",
            high / low.max(1e-9)
        );
    }

    /// Runs the eyedropper arithmetic over patches of a real developed image
    /// and reports what it would have said. Diagnostic: the numbers matter more
    /// than the assertion, since what a given patch should read depends on what
    /// is in the frame.
    #[test]
    fn reports_what_the_eyedropper_would_pick() {
        use crate::camera_profile::{mat_apply, mat_invert, xy_to_temp_tint};

        let Some(path) = sample_dng() else { return };
        let bytes = std::fs::read(&path).expect("read file");
        let (image, profile) =
            super::develop_raw_image_with_profile(&bytes, true, 2.5, String::new(), None)
                .expect("develop");
        let profile = profile.expect("profile");
        let buffer = image.as_rgba32f().expect("float");

        let (as_shot_kelvin, as_shot_tint) = profile.as_shot_temp_tint();
        let as_shot = profile
            .camera_to_linear_srgb(profile.as_shot_xy())
            .expect("matrix");
        let to_camera = mat_invert(&as_shot).expect("invertible");

        eprintln!("  as-shot {as_shot_kelvin:.0}K {as_shot_tint:+.1}");

        let (w, h) = (image.width(), image.height());
        for (label, u, v) in [
            ("centre", 0.5f32, 0.5f32),
            ("upper left", 0.25, 0.25),
            ("lower right", 0.75, 0.75),
        ] {
            let (cx, cy) = ((u * (w - 1) as f32) as u32, (v * (h - 1) as f32) as u32);
            let mut total = [0.0f64; 3];
            let mut n = 0u32;
            for dy in -5i64..=5 {
                for dx in -5i64..=5 {
                    let (x, y) = (cx as i64 + dx, cy as i64 + dy);
                    if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
                        continue;
                    }
                    let p = buffer.get_pixel(x as u32, y as u32);
                    for c in 0..3 {
                        total[c] += p[c] as f64;
                    }
                    n += 1;
                }
            }
            let avg = [
                (total[0] / n as f64) as f32,
                (total[1] / n as f64) as f32,
                (total[2] / n as f64) as f32,
            ];
            let camera_rgb = mat_apply(&to_camera, avg);
            let (x, y) = profile.neutral_to_xy(camera_rgb);
            let (kelvin, tint) = xy_to_temp_tint(x, y);
            eprintln!(
                "  {label:<12} rendered {avg:.4?} -> camera {camera_rgb:.4?} -> {kelvin:.0}K {tint:+.1}"
            );
        }
    }

    /// At the as-shot white the render must not move at all, because the
    /// correction there is exactly `A * inverse(A)`. If this drifts, every
    /// photo opens looking slightly unlike how it was developed, which is the
    /// kind of error that hides for months.
    #[test]
    fn the_as_shot_correction_is_the_identity() {
        let Some(path) = sample_dng() else { return };
        let bytes = std::fs::read(&path).expect("read file");
        let profile = profile_of(&bytes);

        let (kelvin, tint) = profile.as_shot_temp_tint();
        let correction = profile
            .relative_correction(kelvin, tint)
            .expect("correction exists");

        for row in 0..3 {
            for col in 0..3 {
                let expected = if row == col { 1.0 } else { 0.0 };
                assert!(
                    (correction[row][col] - expected).abs() < 2e-3,
                    "as-shot correction is not the identity at [{row}][{col}]: {}",
                    correction[row][col]
                );
            }
        }
    }

    /// Raising Kelvin claims the light was warmer, which cools the render, and
    /// lowering it warms the render. This is the direction a photographer
    /// notices instantly and the one an inverted matrix would flip.
    #[test]
    fn raising_kelvin_cools_the_render() {
        let Some(path) = sample_dng() else { return };
        let bytes = std::fs::read(&path).expect("read file");
        let profile = profile_of(&bytes);
        let (as_shot_kelvin, tint) = profile.as_shot_temp_tint();

        let grey = [0.18f32, 0.18, 0.18];
        let warmth = |kelvin: f32| {
            let m = profile
                .relative_correction(kelvin, tint)
                .expect("correction exists");
            let out = mat_apply(&m, grey);
            // Red against blue, so overall brightness drops out.
            out[0] / out[2].max(1e-9)
        };

        let cooler = warmth(as_shot_kelvin * 0.7);
        let neutral = warmth(as_shot_kelvin);
        let warmer = warmth(as_shot_kelvin * 1.4);

        // The DNG recipe inverts the reference neutral into the matrix, so
        // unless that neutral is normalised the whole matrix scales with
        // temperature and the slider doubles as a brightness control. Measured
        // at 17.9% across one sweep before it was normalised.
        //
        // The invariant that pins it is not "a fixed pixel keeps its
        // luminance": a pixel that is neutral at one temperature is a coloured
        // pixel at another, and its luminance genuinely moves. What must hold
        // is that *the neutral at each temperature* renders identically, since
        // the forward matrix carries the reference neutral to the D50 white by
        // construction, whichever temperature it was interpolated at.
        for &kelvin in &[2000.0f32, 3000.0, 4000.0, 5500.0, 7000.0, 9000.0] {
            let xy = crate::camera_profile::temp_tint_to_xy(kelvin, 0.0);
            let m = profile
                .camera_to_linear_srgb(xy)
                .expect("conversion exists");
            let rendered = mat_apply(&m, profile.camera_neutral(xy));
            for channel in rendered {
                assert!(
                    (channel - 1.0).abs() < 5e-3,
                    "the neutral at {kelvin:.0}K renders as {rendered:?}, not white"
                );
            }
        }

        // A fixed pixel does move, and by how much is worth watching: a jump
        // back to double figures would mean the normalisation was lost again.
        let luma = |kelvin: f32| {
            let m = profile
                .relative_correction(kelvin, tint)
                .expect("correction exists");
            let out = mat_apply(&m, grey);
            0.2126 * out[0] + 0.7152 * out[1] + 0.0722 * out[2]
        };
        let (dark, mid, bright) = (
            luma(as_shot_kelvin * 0.7),
            luma(as_shot_kelvin),
            luma(as_shot_kelvin * 1.4),
        );
        let swing = (dark.max(bright).max(mid) / dark.min(bright).min(mid)) - 1.0;
        eprintln!("  luminance of one fixed grey: {dark:.4} / {mid:.4} / {bright:.4}");
        eprintln!("  swing across the sweep: {:.1}%", swing * 100.0);
        assert!(
            swing < 0.10,
            "a fixed pixel swings {:.1}% in luminance, which is too much to be the colour shift alone",
            swing * 100.0
        );

        eprintln!(
            "  red over blue: {:.0}K {cooler:.3}, {:.0}K {neutral:.3}, {:.0}K {warmer:.3}",
            as_shot_kelvin * 0.7,
            as_shot_kelvin,
            as_shot_kelvin * 1.4
        );
        assert!(
            warmer > neutral && neutral > cooler,
            "the Kelvin slider runs backwards"
        );
    }

    /// What a fully clipped sensor pixel becomes, reported rather than pinned.
    ///
    /// rawler row-normalised its matrix so clipped white came out exactly
    /// white, which hides the magenta highlight rather than solving it. Doing
    /// the conversion honestly means clipped highlights land off-white and the
    /// highlight compression has to carry them, so this is worth being able to
    /// watch. The neutral mid grey beside it is the part that must hold.
    #[test]
    fn reports_what_a_clipped_highlight_becomes() {
        let Some(path) = sample_dng() else { return };
        let bytes = std::fs::read(&path).expect("read file");
        let profile = profile_of(&bytes);

        let matrix = profile
            .balanced_camera_to_linear_srgb_as_shot()
            .expect("matrix");
        let m = profile.as_shot_multipliers();

        // This matrix takes already-balanced camera RGB, since the mosaic pass
        // applied the multipliers before demosaic. A neutral subject reads
        // as_shot_neutral on the sensor, which those multipliers carry to equal
        // channels, so an equal triple is what neutral looks like going in.
        let grey = mat_apply(&matrix, [0.18, 0.18, 0.18]);

        // A clipped pixel is every sensor channel at the white level, so it is
        // the multipliers themselves once balanced. They are not equal, which
        // is why clipped highlights are not white.
        let clipped = mat_apply(&matrix, [m[0], m[1], m[2]]);

        eprintln!("  a neutral mid grey renders as   {grey:.3?}");
        eprintln!("  a clipped sensor pixel renders as {clipped:.3?}");

        assert!(
            (grey[0] - grey[1]).abs() < 5e-3 && (grey[2] - grey[1]).abs() < 5e-3,
            "a neutral sensor value did not render neutral: {grey:?}"
        );

        // Every channel of a clipped pixel must land at or above white, so the
        // highlight compression sees it as a highlight and rolls it off. If one
        // channel came out below 1 the pixel would read as a coloured patch
        // rather than a blown one.
        assert!(
            clipped.iter().all(|c| *c > 0.9),
            "a clipped sensor pixel did not render as a highlight: {clipped:?}"
        );
    }
}
// =================== BLITZRAW END: camera-calibrated colour ===================
