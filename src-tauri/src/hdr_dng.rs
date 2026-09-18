//! Merges written as linear DNGs with JPEG XL pixels.
//!
//! # Why
//!
//! A merge is written today as an uncompressed 32-bit float TIFF, which for a
//! Z9 bracket is 545 MB: exactly width times height times three times four,
//! plus a header. Lightroom fits the same picture into 19 MB, and reading the
//! tag structure of one of theirs says how. It is not a container trick. The
//! main image is JPEG XL, which Adobe added to DNG in version 1.7, at 0.133
//! bytes a sample where 16-bit lossless JPEG-92 needs 1.5.
//!
//! Measured on one of ours, at distance 0.15: **19.7 MB**, against 545 MB for
//! the TIFF and 206 MB for the best a lossless DNG can do.
//!
//! # Why not through rawler's DngWriter
//!
//! Because it cannot: `DngCompression` has `Uncompressed` and `Lossless` and
//! nothing else, and adding a third means forking a dependency whose camera
//! database alone is 29 MB. Its `TiffWriter` and `DirectoryWriter` are public,
//! though, so the container can be assembled here from the same parts the
//! writer uses, and the fork stays unnecessary.
//!
//! # Why integer samples, when Adobe uses float
//!
//! So that we can read our own files. rawler chooses a decompressor by sample
//! format first and compression second: its `Uint` branch handles JPEG XL and
//! its `IEEEFP` branch does not. Sixteen-bit integers land in the branch that
//! already works. This is also exactly why Lightroom's own merges currently
//! open here as their 256x171 thumbnails, which is a separate fix and needs
//! that fork.
//!
//! Sixteen bits is not a compromise here in any case. `image-hdr` stretches the
//! merge to 0..1 before it is written, so there is no range above one to
//! preserve, and sixteen bits across that range is the same picture.
//!
//! # What the pixels are
//!
//! Not camera space. Our decode produces an as-shot render in sRGB, and the
//! merge is made from those, so the file describes sRGB primaries and a
//! neutral white: `AsShotNeutral` of one, and the standard matrix. Lightroom's
//! merges are camera-space because Lightroom merges in the raw domain, which is
//! a different design and not one this reaches for. The camera calibration that
//! gives the Kelvin slider its meaning keeps travelling in the `.rrdata`
//! sidecar, unchanged.
//!
//! **They are sRGB-encoded, not linear**, and that is the one thing this file
//! has to say out loud. `merge_hdr` finishes with `apply_linear_to_srgb` before
//! anything sees the result, so the merge held in memory, the preview, the old
//! TIFF and the samples here are all display-referred. The first version of
//! this module had it the other way round on the strength of a comment, tagged
//! the samples `LinearRaw` and stopped there, and every merge opened about two
//! and a half times too bright and flat, because the pipeline linearises a
//! non-raw file and trusts a raw one. Measured on a real merge: median sample
//! 0.468, which is mid-grey through a curve, where scene-linear would be 0.186.
//!
//! So the curve is declared, in the tag that exists for exactly this:
//! `LinearizationTable`, 65536 entries mapping each stored code to its linear
//! value. rawler applies it on decode, and so will anything else that reads
//! DNG, which is the point of writing it rather than special-casing our own
//! reader. Storing the curve and declaring it is also better than storing
//! linear samples: the bits land where the eye is, which is what the codec
//! wants, and the table costs 128 KB against a 20 MB file. Going back through
//! sixteen-bit integers costs rms 0.003 display levels out of 255, worst 0.025,
//! over six million samples, including the shadows where the loss was expected
//! to show.

use std::io::BufWriter;
use std::path::Path;

use anyhow::{Result, anyhow};
use rayon::prelude::*;
use image::DynamicImage;
use rawler::formats::tiff::writer::{DirectoryWriter, TiffWriter};
use rawler::formats::tiff::{CompressionMethod, PhotometricInterpretation};
use rawler::tags::{DngTag, TiffCommonTag};

