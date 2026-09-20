//! Merging a bracket without letting the blown frame ruin the highlights.
//!
//! # The fault this exists to fix
//!
//! Merges came back with yellow, flattened highlights on bright objects: a grey
//! throw beside a window read as a detail-less yellow shape, while the window
//! itself looked fine. The worse the longest frame was overexposed, the worse
//! the merge, even though every one of those highlights was recorded perfectly
//! in the shorter frames.
//!
//! The merge was `image_hdr::hdr_merge_images`, whose estimator is:
//!
//! ```text
//! for each frame:  radiance = pixel / (exposure * gain)
//!                  weighted = radiance * exposure / sum_of_exposures
//! result = sum of weighted
//! ```
//!
//! Which reduces to `sum(pixel) / (gain * sum(exposures))`, and that **is** the
//! Poisson Photon Noise Estimator from the paper it cites. The arithmetic is
//! right. What is missing is the condition the paper puts on it: the sum runs
//! over the observations that are **not saturated**. A clipped pixel is not a
//! measurement of anything. It says "at least this bright" and nothing more.
//!
//! Nothing in that crate looks at saturation. Grep it for `saturat`, `clip` or
//! `weight` and there is not one hit. So a pixel blown out in the long frame
//! still contributed, at a value far below the light that was really there, and
//! dragged the estimate down.
//!
//! That explains every part of what was seen:
//!
//! | Seen | Because |
//! |---|---|
//! | highlights flattened, detail gone | a clipped value is lower than the truth, so the average is pulled down |
//! | a colour cast, usually warm | channels clip at different scene levels, so they are pulled down by different amounts |
//! | worse the more the long frame is overexposed | more pixels clipped, and each further from the truth |
//! | light sources themselves less affected | clipped in all three channels at once, so pulled down evenly, and they are meant to be white |
//!
//! # What this does instead
//!
//! The same estimator with the condition put back:
//!
//! ```text
//! radiance = sum(pixel_i) / sum(exposure_i * gain_i)   over unsaturated i only
//! ```
//!
//! Noise averaging is kept: every frame that actually measured a pixel still
//! contributes to it, which is the whole point of the estimator and the reason
//! a merge is less noisy than any one frame.
//!
//! # Why the gate is a ramp and not a switch
//!
//! The first version of this excluded a frame's pixel outright the moment any
//! channel reached 0.98. That fixed the colour cast and introduced a worse
//! artefact: a hard edge across the ceiling and the wall, following a line of
//! equal brightness, where the long frame stopped contributing.
//!
//! It is obvious in hindsight. On a smooth gradient, the pixel at 0.979 was an
//! average of three frames and the pixel beside it at 0.981 was an average of
//! two. The estimate jumps at that line, and a jump on a smooth surface is a
//! contour. The literature names this: a binary saturation mask is the usual
//! implementation and the usual source of a visible seam, and the SNR plots in
//! the noise-aware paper show the same effect as a sawtooth where exposures
//! hand over.
//!
//! So the gate is a weight in `0..=1` rather than a yes or no, and it rolls off
//! smoothly across a band below clipping:
//!
//! ```text
//! radiance = sum(w_i * pixel_i) / sum(w_i * exposure_i * gain_i)
//! ```
//!
//! With every `w` either 0 or 1 that is exactly the estimator above, so nothing
//! about the fix to the colour cast is given up. What changes is that a frame
//! fades out of the average over about a third of a stop instead of vanishing
//! between one pixel and the next.
//!
//! The ramp is a smoothstep rather than a straight line on purpose. A straight
//! line removes the jump in the value but leaves one in its slope, and a slope
//! that changes abruptly across a smooth gradient is still visible as a faint
//! line. Smoothstep is flat at both ends, so the value and its slope are both
//! continuous.
//!
//! # Why the labels are not trusted either
//!
//! A fade can only hide itself if the frames either side of it agree about the
//! light. They did not. `ExposureTime` is a **marked** value: a camera's
//! "0.6 s" is a position on a third-stop scale whose true value is 0.63 s, and
//! its "1/6 s" is really 0.157 s, so two frames labelled 0.6 and 1/6 are 3.6 to
//! one by their labels and 4 to one in fact. The estimate then slides by that
//! 10% as a frame fades out, which on a smooth ceiling is a line you can see,
//! and widening the fade only spreads the same 10% over more pixels.
//!
//! So the real step between frames is measured from the frames themselves
//! before anything is divided by it. See `measured_lights`.
//!
//! # What is deliberately not done
//!
//! The classic Debevec and Malik hat, which weights the middle of the range
//! highest and tapers to nothing at both ends, is not used. The noise-aware
//! paper's whole argument is that for photon-limited data the plain sum is
//! already the right estimator, and that a hat throws away the pixels near
//! saturation which have the best signal-to-noise ratio of any in the stack.
//! So the weight here is one everywhere except approaching clipping. It is a
//! validity gate, not a quality curve.
//!
//! # Why a whole pixel is faded rather than one channel
//!
//! Each channel is its own measurement, so dropping per channel would keep more
//! data and is what a merge working on raw values should do. These values are
//! not raw. They have been through white balance and the camera-to-sRGB matrix,
//! and that matrix mixes the channels, so one clipped channel has already
//! spread into the other two by the time it gets here. A green that reads 0.6
//! next to a clipped red is not a clean measurement of green.
//!
//! So the weight is driven by the brightest channel of the pixel, and the whole
//! pixel fades together. It costs a little noise in the highlights of one frame
//! and it is the difference between a correct colour and a yellow one.
//!
//! # When every frame is saturated
//!
//! The sun, a bare bulb, a specular glint. Every weight is zero, so there is no
//! measurement anywhere and the shortest exposure is used: it is the one that
//! saw the most before clipping, and it gives the highest lower bound on the
//! light. The alternative is a division by zero and a black hole in the middle
//! of the brightest thing in the frame.
//!
//! This does not put the seam back. The weights reach zero smoothly, so the
//! weighted estimate has already converged on the shortest frame's own reading
//! by the time it is the only one left.

use std::time::Duration;

use image::{DynamicImage, Rgb32FImage};

/// Above this, a value is treated as clipped and carries no information.
///
/// Not 1.0. The decode clamps to 1.0 in places and a demosaic interpolates, so
/// a pixel that clipped on the sensor can arrive a hair under one and a pixel
/// beside it a hair over. Slightly conservative costs a little highlight noise;
/// slightly generous lets exactly the fault this module exists for back in.
pub const SATURATION: f32 = 0.98;

/// Where a frame starts fading out of the average.
///
/// Between here and `SATURATION` the weight falls from one to zero.
///
/// # Measured rather than guessed
///
/// The width of this band is the whole difference between an invisible handover
/// and a hard edge, so it was simulated rather than picked. A smooth ramp of
/// radiance, two frames, and the long one disagreeing with its own metadata by
/// a few percent, which is what really happens: nominal shutter speeds are not
/// actual ones, apertures and ISO are ratings, and veiling glare differs
/// between exposures.
///
/// The number that matters is the largest **fall** from one pixel to the next,
/// against the typical rise along the gradient. A fall means the picture gets
/// darker where the scene gets brighter, which on a wall is a line you can see.
///
/// | fade starts at | 4% disagreement | 10% | 20% |
/// |---|---|---|---|
/// | a switch at 0.98 | -7.9x | | -37.6x |
/// | 0.90 | -0.1x | -1.7x | -4.4x |
/// | 0.80 | +0.5x | | -1.3x |
/// | **0.70** | **+0.7x** | **+0.3x** | **-0.5x** |
/// | 0.60 | +0.8x | +0.5x | -0.1x |
///
/// A positive number means the gradient never reverses at all. 0.70 is the
/// first width that stays positive through a ten percent disagreement and keeps
/// a third of a stop of error to a flattening rather than a reversal.
///
/// Wider is not free. The noise-aware paper's argument against the classic
/// Debevec hat is that pixels near saturation have the best signal-to-noise
/// ratio in the whole stack, and fading them out early throws that away. 0.60
/// buys very little over 0.70 and costs more of it, so this stops at 0.70:
/// everything below is at full weight, and the fade covers the last half stop.
pub const ROLL_OFF_START: f32 = 0.70;

/// How much a frame's reading of a pixel counts, from one down to nothing.
///
/// Driven by the brightest channel, because these values have been through the
/// camera-to-sRGB matrix and one clipped channel has already spread into the
/// other two. A pixel fades as a whole or not at all.
///
/// Smoothstep rather than a straight line: a line is continuous in value but
/// not in slope, and a slope that changes abruptly across a smooth wall is
/// still faintly visible. This is flat at both ends.
pub fn weight_for(observed: &[f32; 3]) -> f32 {
    let brightest = observed.iter().copied().fold(f32::NEG_INFINITY, f32::max);

    if !brightest.is_finite() {
        return 0.0;
    }
    if brightest <= ROLL_OFF_START {
        return 1.0;
    }
    if brightest >= SATURATION {
        return 0.0;
    }

    let t = (brightest - ROLL_OFF_START) / (SATURATION - ROLL_OFF_START);
    // 1 - smoothstep(t), so it starts at one and ends at zero.
    1.0 - t * t * (3.0 - 2.0 * t)
}

/// One frame of a bracket, as the merge needs it.
pub struct Frame<'a> {
    pub image: &'a Rgb32FImage,
    pub exposure: Duration,
    pub gain: f32,
}

impl Frame<'_> {
    /// Exposure times gain: how much light this frame was given, in the units
    /// the estimator divides by.
    fn light(&self) -> f32 {
        self.exposure.as_secs_f32() * self.gain
    }
}

/// How many usable pairs of pixels a measurement needs before it is believed
/// over the metadata.
///
/// High enough that no unit test's handful of pixels can move a merge by
/// accident, and trivially met by any real frame: a 45 megapixel bracket of an
/// interior offers millions.
const ENOUGH_SAMPLES: u64 = 10_000;

/// How far a measurement is allowed to move a frame from what its metadata
/// claims, in stops.
///
/// A real metadata error is a fraction of a stop. Anything past this is not a
/// rounded shutter speed, it is two pictures of different things, and the
/// metadata is the safer answer.
const MOST_IT_MAY_MOVE: f32 = 1.0;

