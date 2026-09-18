//! Reading the XMP a camera writes inside the photo file itself.
//!
//! Stars set on the camera during a shoot never reach a `.xmp` sidecar, because
//! nothing has written one yet. Nikon puts them in an XMP packet stored in the
//! file, at TIFF tag `0x02bc` (`XMLPacket`) of IFD0. That is the same packet
//! Lightroom, Bridge and Windows Explorer read, which is why all three show the
//! rating and BlitzRaw did not.
//!
//! Measured across 2,425 real NEFs: every one carries the
//! packet, 343 of them with a non-zero rating, and the packet always begins
//! within the first 900 bytes of the file. There is exactly one `<xmp:Rating>`
//! element per file, so there is nothing to disambiguate.
//!
//! Nothing here decodes image data. IFD0 is walked for TIFF containers (NEF,
//! DNG, TIFF, ARW, CR2) and the APP1 segments for JPEG, so the cost is a couple
//! of short reads. Only a container neither of those recognises falls back to a
//! bounded scan.

use std::fs::File;
use std::path::Path;

use memmap2::Mmap;

use crate::nef_compression::{Entry, Reader, find_entry};

/// TIFF `XMLPacket`, which holds the XMP as raw bytes.
const TAG_XMP: u16 = 0x02bc;

/// The rating Windows Explorer reads and writes, 0 to 5. Some bodies write it
/// beside the packet; Nikon does not, so it is a second opinion, not the first.
const TAG_RATING: u16 = 0x4746;

/// The APP1 payload prefix that introduces XMP in a JPEG.
const JPEG_XMP_ID: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

/// The APP1 payload prefix that introduces a whole TIFF block of EXIF.
const JPEG_EXIF_ID: &[u8] = b"Exif\0\0";

/// How far into a container we do not parse we are willing to look. A CR3 or
/// HEIF keeps its packet near the front, and this bounds the cost of being
/// wrong about that.
const SCAN_LIMIT: usize = 256 * 1024;

/// A packet larger than this is a corrupt length field, not a photographer's
/// metadata. Nikon's is 32 KB, most of it padding.
const MAX_XMP_BYTES: usize = 4 * 1024 * 1024;

/// The closing tag of an XMP packet.
const PACKET_END: &[u8] = b"</x:xmpmeta>";

/// What one file carries. Both fields are absent for a file with no opinion.
#[derive(Debug, Default, Clone)]
pub struct Embedded {
    /// The XMP packet as text, trimmed to the end of the metadata.
    pub xmp: Option<String>,
    /// EXIF tag `0x4746`, for the cameras that write it.
    pub exif_rating: Option<u8>,
}

impl Embedded {
    /// Used by the tests, which are the only caller that cares about "nothing
    /// at all" rather than about one field.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.xmp.is_none() && self.exif_rating.is_none()
    }
}

/// Reads whatever metadata the file carries about itself.
///
/// Never fails: an unreadable or unrecognised file simply carries nothing, and
/// callers must treat that as "no opinion", never as "rated zero".
pub fn read<P: AsRef<Path>>(path: P) -> Embedded {
    let Ok(file) = File::open(path.as_ref()) else {
        return Embedded::default();
    };

    // SAFETY: a concurrent truncation of the file could invalidate the mapping.
    // These are the user's own photos, which nothing else writes during a
    // library scan, and every read below is bounds-checked.
    let Ok(map) = (unsafe { Mmap::map(&file) }) else {
        return Embedded::default();
    };

    parse(&map)
}

fn parse(buf: &[u8]) -> Embedded {
    if let Some(found) = from_tiff(buf, 0) {
        return found;
    }
    if let Some(found) = from_jpeg(buf) {
        return found;
    }
    Embedded {
        xmp: scan_for_packet(buf),
        exif_rating: None,
    }
}

