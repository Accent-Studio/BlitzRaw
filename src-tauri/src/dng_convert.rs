//! Converting undecodable RAW files to DNG via Adobe's free converter.
//!
//! Nikon's High Efficiency modes cannot be decoded by any open source library.
//! Adobe licenses the codec, so its DNG Converter can read those files and
//! write a DNG that `rawler` handles normally. Running it turns an
//! undevelopable NEF into a working negative without involving Lightroom.
//!
//! Originals are never touched. The converter writes a new `.dng` beside the
//! source and both files stay on disk; `format_precedence` then shows the DNG
//! and hides the NEF, so the library looks like the file was replaced while
//! nothing was actually lost.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

/// Files handed to one converter invocation. Adobe's converter spends close to
/// a second initialising, so a process per file would pay that a thousand
/// times over; batching amortises it while keeping progress reporting useful.
const BATCH_SIZE: usize = 8;

// Conversion deliberately runs one converter process at a time. Measured on a
// 10-core machine over 45 MP HE★ NEFs, eight concurrent processes returned only
// about 1.4x because Adobe's converter is already internally threaded, and they
// began contending over scratch files. That is a poor trade for a background
// job that should leave the machine usable while it runs.

/// A DNG smaller than this is a stub or a failed write, never a real negative.
const MIN_PLAUSIBLE_DNG_BYTES: u64 = 256 * 1024;

#[cfg(target_os = "windows")]
const CONVERTER_CANDIDATES: &[&str] = &[
    r"C:\Program Files\Adobe\Adobe DNG Converter\Adobe DNG Converter.exe",
    r"C:\Program Files (x86)\Adobe\Adobe DNG Converter\Adobe DNG Converter.exe",
];