/// Butteraugli distance the merges are written at.
///
/// Measured on a Z9 merge, in display units out of 255 after a shadow-lifting
/// curve, over 136 million samples:
///
/// | distance | size | rms | >=10 levels | >=50 |
/// |---|---|---|---|---|
/// | 0.1 | 24.4 MB | 0.33 | 116 | 1 |
/// | 0.15 | 19.7 MB | | | |
/// | 0.25 | 14.4 MB | 0.54 | 885 | 0 |
/// | 0.5 | 9.0 MB | 0.81 | 4261 | 1 |
///
/// Deliberately not the smallest. Distance 0.5 measures fine and would be nine
/// megabytes, but a master is worth paying weight for, and the difference
/// between 9 MB and 20 MB is nothing next to the 545 MB either replaces.
pub const DEFAULT_DISTANCE: f32 = 0.15;

/// Tile size the image is cut into before encoding.
///
/// Tiles rather than one strip because rawler decodes them through rayon's
/// `par_bridge`, so every core works at once. A single 45 megapixel codestream
/// took 2.1 seconds to open; this is the one lever that changes that, and it
/// speeds the write up the same way since the tiles encode in parallel too.
///
/// Both are multiples of sixteen, which TIFF requires. Lightroom picks 416 by
/// 400 for the same picture, which is smaller still: many small tiles cost a
/// little compression, since each carries its own header and sees less
/// context, and buy more parallelism.
const TILE_WIDTH: u32 = 512;
const TILE_HEIGHT: u32 = 512;

/// Longest edge of the thumbnail in the root directory.
///
/// Something has to be there: the root of a DNG is conventionally a small
/// preview, and our own loader falls back to it when a decode fails, so a
/// useless one would make a failure look like a success.
const THUMBNAIL_EDGE: u32 = 256;

/// XYZ to linear sRGB at D65, in the row-major order the DNG `ColorMatrix` tag
/// wants, written as a signed rational over ten thousand, which is the
/// precision Adobe's own files use.
///
/// D65 because that is sRGB's own white, and the tag has to agree with the
/// `CalibrationIlluminant` beside it. The first draft of this declared D65 and
/// then supplied the Bradford-adapted D50 matrix, which is a different thing
/// wearing the same label. It cost almost nothing, a per channel bias of about
/// 0.03% against codec noise of 0.5%, but a file that describes itself wrongly
/// is a trap for whatever reads it next.
const XYZ_D65_TO_SRGB: [f64; 9] = [
    3.2404542, -1.5371385, -0.4985314, //
    -0.9692660, 1.8760108, 0.0415560, //
    0.0556434, -0.2040259, 1.0572252,
];

/// The sRGB transfer curve as a DNG `LinearizationTable`.
///
/// Entry `i` is what the stored code `i` means in linear light, over the same
/// sixteen-bit range. rawler dithers between neighbouring entries as it applies
/// this, so the flat stretch at the bottom of the curve does not band.
///
/// Built rather than approximated: the curve here has to be the exact inverse
/// of `apply_linear_to_srgb`, or the merge comes back with a cast that no
/// amount of white balance will take out.
fn srgb_linearization_table() -> Vec<u16> {
    (0..=u16::MAX)
        .map(|code| {
            let value = code as f64 / u16::MAX as f64;
            let linear = if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            };
            (linear * u16::MAX as f64).round() as u16
        })
        .collect()
}

fn srational_10000(values: &[f64]) -> Vec<rawler::formats::tiff::SRational> {
    values
        .iter()
        .map(|v| rawler::formats::tiff::SRational::new((v * 10_000.0).round() as i32, 10_000))
        .collect()
}