/// Walks IFD0 of a TIFF container. Returns `None` only when the bytes at `base`
/// are not a TIFF header at all, so a TIFF carrying no XMP does not fall
/// through to the scan and pay for it on every photo in the folder.
fn from_tiff(buf: &[u8], base: usize) -> Option<Embedded> {
    let little = match buf.get(base..base + 2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };

    let reader = Reader { buf, base, little };

    // BigTIFF numbers itself 43 and lays its entries out differently. Reading
    // it with this parser would produce plausible nonsense.
    if reader.u16(2)? != 42 {
        return None;
    }

    let Some(ifd0) = reader.u32(4).map(|offset| offset as usize) else {
        return Some(Embedded::default());
    };

    let xmp = find_entry(&reader, ifd0, TAG_XMP)
        .and_then(|entry| entry_bytes(buf, base, &entry))
        .and_then(packet_text);

    let exif_rating = find_entry(&reader, ifd0, TAG_RATING)
        .map(|entry| inline_short(&entry, little))
        .and_then(|value| u8::try_from(value).ok())
        .filter(|value| *value <= 5);

    Some(Embedded { xmp, exif_rating })
}

/// Walks the JPEG segment chain. Returns `None` when the file is not a JPEG.
fn from_jpeg(buf: &[u8]) -> Option<Embedded> {
    if buf.get(0..2)? != b"\xff\xd8" {
        return None;
    }

    let mut found = Embedded::default();
    let mut at = 2usize;

    loop {
        // A run of 0xff bytes is legal padding before a marker.
        while buf.get(at) == Some(&0xff) && buf.get(at + 1) == Some(&0xff) {
            at += 1;
        }

        if buf.get(at).copied() != Some(0xff) {
            break;
        }
        let Some(marker) = buf.get(at + 1).copied() else {
            break;
        };

        // Standalone markers carry no length field.
        if marker == 0x01 || (0xd0..=0xd8).contains(&marker) {
            at += 2;
            continue;
        }

        // Start of scan is the image data, and end of image is the end.
        if marker == 0xda || marker == 0xd9 {
            break;
        }

        let Some(length) = buf
            .get(at + 2..at + 4)
            .and_then(|bytes| <[u8; 2]>::try_from(bytes).ok())
            .map(u16::from_be_bytes)
            .map(usize::from)
        else {
            break;
        };
        if length < 2 {
            break;
        }

        let Some(payload) = buf.get(at + 4..at + 2 + length) else {
            break;
        };

        if marker == 0xe1 {
            if let Some(body) = payload.strip_prefix(JPEG_XMP_ID) {
                // The packet is the better answer, so stop as soon as it turns
                // up rather than reading the rest of the segments.
                found.xmp = packet_text(body);
                return Some(found);
            }
            if let Some(tiff) = payload.strip_prefix(JPEG_EXIF_ID)
                && let Some(inner) = from_tiff(tiff, 0)
            {
                found.exif_rating = found.exif_rating.or(inner.exif_rating);
                found.xmp = found.xmp.or(inner.xmp);
            }
        }

        at += 2 + length;
    }

    Some(found)
}

/// Last resort for a container we do not parse, such as CR3 or HEIF.
fn scan_for_packet(buf: &[u8]) -> Option<String> {
    let window = &buf[..buf.len().min(SCAN_LIMIT)];
    let start = find(window, b"<x:xmpmeta")?;
    packet_text(&window[start..])
}

/// The data an IFD entry points at. Values of four bytes or fewer are stored in
/// the entry itself, which is never how a packet is stored.
fn entry_bytes<'a>(buf: &'a [u8], base: usize, entry: &Entry) -> Option<&'a [u8]> {
    if entry.length <= 4 || entry.length > MAX_XMP_BYTES {
        return None;
    }
    let start = base.checked_add(entry.value as usize)?;
    buf.get(start..start.checked_add(entry.length)?)
}

/// A one-element SHORT sits in the first two bytes of the value field, so which
/// half of the u32 it occupies depends on the file's byte order.
fn inline_short(entry: &Entry, little: bool) -> u32 {
    if little {
        entry.value & 0xffff
    } else {
        entry.value >> 16
    }
}

