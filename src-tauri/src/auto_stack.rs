//! Finding exposure brackets in a selection.
//!
//! A stack is several *different* captures treated as one item. That is a
//! distinct idea from the existing `group_id`, which ties together several
//! *files of one capture* (`_DSC1794.NEF` and `_DSC1794.JPG`). The two nest: a
//! three-shot bracket shot RAW+JPEG is three groups inside one stack.
//!
//! Detection keys on the exposure sequence rather than on timing, because
//! timing alone does not work. A bracket's brightest frame can be an eight
//! second exposure followed by roughly as long again of in-camera noise
//! reduction, so frames of one bracket can be half a minute apart while two
//! separate brackets can be shot seconds apart.
//!
//! The camera writes a bracket in a characteristic order: the metered frame
//! first, then the rest ascending from darkest to brightest. So a run is a
//! bracket when its first frame carries the median exposure of the run and
//! every frame after it is brighter than the last. That signature also rules
//! out a burst of identical frames, since those never ascend.
//!
//! Bracket sizes are odd by construction. Even-sized runs are a sign the
//! detection went wrong, not a bracket the camera produced.
//!
//! Bursts are the other half, and they are the opposite problem. A bracket is
//! found by its exposure signature because its timing is unreliable; a burst
//! has no exposure signature at all, so it is found by timing alone. The two
//! detectors share this module's EXIF reading and its result types and share
//! nothing else. See `propose_bursts`.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use exif::{In, Value};
use memmap2::Mmap;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// One frame, reduced to what bracket detection needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub path: String,
    /// Capture time in seconds. Frames without one cannot be stacked.
    pub captured_at: Option<f64>,
    /// Exposure compensation in stops. Without it there is no signature.
    pub exposure_bias: Option<f64>,
    /// Shutter time in seconds, used to size the allowance for the gap that
    /// follows this frame.
    pub shutter_seconds: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoStackParams {
    /// Bracket sizes to look for, tried largest first. Even values are ignored.
    pub sizes: Vec<usize>,
    /// Allowance between consecutive frames before any exposure is accounted for.
    pub base_gap_seconds: f64,
    /// Extra allowance per second of the preceding frame's shutter time. A long
    /// capture is followed by in-camera noise reduction of roughly the same
    /// length, so the budget has to grow with the exposure.
    pub exposure_gap_factor: f64,
    /// How far the first frame's exposure may sit from the run's median.
    pub median_tolerance_ev: f64,
}