#[cfg(target_os = "macos")]
const CONVERTER_CANDIDATES: &[&str] =
    &["/Applications/Adobe DNG Converter.app/Contents/MacOS/Adobe DNG Converter"];

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const CONVERTER_CANDIDATES: &[&str] = &[];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConverterInfo {
    pub available: bool,
    pub path: Option<String>,
    /// Where to get it, surfaced in the UI when it is missing.
    pub download_url: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConversionOutcome {
    Converted,
    /// A DNG of that name was already there and `overwrite` was not requested.
    Skipped,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionResult {
    pub source: String,
    pub output: Option<String>,
    pub outcome: ConversionOutcome,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionProgress {
    pub completed: usize,
    pub total: usize,
    pub current: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionSummary {
    pub converted: usize,
    pub skipped: usize,
    pub failed: usize,
    pub results: Vec<ConversionResult>,
}

fn locate_converter() -> Option<PathBuf> {
    CONVERTER_CANDIDATES
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

/// Where the converter will place its output for a given source file.
///
/// Built by appending rather than `with_extension`, which would eat the last
/// dot-segment of a stem like `shoot.02.raw` and look for the wrong file.
fn expected_output(source: &Path) -> Option<PathBuf> {
    let parent = source.parent()?;
    let mut name = source.file_stem()?.to_os_string();
    name.push(".dng");
    Some(parent.join(name))
}

/// Runs one converter invocation and judges each output by what landed on disk.
///
/// Files in a batch normally share a directory, but they are grouped by actual
/// parent so a selection spanning folders still writes beside each source.
fn run_batch(
    converter: &Path,
    items: &[(PathBuf, PathBuf)],
    overwrite: bool,
) -> Vec<ConversionResult> {
    let mut by_parent: HashMap<PathBuf, Vec<&(PathBuf, PathBuf)>> = HashMap::new();
    for item in items {
        let parent = item.0.parent().unwrap_or(Path::new(".")).to_path_buf();
        by_parent.entry(parent).or_default().push(item);
    }

    let mut results = Vec::with_capacity(items.len());

    for (parent, group) in by_parent {
        // The converter never overwrites: given an existing `x.dng` it silently
        // writes `x_1.dng` instead. Left alone that would litter the folder with
        // numbered duplicates, so clear the way first by recycling the old file.
        // Nothing is permanently deleted.
        if overwrite {
            for (_, output) in &group {
                if output.is_file() {
                    let _ = trash::delete(output);
                }
            }
        }

        let mut command = Command::new(converter);
        command
            // Lossless compression, and a full-size JPEG preview so the library
            // stays as fast to browse as the original NEF.
            .arg("-c")
            .arg("-p2")
            .arg("-d")
            .arg(&parent);

        for (source, _) in &group {
            command.arg(source);
        }

        #[cfg(target_os = "windows")]
        {
            // Keep the converter's console window from flashing up per batch.
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let run = command.output();

        for (source, output) in group {
            let source_str = source.to_string_lossy().into_owned();

            results.push(match &run {
                Err(e) => ConversionResult {
                    source: source_str,
                    output: None,
                    outcome: ConversionOutcome::Failed,
                    message: Some(format!("Could not start Adobe DNG Converter: {e}")),
                },
                // Trust the file on disk rather than the exit code; the converter
                // reports success for a batch even when one file produced nothing.
                Ok(_) => match std::fs::metadata(output) {
                    Ok(meta) if meta.len() >= MIN_PLAUSIBLE_DNG_BYTES => ConversionResult {
                        source: source_str,
                        output: Some(output.to_string_lossy().into_owned()),
                        outcome: ConversionOutcome::Converted,
                        message: None,
                    },
                    Ok(meta) => ConversionResult {
                        source: source_str,
                        output: None,
                        outcome: ConversionOutcome::Failed,
                        message: Some(format!(
                            "Output was only {} bytes, treating as a failed write",
                            meta.len()
                        )),
                    },
                    Err(_) => ConversionResult {
                        source: source_str,
                        output: None,
                        outcome: ConversionOutcome::Failed,
                        message: Some("Converter produced no output for this file".into()),
                    },
                },
            });
        }
    }

    results
}

#[tauri::command]
pub fn find_dng_converter() -> ConverterInfo {
    let path = locate_converter();
    ConverterInfo {
        available: path.is_some(),
        path: path.map(|p| p.to_string_lossy().into_owned()),
        download_url: "https://helpx.adobe.com/camera-raw/using/adobe-dng-converter.html",
    }
}

/// Converts RAW files to DNG in place beside each source.
///
/// Sources are never modified or removed. Existing DNGs are left alone unless
/// `overwrite` is set. Each output is checked for plausible size before being
/// reported as converted, so a silent converter failure is recorded as `Failed`
/// rather than passing as success.
#[tauri::command]
pub async fn convert_to_dng(
    paths: Vec<String>,
    overwrite: bool,
    app_handle: AppHandle,
) -> Result<ConversionSummary, String> {
    let Some(converter) = locate_converter() else {
        return Err(
            "Adobe DNG Converter was not found. Install it, then try again.".to_string(),
        );
    };

    let total = paths.len();
    let mut results: Vec<ConversionResult> = Vec::with_capacity(total);
    let mut pending: Vec<(PathBuf, PathBuf)> = Vec::new();

    // Decide up front what actually needs work, so the skip count is accurate
    // even if the converter never runs.
    for raw in paths {
        let source = PathBuf::from(&raw);

        let Some(output) = expected_output(&source) else {
            results.push(ConversionResult {
                source: raw,
                output: None,
                outcome: ConversionOutcome::Failed,
                message: Some("Could not derive an output path".into()),
            });
            continue;
        };

        if !source.is_file() {
            results.push(ConversionResult {
                source: raw,
                output: None,
                outcome: ConversionOutcome::Failed,
                message: Some("Source file no longer exists".into()),
            });
            continue;
        }

        if output.is_file() && !overwrite {
            results.push(ConversionResult {
                source: raw,
                output: Some(output.to_string_lossy().into_owned()),
                outcome: ConversionOutcome::Skipped,
                message: Some("A DNG of this name already exists".into()),
            });
            continue;
        }

        pending.push((source, output));
    }

    // One batch at a time, so results reach the UI steadily instead of arriving
    // all at once when the whole selection finishes.
    for batch in pending.chunks(BATCH_SIZE) {
        let label = batch
            .first()
            .map(|(source, _)| source.to_string_lossy().into_owned())
            .unwrap_or_default();

        let produced = run_batch(&converter, batch, overwrite);
        results.extend(produced);

        let _ = app_handle.emit(
            "dng-conversion-progress",
            ConversionProgress {
                completed: results.len(),
                total,
                current: label,
            },
        );
    }

    let _ = app_handle.emit(
        "dng-conversion-progress",
        ConversionProgress {
            completed: total,
            total,
            current: String::new(),
        },
    );

    Ok(ConversionSummary {
        converted: results
            .iter()
            .filter(|r| r.outcome == ConversionOutcome::Converted)
            .count(),
        skipped: results
            .iter()
            .filter(|r| r.outcome == ConversionOutcome::Skipped)
            .count(),
        failed: results
            .iter()
            .filter(|r| r.outcome == ConversionOutcome::Failed)
            .count(),
        results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_lands_beside_the_source_with_a_dng_extension() {
        let out = expected_output(Path::new(r"C:\s\RAW\_DSC1794.NEF")).unwrap();
        assert_eq!(out, PathBuf::from(r"C:\s\RAW\_DSC1794.dng"));
    }

    #[test]
    fn output_keeps_dots_inside_the_stem() {
        let out = expected_output(Path::new(r"C:\s\RAW\shoot.02.raw.NEF")).unwrap();
        assert_eq!(out, PathBuf::from(r"C:\s\RAW\shoot.02.raw.dng"));
    }

    #[test]
    fn a_bare_filename_has_no_derivable_output() {
        assert!(expected_output(Path::new("")).is_none());
    }

    #[test]
    fn converter_info_always_reports_a_download_url() {
        assert!(find_dng_converter().download_url.starts_with("https://"));
    }

    /// The whole point of converting is that `rawler` can then decode the
    /// result, so prove it against a real converted file. Set
    /// `RAPIDRAW_TEST_DNG` to a DNG produced by Adobe DNG Converter.
    #[test]
    fn rawler_can_decode_a_converted_dng() {
        let Ok(path) = std::env::var("RAPIDRAW_TEST_DNG") else {
            eprintln!("RAPIDRAW_TEST_DNG unset, skipping");
            return;
        };

        let decoded = rawler::analyze::extract_raw_pixels(
            &path,
            &rawler::decoders::RawDecodeParams::default(),
        );

        match decoded {
            Ok(raw) => eprintln!("decoded {} at {}x{}", path, raw.width, raw.height),
            Err(e) => panic!("rawler could not decode the converted DNG: {e:?}"),
        }
    }
}
