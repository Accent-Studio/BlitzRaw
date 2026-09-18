//! Reading which compression a Nikon NEF actually uses.
//!
//! Nikon's High Efficiency and High Efficiency★ modes use the patented intoPIX
//! TicoRAW codec. No open source decoder implements them, `rawler` included, so
//! a file in either mode cannot be developed. RapidRAW currently discovers this
//! only by attempting a decode, failing, and silently substituting the embedded
//! JPEG preview, which leaves the user editing an 8-bit camera JPEG without
//! knowing it.
//!
//! This module answers the question cheaply and up front, so the library can
//! label such files and offer a DNG conversion instead of failing quietly.
//!
//! Detection mirrors `rawler`'s own logic. Older bodies store the mode in Nikon
//! maker note tag `0x0093`. Newer Z bodies (Z8, Z9) drop that tag and put the
//! value inside tag `0x0051` at byte offset 10, always little-endian.

use std::fs::File;
use std::path::Path;

use memmap2::Mmap;
use serde::{Deserialize, Serialize};

const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_MAKERNOTE: u16 = 0x927c;
const NIKON_TAG_MAKERNOTES_0X51: u16 = 0x0051;
const NIKON_TAG_NEF_COMPRESSION: u16 = 0x0093;

/// Compression modes as numbered by Nikon. Values match `rawler`'s enum so the
/// two agree about what is decodable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NefCompression {
    LossyType1,
    Uncompressed,
    Lossless,
    LossyType2,
    StripedPacked12Bits,
    UncompressedReduced12Bits,
    Unpacked12Bits,
    Small,
    Packed12Bits,
    Packed14Bits,
    HighEfficiency,
    HighEfficiencyStar,
}

impl NefCompression {
    fn from_code(code: u16) -> Option<Self> {
        Some(match code {
            1 => Self::LossyType1,
            2 => Self::Uncompressed,
            3 => Self::Lossless,
            4 => Self::LossyType2,
            5 => Self::StripedPacked12Bits,
            6 => Self::UncompressedReduced12Bits,
            7 => Self::Unpacked12Bits,
            8 => Self::Small,
            9 => Self::Packed12Bits,
            10 => Self::Packed14Bits,
            13 => Self::HighEfficiency,
            14 => Self::HighEfficiencyStar,
            _ => return None,
        })
    }

    /// Wording matches Nikon's own menu so it reads correctly in the UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::LossyType1 => "Lossy (type 1)",
            Self::Uncompressed => "Uncompressed",
            Self::Lossless => "Lossless Compressed",
            Self::LossyType2 => "Lossy (type 2)",
            Self::StripedPacked12Bits => "Striped packed 12-bit",
            Self::UncompressedReduced12Bits => "Uncompressed 12-bit",
            Self::Unpacked12Bits => "Unpacked 12-bit",
            Self::Small => "Small RAW",
            Self::Packed12Bits => "Packed 12-bit",
            Self::Packed14Bits => "Packed 14-bit",
            Self::HighEfficiency => "High Efficiency",
            Self::HighEfficiencyStar => "High Efficiency★",
        }
    }

    /// Whether an open source decoder can develop this file at all.
    pub fn is_decodable(self) -> bool {
        !matches!(self, Self::HighEfficiency | Self::HighEfficiencyStar)
    }
}

/// What the library needs to know about one file, in one round trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressionReport {
    pub path: String,
    /// `None` when the file is not a NEF or the mode could not be read.
    pub compression: Option<NefCompression>,
    pub label: Option<String>,
    /// True only when we positively know the RAW data cannot be decoded.
    pub needs_conversion: bool,
}

// --- minimal, bounds-checked TIFF reader -------------------------------------

pub(crate) struct Reader<'a> {
    pub(crate) buf: &'a [u8],
    pub(crate) base: usize,
    pub(crate) little: bool,
}

impl<'a> Reader<'a> {
    pub(crate) fn u16(&self, offset: usize) -> Option<u16> {
        let at = self.base.checked_add(offset)?;
        let bytes: [u8; 2] = self.buf.get(at..at + 2)?.try_into().ok()?;
        Some(if self.little {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        })
    }