impl Default for AutoStackParams {
    fn default() -> Self {
        Self {
            sizes: vec![3, 5, 7],
            // Measured across three real estate shoots: every genuine gap
            // between bracket frames shot at normal shutter speeds was one
            // second or less, while coincidental EV matches across a change of
            // setup were 15 to 135 seconds with the same fast shutter. Three
            // seconds sits well clear of both.
            base_gap_seconds: 3.0,
            // An eight second exposure plus its noise reduction needs about
            // sixteen seconds; this allows twenty three.
            exposure_gap_factor: 2.5,
            median_tolerance_ev: 0.01,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProposedStack {
    pub paths: Vec<String>,
    /// Exposure values in capture order, for showing the user what was matched.
    pub exposure_values: Vec<f64>,
    pub span_seconds: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoStackPreview {
    pub stacks: Vec<ProposedStack>,
    /// Stack size to how many stacks of that size, for the summary line.
    pub size_counts: BTreeMap<usize, usize>,
    /// Frames that are not part of any bracket. Reported so nothing vanishes.
    pub ungrouped: Vec<String>,
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    sorted[sorted.len() / 2]
}

/// How long a pause is acceptable after this frame.
///
/// A frame shot at 1/100 gets the base allowance. An eight second exposure gets
/// far more, because the camera is still busy denoising it.
fn gap_budget(frame: &Frame, params: &AutoStackParams) -> f64 {
    params.base_gap_seconds + params.exposure_gap_factor * frame.shutter_seconds.unwrap_or(0.0)
}

/// Whether a run of frames matches the bracket signature.
fn is_bracket(run: &[Frame], params: &AutoStackParams) -> bool {
    if run.len() < 3 || run.len().is_multiple_of(2) {
        return false;
    }

    let Some(exposures) = run.iter().map(|f| f.exposure_bias).collect::<Option<Vec<_>>>() else {
        return false;
    };

    // The metered frame leads, so its exposure is the middle of the set.
    if (exposures[0] - median(&exposures)).abs() > params.median_tolerance_ev {
        return false;
    }

    // Everything after it climbs, which a burst of equal frames never does.
    if exposures[1..].windows(2).any(|pair| pair[1] <= pair[0]) {
        return false;
    }

    // Each pause has to be explainable by the exposure that preceded it.
    run.windows(2).all(|pair| {
        match (pair[0].captured_at, pair[1].captured_at) {
            (Some(before), Some(after)) => (after - before) <= gap_budget(&pair[0], params),
            _ => false,
        }
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BurstParams {
    /// Longest pause allowed inside one burst.
    pub max_gap_seconds: f64,
    /// Smallest run worth calling a burst.
    pub min_frames: usize,
    /// How far two frames' exposure compensation may differ and still count as
    /// the same burst.
    pub exposure_tolerance_ev: f64,
}

impl Default for BurstParams {
    fn default() -> Self {
        Self {
            // Measured over 1003 frames of a real event shoot and 215 of a
            // real estate shoot; `report_burst_thresholds` sweeps it and prints
            // what each value gives.
            //
            // The data has no sharp answer. The number of bursts found peaks
            // between 0.75 and 1.5 seconds and the peak is flat, 295 then 304
            // then 298, which is inside the noise. Below it real bursts get
            // split and above it separate ones get joined, and neither edge is
            // abrupt. So this follows the photographer's own description of the
            // work, frames less than a second apart, and the slider exists
            // because the measurement could not settle it.
            //
            // What this costs on that shoot: 304 bursts covering 79% of the
            // frames, median size 2, largest 7. Dropping to 0.5 gives 242
            // covering 57%, which is the setting to reach for if it groups more
            // than intended.
            max_gap_seconds: 1.0,
            min_frames: 2,
            // Exposure compensation is a setting the photographer turns, not
            // something that drifts, so anything that moves at all is a
            // different intent. Loose enough only for rounding.
            exposure_tolerance_ev: 0.01,
        }
    }
}

/// Proposes bursts for a selection. Pure: no file access, no side effects.
///
/// A burst is a run of frames shot close enough together to be one attempt at
/// one moment. Nothing here looks at shutter speed, and deliberately so: a
/// burst is event and sports work at 1/200 or faster, so the allowance a
/// bracket needs for in-camera noise reduction has nothing to model.
///
/// Exposure compensation is used, but only as a guard. It is a setting the
/// photographer turns rather than something the camera varies, so a run where
/// it moves is a bracket or a change of intent, not one burst. Without that
/// guard a bracket shot on a fast shutter looks exactly like a burst: on a real
/// shoot the bracket frames were a second or less apart, well inside any
/// threshold worth using here.
pub fn propose_bursts(mut frames: Vec<Frame>, params: &BurstParams) -> AutoStackPreview {
    frames.sort_by(|a, b| {
        a.captured_at
            .unwrap_or(f64::MAX)
            .partial_cmp(&b.captured_at.unwrap_or(f64::MAX))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
    });

    let min_frames = params.min_frames.max(2);
    let same_burst = |before: &Frame, after: &Frame| -> bool {
        let (Some(a), Some(b)) = (before.captured_at, after.captured_at) else {
            return false;
        };
        if b - a > params.max_gap_seconds {
            return false;
        }
        match (before.exposure_bias, after.exposure_bias) {
            (Some(x), Some(y)) => (x - y).abs() <= params.exposure_tolerance_ev,
            // A file that never recorded one cannot contradict the next.
            _ => true,
        }
    };

    let mut stacks: Vec<ProposedStack> = Vec::new();
    let mut ungrouped: Vec<String> = Vec::new();

    let mut index = 0;
    while index < frames.len() {
        let mut end = index + 1;
        while end < frames.len() && same_burst(&frames[end - 1], &frames[end]) {
            end += 1;
        }

        if end - index >= min_frames {
            let run = &frames[index..end];
            stacks.push(ProposedStack {
                paths: run.iter().map(|f| f.path.clone()).collect(),
                exposure_values: run.iter().filter_map(|f| f.exposure_bias).collect(),
                span_seconds: run
                    .last()
                    .and_then(|f| f.captured_at)
                    .zip(run.first().and_then(|f| f.captured_at))
                    .map(|(last, first)| last - first)
                    .unwrap_or(0.0),
            });
        } else {
            ungrouped.extend(frames[index..end].iter().map(|f| f.path.clone()));
        }
        index = end;
    }

    let mut size_counts: BTreeMap<usize, usize> = BTreeMap::new();
    for stack in &stacks {
        *size_counts.entry(stack.paths.len()).or_default() += 1;
    }

    AutoStackPreview {
        stacks,
        size_counts,
        ungrouped,
    }
}

/// Proposes brackets for a selection. Pure: no file access, no side effects.
pub fn propose_stacks(mut frames: Vec<Frame>, params: &AutoStackParams) -> AutoStackPreview {
    frames.sort_by(|a, b| {
        a.captured_at
            .unwrap_or(f64::MAX)
            .partial_cmp(&b.captured_at.unwrap_or(f64::MAX))
            .unwrap_or(std::cmp::Ordering::Equal)
            // Brackets often share a whole-second timestamp, so fall back to
            // the path to keep results stable between runs.
            .then_with(|| a.path.cmp(&b.path))
    });

    let mut sizes: Vec<usize> = params
        .sizes
        .iter()
        .copied()
        .filter(|n| *n >= 3 && !n.is_multiple_of(2))
        .collect();
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    sizes.dedup();

    let mut stacks: Vec<ProposedStack> = Vec::new();
    let mut ungrouped: Vec<String> = Vec::new();

    let mut index = 0;
    while index < frames.len() {
        // Largest first: the leading frames of a five-shot bracket do not match
        // the three-shot signature, but trying big to small keeps intent clear.
        let matched = sizes
            .iter()
            .find(|size| index + **size <= frames.len() && is_bracket(&frames[index..index + **size], params));

        match matched {
            Some(&size) => {
                let run = &frames[index..index + size];
                stacks.push(ProposedStack {
                    paths: run.iter().map(|f| f.path.clone()).collect(),
                    exposure_values: run.iter().filter_map(|f| f.exposure_bias).collect(),
                    span_seconds: run
                        .last()
                        .and_then(|f| f.captured_at)
                        .zip(run.first().and_then(|f| f.captured_at))
                        .map(|(end, start)| end - start)
                        .unwrap_or(0.0),
                });
                index += size;
            }
            None => {
                ungrouped.push(frames[index].path.clone());
                index += 1;
            }
        }
    }

    let mut size_counts: BTreeMap<usize, usize> = BTreeMap::new();
    for stack in &stacks {
        *size_counts.entry(stack.paths.len()).or_default() += 1;
    }

    AutoStackPreview {
        stacks,
        size_counts,
        ungrouped,
    }
}

// --- reading the three values a frame needs -----------------------------------

fn first_rational(field: &exif::Field) -> Option<f64> {
    match &field.value {
        Value::Rational(v) => v.first().map(|r| r.num as f64 / r.denom as f64),
        Value::SRational(v) => v.first().map(|r| r.num as f64 / r.denom as f64),
        _ => None,
    }
}

/// Converts an Exif timestamp to seconds, keeping sub-second precision when the
/// camera recorded it. Brackets routinely share a whole second, so that extra
/// digit is what keeps frames in the order they were shot.
fn parse_capture_time(exif: &exif::Exif) -> Option<f64> {
    let field = exif
        .get_field(exif::Tag::DateTimeOriginal, In::PRIMARY)
        .or_else(|| exif.get_field(exif::Tag::DateTimeDigitized, In::PRIMARY))?;

    let Value::Ascii(ref parts) = field.value else {
        return None;
    };
    let text = std::str::from_utf8(parts.first()?).ok()?;
    let parsed = exif::DateTime::from_ascii(text.as_bytes()).ok()?;

    let date = chrono::NaiveDate::from_ymd_opt(
        parsed.year as i32,
        parsed.month as u32,
        parsed.day as u32,
    )?;
    let time = chrono::NaiveTime::from_hms_opt(
        parsed.hour as u32,
        parsed.minute as u32,
        parsed.second as u32,
    )?;
    let mut seconds = date.and_time(time).and_utc().timestamp() as f64;

    if let Some(sub) = exif.get_field(exif::Tag::SubSecTimeOriginal, In::PRIMARY)
        && let Value::Ascii(ref parts) = sub.value
        && let Some(text) = parts.first().and_then(|b| std::str::from_utf8(b).ok())
        && let Ok(fraction) = format!("0.{}", text.trim()).parse::<f64>()
    {
        seconds += fraction;
    }

    Some(seconds)
}

/// Reads one frame's stacking inputs without decoding any image data.
fn read_frame(path: &str) -> Frame {
    let mut frame = Frame {
        path: path.to_string(),
        captured_at: None,
        exposure_bias: None,
        shutter_seconds: None,
    };

    let Ok(file) = File::open(Path::new(path)) else {
        return frame;
    };
    // SAFETY: the user's own files, not written by anything else during a scan,
    // and the exif reader below only reads.
    let Ok(map) = (unsafe { Mmap::map(&file) }) else {
        return frame;
    };

    let mut cursor = std::io::Cursor::new(&map[..]);
    let Ok(exif) = exif::Reader::new().read_from_container(&mut cursor) else {
        return frame;
    };

    frame.captured_at = parse_capture_time(&exif);
    frame.exposure_bias = exif
        .get_field(exif::Tag::ExposureBiasValue, In::PRIMARY)
        .and_then(first_rational);
    frame.shutter_seconds = exif
        .get_field(exif::Tag::ExposureTime, In::PRIMARY)
        .and_then(first_rational);

    frame
}

/// Proposes brackets for a set of paths. Reads metadata only; writes nothing.
/// The frames a detector should consider, read from disk.
///
/// Shared by both detectors because the two exclusions are about the selection
/// rather than about what is being looked for.
fn frames_to_consider(paths: Vec<String>) -> Vec<Frame> {
    // Virtual copies share a file on disk, so collapse to real paths first.
    let mut sources: Vec<String> = paths
        .into_iter()
        .map(|p| p.split("?vc=").next().unwrap_or(&p).to_string())
        .collect();
    sources.sort();
    sources.dedup();

    // A file already in a stack is left where it is. Detection would otherwise
    // propose stacks that cut across ones the photographer confirmed, and
    // re-running it over a folder would keep offering to redo work already
    // done. Skipping them makes a second run over the same folder a no-op.
    let already_stacked = crate::stacks::stacked_file_names(&sources);
    if !already_stacked.is_empty() {
        let before = sources.len();
        sources.retain(|p| !already_stacked.contains(p));
        log::info!(
            "Auto-stack skipping {} file(s) that already belong to a stack",
            before - sources.len()
        );
    }

    sources.par_iter().map(|p| read_frame(p)).collect()
}

/// Proposes brackets for a set of paths. Reads metadata only; writes nothing.
#[tauri::command]
pub fn preview_auto_stacks(paths: Vec<String>, params: AutoStackParams) -> AutoStackPreview {
    propose_stacks(frames_to_consider(paths), &params)
}

/// Proposes bursts for a set of paths. Reads metadata only; writes nothing.
#[tauri::command]
pub fn preview_burst_stacks(paths: Vec<String>, params: BurstParams) -> AutoStackPreview {
    propose_bursts(frames_to_consider(paths), &params)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a frame with a fast shutter, the common case.
    fn f(path: &str, at: f64, ev: f64) -> Frame {
        Frame {
            path: path.to_string(),
            captured_at: Some(at),
            exposure_bias: Some(ev),
            shutter_seconds: Some(0.01),
        }
    }

    fn paths(stack: &ProposedStack) -> Vec<&str> {
        stack.paths.iter().map(|p| p.as_str()).collect()
    }

    #[test]
    fn finds_a_three_shot_bracket_in_camera_order() {
        // Median first, then dark to bright, exactly as the camera writes it.
        let frames = vec![f("a", 0.0, 0.33), f("b", 1.0, -1.67), f("c", 2.0, 2.33)];
        let preview = propose_stacks(frames, &AutoStackParams::default());

        assert_eq!(preview.stacks.len(), 1);
        assert_eq!(paths(&preview.stacks[0]), vec!["a", "b", "c"]);
    }

    #[test]
    fn finds_a_five_shot_bracket_with_a_non_zero_median() {
        let frames = vec![
            f("a", 0.0, 1.0),
            f("b", 1.0, -3.0),
            f("c", 2.0, -1.0),
            f("d", 3.0, 3.0),
            f("e", 4.0, 5.0),
        ];
        let preview = propose_stacks(frames, &AutoStackParams::default());

        assert_eq!(preview.stacks.len(), 1);
        assert_eq!(preview.stacks[0].paths.len(), 5);
        assert!(preview.ungrouped.is_empty());
    }

    #[test]
    fn never_proposes_an_even_sized_stack() {
        // Four frames that look bracket-ish. Real cameras do not shoot fours.
        let frames = vec![
            f("a", 0.0, 0.0),
            f("b", 1.0, -2.0),
            f("c", 2.0, -1.0),
            f("d", 3.0, 2.0),
        ];
        let preview = propose_stacks(frames, &AutoStackParams::default());

        assert!(preview.stacks.iter().all(|s| !s.paths.len().is_multiple_of(2)));
    }

    #[test]
    fn a_five_shot_bracket_is_not_mistaken_for_a_three_shot_one() {
        // The leading three frames of a five are [median, darkest, dark], whose
        // median is not the first frame, so the three-shot check must reject it.
        let run = vec![f("a", 0.0, 0.0), f("b", 1.0, -4.0), f("c", 2.0, -2.0)];
        assert!(!is_bracket(&run, &AutoStackParams::default()));
    }

    #[test]
    fn a_burst_of_identical_exposures_is_not_a_bracket() {
        let frames = vec![f("a", 0.0, 0.0), f("b", 0.3, 0.0), f("c", 0.6, 0.0)];
        let preview = propose_stacks(frames, &AutoStackParams::default());

        assert!(preview.stacks.is_empty());
        assert_eq!(preview.ungrouped.len(), 3);
    }

    #[test]
    fn a_long_pause_between_setups_breaks_a_coincidental_match() {
        // The exposures form a valid signature, but 60s at 1/100 is a new setup.
        let frames = vec![f("a", 0.0, 0.0), f("b", 1.0, -2.0), f("c", 61.0, 2.0)];
        let preview = propose_stacks(frames, &AutoStackParams::default());

        assert!(preview.stacks.is_empty());
        assert_eq!(preview.ungrouped.len(), 3);
    }

    #[test]
    fn a_long_exposure_earns_a_longer_pause() {
        // An eight second capture plus its noise reduction takes about sixteen.
        let mut frames = vec![f("a", 0.0, 0.0), f("b", 1.0, -2.0), f("c", 18.0, 2.0)];
        frames[1].shutter_seconds = Some(8.0);

        let preview = propose_stacks(frames, &AutoStackParams::default());
        assert_eq!(preview.stacks.len(), 1);
    }

    #[test]
    fn two_consecutive_brackets_are_kept_apart() {
        let frames = vec![
            f("a1", 0.0, 0.0),
            f("a2", 1.0, -2.0),
            f("a3", 2.0, 2.0),
            f("b1", 30.0, 0.0),
            f("b2", 31.0, -2.0),
            f("b3", 32.0, 2.0),
        ];
        let preview = propose_stacks(frames, &AutoStackParams::default());

        assert_eq!(preview.stacks.len(), 2);
        assert_eq!(preview.size_counts.get(&3), Some(&2));
    }

    #[test]
    fn a_stray_frame_between_brackets_is_left_alone() {
        let frames = vec![
            f("single", 0.0, 0.0),
            f("a1", 30.0, 0.0),
            f("a2", 31.0, -2.0),
            f("a3", 32.0, 2.0),
        ];
        let preview = propose_stacks(frames, &AutoStackParams::default());

        assert_eq!(preview.stacks.len(), 1);
        assert_eq!(preview.ungrouped, vec!["single"]);
    }

    #[test]
    fn frames_without_an_exposure_reading_cannot_be_bracketed() {
        let mut frames = vec![f("a", 0.0, 0.0), f("b", 1.0, -2.0), f("c", 2.0, 2.0)];
        frames[1].exposure_bias = None;

        let preview = propose_stacks(frames, &AutoStackParams::default());
        assert!(preview.stacks.is_empty());
        assert_eq!(preview.ungrouped.len(), 3);
    }

    #[test]
    fn frames_without_a_capture_time_cannot_be_bracketed() {
        let mut frames = vec![f("a", 0.0, 0.0), f("b", 1.0, -2.0), f("c", 2.0, 2.0)];
        frames[1].captured_at = None;

        let preview = propose_stacks(frames, &AutoStackParams::default());
        assert!(preview.stacks.is_empty());
    }

    #[test]
    fn input_order_does_not_change_the_result() {
        let ordered = vec![f("a", 0.0, 0.0), f("b", 1.0, -2.0), f("c", 2.0, 2.0)];
        let shuffled = vec![f("c", 2.0, 2.0), f("a", 0.0, 0.0), f("b", 1.0, -2.0)];

        let params = AutoStackParams::default();
        assert_eq!(
            propose_stacks(ordered, &params).stacks,
            propose_stacks(shuffled, &params).stacks
        );
    }

    #[test]
    fn restricting_sizes_to_five_ignores_three_shot_brackets() {
        let frames = vec![f("a", 0.0, 0.0), f("b", 1.0, -2.0), f("c", 2.0, 2.0)];
        let params = AutoStackParams {
            sizes: vec![5],
            ..Default::default()
        };

        assert!(propose_stacks(frames, &params).stacks.is_empty());
    }

    #[test]
    fn even_sizes_requested_by_a_caller_are_ignored() {
        let frames = vec![f("a", 0.0, 0.0), f("b", 1.0, -2.0), f("c", 2.0, 2.0)];
        let params = AutoStackParams {
            sizes: vec![4, 3],
            ..Default::default()
        };

        let preview = propose_stacks(frames, &params);
        assert_eq!(preview.stacks.len(), 1);
        assert_eq!(preview.stacks[0].paths.len(), 3);
    }

    #[test]
    fn an_empty_selection_yields_an_empty_proposal() {
        let preview = propose_stacks(Vec::new(), &AutoStackParams::default());
        assert!(preview.stacks.is_empty());
        assert!(preview.ungrouped.is_empty());
    }

    fn burst_frame(path: &str, at: f64, bias: f64) -> Frame {
        Frame {
            path: path.to_string(),
            captured_at: Some(at),
            exposure_bias: Some(bias),
            // Deliberately slow, to prove burst detection never looks at it.
            shutter_seconds: Some(8.0),
        }
    }

    fn burst_paths(preview: &AutoStackPreview) -> Vec<Vec<String>> {
        preview.stacks.iter().map(|s| s.paths.clone()).collect()
    }

    #[test]
    fn frames_inside_the_gap_are_one_burst_and_a_pause_ends_it() {
        let frames = vec![
            burst_frame("a", 0.0, 0.0),
            burst_frame("b", 0.3, 0.0),
            burst_frame("c", 0.6, 0.0),
            // Four seconds later: a different moment.
            burst_frame("d", 4.6, 0.0),
            burst_frame("e", 4.9, 0.0),
        ];
        let preview = propose_bursts(frames, &BurstParams { max_gap_seconds: 1.0, ..Default::default() });
        assert_eq!(burst_paths(&preview), vec![vec!["a", "b", "c"], vec!["d", "e"]]);
        assert!(preview.ungrouped.is_empty());
    }

    #[test]
    fn a_slow_shutter_buys_no_extra_time() {
        // The bracket detector would allow this gap, because it grows the
        // budget by the exposure that preceded it. A burst is fast glass by
        // definition, so nothing here does that.
        let frames = vec![burst_frame("a", 0.0, 0.0), burst_frame("b", 3.0, 0.0)];
        let preview = propose_bursts(frames, &BurstParams { max_gap_seconds: 1.0, ..Default::default() });
        assert!(preview.stacks.is_empty(), "three seconds is not one burst");
        assert_eq!(preview.ungrouped, vec!["a", "b"]);
    }

    #[test]
    fn a_bracket_shot_fast_is_not_taken_for_a_burst() {
        // Three frames a third of a second apart, well inside any threshold
        // worth using, but the exposure compensation moves. This is the case the
        // guard came from: on one real shoot it turned 12 proposed bursts into 9.
        let frames = vec![
            burst_frame("mid", 0.0, 0.0),
            burst_frame("dark", 0.33, -2.0),
            burst_frame("bright", 0.66, 2.0),
        ];
        let preview = propose_bursts(frames, &BurstParams { max_gap_seconds: 1.0, ..Default::default() });
        assert!(preview.stacks.is_empty(), "a bracket is not a burst");
        assert_eq!(preview.ungrouped.len(), 3);
    }

    #[test]
    fn a_frame_with_no_exposure_compensation_does_not_break_a_burst() {
        let mut frames = vec![
            burst_frame("a", 0.0, 0.0),
            burst_frame("b", 0.3, 0.0),
            burst_frame("c", 0.6, 0.0),
        ];
        frames[1].exposure_bias = None;
        let preview = propose_bursts(frames, &BurstParams { max_gap_seconds: 1.0, ..Default::default() });
        assert_eq!(burst_paths(&preview), vec![vec!["a", "b", "c"]]);
    }

    #[test]
    fn a_frame_with_no_capture_time_is_never_grouped() {
        let mut frames = vec![burst_frame("a", 0.0, 0.0), burst_frame("b", 0.3, 0.0)];
        frames[1].captured_at = None;
        let preview = propose_bursts(frames, &BurstParams { max_gap_seconds: 1.0, ..Default::default() });
        assert!(preview.stacks.is_empty());
        assert_eq!(preview.ungrouped.len(), 2);
    }

    #[test]
    fn a_run_shorter_than_the_minimum_is_left_alone() {
        let frames = vec![
            burst_frame("a", 0.0, 0.0),
            burst_frame("b", 0.3, 0.0),
            burst_frame("c", 10.0, 0.0),
            burst_frame("d", 10.3, 0.0),
            burst_frame("e", 10.6, 0.0),
        ];
        let preview = propose_bursts(
            frames,
            &BurstParams { max_gap_seconds: 1.0, min_frames: 3, ..Default::default() },
        );
        assert_eq!(burst_paths(&preview), vec![vec!["c", "d", "e"]]);
        assert_eq!(preview.ungrouped, vec!["a", "b"]);
    }

    #[test]
    fn nothing_is_invented_or_lost_whatever_order_it_arrives_in() {
        let frames = vec![
            burst_frame("a", 0.0, 0.0),
            burst_frame("b", 0.3, 0.0),
            burst_frame("c", 9.0, 0.0),
            burst_frame("d", 9.2, 0.0),
            burst_frame("e", 9.4, 0.0),
            burst_frame("f", 40.0, 0.0),
        ];
        let params = BurstParams { max_gap_seconds: 1.0, ..Default::default() };
        let forwards = propose_bursts(frames.clone(), &params);

        let mut backwards_input = frames.clone();
        backwards_input.reverse();
        let backwards = propose_bursts(backwards_input, &params);
        assert_eq!(
            burst_paths(&forwards),
            burst_paths(&backwards),
            "the order the paths arrive in must not change the answer"
        );

        let mut seen: Vec<String> = forwards
            .stacks
            .iter()
            .flat_map(|s| s.paths.clone())
            .chain(forwards.ungrouped.clone())
            .collect();
        seen.sort();
        assert_eq!(seen, vec!["a", "b", "c", "d", "e", "f"]);
    }

    #[test]
    fn a_burst_reports_how_long_it_took() {
        let frames = vec![
            burst_frame("a", 100.0, 0.0),
            burst_frame("b", 100.4, 0.0),
            burst_frame("c", 100.9, 0.0),
        ];
        let preview = propose_bursts(frames, &BurstParams { max_gap_seconds: 1.0, ..Default::default() });
        assert_eq!(preview.stacks.len(), 1);
        assert!((preview.stacks[0].span_seconds - 0.9).abs() < 1e-6);
        assert_eq!(preview.size_counts.get(&3), Some(&1));
    }

    fn nef_paths_under(dir: &str) -> Vec<String> {
        walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x.eq_ignore_ascii_case("nef"))
                    .unwrap_or(false)
            })
            .map(|e| e.path().to_string_lossy().into_owned())
            .collect()
    }

    /// What the gaps between frames actually look like on a real shoot.
    ///
    /// A report, not an assertion. Burst detection has to key on timing alone,
    /// so before choosing any threshold this asks two questions of real files:
    /// whether the camera records a usable fraction of a second, and where the
    /// gaps inside a burst sit against the gaps between separate shots.
    #[test]
    fn report_frame_timing() {
        let Ok(dir) = std::env::var("RAPIDRAW_TEST_NEF_DIR") else {
            eprintln!("RAPIDRAW_TEST_NEF_DIR unset, skipping");
            return;
        };
        let paths = nef_paths_under(&dir);
        if paths.is_empty() {
            eprintln!("no NEF files under {dir}, skipping");
            return;
        }

        let mut frames: Vec<Frame> = paths.par_iter().map(|p| read_frame(p)).collect();
        frames.sort_by(|a, b| {
            a.captured_at
                .unwrap_or(f64::MAX)
                .partial_cmp(&b.captured_at.unwrap_or(f64::MAX))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.path.cmp(&b.path))
        });

        let timed = frames.iter().filter(|f| f.captured_at.is_some()).count();
        // A whole-second timestamp is one with nothing after the decimal point.
        // If most frames look like that the camera is not recording sub-seconds
        // and no threshold under a second can mean anything.
        let sub_second = frames
            .iter()
            .filter_map(|f| f.captured_at)
            .filter(|t| (t.fract()).abs() > 1e-9)
            .count();
        eprintln!(
            "{} frames under {dir}: {timed} with a capture time, {sub_second} of those carrying a fraction of a second",
            frames.len()
        );

        let gaps: Vec<f64> = frames
            .windows(2)
            .filter_map(|pair| match (pair[0].captured_at, pair[1].captured_at) {
                (Some(a), Some(b)) => Some(b - a),
                _ => None,
            })
            .collect();
        if gaps.is_empty() {
            eprintln!("no gaps to report");
            return;
        }

        let mut sorted = gaps.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q) as usize];
        eprintln!(
            "gaps between consecutive frames, {} of them: min {:.3}s  p10 {:.3}  p25 {:.3}  median {:.3}  p75 {:.3}  p90 {:.3}  max {:.1}",
            sorted.len(),
            sorted[0],
            at(0.10),
            at(0.25),
            at(0.50),
            at(0.75),
            at(0.90),
            sorted[sorted.len() - 1]
        );

        // Where the gaps pile up is what a threshold has to sit between.
        let buckets: [(f64, f64); 9] = [
            (0.0, 0.10),
            (0.10, 0.20),
            (0.20, 0.35),
            (0.35, 0.50),
            (0.50, 1.0),
            (1.0, 2.0),
            (2.0, 5.0),
            (5.0, 30.0),
            (30.0, f64::INFINITY),
        ];
        eprintln!("how they are spread:");
        for (low, high) in buckets {
            let count = sorted.iter().filter(|g| **g >= low && **g < high).count();
            if count == 0 {
                continue;
            }
            eprintln!(
                "  {low:>5.2} to {high:>6.2}s : {count:>5}  ({:.1}%)",
                100.0 * count as f64 / sorted.len() as f64
            );
        }

        // Shutter speeds, to check the claim that a burst is always fast glass.
        let mut shutters: Vec<f64> = frames.iter().filter_map(|f| f.shutter_seconds).collect();
        shutters.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        if !shutters.is_empty() {
            let sat = |q: f64| shutters[((shutters.len() - 1) as f64 * q) as usize];
            eprintln!(
                "shutter: median 1/{:.0}  p90 1/{:.0}  slowest {:.2}s",
                1.0 / sat(0.5).max(1e-9),
                1.0 / sat(0.9).max(1e-9),
                shutters[shutters.len() - 1]
            );
        }
    }

    /// What each burst threshold actually produces on a real shoot.
    ///
    /// A report, not an assertion, and the thing that sets the default. The gap
    /// histogram in `report_frame_timing` has no clean valley to put a
    /// threshold in, so the question has to be asked the other way round: for
    /// each candidate, how many bursts, how big, and how much of the shoot ends
    /// up swept into one.
    #[test]
    fn report_burst_thresholds() {
        let Ok(dir) = std::env::var("RAPIDRAW_TEST_NEF_DIR") else {
            eprintln!("RAPIDRAW_TEST_NEF_DIR unset, skipping");
            return;
        };
        let paths = nef_paths_under(&dir);
        if paths.is_empty() {
            eprintln!("no NEF files under {dir}, skipping");
            return;
        }
        let frames: Vec<Frame> = paths.par_iter().map(|p| read_frame(p)).collect();
        eprintln!("{} frames under {dir}\n", frames.len());
        eprintln!(
            "{:>6}  {:>7}  {:>7}  {:>8}  {:>8}  {:>7}",
            "gap", "bursts", "framed", "of shoot", "biggest", "median"
        );

        for gap in [0.25f64, 0.35, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0] {
            let preview = propose_bursts(
                frames.clone(),
                &BurstParams {
                    max_gap_seconds: gap,
                    ..Default::default()
                },
            );
            let framed: usize = preview.stacks.iter().map(|s| s.paths.len()).sum();
            let mut sizes: Vec<usize> = preview.stacks.iter().map(|s| s.paths.len()).collect();
            sizes.sort_unstable();
            eprintln!(
                "{gap:>6.2}  {:>7}  {:>7}  {:>7.0}%  {:>8}  {:>7}",
                preview.stacks.len(),
                framed,
                100.0 * framed as f64 / frames.len() as f64,
                sizes.last().copied().unwrap_or(0),
                sizes.get(sizes.len() / 2).copied().unwrap_or(0),
            );
        }

        // The guard, measured rather than asserted: how much the run would grow
        // if exposure compensation were ignored. On an event shoot this should
        // be nothing, because nobody is bracketing.
        let loose = propose_bursts(
            frames.clone(),
            &BurstParams {
                max_gap_seconds: 0.5,
                exposure_tolerance_ev: f64::INFINITY,
                ..Default::default()
            },
        );
        let strict = propose_bursts(frames, &BurstParams { max_gap_seconds: 0.5, ..Default::default() });
        eprintln!(
            "\nat 0.50s, ignoring exposure compensation would give {} bursts instead of {}",
            loose.stacks.len(),
            strict.stacks.len()
        );
    }

    /// Runs the whole path, EXIF reading included, over a folder of real camera
    /// files. Set `RAPIDRAW_TEST_NEF_DIR` to a shoot to check that detection
    /// still agrees with what the camera actually shot.
    #[test]
    fn detects_brackets_in_a_real_folder_when_one_is_provided() {
        let Ok(dir) = std::env::var("RAPIDRAW_TEST_NEF_DIR") else {
            eprintln!("RAPIDRAW_TEST_NEF_DIR unset, skipping");
            return;
        };

        let paths = nef_paths_under(&dir);

        if paths.is_empty() {
            eprintln!("no NEF files under {dir}, skipping");
            return;
        }

        let preview = preview_auto_stacks(paths.clone(), AutoStackParams::default());

        eprintln!("{} frames under {dir}", paths.len());
        for (size, count) in &preview.size_counts {
            eprintln!("  {count:>4} x {size}-frame");
        }
        eprintln!("  {:>4} ungrouped", preview.ungrouped.len());

        // Every bracket a camera shoots has an odd number of frames.
        for stack in &preview.stacks {
            assert!(
                !stack.paths.len().is_multiple_of(2),
                "even-sized stack of {} found: {:?}",
                stack.paths.len(),
                stack.paths
            );
        }

        // Nothing may be invented or lost.
        let accounted: usize =
            preview.stacks.iter().map(|s| s.paths.len()).sum::<usize>() + preview.ungrouped.len();
        assert_eq!(accounted, paths.len(), "frames went missing or were duplicated");
    }
}