/// Turns raw packet bytes into text, cut off at the end of the metadata so the
/// 32 KB of padding Nikon writes does not travel any further.
fn packet_text(bytes: &[u8]) -> Option<String> {
    let end = find(bytes, PACKET_END)
        .map(|at| at + PACKET_END.len())
        .unwrap_or(bytes.len());

    let text = String::from_utf8_lossy(&bytes[..end])
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string();

    if text.is_empty() { None } else { Some(text) }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the smallest little-endian TIFF that carries one XMP packet, so
    /// the walk can be tested without a camera file.
    fn tiff_with_xmp(packet: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"II");
        buf.extend_from_slice(&42u16.to_le_bytes());
        buf.extend_from_slice(&8u32.to_le_bytes()); // IFD0 starts at byte 8

        buf.extend_from_slice(&2u16.to_le_bytes()); // two entries

        let data_at = 8 + 2 + 2 * 12 + 4;

        buf.extend_from_slice(&TAG_XMP.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes()); // BYTE
        buf.extend_from_slice(&(packet.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(data_at as u32).to_le_bytes());

        buf.extend_from_slice(&TAG_RATING.to_le_bytes());
        buf.extend_from_slice(&3u16.to_le_bytes()); // SHORT
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&4u32.to_le_bytes()); // inline value of 4

        buf.extend_from_slice(&0u32.to_le_bytes()); // no next IFD

        assert_eq!(buf.len(), data_at);
        buf.extend_from_slice(packet);
        buf
    }

    #[test]
    fn reads_the_packet_and_the_rating_out_of_a_tiff() {
        let packet = b"<x:xmpmeta><xmp:Rating>2</xmp:Rating></x:xmpmeta>";
        let found = parse(&tiff_with_xmp(packet));

        assert_eq!(
            found.xmp.as_deref(),
            Some("<x:xmpmeta><xmp:Rating>2</xmp:Rating></x:xmpmeta>")
        );
        assert_eq!(found.exif_rating, Some(4));
    }

    #[test]
    fn drops_the_padding_a_camera_writes_after_the_packet() {
        let mut packet = b"<x:xmpmeta><xmp:Rating>1</xmp:Rating></x:xmpmeta>".to_vec();
        packet.extend(std::iter::repeat(b' ').take(32_000));

        let found = parse(&tiff_with_xmp(&packet));
        let text = found.xmp.expect("packet");

        assert!(text.ends_with("</x:xmpmeta>"), "{text}");
        assert!(text.len() < 100, "padding survived: {} bytes", text.len());
    }

    #[test]
    fn garbage_input_returns_nothing_rather_than_panicking() {
        assert!(parse(&[]).is_empty());
        assert!(parse(b"II").is_empty());
        assert!(parse(b"NOTATIFFHEADER....").is_empty());
        assert!(parse(&[0xff; 64]).is_empty());
        assert!(parse(&[0x00; 4096]).is_empty());
    }

    #[test]
    fn a_bigtiff_header_is_refused_rather_than_misread() {
        let mut buf = b"II".to_vec();
        buf.extend_from_slice(&43u16.to_le_bytes());
        buf.extend_from_slice(&[0u8; 4096]);

        assert!(parse(&buf).is_empty());
    }

    /// Reads every NEF and DNG under `RAPIDRAW_TEST_NEF_DIR` and reports the
    /// ratings it found. Skips silently when the variable is unset. This is the
    /// check that the readout is not a private invention: the counts should
    /// match what Lightroom or Explorer shows for the same folder.
    #[test]
    fn reads_in_camera_ratings_from_real_files() {
        use std::collections::BTreeMap;

        let Ok(dir) = std::env::var("RAPIDRAW_TEST_NEF_DIR") else {
            eprintln!("RAPIDRAW_TEST_NEF_DIR unset, skipping");
            return;
        };

        let mut counts: BTreeMap<u8, usize> = BTreeMap::new();
        let mut total = 0;
        let mut without_packet = 0;

        for entry in walkdir::WalkDir::new(&dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();
            let is_raw = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("nef") || e.eq_ignore_ascii_case("dng"))
                .unwrap_or(false);
            if !is_raw {
                continue;
            }

            total += 1;
            let found = read(path);
            let Some(text) = found.xmp.as_deref() else {
                without_packet += 1;
                continue;
            };

            let rating = crate::file_management::extract_xmp_rating(text).unwrap_or(0);
            assert!(rating <= 5, "{} rated {rating}", path.display());
            *counts.entry(rating).or_default() += 1;
        }

        eprintln!("read {total} raw files under {dir}");
        eprintln!("  {without_packet:>6}  no embedded packet");
        for (stars, n) in &counts {
            eprintln!("  {n:>6}  {stars} star");
        }

        assert!(total > 0, "no raw files found under {dir}");
    }
}