/// One tile's worth of pixels, always a full tile.
///
/// Tiles at the right and bottom edges run past the picture, and the decoder
/// allocates for whole tiles and crops afterwards, so the overspill has to be
/// filled with something. The nearest real pixel rather than black: a hard edge
/// against black is expensive to encode and would bleed back over the border
/// when the codec rings.
fn extract_tile(
    image: &image::ImageBuffer<image::Rgb<u16>, Vec<u16>>,
    tile_x: u32,
    tile_y: u32,
) -> Vec<u16> {
    let (width, height) = (image.width(), image.height());
    let mut out = Vec::with_capacity((TILE_WIDTH * TILE_HEIGHT * 3) as usize);
    for row in 0..TILE_HEIGHT {
        let y = (tile_y * TILE_HEIGHT + row).min(height - 1);
        for column in 0..TILE_WIDTH {
            let x = (tile_x * TILE_WIDTH + column).min(width - 1);
            let pixel = image.get_pixel(x, y);
            out.extend_from_slice(&pixel.0);
        }
    }
    out
}

/// A merge put into the space a decode of the file it was written to produces.
///
/// A merge in memory is display-referred, because `merge_hdr` finishes with
/// `apply_linear_to_srgb`. What a decode gives back depends on where it was
/// written: a DNG says its samples are curved and hands back linear, while a
/// PNG is read in display space like any other ordinary picture.
///
/// This exists so that anything reusing a merge instead of decoding the file
/// again puts it in the right space, and so that one test can check the two
/// really do agree rather than checking a copy of the rule.
pub fn as_decoded(merged: DynamicImage, written_to: &Path) -> DynamicImage {
    if crate::formats::is_raw_file(written_to) {
        crate::image_processing::apply_srgb_to_linear(merged)
    } else {
        merged
    }
}