    pub(crate) fn u32(&self, offset: usize) -> Option<u32> {
        let at = self.base.checked_add(offset)?;
        let bytes: [u8; 4] = self.buf.get(at..at + 4)?.try_into().ok()?;
        Some(if self.little {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Entry {
    /// Raw contents of the 4-byte value field, already decoded as a u32.
    pub(crate) value: u32,
    /// Byte length of the entry's data.
    pub(crate) length: usize,
}

/// Walks one IFD and returns the entry for `wanted`, if present. Entry counts
/// are capped so a corrupt file cannot send us scanning the whole mapping.
pub(crate) fn find_entry(r: &Reader, ifd_offset: usize, wanted: u16) -> Option<Entry> {
    let count = r.u16(ifd_offset)?;
    if count > 4096 {
        return None;
    }

    for i in 0..count as usize {
        let entry = ifd_offset + 2 + i * 12;
        if r.u16(entry)? != wanted {
            continue;
        }

        let field_type = r.u16(entry + 2)?;
        let n = r.u32(entry + 4)? as usize;
        let unit: usize = match field_type {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 11 => 4,
            5 | 10 | 12 => 8,
            _ => 1,
        };

        return Some(Entry {
            value: r.u32(entry + 8)?,
            length: unit.saturating_mul(n),
        });
    }
    None
}

/// Reads the compression mode out of an already-mapped NEF.
fn parse(buf: &[u8]) -> Option<NefCompression> {
    let little = match buf.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };

    let tiff = Reader {
        buf,
        base: 0,
        little,
    };
    let ifd0 = tiff.u32(4)? as usize;

    let exif_ifd = find_entry(&tiff, ifd0, TAG_EXIF_IFD)?.value as usize;
    let makernote = find_entry(&tiff, exif_ifd, TAG_MAKERNOTE)?.value as usize;

    // Nikon type 3: "Nikon\0" then two version bytes and padding, then a
    // complete TIFF header of its own at offset 10.
    if buf.get(makernote..makernote + 5)? != b"Nikon" {
        return None;
    }
    let nikon_base = makernote + 10;

    let nikon_little = match buf.get(nikon_base..nikon_base + 2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let nikon = Reader {
        buf,
        base: nikon_base,
        little: nikon_little,
    };
    let nikon_ifd = nikon.u32(4)? as usize;

    // Newer Z bodies: the value lives inside tag 0x51, always little-endian,
    // at byte offset 10 of that tag's data.
    if let Some(entry) = find_entry(&nikon, nikon_ifd, NIKON_TAG_MAKERNOTES_0X51)
        && entry.length > 12
    {
        // Nikon is inconsistent about whether maker note data offsets are
        // relative to its own TIFF header or absolute in the file, so try both
        // and accept whichever yields a code we recognise.
        for start in [nikon_base + entry.value as usize, entry.value as usize] {
            let at = start + 10;
            if let Some(bytes) = buf.get(at..at + 2)
                && let Ok(bytes) = <[u8; 2]>::try_from(bytes)
                && let Some(found) = NefCompression::from_code(u16::from_le_bytes(bytes))
            {
                return Some(found);
            }
        }
    }

    // Older bodies: a plain SHORT in tag 0x93.
    let entry = find_entry(&nikon, nikon_ifd, NIKON_TAG_NEF_COMPRESSION)?;
    let code = if nikon_little {
        (entry.value & 0xffff) as u16
    } else {
        (entry.value >> 16) as u16
    };
    NefCompression::from_code(code)
}

/// Reads the compression mode of a NEF without decoding any image data.
///
/// Returns `None` for non-NEF files and for anything unreadable, which callers
/// must treat as "unknown", never as "fine".
pub fn probe<P: AsRef<Path>>(path: P) -> Option<NefCompression> {
    let path = path.as_ref();

    let is_nef = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("nef") || e.eq_ignore_ascii_case("nrw"))
        .unwrap_or(false);
    if !is_nef {
        return None;
    }

    let file = File::open(path).ok()?;
    // SAFETY: a concurrent truncation of the file could invalidate the mapping.
    // These are the user's own RAW files, which nothing else writes during a
    // library scan, and every read below is bounds-checked.
    let map = unsafe { Mmap::map(&file) }.ok()?;
    parse(&map)
}

/// Reports on many files at once, for labelling a folder in the library view.
#[tauri::command]
pub fn probe_raw_compression(paths: Vec<String>) -> Vec<CompressionReport> {
    use rayon::prelude::*;

    paths
        .into_par_iter()
        .map(|path| {
            let compression = probe(&path);
            CompressionReport {
                path,
                compression,
                label: compression.map(|c| c.label().to_string()),
                needs_conversion: compression.map(|c| !c.is_decodable()).unwrap_or(false),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_codes_and_rejects_the_rest() {
        assert_eq!(NefCompression::from_code(3), Some(NefCompression::Lossless));
        assert_eq!(
            NefCompression::from_code(14),
            Some(NefCompression::HighEfficiencyStar)
        );
        assert_eq!(NefCompression::from_code(0), None);
        assert_eq!(NefCompression::from_code(99), None);
    }

    #[test]
    fn only_high_efficiency_modes_are_undecodable() {
        assert!(NefCompression::Lossless.is_decodable());
        assert!(NefCompression::Packed14Bits.is_decodable());
        assert!(!NefCompression::HighEfficiency.is_decodable());
        assert!(!NefCompression::HighEfficiencyStar.is_decodable());
    }

    #[test]
    fn ignores_files_that_are_not_nef() {
        assert!(probe("some/photo.jpg").is_none());
        assert!(probe("some/photo.dng").is_none());
        assert!(probe("some/photo.cr3").is_none());
    }

    #[test]
    fn garbage_input_returns_none_rather_than_panicking() {
        assert!(parse(&[]).is_none());
        assert!(parse(b"II").is_none());
        assert!(parse(b"NOTATIFFHEADER....").is_none());
        assert!(parse(&[0xff; 64]).is_none());
    }

    /// Reads every NEF under `RAPIDRAW_TEST_NEF_DIR` and reports what it found.
    /// Skips silently when the variable is unset, so CI stays green without
    /// sample files. Set it to a folder of real camera files to verify that
    /// detection agrees with what the camera actually wrote.
    #[test]
    fn probes_real_files_when_a_sample_folder_is_provided() {
        use std::collections::BTreeMap;

        let Ok(dir) = std::env::var("RAPIDRAW_TEST_NEF_DIR") else {
            eprintln!("RAPIDRAW_TEST_NEF_DIR unset, skipping");
            return;
        };

        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut total = 0;

        for entry in walkdir::WalkDir::new(&dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();
            let is_nef = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("nef"))
                .unwrap_or(false);
            if !is_nef {
                continue;
            }

            total += 1;
            let key = probe(path)
                .map(|c| c.label().to_string())
                .unwrap_or_else(|| "UNREADABLE".to_string());
            *counts.entry(key).or_default() += 1;
        }

        eprintln!("probed {total} NEF files under {dir}");
        for (label, n) in &counts {
            eprintln!("  {n:>6}  {label}");
        }

        assert!(total > 0, "no NEF files found under {dir}");
        assert_eq!(
            counts.get("UNREADABLE").copied().unwrap_or(0),
            0,
            "some files could not be probed"
        );
    }
}