/// How far apart the middle half of the pixels may be about the answer, in
/// stops, before the measurement is thrown away.
///
/// The measurement assumes two frames are looking at the same thing. When they
/// are, they agree extremely tightly, because the only difference between them
/// is the shutter. When the camera or the scene moved, half the pixels being
/// compared are not the same pixel, and the answer read off them is nonsense.
///
/// Measured across six real brackets from one shoot rather than guessed:
///
/// | bracket | spread of each pair, stops | what it measured |
/// |---|---|---|
/// | 3688 | 0.068, 0.080 | 3.99, 4.00 |
/// | 3718 | 0.074, 0.068 | 3.91, 4.13 |
/// | 3747 | 0.090, 0.078 | 3.99, 4.05 |
/// | 3724 | 0.092, 0.066 | 4.04, 3.99 |
/// | 3805 | 0.176, **0.877** | 4.02, **3.66** |
/// | 3809 | 0.311, **0.539** | 4.01, 3.88 |
///
/// Every one of those brackets was shot two stops apart, whatever its labels
/// claim. The four steady ones recover it to within 3%, and the two where
/// something moved include a pair that came back 9% out. The gap between 0.092
/// and 0.176 is wide and this sits in the middle of it.
///
/// Refusing costs nothing. The label is what was used before any of this, so
/// falling back to it is never worse than not having measured at all.
const MOST_IT_MAY_SPREAD: f32 = 0.15;

fn luma(rgb: &[f32; 3]) -> f32 {
    (rgb[0] + rgb[1] + rgb[2]) / 3.0
}