/// Encodes one merged image and writes it as a linear JPEG XL DNG.
///
/// `distance` is butteraugli's, where lower is better; see [`DEFAULT_DISTANCE`].
pub fn write_linear_jxl_dng(path: &Path, image: &DynamicImage, distance: f32) -> Result<()> {
    let rgb16 = image.to_rgb16();
    let (width, height) = (rgb16.width(), rgb16.height());
    if width == 0 || height == 0 {
        return Err(anyhow!("Refusing to write an empty merge"));
    }

    let columns = width.div_ceil(TILE_WIDTH);
    let rows = height.div_ceil(TILE_HEIGHT);
    let tiles: Vec<Vec<u8>> = (0..columns * rows)
        .into_par_iter()
        .map(|index| {
            let tile = extract_tile(&rgb16, index % columns, index / columns);
            let bytes: &[u8] = bytemuck::cast_slice(tile.as_slice());
            jxl_encoder::LossyConfig::new(distance)
                .encode(bytes, TILE_WIDTH, TILE_HEIGHT, jxl_encoder::PixelLayout::Rgb16)
                .map_err(|e| anyhow!("JPEG XL encode failed: {e}"))
        })
        .collect::<Result<Vec<_>>>()?;

    // Never larger than what it is a thumbnail of. resize fits within the box
    // given, and will happily enlarge to reach it, which on a small merge wrote
    // a preview several times the size of the picture.
    let thumb_edge = THUMBNAIL_EDGE.min(width.max(height));
    let thumbnail = image
        .resize(thumb_edge, thumb_edge, image::imageops::FilterType::Triangle)
        .to_rgb8();

    // Written beside and renamed, so an interrupted write never leaves a file
    // that looks like a merge and is not one.
    let temp = path.with_extension("dng.part");
    {
        let file = std::fs::File::create(&temp)?;
        let mut tiff = TiffWriter::new(BufWriter::new(file))
            .map_err(|e| anyhow!("Could not start the DNG: {e:?}"))?;

        // Both payloads first: an IFD can only point at bytes already written.
        let thumb_offset = tiff
            .write_data(&thumbnail)
            .map_err(|e| anyhow!("Could not write the thumbnail: {e:?}"))?;
        // In reading order, left to right and top to bottom, which is the
        // order a decoder puts them back in.
        let mut tile_offsets = Vec::with_capacity(tiles.len());
        let mut tile_sizes = Vec::with_capacity(tiles.len());
        for tile in &tiles {
            tile_offsets.push(
                tiff.write_data(tile)
                    .map_err(|e| anyhow!("Could not write a tile: {e:?}"))?,
            );
            tile_sizes.push(tile.len() as u32);
        }

        let mut raw_ifd = DirectoryWriter::new();
        raw_ifd.add_tag(TiffCommonTag::NewSubFileType, 0_u32);
        raw_ifd.add_tag(TiffCommonTag::ImageWidth, width);
        raw_ifd.add_tag(TiffCommonTag::ImageLength, height);
        raw_ifd.add_tag(TiffCommonTag::BitsPerSample, [16_u16, 16, 16]);
        // Integer, which is what puts this in the branch of rawler that can
        // read JPEG XL back. See the note at the top.
        raw_ifd.add_tag(TiffCommonTag::SampleFormat, [1_u16, 1, 1]);
        raw_ifd.add_tag(TiffCommonTag::SamplesPerPixel, 3_u16);
        raw_ifd.add_tag(TiffCommonTag::PhotometricInt, PhotometricInterpretation::LinearRaw);
        raw_ifd.add_tag(TiffCommonTag::Compression, CompressionMethod::JPEGXL);
        raw_ifd.add_tag(TiffCommonTag::TileWidth, TILE_WIDTH);
        raw_ifd.add_tag(TiffCommonTag::TileLength, TILE_HEIGHT);
        raw_ifd.add_tag(TiffCommonTag::TileOffsets, tile_offsets.as_slice());
        raw_ifd.add_tag(TiffCommonTag::TileByteCounts, tile_sizes.as_slice());
        // Says the samples above are sRGB-encoded and what they mean in
        // linear light. Without it a reader takes them at face value and the
        // merge opens far too bright; see the note at the top of the module.
        // Black and white levels are in linearised units, which is why they
        // stay 0 and full scale.
        raw_ifd.add_tag(DngTag::LinearizationTable, srgb_linearization_table().as_slice());
        raw_ifd.add_tag(DngTag::WhiteLevel, [u16::MAX, u16::MAX, u16::MAX]);
        raw_ifd.add_tag(DngTag::BlackLevel, [0_u16, 0, 0]);

        let raw_offset = raw_ifd
            .build(&mut tiff)
            .map_err(|e| anyhow!("Could not write the image directory: {e:?}"))?;

        let mut root = DirectoryWriter::new();
        root.add_tag(TiffCommonTag::NewSubFileType, 1_u32);
        root.add_tag(TiffCommonTag::ImageWidth, thumbnail.width());
        root.add_tag(TiffCommonTag::ImageLength, thumbnail.height());
        root.add_tag(TiffCommonTag::BitsPerSample, [8_u16, 8, 8]);
        root.add_tag(TiffCommonTag::SampleFormat, [1_u16, 1, 1]);
        root.add_tag(TiffCommonTag::SamplesPerPixel, 3_u16);
        root.add_tag(TiffCommonTag::PhotometricInt, PhotometricInterpretation::RGB);
        root.add_tag(TiffCommonTag::Compression, CompressionMethod::None);
        root.add_tag(TiffCommonTag::StripOffsets, thumb_offset);
        root.add_tag(TiffCommonTag::StripByteCounts, thumbnail.len() as u32);
        root.add_tag(TiffCommonTag::RowsPerStrip, thumbnail.height());

        // 1.7 is the version that defines JPEG XL, so a reader older than that
        // is told plainly it cannot manage rather than being left to guess.
        root.add_tag(DngTag::DNGVersion, [1_u8, 7, 0, 0]);
        root.add_tag(DngTag::DNGBackwardVersion, [1_u8, 7, 0, 0]);
        root.add_tag(DngTag::UniqueCameraModel, "BlitzRaw HDR merge");
        root.add_tag(TiffCommonTag::Make, "BlitzRaw");
        root.add_tag(TiffCommonTag::Model, "HDR merge");
        // The pixels are already white balanced and already in sRGB primaries,
        // so the neutral is one and the matrix is the standard one. Anything
        // else would be describing a camera this file did not come from.
        root.add_tag(DngTag::AsShotNeutral, [
            rawler::formats::tiff::Rational::new(1, 1),
            rawler::formats::tiff::Rational::new(1, 1),
            rawler::formats::tiff::Rational::new(1, 1),
        ]);
        root.add_tag(DngTag::CalibrationIlluminant1, 21_u16); // D65, matching the matrix below
        root.add_tag(DngTag::ColorMatrix1, srational_10000(&XYZ_D65_TO_SRGB).as_slice());
        root.add_tag(TiffCommonTag::SubIFDs, raw_offset);

        tiff.build(root)
            .map_err(|e| anyhow!("Could not finish the DNG: {e:?}"))?;
    }

    std::fs::rename(&temp, path)?;
    Ok(())
}

/// The same, straight to bytes. Only the tests want this: everything else has
/// somewhere to put the file.
#[cfg(test)]
pub fn encode_linear_jxl_dng(image: &DynamicImage, distance: f32) -> Result<Vec<u8>> {
    let dir = std::env::temp_dir().join(format!(
        "blitzraw-jxl-{}.dng",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    write_linear_jxl_dng(&dir, image, distance)?;
    let bytes = std::fs::read(&dir)?;
    let _ = std::fs::remove_file(&dir);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture with the things that break codecs and containers both: a hard
    /// edge, a smooth ramp, and a region of near black where a linear encoding
    /// spends the fewest bits.
    fn test_image(width: u32, height: u32) -> DynamicImage {
        DynamicImage::ImageRgb16(image::ImageBuffer::from_fn(width, height, |x, y| {
            let fx = x as f32 / width as f32;
            let fy = y as f32 / height as f32;
            let ramp = (fx * 65535.0) as u16;
            let edge = if fx > 0.5 { 60000 } else { 400 };
            let dark = (fy * 2000.0) as u16;
            image::Rgb([ramp, edge, dark])
        }))
    }

    #[test]
    fn a_written_merge_is_a_dng_with_the_expected_shape() {
        let img = test_image(320, 200);
        let bytes = encode_linear_jxl_dng(&img, 0.15).expect("write");

        assert!(bytes.len() > 64, "something was written");
        assert!(
            bytes.starts_with(b"II") || bytes.starts_with(b"MM"),
            "a DNG is a TIFF and starts with a byte order mark"
        );
        assert!(
            bytes.len() < 320 * 200 * 6,
            "and is smaller than the raw pixels it came from: {} bytes",
            bytes.len()
        );
    }


    /// The one that matters: a file we wrote, opened by the loader that will
    /// have to open it.
    ///
    /// The size assertion is the real test. A failed decode does not return an
    /// error, it quietly falls back to the embedded thumbnail and reports
    /// success, which is exactly how Lightroom's merges look like they open
    /// here. Anything smaller than what went in means the JPEG XL path did not
    /// run at all.
    #[test]
    fn a_written_merge_reads_back_at_full_size_through_our_own_loader() {
        let (w, h) = (640u32, 400u32);
        let img = test_image(w, h);
        let bytes = encode_linear_jxl_dng(&img, 0.1).expect("write");

        // Named somewhere disposable rather than "merge.dng": opening a file
        // leaves a sidecar beside it, and a relative name puts that in the
        // source tree.
        let scratch = std::env::temp_dir().join("blitzraw-jxl-roundtrip.dng");
        let settings = crate::app_settings::AppSettings::default();
        let decoded = crate::image_loader::load_base_image_from_bytes(
            &bytes,
            &scratch.to_string_lossy(),
            false,
            &settings,
            None,
        )
        .expect("our loader should open what we wrote");
        let _ = std::fs::remove_file(scratch.with_extension("dng.rrdata"));

        assert_eq!(
            (decoded.width(), decoded.height()),
            (w, h),
            "full size, not the {THUMBNAIL_EDGE} pixel thumbnail the loader falls back to"
        );

        // And the picture has to survive, not merely the dimensions. Compared
        // loosely: the pipeline white balances and applies a colour matrix on
        // the way through, so this is asking whether the structure is there,
        // not whether the numbers match.
        let out = decoded.to_rgb16();
        let original = img.to_rgb16();
        let sample = |b: &image::ImageBuffer<image::Rgb<u16>, Vec<u16>>, x: u32, y: u32| {
            let p = b.get_pixel(x, y);
            (p[0] as f32, p[1] as f32, p[2] as f32)
        };

        // The hard edge in the green channel is the feature least likely to
        // survive a container mistake: left of centre is dark, right is bright.
        let (_, left, _) = sample(&out, w / 4, h / 2);
        let (_, right, _) = sample(&out, (w * 3) / 4, h / 2);
        assert!(
            right > left * 2.0,
            "the edge survived the round trip: left {left}, right {right}"
        );

        // And the horizontal ramp still rises.
        let (near, _, _) = sample(&out, 8, h / 2);
        let (far, _, _) = sample(&out, w - 8, h / 2);
        assert!(far > near, "the ramp still rises: {near} to {far}");

        let (o_near, _, _) = sample(&original, 8, h / 2);
        let (o_far, _, _) = sample(&original, w - 8, h / 2);
        eprintln!("ramp in {o_near}..{o_far}, out {near}..{far}");
    }

    /// The regression that shipped: a merge that opened two and a half times
    /// too bright because nothing in the file said its samples were curved.
    ///
    /// The two tests above pass either way. They ask whether the edge is still
    /// an edge and whether the ramp still rises, and a wrong transfer function
    /// preserves both, which is exactly how this got out. This one asks what
    /// the numbers mean.
    #[test]
    fn the_stored_curve_is_undone_on_the_way_back_in() {
        // Uniform bands, so what comes back is the transfer function and not
        // the codec working on detail. Mid-grey first: a merge is written
        // through `apply_linear_to_srgb`, so scene 0.18 is stored as 0.4620,
        // and 0.4620 is what a reader that ignores the curve would hand back.
        let bands: [(f64, f64); 5] = [
            (0.4620, 0.18),
            (0.2140, 0.0382),
            (0.7354, 0.5),
            (0.0900, 0.0080),
            (1.0000, 1.0),
        ];
        let (w, h) = (256u32, 64u32 * bands.len() as u32);
        let img = DynamicImage::ImageRgb16(image::ImageBuffer::from_fn(w, h, |_, y| {
            let stored = bands[(y / 64) as usize].0;
            let code = (stored * u16::MAX as f64).round() as u16;
            image::Rgb([code, code, code])
        }));

        let bytes = encode_linear_jxl_dng(&img, 0.1).expect("write");
        let scratch = std::env::temp_dir().join("blitzraw-jxl-curve-check.dng");
        let decoded = crate::image_loader::load_base_image_from_bytes(
            &bytes,
            &scratch.to_string_lossy(),
            false,
            &crate::app_settings::AppSettings::default(),
            None,
        )
        .expect("our loader should open what we wrote");
        let _ = std::fs::remove_file(scratch.with_extension("dng.rrdata"));

        let out = decoded.to_rgb32f();
        for (index, (stored, expected_linear)) in bands.iter().enumerate() {
            let pixel = out.get_pixel(w / 2, index as u32 * 64 + 32);
            let got = pixel[1] as f64;
            eprintln!(
                "stored {stored:.4} -> got {got:.4}, linear should be {expected_linear:.4}"
            );
            // Wide enough for the codec and the near-identity colour matrix,
            // nowhere near wide enough to let the uncurved value through: the
            // two differ by more than a factor of two everywhere but the ends.
            let tolerance = 0.02 + expected_linear * 0.05;
            assert!(
                (got - expected_linear).abs() < tolerance,
                "band {index}: stored {stored}, expected linear {expected_linear}, got {got}"
            );
        }
    }

    /// What a merge hands the renderer has to be what decoding the file would
    /// have produced.
    ///
    /// `warm_caches_for_merge` writes the preview and the thumbnail from the
    /// merge still in memory instead of decoding the file it just wrote. That
    /// only holds if the two agree. The merge is display-referred and the DNG
    /// decodes to linear, so the caller converts on the way in; drop that
    /// conversion and both caches are written two and a half times too bright,
    /// and unlike a live render they stay wrong until someone discards them.
    #[test]
    fn a_merge_converted_for_the_renderer_matches_its_own_decode() {
        // Float, because that is what a merge is, and because the conversion
        // under test does nothing to any other kind.
        let (w, h) = (192u32, 128u32);
        let merged = DynamicImage::ImageRgb32F(image::ImageBuffer::from_fn(w, h, |x, y| {
            let across = x as f32 / (w - 1) as f32;
            let down = y as f32 / (h - 1) as f32;
            image::Rgb([across, 0.25 + down * 0.5, 1.0 - across * 0.75])
        }));

        let bytes = encode_linear_jxl_dng(&merged, 0.1).expect("write");
        let scratch = std::env::temp_dir().join("blitzraw-preloaded-space.dng");
        let decoded = crate::image_loader::load_base_image_from_bytes(
            &bytes,
            &scratch.to_string_lossy(),
            false,
            &crate::app_settings::AppSettings::default(),
            None,
        )
        .expect("our loader should open what we wrote");
        let _ = std::fs::remove_file(scratch.with_extension("dng.rrdata"));

        // The function warm_caches_for_merge calls, not a copy of what it does,
        // so removing the conversion there fails here.
        let preloaded = as_decoded(merged, &scratch);

        let a = preloaded.to_rgb32f();
        let b = decoded.to_rgb32f();
        assert_eq!(a.dimensions(), b.dimensions());

        let mut worst = 0f32;
        let mut sum_sq = 0f64;
        for (x, y) in a.as_raw().iter().zip(b.as_raw().iter()) {
            let d = (x - y).abs();
            worst = worst.max(d);
            sum_sq += (d as f64) * (d as f64);
        }
        let rms = (sum_sq / a.as_raw().len() as f64).sqrt();
        eprintln!("preloaded against decoded: rms {rms:.5}, worst {worst:.5}");

        // Loose enough for the codec and the near-identity colour matrix. The
        // failure this guards against is not subtle: skipping the conversion
        // leaves mid-grey at 0.46 where it should be 0.18.
        assert!(
            rms < 0.01 && worst < 0.05,
            "the preloaded image is in the same space as the decode: rms {rms}, worst {worst}"
        );
    }

    #[test]
    fn the_temporary_file_does_not_survive_a_write() {
        let dir = std::env::temp_dir().join("blitzraw-jxl-part-check");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("merge.dng");

        write_linear_jxl_dng(&target, &test_image(64, 64), 0.5).expect("write");

        assert!(target.exists(), "the merge is there");
        assert!(
            !target.with_extension("dng.part").exists(),
            "and the part file it was built as is not"
        );
    }

    #[test]
    fn the_thumbnail_is_never_bigger_than_the_picture() {
        // A 64 pixel merge once got a 256 pixel thumbnail, which was three
        // times the weight of the image it described. That thumbnail would be
        // 196608 bytes of RGB8, so a budget of six bytes a pixel catches it.
        //
        // The linearization table is a fixed 128 KB whatever the picture is,
        // and taking it off rather than raising the budget keeps this test
        // measuring the thumbnail and nothing else.
        const TABLE_BYTES: usize = 65536 * 2;
        let bytes = encode_linear_jxl_dng(&test_image(64, 64), 0.5).expect("write");
        let without_table = bytes.len().saturating_sub(TABLE_BYTES);
        assert!(
            without_table < 64 * 64 * 6,
            "a small merge stays small: {} bytes, {without_table} of them not the table",
            bytes.len()
        );
    }

    #[test]
    fn an_empty_image_is_refused_rather_than_written() {
        let empty = DynamicImage::ImageRgb16(image::ImageBuffer::new(0, 0));
        assert!(write_linear_jxl_dng(Path::new("unused.dng"), &empty, 0.15).is_err());
    }
}