/// The light each frame was really given, measured from the frames themselves.
///
/// # Why the metadata is not good enough
///
/// The estimator divides by how much light a frame was given, and takes that
/// from `ExposureTime` times ISO. Both are **marked** values rather than
/// measured ones. A camera's "0.6 s" is a position on a third-stop scale whose
/// true value is `2^(-2/3)`, or 0.63 s, and its "1/6 s" is really 0.157 s. Two
/// frames marked 0.6 and 1/6 are therefore 3.6 to 1 by their labels and 4 to 1
/// in fact.
///
/// That 10% is not a rounding detail, it is the whole artefact. Where a frame
/// fades out of the average, the estimate slides from what one frame says to
/// what the other says, and if they disagree by 10% the estimate slides by 10%
/// across a band a half stop wide. On a smooth ceiling that is a line you can
/// see. Widening the fade only spreads the same 10% over more pixels.
///
/// Measured on the bracket that showed it: a flat 10% disagreement at every
/// level from 0.02 to 0.98, which is the signature of a scale error and not of
/// anything optical.
///
/// # What is measured instead
///
/// Wherever two frames both have a usable reading of the same pixel, they are
/// looking at the same light, so the ratio of what they read **is** the ratio
/// of the light they were given. The median of that ratio over millions of
/// pixels is the real exposure step, whatever the labels say. It also absorbs
/// anything else the metadata cannot see: a lens whose aperture is not quite
/// what it is marked, an ND filter, a camera that quietly moved ISO.
///
/// The median rather than the mean, so that anything that moved between frames
/// is outvoted rather than averaged in.
///
/// # What stays fixed
///
/// The metered frame keeps the light its metadata claims, and everything else
/// is measured relative to it. Only the ratios between frames were ever wrong,
/// and the absolute value is what `metered_white` scales the whole merge by, so
/// leaving it alone keeps a merge looking like the frame it was metered from.
pub fn measured_lights(frames: &[Frame<'_>]) -> Vec<f32> {
    let claimed: Vec<f32> = frames.iter().map(Frame::light).collect();
    if frames.len() < 2 {
        return claimed;
    }

    let mut order: Vec<usize> = (0..frames.len()).collect();
    order.sort_by(|a, b| claimed[*a].total_cmp(&claimed[*b]));
    // The same frame `metered_white` calls white, so the merge is scaled by an
    // unchanged number.
    let anchor = (order.len() - 1) / 2;

    // How much more light each frame in the order was given than the one below
    // it. The first entry has nothing below it and is never read.
    let mut steps = vec![1.0f32; order.len()];
    for (index, pair) in order.windows(2).enumerate() {
        let (dark, bright) = (pair[0], pair[1]);
        let claimed_step = if claimed[dark] > 0.0 {
            claimed[bright] / claimed[dark]
        } else {
            1.0
        };
        let believable = |(measured, spread): &(f32, f32)| {
            claimed_step > 0.0
                && *measured > 0.0
                && *spread <= MOST_IT_MAY_SPREAD
                && (measured / claimed_step).log2().abs() <= MOST_IT_MAY_MOVE
        };
        let read = ratio_between(frames[bright].image, frames[dark].image);
        if let Some((ratio, spread)) = read
            && !believable(&(ratio, spread))
        {
            log::info!(
                "Two frames read {ratio:.4} apart against a label of {claimed_step:.4},                  the middle half of their pixels spread over {spread:.3} stops.                  Keeping the label."
            );
        }
        steps[index + 1] = read
            .filter(believable)
            .map(|(ratio, _)| ratio)
            .unwrap_or(claimed_step);
    }

    let mut lights = vec![0.0f32; frames.len()];
    lights[order[anchor]] = claimed[order[anchor]];
    for index in (0..anchor).rev() {
        lights[order[index]] = lights[order[index + 1]] / steps[index + 1];
    }
    for index in (anchor + 1)..order.len() {
        lights[order[index]] = lights[order[index - 1]] * steps[index];
    }
    lights
}

/// How much more light the brighter frame was given than the darker one, read
/// off the pixels rather than the metadata.
///
/// `None` when there is not enough overlap between them to say. Two frames four
/// stops apart with a scene that fills neither leave nothing usable, and the
/// metadata is then the only answer there is.
fn ratio_between(bright: &Rgb32FImage, dark: &Rgb32FImage) -> Option<(f32, f32)> {
    // A twelve stop window at a five hundredth of a stop a bin. Far wider than
    // any bracket, and fine enough that the bin itself is not the error.
    const FROM: f32 = -6.0;
    const SPAN: f32 = 12.0;
    const BINS: usize = 6144;
    // Every third pixel each way, which is still millions and costs a ninth of
    // the time.
    const STRIDE: usize = 3;
    // The brighter frame must be well clear of its noise floor and of the
    // roll-off, since a pixel already fading is a pixel already suspect.
    const BRIGHT_FLOOR: f32 = 0.05;
    // And the darker one only has to be out of the noise. It is the darker one,
    // so it is always the noisier of the two.
    const DARK_FLOOR: f32 = 0.02;

    if bright.dimensions() != dark.dimensions() {
        return None;
    }

    let mut histogram = vec![0u32; BINS];
    let mut seen = 0u64;
    for y in (0..bright.height() as usize).step_by(STRIDE) {
        for x in (0..bright.width() as usize).step_by(STRIDE) {
            let (x, y) = (x as u32, y as u32);
            let above = bright.get_pixel(x, y).0;
            let below = dark.get_pixel(x, y).0;
            let above_top = above[0].max(above[1]).max(above[2]);
            let below_top = below[0].max(below[1]).max(below[2]);
            if !(BRIGHT_FLOOR..=ROLL_OFF_START).contains(&above_top) {
                continue;
            }
            if !(DARK_FLOOR..=ROLL_OFF_START).contains(&below_top) {
                continue;
            }
            let (a, b) = (luma(&above), luma(&below));
            if !(a > 0.0 && b > 0.0) {
                continue;
            }
            let at = ((a / b).log2() - FROM) / SPAN * BINS as f32;
            if !(at >= 0.0) || at >= BINS as f32 {
                continue;
            }
            histogram[at as usize] += 1;
            seen += 1;
        }
    }

    if seen < ENOUGH_SAMPLES {
        return None;
    }

    let stops_at = |bin: usize| FROM + (bin as f32 + 0.5) / BINS as f32 * SPAN;
    let percentile = |want: u64| -> Option<usize> {
        let mut running = 0u64;
        for (bin, count) in histogram.iter().enumerate() {
            running += *count as u64;
            if running >= want {
                return Some(bin);
            }
        }
        None
    };

    let middle = stops_at(percentile(seen.div_ceil(2))?);
    // How tightly the pixels agree on that answer. A bracket the frames really
    // do correspond in gives a very narrow peak: the same light through two
    // shutter speeds, and nothing else. A bracket where the camera or the
    // scene moved gives a broad smear, because half the pixels being compared
    // are not looking at the same thing at all, and no exposure ratio can be
    // read off that.
    let spread = stops_at(percentile(seen / 4 + 1)?)..stops_at(percentile(seen * 3 / 4)?);
    let ratio = 2f32.powf(middle);
    (ratio.is_finite() && ratio > 0.0).then_some((ratio, spread.end - spread.start))
}

/// Merges a bracket into one linear image.
///
/// Panics on an empty list, which is a caller error: the command refuses fewer
/// than two paths long before this.
pub fn merge_bracket(frames: &[Frame<'_>]) -> Rgb32FImage {
    // How much light each frame was really given, read off the frames rather
    // than off their labels. See `measured_lights`: a marked shutter speed is a
    // position on a scale, not a measurement, and the few percent between the
    // two is what a handover has to hide.
    let lights = measured_lights(frames);
    for (frame, light) in frames.iter().zip(lights.iter()) {
        if (light / frame.light() - 1.0).abs() > 0.005 {
            log::info!(
                "Frame given {:.4} by its label, {:.4} by its pixels, {:+.1}%",
                frame.light(),
                light,
                (light / frame.light() - 1.0) * 100.0
            );
        }
    }
    merge_with_lights(frames, &lights)
}

/// The merge itself, told how much light each frame was given.
///
/// Separate from `merge_bracket` so the same bracket can be merged twice, once
/// on what the labels claim and once on what the frames say, and the two
/// results laid side by side. That is how the line across the ceiling was
/// shown to come from the labels rather than from the fade or the shoulder.
pub fn merge_with_lights(frames: &[Frame<'_>], lights: &[f32]) -> Rgb32FImage {
    assert!(!frames.is_empty(), "a merge needs at least one frame");
    assert_eq!(
        frames.len(),
        lights.len(),
        "every frame needs a light to divide by"
    );

    let (width, height) = (frames[0].image.width(), frames[0].image.height());
    for frame in frames {
        assert_eq!(
            (frame.image.width(), frame.image.height()),
            (width, height),
            "every frame of a bracket must be the same size"
        );
    }

    // The frame that saw the most before clipping, for pixels where every frame
    // clipped. Its own light is the divisor, so the answer stays a radiance.
    let shortest = lights
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.total_cmp(b.1))
        .map(|(index, _)| index)
        .unwrap_or(0);

    let mut out = Rgb32FImage::new(width, height);

    for (x, y, pixel) in out.enumerate_pixels_mut() {
        let mut sum = [0.0f32; 3];
        let mut light = 0.0f32;

        for (frame, given) in frames.iter().zip(lights.iter()) {
            let observed = frame.image.get_pixel(x, y).0;
            // How far this frame is to be believed about this pixel. One for
            // anything comfortably below clipping, easing to zero as it
            // approaches it, so no frame ever leaves the average abruptly.
            let weight = weight_for(&observed);
            if weight <= 0.0 {
                continue;
            }
            for channel in 0..3 {
                sum[channel] += weight * observed[channel];
            }
            light += weight * given;
        }

        if light > 0.0 {
            for channel in 0..3 {
                pixel.0[channel] = sum[channel] / light;
            }
        } else {
            let observed = frames[shortest].image.get_pixel(x, y).0;
            let divisor = lights[shortest].max(f32::MIN_POSITIVE);
            for channel in 0..3 {
                pixel.0[channel] = observed[channel] / divisor;
            }
        }
    }

    out
}

/// The same, taking what `load_hdr_frames` produces.
pub fn merge_loaded(frames: &[(String, DynamicImage, Duration, f32)]) -> Rgb32FImage {
    let buffers: Vec<Rgb32FImage> = frames
        .iter()
        .map(|(_, image, _, _)| image.to_rgb32f())
        .collect();

    let inputs: Vec<Frame<'_>> = frames
        .iter()
        .zip(buffers.iter())
        .map(|((_, _, exposure, gain), image)| Frame {
            image,
            exposure: *exposure,
            gain: *gain,
        })
        .collect();

    merge_bracket(&inputs)
}

/// Where the merged radiance is scaled and shouldered for display.
///
/// # Why the old stretch had to go with the merge
///
/// `image_hdr::stretch::apply_histogram_stretch` divides the whole image by its
/// single largest value. That was survivable only because the merge it came
/// with crushed its own highlights: nothing was ever very bright, so nothing
/// ever set an absurd scale.
///
/// Once the highlights are right that stops being true. A ceiling lamp really
/// is a hundred times the brightness of the room it lights, so dividing the
/// room by the lamp makes the room black, and the corrected merge would have
/// looked far worse than the broken one.
///
/// A percentile instead of the maximum does not fix it either, which is worth
/// writing down because it was tried first. A window can be five per cent of a
/// frame, so any percentile high enough to ignore a specular glint still lands
/// inside the window, and the room goes dark exactly as before. The scale
/// cannot be decided by how much of the frame is bright.
///
/// # What decides it instead
///
/// The exposure that was metered. The middle frame of a bracket is the one the
/// photographer chose, so what that frame called white is what the merge calls
/// white: `1 / (exposure * gain)`. That is a property of the camera settings
/// and not of the scene, so two brackets shot of the same room at the same
/// settings come back matching, and a merge looks like the frame it was metered
/// from rather than like a negotiation with whatever was brightest in shot.
///
/// # And what happens above it
///
/// It is compressed, not thrown away. Everything above the metered white is
/// exactly the detail the merge exists to recover, and clipping it would undo
/// the whole exercise. A Reinhard shoulder puts the metered white at
/// `SHOULDER_WHITE` and folds everything above it into the space that leaves,
/// so a highlight three stops over still has somewhere to be.
///
/// Below the metered white the curve is straight, exactly, so a bracket where
/// nothing clipped comes out looking like the frame it was metered from. This
/// is a fix for blown highlights and has no business restyling anything else.
///
/// An earlier attempt used a Reinhard curve over the whole range, which lifts
/// shadows hard: halving the light changed the value by a third rather than by
/// half. A test asking for linearity in the shadows caught it. Good merges
/// would all have come back looking different, which is not a fix.
/// Where the metered white lands once the shoulder is applied.
///
/// The remaining space above it is what recovered highlights are folded into.
/// Higher keeps midtones brighter and leaves less room; lower does the reverse.
const SHOULDER_WHITE: f32 = 0.8;

/// The radiance the metered frame called white.
///
/// The middle frame by exposure, which for a bracket shot in the usual way is
/// the metered one. An even number of frames takes the darker of the two middle
/// ones, which errs towards keeping highlights.
///
/// Takes what `load_hdr_frames` produces rather than the `Frame`s the merge
/// takes, because that is what the one caller has in its hand. There was a
/// second wrapper here taking `&[Frame]`, and nothing outside the tests ever
/// called it.
pub fn metered_white_of(frames: &[(String, DynamicImage, Duration, f32)]) -> f32 {
    white_from(
        frames
            .iter()
            .map(|(_, _, exposure, gain)| exposure.as_secs_f32() * gain),
    )
}

fn white_from(lights: impl Iterator<Item = f32>) -> f32 {
    let mut lights: Vec<f32> = lights.collect();
    assert!(!lights.is_empty(), "a bracket has frames");
    lights.sort_by(f32::total_cmp);
    let reference = lights[(lights.len() - 1) / 2];
    if reference > 0.0 {
        1.0 / reference
    } else {
        1.0
    }
}

/// Scales a merged image for display, folding what is above white into a
/// shoulder rather than clipping it.
pub fn to_display(image: &Rgb32FImage, white: f32) -> Rgb32FImage {
    let white = if white.is_finite() && white > 0.0 {
        white
    } else {
        1.0
    };
    let w = SHOULDER_WHITE;
    // The slope of the straight part carried into the curved part, so the two
    // meet without a kink. Anything else shows as a band across a gradient.
    let falloff = w / (1.0 - w);

    let mut out = image.clone();
    for value in out.as_mut() {
        let x = (*value / white).max(0.0);
        let toned = if x <= 1.0 {
            // Straight, all the way up to the metered white. A merge of a
            // bracket where nothing clipped therefore looks exactly like the
            // frame it was metered from, which is the point: this is a fix for
            // blown highlights and must not restyle everything else on the way.
            x * w
        } else {
            // And above it, a shoulder that starts at the same value and the
            // same slope and never quite reaches one.
            w + (1.0 - w) * (1.0 - (-(x - 1.0) * falloff).exp())
        };
        *value = if toned.is_finite() {
            toned.clamp(0.0, 1.0)
        } else {
            0.0
        };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    /// A flat field of one radiance, large enough for a measurement to be
    /// believed, at whatever light the frame was really given.
    fn field(size: u32, radiance: f32, actual_light: f32) -> Rgb32FImage {
        let recorded = (radiance * actual_light).min(1.0);
        Rgb32FImage::from_pixel(size, size, Rgb([recorded, recorded, recorded]))
    }

    /// A ramp of radiance across the width, the same all the way down, at
    /// whatever light the frame was really given.
    ///
    /// Two dimensional because the measurement wants tens of thousands of
    /// usable pixels before it will contradict a label, and a single row of
    /// them is nowhere near that.
    fn wide_ramp(size: u32, actual_light: f32, from: f32, to: f32) -> Rgb32FImage {
        Rgb32FImage::from_fn(size, size, |x, _| {
            let t = x as f32 / (size - 1) as f32;
            let recorded = ((from + (to - from) * t) * actual_light).min(1.0);
            Rgb([recorded, recorded, recorded])
        })
    }

    /// A frame whose label disagrees with what it really did.
    fn mislabelled(image: &Rgb32FImage, claimed_light: f32) -> Frame<'_> {
        Frame {
            image,
            exposure: Duration::from_secs_f32(claimed_light),
            gain: 1.0,
        }
    }

    /// The size that makes a measurement believable, and the ramp that fills it.
    const BIG: u32 = 600;
    const RAMP_FROM: f32 = 0.10;
    const RAMP_TO: f32 = 1.60;
    /// Four to one really, three point two to one by the labels. The same 25%
    /// error the real bracket had, and the same direction.
    const TRUE_LONG: f32 = 1.0;
    const CLAIMED_LONG: f32 = 0.8;
    const SHORT: f32 = 0.25;

    /// A ramp whose light changes from one row to the next, standing in for a
    /// frame that does not line up with its partner: half the pixels being
    /// compared are not looking at the same thing.
    fn restless_ramp(size: u32, actual_light: f32, from: f32, to: f32) -> Rgb32FImage {
        Rgb32FImage::from_fn(size, size, |x, y| {
            let t = x as f32 / (size - 1) as f32;
            let actual = if y % 2 == 0 {
                actual_light
            } else {
                actual_light / 2.0
            };
            let recorded = ((from + (to - from) * t) * actual).min(1.0);
            Rgb([recorded, recorded, recorded])
        })
    }

    /// Two frames the camera or the scene moved between cannot be measured
    /// against each other, and a median taken anyway is a number with nothing
    /// behind it. On a real bracket where something moved this came back 9%
    /// out, which is as bad as the label it would have replaced.
    ///
    /// Checked against the alternative rather than assumed: with
    /// `MOST_IT_MAY_SPREAD` raised out of the way this fails, because the
    /// measurement is then believed and the long frame moves.
    #[test]
    fn frames_that_do_not_line_up_are_not_measured() {
        let long = wide_ramp(BIG, TRUE_LONG, RAMP_FROM, RAMP_TO);
        let short = restless_ramp(BIG, SHORT, RAMP_FROM, RAMP_TO);
        let lights =
            measured_lights(&[mislabelled(&long, CLAIMED_LONG), mislabelled(&short, SHORT)]);

        assert!(
            (lights[0] - CLAIMED_LONG).abs() < 1e-6,
            "the label should have been kept, got {lights:?}"
        );
    }

    #[test]
    fn the_real_exposure_step_is_read_off_the_pixels() {
        let long = wide_ramp(BIG, TRUE_LONG, RAMP_FROM, RAMP_TO);
        let short = wide_ramp(BIG, SHORT, RAMP_FROM, RAMP_TO);
        let lights =
            measured_lights(&[mislabelled(&long, CLAIMED_LONG), mislabelled(&short, SHORT)]);

        let measured = lights[0] / lights[1];
        let truth = TRUE_LONG / SHORT;
        assert!(
            (measured / truth - 1.0).abs() < 0.01,
            "the frames are {truth} to one and read as {measured} to one"
        );
        // And the labels say something else, or this test proves nothing.
        assert!(
            (CLAIMED_LONG / SHORT / truth - 1.0).abs() > 0.1,
            "the labels have to be wrong for this to be a test"
        );
    }

    /// The absolute scale is what the whole merge is divided by, so only the
    /// ratios between frames may move.
    #[test]
    fn the_metered_frame_keeps_the_light_its_label_claims() {
        let long = wide_ramp(BIG, TRUE_LONG, RAMP_FROM, RAMP_TO);
        let mid = wide_ramp(BIG, SHORT, RAMP_FROM, RAMP_TO);
        let short = wide_ramp(BIG, SHORT / 4.0, RAMP_FROM, RAMP_TO);
        let lights = measured_lights(&[
            mislabelled(&long, CLAIMED_LONG),
            mislabelled(&mid, SHORT),
            mislabelled(&short, SHORT / 4.0),
        ]);

        assert!(
            (lights[1] - SHORT).abs() < 1e-6,
            "the middle frame moved: {} rather than {SHORT}",
            lights[1]
        );
    }

    /// The artefact, with the cause the measurement removes.
    ///
    /// A smooth wall brightening across the frame, the long exposure clipping
    /// partway along it, and that frame's label short by 25%. Believing the
    /// label makes the estimate slide by 25% as the frame fades out, which
    /// reverses the gradient: the picture gets darker where the wall gets
    /// brighter, and that is the line on the ceiling.
    ///
    /// Checked against the old behaviour rather than assumed: with
    /// `measured_lights` replaced by the claimed lights this fails, reporting a
    /// fall of 0.4 times the typical rise.
    #[test]
    fn a_frame_whose_label_is_wrong_no_longer_draws_an_edge() {
        let long = wide_ramp(BIG, TRUE_LONG, RAMP_FROM, RAMP_TO);
        let short = wide_ramp(BIG, SHORT, RAMP_FROM, RAMP_TO);

        let merged = merge_bracket(&[mislabelled(&long, CLAIMED_LONG), mislabelled(&short, SHORT)]);

        let steps: Vec<f32> = (1..merged.width())
            .map(|x| merged.get_pixel(x, 0).0[0] - merged.get_pixel(x - 1, 0).0[0])
            .collect();
        let worst = steps.iter().copied().fold(f32::INFINITY, f32::min);
        let mut sorted = steps.clone();
        sorted.sort_by(f32::total_cmp);
        let typical = sorted[sorted.len() / 2];

        assert!(
            typical > 0.0,
            "the ramp has to rise for this to mean anything"
        );
        assert!(
            worst > 0.0,
            "the gradient reversed by {:.1} times the typical rise",
            -worst / typical
        );
    }

    /// A handful of pixels is not evidence, and a unit test's two by two frame
    /// must never be able to move a real merge.
    #[test]
    fn too_few_pixels_to_measure_leaves_the_label_alone() {
        let long = field(40, 0.5, TRUE_LONG);
        let short = field(40, 0.5, SHORT);
        let lights =
            measured_lights(&[mislabelled(&long, CLAIMED_LONG), mislabelled(&short, SHORT)]);

        assert!((lights[0] - CLAIMED_LONG).abs() < 1e-6, "{:?}", lights);
        assert!((lights[1] - SHORT).abs() < 1e-6, "{:?}", lights);
    }

    /// And a measurement wilder than a rounded label could ever explain is two
    /// pictures of different things, not a bracket.
    #[test]
    fn a_measurement_more_than_a_stop_out_is_not_believed() {
        let long = wide_ramp(BIG, TRUE_LONG, RAMP_FROM, RAMP_TO);
        let short = wide_ramp(BIG, SHORT, RAMP_FROM, RAMP_TO);
        // Both labelled the same, while the pixels are two stops apart.
        let lights = measured_lights(&[mislabelled(&long, SHORT), mislabelled(&short, SHORT)]);

        assert!((lights[0] - SHORT).abs() < 1e-6, "{:?}", lights);
        assert!((lights[1] - SHORT).abs() < 1e-6, "{:?}", lights);
    }

    /// One frame is not a bracket and has nothing to be measured against.
    #[test]
    fn a_single_frame_keeps_its_own_label() {
        let only = field(40, 0.5, TRUE_LONG);
        let lights = measured_lights(&[mislabelled(&only, CLAIMED_LONG)]);
        assert_eq!(lights.len(), 1);
        assert!((lights[0] - CLAIMED_LONG).abs() < 1e-6);
    }

    /// A frame of one colour, everywhere.
    fn flat(rgb: [f32; 3]) -> Rgb32FImage {
        Rgb32FImage::from_pixel(2, 2, Rgb(rgb))
    }

    fn secs(t: f32) -> Duration {
        Duration::from_secs_f32(t)
    }

    /// What the old merge did, for comparison: the same estimator with no
    /// saturation condition on it.
    fn without_the_condition(frames: &[Frame<'_>]) -> [f32; 3] {
        let mut sum = [0.0f32; 3];
        let mut light = 0.0f32;
        for frame in frames {
            let observed = frame.image.get_pixel(0, 0).0;
            for c in 0..3 {
                sum[c] += observed[c];
            }
            light += frame.light();
        }
        [sum[0] / light, sum[1] / light, sum[2] / light]
    }

    #[test]
    fn a_bracket_with_nothing_clipped_is_unchanged() {
        // The ordinary case, and the one that must not move: a scene at 0.4
        // radiance seen at three exposures, none of them clipping.
        let truth = 0.4;
        let images: Vec<Rgb32FImage> = [0.25f32, 0.5, 1.0]
            .iter()
            .map(|t| flat([truth * t, truth * t, truth * t]))
            .collect();
        let frames: Vec<Frame<'_>> = images
            .iter()
            .zip([0.25f32, 0.5, 1.0])
            .map(|(image, t)| Frame {
                image,
                exposure: secs(t),
                gain: 1.0,
            })
            .collect();

        let merged = merge_bracket(&frames);
        let got = merged.get_pixel(0, 0).0;
        assert!((got[0] - truth).abs() < 1e-5, "{got:?} should be {truth}");

        // And it agrees with the old merge, so a clean bracket is untouched.
        let old = without_the_condition(&frames);
        assert!(
            (old[0] - got[0]).abs() < 1e-5,
            "clean brackets must not change"
        );
    }

    #[test]
    fn a_clipped_frame_no_longer_drags_the_highlight_down() {
        // This is the bug. A bright highlight, radiance 3.0. The short frame
        // records it correctly at 0.75; the long frame clips at 1.0 when it
        // should have read 3.0.
        let truth = 3.0;
        let short = flat([truth * 0.25, truth * 0.25, truth * 0.25]);
        let long = flat([1.0, 1.0, 1.0]);
        let frames = [
            Frame {
                image: &short,
                exposure: secs(0.25),
                gain: 1.0,
            },
            Frame {
                image: &long,
                exposure: secs(1.0),
                gain: 1.0,
            },
        ];

        let got = merge_bracket(&frames).get_pixel(0, 0).0;
        assert!((got[0] - truth).abs() < 1e-4, "{got:?} should be {truth}");

        let old = without_the_condition(&frames);
        assert!(
            old[0] < truth * 0.6,
            "the old merge should be far too dark: {old:?}"
        );
    }

    #[test]
    fn a_clipped_channel_no_longer_tints_the_result() {
        // A warm highlight: red clips in the long frame, green and blue do not.
        // The old merge pulled red down and left the others, which turns a warm
        // grey into something with the wrong hue entirely.
        let truth = [2.4f32, 2.0, 1.6];
        let short = flat([truth[0] * 0.25, truth[1] * 0.25, truth[2] * 0.25]);
        // At one second red would be 2.4 and clips; the others fit.
        let long = flat([1.0, truth[1], truth[2]]);
        let frames = [
            Frame {
                image: &short,
                exposure: secs(0.25),
                gain: 1.0,
            },
            Frame {
                image: &long,
                exposure: secs(1.0),
                gain: 1.0,
            },
        ];

        let got = merge_bracket(&frames).get_pixel(0, 0).0;
        for c in 0..3 {
            assert!(
                (got[c] - truth[c]).abs() < 1e-4,
                "{got:?} should be {truth:?}"
            );
        }

        // The ratios are what the eye reads as colour, and the old merge got
        // them wrong.
        let old = without_the_condition(&frames);
        let truth_ratio = truth[0] / truth[2];
        let old_ratio = old[0] / old[2];
        assert!(
            (old_ratio - truth_ratio).abs() > 0.2,
            "the old merge should have skewed the colour: {old:?}"
        );
        let got_ratio = got[0] / got[2];
        assert!(
            (got_ratio - truth_ratio).abs() < 1e-3,
            "and this one should not"
        );
    }

    #[test]
    fn a_pixel_clipped_everywhere_stays_bright() {
        // The sun. No frame measured it, so there is nothing to average, and the
        // answer must not be zero or the brightest thing in the picture becomes
        // a black hole.
        let short = flat([1.0, 1.0, 1.0]);
        let long = flat([1.0, 1.0, 1.0]);
        let frames = [
            Frame {
                image: &short,
                exposure: secs(0.25),
                gain: 1.0,
            },
            Frame {
                image: &long,
                exposure: secs(1.0),
                gain: 1.0,
            },
        ];

        let got = merge_bracket(&frames).get_pixel(0, 0).0;
        // The shortest exposure gives the highest lower bound: 1.0 / 0.25.
        assert!(
            (got[0] - 4.0).abs() < 1e-4,
            "{got:?} should fall back to the shortest frame"
        );
    }

    #[test]
    fn every_frame_that_measured_a_pixel_still_contributes() {
        // Noise averaging is the reason to merge at all, so a mid-tone must not
        // quietly become "the shortest frame only".
        let truth = 0.3;
        let a = flat([truth * 0.25, truth * 0.25, truth * 0.25]);
        let b = flat([truth * 1.0, truth * 1.0, truth * 1.0]);
        let frames = [
            Frame {
                image: &a,
                exposure: secs(0.25),
                gain: 1.0,
            },
            Frame {
                image: &b,
                exposure: secs(1.0),
                gain: 1.0,
            },
        ];
        let got = merge_bracket(&frames).get_pixel(0, 0).0;
        assert!((got[0] - truth).abs() < 1e-5);

        // Only the short frame would give the same answer here, so prove the
        // long one was used: drop it and the divisor changes.
        let only_short = [Frame {
            image: &a,
            exposure: secs(0.25),
            gain: 1.0,
        }];
        let alone = merge_bracket(&only_short).get_pixel(0, 0).0;
        assert!(
            (alone[0] - truth).abs() < 1e-5,
            "one frame alone is still its own radiance"
        );
    }

    /// A ramp of true radiance, and what a frame records of it.
    ///
    /// `actual` is the light the frame really received. What is handed to the
    /// merge is what its metadata claims, and the two are not the same in a real
    /// bracket. That difference is the whole of this artefact. A shutter marked
    /// 1/60 runs at 1/64, apertures are nominal, ISO is a rating rather than a
    /// measurement, and veiling glare differs between exposures, so two frames
    /// of one scene disagree about absolute radiance by a few percent.
    ///
    /// A frame switched out at a threshold turns that disagreement into a step:
    /// the pixel before the line is an average that includes the long frame's
    /// slightly-off reading and the pixel after it is not. On a smooth wall a
    /// step is a hard edge. Faded out, the same disagreement is spread over a
    /// band and reads as part of the gradient.
    fn ramp_with(width: u32, actual: f32, from: f32, to: f32) -> Rgb32FImage {
        Rgb32FImage::from_fn(width, 1, |x, _| {
            let t = x as f32 / (width - 1) as f32;
            let radiance = from + (to - from) * t;
            let recorded = (radiance * actual).min(1.0);
            Rgb([recorded, recorded, recorded])
        })
    }

    /// How far a real bracket's frames disagree about absolute radiance. Four
    /// percent is modest; nominal shutter speeds alone account for more.
    const MISCALIBRATION: f32 = 1.04;

    /// The differences between one pixel and the next, along the ramp.
    fn steps(merged: &Rgb32FImage) -> Vec<f32> {
        (1..merged.width())
            .map(|x| merged.get_pixel(x, 0).0[0] - merged.get_pixel(x - 1, 0).0[0])
            .collect()
    }

    /// The artefact this module was rewritten for.
    ///
    /// A smooth wall, brightening across the frame, with the long exposure
    /// clipping partway along it. The estimate must stay smooth as that frame
    /// stops contributing. With the frame switched out at a threshold instead of
    /// faded out, the pixel on one side of the line is an average of two frames
    /// and the pixel on the other is an average of one, and the jump between
    /// them draws a hard edge across the wall.
    ///
    /// Measured as the largest step against the typical step. On a straight ramp
    /// every step should be about the same size; a seam is one step several
    /// times the others. The binary version scored above 8 here.
    #[test]
    fn a_frame_leaving_the_average_does_not_draw_an_edge() {
        let width = 512;
        // The long frame received a little more light than its metadata says.
        let long = ramp_with(width, MISCALIBRATION, 0.2, 2.0);
        let short = ramp_with(width, 0.2, 0.2, 2.0);

        let merged = merge_bracket(&[
            Frame {
                image: &long,
                exposure: Duration::from_secs_f32(1.0),
                gain: 1.0,
            },
            Frame {
                image: &short,
                exposure: Duration::from_secs_f32(0.2),
                gain: 1.0,
            },
        ]);

        let steps = steps(&merged);
        assert!(
            steps.iter().all(|s| *s > 0.0),
            "the ramp must stay increasing"
        );

        let mut sorted = steps.clone();
        sorted.sort_by(f32::total_cmp);
        let typical = sorted[sorted.len() / 2];
        let largest = *sorted.last().unwrap();

        assert!(
            largest < typical * 2.0,
            "a step of {largest} against a typical {typical} is a visible seam"
        );
    }

    /// The same ramp, in colour, since a seam that only shows in one channel is
    /// a coloured line rather than a grey one and is worse, not better.
    #[test]
    fn the_edge_does_not_appear_in_one_channel_either() {
        let width = 512;
        // Warmer than neutral, so the channels reach clipping at different
        // points along the ramp and each hands over at its own place.
        let tint = [1.0f32, 0.82, 0.65];
        let frame = |actual: f32| {
            Rgb32FImage::from_fn(width, 1, |x, _| {
                let t = x as f32 / (width - 1) as f32;
                let radiance = 0.2 + 1.8 * t;
                Rgb([
                    (radiance * tint[0] * actual).min(1.0),
                    (radiance * tint[1] * actual).min(1.0),
                    (radiance * tint[2] * actual).min(1.0),
                ])
            })
        };
        let long = frame(MISCALIBRATION);
        let short = frame(0.2);

        let merged = merge_bracket(&[
            Frame {
                image: &long,
                exposure: Duration::from_secs_f32(1.0),
                gain: 1.0,
            },
            Frame {
                image: &short,
                exposure: Duration::from_secs_f32(0.2),
                gain: 1.0,
            },
        ]);

        for channel in 0..3 {
            let mut steps: Vec<f32> = (1..width)
                .map(|x| merged.get_pixel(x, 0).0[channel] - merged.get_pixel(x - 1, 0).0[channel])
                .collect();
            steps.sort_by(f32::total_cmp);
            let typical = steps[steps.len() / 2];
            let largest = *steps.last().unwrap();
            let smallest = steps[0];

            assert!(
                largest < typical * 2.0,
                "channel {channel} rises by {largest} where {typical} is typical"
            );
            // The fall is the one that matters, and the one the earlier version
            // of this test missed. A seam is the picture getting darker where
            // the scene gets brighter, so it shows here and not above.
            assert!(
                smallest > 0.0,
                "channel {channel} falls by {smallest} against a typical rise of {typical},                  which is an edge"
            );
        }
    }

    /// The weight itself, since everything above rests on it being smooth.
    #[test]
    fn the_weight_falls_off_without_a_step_in_it() {
        assert_eq!(weight_for(&[0.0, 0.0, 0.0]), 1.0);
        assert_eq!(
            weight_for(&[ROLL_OFF_START, 0.0, 0.0]),
            1.0,
            "full weight up to the ramp"
        );
        assert_eq!(
            weight_for(&[SATURATION, 0.0, 0.0]),
            0.0,
            "nothing at clipping"
        );
        assert_eq!(weight_for(&[1.4, 0.1, 0.1]), 0.0, "nor beyond it");

        // Driven by the brightest channel, because the colour matrix has already
        // spread a clipped channel into the others.
        assert_eq!(
            weight_for(&[0.1, 0.1, SATURATION]),
            0.0,
            "one clipped channel takes the whole pixel with it"
        );

        // Walk the ramp finely: no two neighbouring samples may differ by more
        // than the function's own steepest slope allows, or there is a step in
        // the weight and therefore a seam in the picture.
        //
        // Checked against what smoothstep actually does rather than a number
        // picked by eye. Its slope peaks at 1.5 halfway along, in units of the
        // ramp, so across a band this wide the steepest it can fall is
        // `1.5 / band` per unit of value. Anything at that bound is the curve
        // behaving; anything above it is a discontinuity. A switch instead of a
        // ramp would score 1.0 here.
        let samples = 4000;
        let span = 1.2f32;
        let spacing = span / samples as f32;
        let steepest = 1.5 / (SATURATION - ROLL_OFF_START);
        let allowed = steepest * spacing * 1.05;

        let mut previous = 1.0f32;
        let mut largest_step = 0.0f32;
        for i in 0..=samples {
            let value = i as f32 / samples as f32 * span;
            let w = weight_for(&[value, 0.0, 0.0]);
            largest_step = largest_step.max((w - previous).abs());
            previous = w;
        }
        assert!(
            largest_step < allowed,
            "the weight jumps by {largest_step}, above the {allowed} its own slope allows"
        );

        // And flat at both ends, which is what a straight ramp would not be.
        let just_inside = weight_for(&[ROLL_OFF_START + 0.0005, 0.0, 0.0]);
        let just_before_end = weight_for(&[SATURATION - 0.0005, 0.0, 0.0]);
        assert!(
            just_inside > 0.999,
            "the fade starts gently, got {just_inside}"
        );
        assert!(
            just_before_end < 0.001,
            "and ends gently, got {just_before_end}"
        );
    }

    #[test]
    fn the_metered_white_is_the_middle_frame_of_the_bracket() {
        let flat_image = flat([0.0, 0.0, 0.0]);
        let frames = [
            Frame {
                image: &flat_image,
                exposure: secs(0.25),
                gain: 1.0,
            },
            Frame {
                image: &flat_image,
                exposure: secs(0.5),
                gain: 1.0,
            },
            Frame {
                image: &flat_image,
                exposure: secs(1.0),
                gain: 1.0,
            },
        ];
        // The middle frame clipped at 1.0 after half a second, so a radiance of
        // 2.0 is what it called white.
        let white = white_from(frames.iter().map(Frame::light));
        assert!((white - 2.0).abs() < 1e-5, "{white}");
    }

    #[test]
    fn what_the_metered_frame_called_white_comes_out_near_white() {
        let image = Rgb32FImage::from_pixel(1, 1, Rgb([2.0, 2.0, 2.0]));
        let shown = to_display(&image, 2.0);
        let v = shown.get_pixel(0, 0).0[0];
        assert!(
            (v - 0.8).abs() < 1e-4,
            "metered white should land at the shoulder: {v}"
        );
    }

    #[test]
    fn a_lamp_does_not_darken_the_room() {
        // The failure the percentile version had. A room at a fifth of metered
        // white, and a lamp fifty times brighter. The room must still read as a
        // room whatever the lamp does, because the scale does not depend on it.
        let white = 2.0;
        let room = to_display(&Rgb32FImage::from_pixel(1, 1, Rgb([0.4, 0.4, 0.4])), white)
            .get_pixel(0, 0)
            .0[0];
        // Normalising by the maximum, which is what the old stretch did, would
        // have put this pixel at 0.4 divided by whatever was brightest in shot.
        assert!(room > 0.1, "the room is a room, not a shadow: {room}");
        // A fifth of the metered white, and the curve is straight down here.
        assert!((room - 0.16).abs() < 1e-4, "{room}");

        // And the brightest thing in shot must not move it at all.
        let mut with_a_lamp = Rgb32FImage::from_pixel(2, 2, Rgb([0.4, 0.4, 0.4]));
        with_a_lamp.put_pixel(0, 0, Rgb([100.0, 100.0, 100.0]));
        let beside_a_lamp = to_display(&with_a_lamp, white).get_pixel(1, 1).0[0];
        assert_eq!(
            room, beside_a_lamp,
            "the scale does not come from the picture"
        );
    }

    #[test]
    fn highlights_above_white_are_kept_rather_than_clipped() {
        let white = 2.0;
        let one = to_display(&Rgb32FImage::from_pixel(1, 1, Rgb([2.0, 2.0, 2.0])), white)
            .get_pixel(0, 0)
            .0[0];
        let two = to_display(&Rgb32FImage::from_pixel(1, 1, Rgb([4.0, 4.0, 4.0])), white)
            .get_pixel(0, 0)
            .0[0];
        let four = to_display(&Rgb32FImage::from_pixel(1, 1, Rgb([8.0, 8.0, 8.0])), white)
            .get_pixel(0, 0)
            .0[0];

        assert!(
            two > one,
            "a stop over white must be brighter than white: {one} then {two}"
        );
        assert!(
            four > two,
            "and two stops brighter still: {two} then {four}"
        );
        assert!(four < 1.0, "without ever reaching pure white");
    }

    #[test]
    fn the_shadows_are_not_bent() {
        // Below metered white the curve should be close to straight, or the
        // correctly exposed part of the picture would not look as it did.
        let white = 2.0;
        let half = to_display(&Rgb32FImage::from_pixel(1, 1, Rgb([1.0, 1.0, 1.0])), white)
            .get_pixel(0, 0)
            .0[0];
        let quarter = to_display(&Rgb32FImage::from_pixel(1, 1, Rgb([0.5, 0.5, 0.5])), white)
            .get_pixel(0, 0)
            .0[0];
        // Halving the light halves the value. Exactly, not approximately: below
        // the metered white this is a straight line and nothing else.
        let ratio = half / quarter;
        assert!(
            (ratio - 2.0).abs() < 1e-4,
            "shadows must stay linear: {ratio}"
        );
    }

    #[test]
    fn black_stays_black_and_nothing_goes_negative() {
        let image = Rgb32FImage::from_pixel(2, 2, Rgb([0.0, -0.5, f32::NAN]));
        let shown = to_display(&image, 2.0);
        for pixel in shown.pixels() {
            for v in pixel.0 {
                assert!(
                    v.is_finite() && (0.0..=1.0).contains(&v),
                    "{v} is not a colour"
                );
            }
        }
        assert_eq!(shown.get_pixel(0, 0).0[0], 0.0);
    }

    #[test]
    fn gain_is_part_of_the_light_a_frame_was_given() {
        // Two frames of the same scene, one at half the shutter and twice the
        // ISO. They saw the same light and must merge to the same radiance.
        let truth = 0.5;
        let slow = flat([truth * 1.0, truth * 1.0, truth * 1.0]);
        let fast = flat([truth * 1.0, truth * 1.0, truth * 1.0]);
        let frames = [
            Frame {
                image: &slow,
                exposure: secs(1.0),
                gain: 1.0,
            },
            Frame {
                image: &fast,
                exposure: secs(0.5),
                gain: 2.0,
            },
        ];
        let got = merge_bracket(&frames).get_pixel(0, 0).0;
        assert!((got[0] - truth).abs() < 1e-5, "{got:?} should be {truth}");
    }
}

// ============ BLITZRAW: where the line across a smooth wall comes from ============
/// Measuring the artefact on a real bracket rather than arguing about it.
///
/// After the two fixes above, merges came back with the yellow gone and a soft
/// but visible line still drawn across the ceiling and the upper wall, along a
/// contour of equal brightness. Two candidates sit at almost the same place in
/// the picture, which is why looking at it cannot separate them:
///
/// - **The handover.** A frame fading out of the average between
///   `ROLL_OFF_START` and `SATURATION`. For the metered frame that band is
///   `x` in 0.70 to 0.98, where `x` is radiance over the metered white.
/// - **The shoulder.** `to_display` joins a straight line to an exponential at
///   `x = 1`. Value and slope match there; curvature does not, jumping from
///   zero to `-w * w / (1 - w)`. A curvature step on a smooth gradient is what
///   a Mach band is made of.
///
/// 0.70 to 0.98 and 1.0 are neighbours, so the two predict a line in nearly the
/// same place. What separates them is that a bracket has more than one frame:
/// each fades at its own scene brightness, so the handover predicts **several**
/// lines spread across the range, and the shoulder predicts exactly **one**, at
/// `x = 1` and nowhere else.
///
/// This prints where each of them falls and paints them onto the picture, so
/// the visible line can be laid against both.
///
/// ```text
/// RAPIDRAW_TEST_HDR_BRACKET="a.dng;b.dng;c.dng" cargo test --lib -- --nocapture report_where_the_line_falls
/// ```
///
/// Frames may be given in any order. `RAPIDRAW_TEST_OUT` says where the
/// pictures go, and defaults to the temporary folder. This prints and writes
/// rather than asserting, because it is a measurement of one shoot and not a
/// statement about the code.
#[cfg(test)]
mod probe {
    use super::*;
    use crate::app_settings::AppSettings;
    use crate::exif_processing::{read_exposure_time_secs, read_iso};
    use crate::formats::is_raw_file;
    use crate::image_loader::load_base_image_from_bytes;
    use crate::image_processing::apply_srgb_to_linear;
    use image::{Rgb, RgbImage};

    /// The sRGB curve a merge is written through, so a picture written here
    /// looks like the one on screen.
    fn encode(x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        if x <= 0.0031308 {
            x * 12.92
        } else {
            1.055 * x.powf(1.0 / 2.4) - 0.055
        }
    }

    fn as_byte(x: f32) -> u8 {
        (encode(x) * 255.0 + 0.5).clamp(0.0, 255.0) as u8
    }

    /// What `load_hdr_frames` does, without the app handle it only wants for
    /// progress messages.
    fn load(path: &str, settings: &AppSettings) -> (Rgb32FImage, Duration, f32) {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let mut image = load_base_image_from_bytes(&bytes, path, false, settings, None)
            .unwrap_or_else(|e| panic!("decode {path}: {e}"));
        if !is_raw_file(path) {
            image = apply_srgb_to_linear(image);
        }
        let gain = read_iso(path, &bytes).expect("ISO") as f32;
        let exposure =
            Duration::from_secs_f32(read_exposure_time_secs(path, &bytes).expect("ExposureTime"));
        let buffer = image.to_rgb32f();
        drop(image);
        (buffer, exposure, gain)
    }

    /// Shrinks by averaging whole blocks, which keeps a one pixel line visible
    /// where dropping samples would lose it.
    fn shrink(image: &RgbImage, target_height: u32) -> RgbImage {
        let factor = (image.height() / target_height.max(1)).max(1);
        let (w, h) = (image.width() / factor, image.height() / factor);
        let mut out = RgbImage::new(w.max(1), h.max(1));
        for (x, y, pixel) in out.enumerate_pixels_mut() {
            let mut sum = [0u32; 3];
            for dy in 0..factor {
                for dx in 0..factor {
                    let p = image.get_pixel(x * factor + dx, y * factor + dy).0;
                    for c in 0..3 {
                        sum[c] += p[c] as u32;
                    }
                }
            }
            let n = factor * factor;
            *pixel = Rgb([(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8]);
        }
        out
    }

    /// How rough the picture is, by level, measured in a way real detail does
    /// not win.
    ///
    /// The second difference along a row, relative to the level. Noise is in
    /// every pixel and detail is in a minority of them, so the median of it
    /// tracks the noise and ignores the edges.
    fn roughness_by_level(image: &Rgb32FImage, scale: f32) -> Vec<(f32, f32, u64)> {
        const BANDS: usize = 6;
        const TOP: f32 = 0.6;
        let mut buckets: Vec<Vec<f32>> = vec![Vec::new(); BANDS];
        for y in (0..image.height()).step_by(3) {
            for x in 1..image.width() - 1 {
                let here = luma(&image.get_pixel(x, y).0);
                if here <= 1e-6 {
                    continue;
                }
                let position = here * scale;
                if position >= TOP {
                    continue;
                }
                let left = luma(&image.get_pixel(x - 1, y).0);
                let right = luma(&image.get_pixel(x + 1, y).0);
                let bend = (left - 2.0 * here + right).abs() / here;
                let band = ((position / TOP) * BANDS as f32) as usize;
                buckets[band.min(BANDS - 1)].push(bend);
            }
        }
        buckets
            .into_iter()
            .enumerate()
            .map(|(band, mut values)| {
                let count = values.len() as u64;
                let middle = if values.is_empty() {
                    0.0
                } else {
                    values.sort_by(f32::total_cmp);
                    values[values.len() / 2]
                };
                (band as f32 * TOP / BANDS as f32, middle, count)
            })
            .collect()
    }

    /// What the three preprocessing sliders cost a merge.
    ///
    /// All three run inside the decode, so every frame of a bracket goes
    /// through them before the merge sees it, and the merge is written as a
    /// `.dng`, so opening it runs all three **again** on the result.
    ///
    /// Three things are measured here rather than argued about:
    ///
    /// - what the colour noise reduction's clamp does to the headroom the
    ///   highlight recovery just made
    /// - whether sharpening every frame and then averaging them is worse than
    ///   averaging first and sharpening once, which is what the load path does
    ///   to the merge anyway
    /// - how much the second pass moves the merge
    ///
    /// `RAPIDRAW_TEST_MERGED_DNG` points at a merge for the last of those.
    #[test]
    fn report_what_the_preprocessing_costs_a_merge() {
        let Ok(list) = std::env::var("RAPIDRAW_TEST_HDR_BRACKET") else {
            eprintln!("RAPIDRAW_TEST_HDR_BRACKET unset, skipping");
            return;
        };
        let paths: Vec<String> = list
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();

        let shipped = AppSettings::default();
        let mut bare = AppSettings::default();
        bare.raw_preprocessing_color_nr = Some(0.0);
        bare.raw_preprocessing_sharpening = Some(0.0);

        // === 1. The headroom the highlight recovery makes, and what is left
        // of it once the colour noise reduction has run. ===
        eprintln!("\n=== what survives the decode, one frame ===");
        for (name, settings) in [("as shipped", &shipped), ("no NR, no sharpening", &bare)] {
            let (image, _, _) = load(&paths[0], settings);
            let mut highest = 0.0f32;
            let mut above_one = 0u64;
            for pixel in image.pixels() {
                let top = pixel.0[0].max(pixel.0[1]).max(pixel.0[2]);
                highest = highest.max(top);
                if top > 1.0 {
                    above_one += 1;
                }
            }
            eprintln!("  {name:<22} brightest {highest:.4}, {above_one} pixels above 1.0",);
        }

        // === 2. Sharpen each frame and then average, against average and then
        // sharpen once. Both end up sharpened the same number of times. ===
        let load_all = |settings: &AppSettings| -> Vec<(Rgb32FImage, Duration, f32)> {
            paths.iter().map(|p| load(p, settings)).collect()
        };
        fn as_frames(loaded: &[(Rgb32FImage, Duration, f32)]) -> Vec<Frame<'_>> {
            loaded
                .iter()
                .map(|(image, exposure, gain)| Frame {
                    image,
                    exposure: *exposure,
                    gain: *gain,
                })
                .collect()
        }

        let preprocessed = load_all(&shipped);
        let first = merge_bracket(&as_frames(&preprocessed));
        let white = white_from(as_frames(&preprocessed).iter().map(Frame::light));
        drop(preprocessed);

        let clean = load_all(&bare);
        let second = merge_bracket(&as_frames(&clean));
        drop(clean);
        // The merge is scene linear and can run past one, and the enhancer
        // clamps, so it is fed the display-referred picture the load path
        // would hand it rather than the raw radiance.
        let mut sharpened_once = DynamicImage::ImageRgb32F(to_display(&second, white));
        crate::image_processing::remove_raw_artifacts_and_enhance(
            &mut sharpened_once,
            12.0 / 0.5 - 10.0,
            0.35,
        );
        let sharpened_once = sharpened_once.to_rgb32f();
        let first_shown = to_display(&first, white);

        eprintln!("\n=== how rough the merge is, the two ways round ===");
        eprintln!(
            "{:>12} {:>16} {:>16} {:>10}",
            "level", "sharpen then merge", "merge then sharpen", "worse by"
        );
        let a = roughness_by_level(&first_shown, 1.0);
        let b = roughness_by_level(&sharpened_once, 1.0);
        for ((from, rough_a, count), (_, rough_b, _)) in a.iter().zip(b.iter()) {
            if *count < 10_000 {
                continue;
            }
            eprintln!(
                "{:>11.2}+ {:>16.5} {:>16.5} {:>9.0}%",
                from,
                rough_a,
                rough_b,
                (rough_a / rough_b - 1.0) * 100.0
            );
        }

        // === 2b. And what the clamped headroom costs the merge itself. The
        // brightest things in the room are clipped in every frame, so the
        // merge falls back to the shortest one's own reading, and that reading
        // has been flattened to 1.0. ===
        eprintln!(
            "
=== what the two merges say, by level ==="
        );
        eprintln!(
            "{:>14} {:>14} {:>16} {:>16} {:>10}",
            "x", "pixels", "with the clamp", "without it", "higher by"
        );
        const STEPS: [f32; 7] = [0.0, 0.25, 0.5, 0.75, 1.0, 1.5, 2.0];
        let scale = 1.0 / white;
        for band in 0..STEPS.len() - 1 {
            let (from, to) = (STEPS[band], STEPS[band + 1]);
            let (mut sum_a, mut sum_b, mut count) = (0.0f64, 0.0f64, 0u64);
            for (a, b) in first.pixels().zip(second.pixels()) {
                let position = luma(&b.0) * scale;
                if position < from || position >= to {
                    continue;
                }
                sum_a += (luma(&a.0) * scale) as f64;
                sum_b += (luma(&b.0) * scale) as f64;
                count += 1;
            }
            if count < 10_000 {
                continue;
            }
            let (a, b) = (sum_a / count as f64, sum_b / count as f64);
            eprintln!(
                "{:>6.2}..{:<7.2} {:>14} {:>16.4} {:>16.4} {:>9.1}%",
                from,
                to,
                count,
                a,
                b,
                (b / a - 1.0) * 100.0
            );
        }

        // === 3. And the second pass, on a merge already written to disk. ===
        let Ok(merged) = std::env::var("RAPIDRAW_TEST_MERGED_DNG") else {
            eprintln!("\nRAPIDRAW_TEST_MERGED_DNG unset, skipping the second pass");
            return;
        };
        eprintln!("\n=== what opening a merge does to it a second time ===");
        let (with, _, _) = load(&merged, &shipped);
        let (without, _, _) = load(&merged, &bare);
        let mut moved = 0u64;
        let mut total = 0u64;
        let mut worst = 0.0f32;
        let mut sum = 0.0f64;
        for (x, y) in with.pixels().zip(without.pixels()) {
            let gap = (0..3)
                .map(|c| (x.0[c] - y.0[c]).abs())
                .fold(0.0f32, f32::max);
            total += 1;
            if gap > 1.0 / 255.0 {
                moved += 1;
            }
            worst = worst.max(gap);
            sum += gap as f64;
        }
        eprintln!(
            "  {moved} of {total} pixels move by more than one part in 255, worst {worst:.4}, mean {:.5}",
            sum / total as f64
        );
    }

    /// Whether RAW Highlight Recovery reaches the pixels a merge believes.
    ///
    /// It is on by default at 2.5, it runs inside the decode, and every frame
    /// of a bracket therefore goes through it before the merge sees it. A
    /// per-frame curve applied before a linear merge would be a real fault, so
    /// this checks rather than assumes.
    ///
    /// Reading the code says it cannot reach them: the compression only fires
    /// where the brightest channel is already above 1.0, and a merge gives any
    /// pixel above `SATURATION`, which is 0.98, a weight of zero. Reading the
    /// code is what this project keeps getting wrong, so this decodes the same
    /// frame twice, once with the recovery at 2.5 and once with it as good as
    /// off, and reports what actually moved.
    #[test]
    fn report_whether_highlight_recovery_reaches_the_merge() {
        let Ok(list) = std::env::var("RAPIDRAW_TEST_HDR_BRACKET") else {
            eprintln!("RAPIDRAW_TEST_HDR_BRACKET unset, skipping");
            return;
        };
        let path = list
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();

        let mut on = AppSettings::default();
        on.raw_highlight_compression = Some(2.5);
        let mut off = AppSettings::default();
        off.raw_highlight_compression = Some(1.01);
        eprintln!(
            "decoding {} twice: recovery at {:?} and at {:?}",
            path, on.raw_highlight_compression, off.raw_highlight_compression
        );

        let (with, _, _) = load(&path, &on);
        let (without, _, _) = load(&path, &off);
        assert_eq!(with.dimensions(), without.dimensions());

        let mut moved_below = 0u64;
        let mut moved_above = 0u64;
        let mut worst_below = 0.0f32;
        let mut worst_above = 0.0f32;
        let mut counted_below = 0u64;
        for (a, b) in with.pixels().zip(without.pixels()) {
            let top = b.0[0].max(b.0[1]).max(b.0[2]);
            let gap = (0..3)
                .map(|c| (a.0[c] - b.0[c]).abs())
                .fold(0.0f32, f32::max);
            // A merge gives this pixel a weight above zero, so it is a pixel
            // the merge actually believes.
            if weight_for(&b.0) > 0.0 {
                counted_below += 1;
                if gap > 1e-6 {
                    moved_below += 1;
                }
                worst_below = worst_below.max(gap);
            } else if top >= SATURATION {
                if gap > 1e-6 {
                    moved_above += 1;
                }
                worst_above = worst_above.max(gap);
            }
        }

        eprintln!(
            "pixels the merge believes:      {counted_below}, of which {moved_below} moved, worst {worst_below:.6}"
        );
        eprintln!("pixels the merge already ignores: {moved_above} moved, worst {worst_above:.6}");
        // And where they sit, because a spatial filter running after the
        // compression would move only the pixels beside a blown highlight,
        // while a curve would move them everywhere.
        let mut moved_by_level = [0u64; 10];
        let mut total_by_level = [0u64; 10];
        for (a, b) in with.pixels().zip(without.pixels()) {
            if weight_for(&b.0) <= 0.0 {
                continue;
            }
            let top = b.0[0].max(b.0[1]).max(b.0[2]);
            let bin = ((top / SATURATION * 10.0) as usize).min(9);
            total_by_level[bin] += 1;
            let gap = (0..3)
                .map(|c| (a.0[c] - b.0[c]).abs())
                .fold(0.0f32, f32::max);
            if gap > 1e-6 {
                moved_by_level[bin] += 1;
            }
        }
        eprintln!(
            "{:>18} {:>14} {:>10}",
            "brightest channel", "pixels", "moved"
        );
        for bin in 0..10 {
            if total_by_level[bin] == 0 {
                continue;
            }
            eprintln!(
                "{:>7.2}..{:<9.2} {:>14} {:>9.2}%",
                bin as f32 * SATURATION / 10.0,
                (bin + 1) as f32 * SATURATION / 10.0,
                total_by_level[bin],
                moved_by_level[bin] as f64 / total_by_level[bin] as f64 * 100.0
            );
        }

        eprintln!(
            "so highlight recovery {} the merge",
            if moved_below == 0 {
                "cannot reach"
            } else {
                "DOES reach"
            }
        );
    }

    #[test]
    fn report_where_the_line_falls() {
        let Ok(list) = std::env::var("RAPIDRAW_TEST_HDR_BRACKET") else {
            eprintln!("RAPIDRAW_TEST_HDR_BRACKET unset, skipping");
            return;
        };
        let out_dir = std::env::var("RAPIDRAW_TEST_OUT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("blitzraw-hdr-probe"));
        std::fs::create_dir_all(&out_dir).expect("output folder");

        let paths: Vec<String> = list
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        assert!(paths.len() >= 2, "a bracket has at least two frames");

        // What a merge really asks the decode for, which is not what an
        // ordinary photo asks for. See `settings_for_merge_frames`.
        let mut settings =
            crate::hdr_deghosting::settings_for_merge_frames(&AppSettings::default());
        // And the overrides, so the same bracket can be measured with the
        // preprocessing put back on, which is the only way to say whether it
        // reaches the merge.
        if let Ok(amount) = std::env::var("RAPIDRAW_TEST_HIGHLIGHT_RECOVERY")
            && let Ok(amount) = amount.parse::<f32>()
        {
            settings.raw_highlight_compression = Some(amount);
        }
        if let Ok(amount) = std::env::var("RAPIDRAW_TEST_PREPROCESSING")
            && let Ok(amount) = amount.parse::<f32>()
        {
            settings.raw_preprocessing_color_nr = Some(amount);
            settings.raw_preprocessing_sharpening = Some(amount);
        }
        eprintln!(
            "highlight recovery {:?}, colour NR {:?}, sharpening {:?}",
            settings.raw_highlight_compression,
            settings.raw_preprocessing_color_nr,
            settings.raw_preprocessing_sharpening
        );
        let began = std::time::Instant::now();
        let loaded: Vec<(Rgb32FImage, Duration, f32)> =
            paths.iter().map(|p| load(p, &settings)).collect();
        eprintln!("decoded {} frames in {:?}", loaded.len(), began.elapsed());

        let frames: Vec<Frame<'_>> = loaded
            .iter()
            .map(|(image, exposure, gain)| Frame {
                image,
                exposure: *exposure,
                gain: *gain,
            })
            .collect();

        let white = white_from(frames.iter().map(Frame::light));
        let metered_light = 1.0 / white;

        // What the labels claim, and what the frames themselves say.
        let lights = measured_lights(&frames);

        eprintln!("\n=== the bracket ===");
        eprintln!(
            "metered white is a radiance of {white:.5}, so x = radiance * {metered_light:.5}"
        );
        eprintln!(
            "{:<24} {:>9} {:>6} {:>10} {:>10} {:>8} {:>16}",
            "frame", "shutter", "ISO", "labelled", "measured", "out by", "fades over x"
        );
        for ((path, frame), light) in paths.iter().zip(frames.iter()).zip(lights.iter()) {
            let claimed = frame.light();
            let name = std::path::Path::new(path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            eprintln!(
                "{:<24} {:>8.5}s {:>6.0} {:>10.4} {:>10.4} {:>7.1}% {:>7.3} .. {:.3}",
                name,
                frame.exposure.as_secs_f32(),
                frame.gain,
                claimed,
                light,
                (light / claimed - 1.0) * 100.0,
                ROLL_OFF_START * metered_light / light,
                SATURATION * metered_light / light,
            );
        }
        eprintln!("the shoulder joins at x = 1.000 exactly, and nowhere else");

        let mut by_light: Vec<usize> = (0..frames.len()).collect();
        by_light.sort_by(|a, b| frames[*a].light().total_cmp(&frames[*b].light()));
        eprintln!(
            "
{:>34} {:>10} {:>10} {:>16}",
            "pair", "labelled", "measured", "spread, stops"
        );
        for pair in by_light.windows(2) {
            let (dark, bright) = (pair[0], pair[1]);
            let short_name = |index: usize| {
                std::path::Path::new(&paths[index])
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            };
            let claimed = frames[bright].light() / frames[dark].light();
            match ratio_between(frames[bright].image, frames[dark].image) {
                Some((ratio, spread)) => eprintln!(
                    "{:>34} {:>10.4} {:>10.4} {:>16.4}",
                    format!("{} over {}", short_name(bright), short_name(dark)),
                    claimed,
                    ratio,
                    spread
                ),
                None => eprintln!(
                    "{:>34} {:>10.4} {:>10} {:>16}",
                    format!("{} over {}", short_name(bright), short_name(dark)),
                    claimed,
                    "too few",
                    "-"
                ),
            }
        }

        let began = std::time::Instant::now();
        let merged = merge_bracket(&frames);
        eprintln!("\nmerged in {:?}", began.elapsed());
        let displayed = to_display(&merged, white);

        // Where the picture actually sits. If the surface carrying the line
        // never reaches x = 1, the shoulder cannot be what draws it.
        let mut histogram = [0u64; 24];
        let mut above_white = 0u64;
        let (width, height) = (merged.width(), merged.height());
        for pixel in merged.pixels() {
            let x = luma(&pixel.0) * metered_light;
            if x >= 1.0 {
                above_white += 1;
            }
            let bin = ((x / 1.2 * 24.0) as usize).min(23);
            histogram[bin] += 1;
        }
        let total = (width as u64) * (height as u64);
        eprintln!("\n=== how much of the picture sits where, in x ===");
        for (bin, count) in histogram.iter().enumerate() {
            let from = bin as f32 * 1.2 / 24.0;
            let share = *count as f64 / total as f64;
            let bar = "#".repeat((share * 200.0) as usize);
            eprintln!(
                "  x {from:.2}..{:.2} {:>6.2}%  {bar}",
                from + 0.05,
                share * 100.0
            );
        }
        eprintln!(
            "  at or above the metered white (x >= 1): {:.2}%",
            above_white as f64 / total as f64 * 100.0
        );

        // Three pictures of the same merge.
        //
        //   display     what the app is handed today, shoulder and all
        //   no-shoulder the same radiance with the shoulder taken off, so
        //               anything above the metered white simply clips
        //   bands       the picture with each frame's fade painted on, and the
        //               shoulder's join line painted on top
        let mut display = RgbImage::new(width, height);
        let mut no_shoulder = RgbImage::new(width, height);
        let mut bands = RgbImage::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let radiance = merged.get_pixel(x, y).0;
                let shown = displayed.get_pixel(x, y).0;
                display.put_pixel(
                    x,
                    y,
                    Rgb([as_byte(shown[0]), as_byte(shown[1]), as_byte(shown[2])]),
                );
                let straight = [
                    (radiance[0] * metered_light * SHOULDER_WHITE).clamp(0.0, 1.0),
                    (radiance[1] * metered_light * SHOULDER_WHITE).clamp(0.0, 1.0),
                    (radiance[2] * metered_light * SHOULDER_WHITE).clamp(0.0, 1.0),
                ];
                no_shoulder.put_pixel(
                    x,
                    y,
                    Rgb([
                        as_byte(straight[0]),
                        as_byte(straight[1]),
                        as_byte(straight[2]),
                    ]),
                );

                // A dimmed grey copy to paint over.
                let grey = as_byte(luma(&shown) * 0.55);
                let mut painted = [grey, grey, grey];
                // Red where the first frame is on its way out, green for the
                // second, blue for the third. Each fades at its own scene
                // brightness, so they cannot be confused with one another.
                for (index, frame) in frames.iter().enumerate() {
                    let w = weight_for(&frame.image.get_pixel(x, y).0);
                    if w > 0.002 && w < 0.998 {
                        painted[index % 3] = 255;
                    }
                }
                // And magenta on the shoulder's join, which is one contour.
                let position = luma(&radiance) * metered_light;
                if (position - 1.0).abs() < 0.006 {
                    painted = [255, 0, 255];
                }
                bands.put_pixel(x, y, Rgb(painted));
            }
        }

        // And what the calibration actually changed, which is the cleanest
        // picture of the artefact there is: the same bracket merged twice,
        // once on what the labels claim and once on what the frames say, and
        // one divided by the other. Every real edge in the room cancels in
        // that ratio, so what is left is only the band, drawn in the shape it
        // really has. Mid grey is no change and full swing is a fifth of a
        // stop either way.
        const FULL_SWING: f32 = 0.2;
        let claimed: Vec<f32> = frames.iter().map(Frame::light).collect();
        let by_label = merge_with_lights(&frames, &claimed);
        let mut difference = RgbImage::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let was = luma(&by_label.get_pixel(x, y).0).max(1e-8);
                let now = luma(&merged.get_pixel(x, y).0).max(1e-8);
                let moved = (now / was).log2() / FULL_SWING * 0.5 + 0.5;
                let byte = (moved * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
                difference.put_pixel(x, y, Rgb([byte, byte, byte]));
            }
        }

        for (name, image) in [
            ("display", &display),
            ("no-shoulder", &no_shoulder),
            ("bands", &bands),
            ("what-the-calibration-moved", &difference),
        ] {
            let small = shrink(image, 1600);
            let path = out_dir.join(format!("{name}.png"));
            small.save(&path).expect("write picture");
            eprintln!(
                "wrote {} at {}x{}",
                path.display(),
                small.width(),
                small.height()
            );
        }

        // And the profile down a few scan lines, which says whether the sharpest
        // bend in the numbers sits where the shoulder is or where a handover is.
        eprintln!("\n=== the sharpest bend down each scan line ===");
        eprintln!(
            "{:>8} {:>8} {:>10} {:>10} {:>12}",
            "column", "row", "x there", "displayed", "bend"
        );
        for share in [0.2f32, 0.35, 0.5, 0.65, 0.8] {
            let centre = (width as f32 * share) as u32;
            let from = centre.saturating_sub(64);
            let to = (centre + 64).min(width);
            let rows = height as usize;
            let mut profile_x = vec![0.0f64; rows];
            let mut profile_v = vec![0.0f64; rows];
            for y in 0..height {
                let (mut sx, mut sv) = (0.0f64, 0.0f64);
                for x in from..to {
                    sx += (luma(&merged.get_pixel(x, y).0) * metered_light) as f64;
                    sv += luma(&displayed.get_pixel(x, y).0) as f64;
                }
                let n = (to - from) as f64;
                profile_x[y as usize] = sx / n;
                profile_v[y as usize] = sv / n;
            }
            // A wide second difference, so sensor noise does not win it.
            const SPAN: usize = 24;
            let mut worst = (0usize, 0.0f64);
            for y in SPAN..(rows - SPAN) {
                let bend = profile_v[y - SPAN] - 2.0 * profile_v[y] + profile_v[y + SPAN];
                if bend.abs() > worst.1.abs() {
                    worst = (y, bend);
                }
            }
            eprintln!(
                "{:>8} {:>8} {:>10.4} {:>10.4} {:>12.6}",
                centre, worst.0, profile_x[worst.0], profile_v[worst.0], worst.1
            );
        }

        // Which frame is which, by how much light it was given.
        let mut order: Vec<usize> = (0..frames.len()).collect();
        order.sort_by(|a, b| frames[*a].light().total_cmp(&frames[*b].light()));
        let metered = order[(order.len() - 1) / 2];
        let longest = *order.last().unwrap();
        // Measured, not claimed, so this reports what is left after the
        // calibration rather than what it was asked to remove.
        let light_metered = lights[metered];
        let light_longest = lights[longest];
        let name_of = |index: usize| {
            std::path::Path::new(&paths[index])
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        };

        // A fade is only invisible if the frames either side of it are saying
        // the same thing about the light. Each frame's own estimate of the
        // radiance is `observed / light`, so wherever two frames both have a
        // usable reading they should agree exactly. Whatever they disagree by
        // is the size of the step the fade has to hide.
        eprintln!("\n=== do the frames either side of the fade agree? ===");
        eprintln!(
            "the longest frame is {} and the metered one is {}",
            name_of(longest),
            name_of(metered)
        );
        eprintln!(
            "{:>16} {:>12} {:>12} {:>12} {:>10}",
            "long reads", "pixels", "long says", "metered says", "disagree"
        );
        const BINS: usize = 20;
        let mut sum_long = [0.0f64; BINS];
        let mut sum_metered = [0.0f64; BINS];
        let mut seen = [0u64; BINS];
        for y in 0..height {
            for x in 0..width {
                let long = frames[longest].image.get_pixel(x, y).0;
                let mid = frames[metered].image.get_pixel(x, y).0;
                let long_top = long[0].max(long[1]).max(long[2]);
                let mid_top = mid[0].max(mid[1]).max(mid[2]);
                if long_top >= SATURATION || mid_top >= SATURATION || long_top < 0.02 {
                    continue;
                }
                let bin = ((long_top / SATURATION) * BINS as f32) as usize;
                let bin = bin.min(BINS - 1);
                sum_long[bin] += (luma(&long) / light_longest) as f64;
                sum_metered[bin] += (luma(&mid) / light_metered) as f64;
                seen[bin] += 1;
            }
        }
        for bin in 0..BINS {
            if seen[bin] < 5000 {
                continue;
            }
            let n = seen[bin] as f64;
            let (a, b) = (sum_long[bin] / n, sum_metered[bin] / n);
            eprintln!(
                "{:>7.2}..{:<7.2} {:>12} {:>12.5} {:>12.5} {:>9.1}%",
                bin as f32 * SATURATION / BINS as f32,
                (bin + 1) as f32 * SATURATION / BINS as f32,
                seen[bin],
                a,
                b,
                (b / a - 1.0) * 100.0
            );
        }

        // And how much rougher the picture gets once the long frame has gone.
        // Losing it costs most of the light the estimate was built from, so
        // the noise goes up, and a change of texture across a smooth ceiling
        // reads as an edge even when the tone through it is perfect.
        eprintln!("\n=== how rough the merge is, by level ===");
        eprintln!(
            "  the long frame fades over x {:.3} .. {:.3}",
            ROLL_OFF_START * metered_light / light_longest,
            SATURATION * metered_light / light_longest
        );
        eprintln!(
            "{:>16} {:>12} {:>14} {:>10}",
            "x", "pixels", "rough", "of the light"
        );
        let mut rough = [0.0f64; BINS];
        let mut rough_seen = [0u64; BINS];
        for y in 0..height {
            for x in 1..width {
                let here = luma(&merged.get_pixel(x, y).0);
                let left = luma(&merged.get_pixel(x - 1, y).0);
                if here <= 1e-6 {
                    continue;
                }
                let position = here * metered_light;
                if position >= 0.6 {
                    continue;
                }
                let bin = ((position / 0.6) * BINS as f32) as usize;
                let bin = bin.min(BINS - 1);
                rough[bin] += ((here - left).abs() / here) as f64;
                rough_seen[bin] += 1;
            }
        }
        for bin in 0..BINS {
            if rough_seen[bin] < 5000 {
                continue;
            }
            let from = bin as f32 * 0.6 / BINS as f32;
            let to = (bin + 1) as f32 * 0.6 / BINS as f32;
            // How much of the bracket's light still counts at this level, which
            // is what sets the noise floor.
            let radiance = (from + to) / 2.0 / metered_light;
            let held: f32 = frames
                .iter()
                .map(|frame| {
                    let observed = (radiance * frame.light()).min(1.0);
                    weight_for(&[observed; 3]) * frame.light()
                })
                .sum();
            let all: f32 = frames.iter().map(Frame::light).sum();
            eprintln!(
                "{:>7.3}..{:<7.3} {:>12} {:>14.5} {:>9.0}%",
                from,
                to,
                rough_seen[bin],
                rough[bin] / rough_seen[bin] as f64,
                held / all * 100.0
            );
        }
    }
}
// ========== BLITZRAW END: where the line across a smooth wall comes from ==========
