//! The pen mask: a closed path of anchors, each able to carry bezier handles.
//!
//! This is the vector selection Lightroom does not have and Illustrator does.
//! A shoot of interiors is full of things a radial or a brush cannot follow
//! cleanly: a window frame, a worktop, the run of a ceiling. Those are straight
//! lines and gentle curves, and drawing them once with a path is both faster
//! and more exact than painting them.
//!
//! # The shape
//!
//! Anchors are in the photo's own pixels, the same space `radial` and `linear`
//! use, so the same `scale` and `crop_offset` place them. Each anchor may carry
//! `handleIn` and `handleOut`, also in photo pixels and **absolute**, not
//! relative to their anchor. Absolute handles survive a dragged anchor without
//! a second edit, and they are what the overlay already has to draw.
//!
//! A segment from anchor `a` to anchor `b` is a straight line when neither
//! `a.handle_out` nor `b.handle_in` is present, and a cubic otherwise, with the
//! missing side falling back to its own anchor. So a path can mix corners and
//! curves freely, which is the whole point of the tool.
//!
//! The path always fills as though closed, whether or not it was closed by
//! hand. A mask is an area; an open path with no area would be nothing at all,
//! and joining the ends is what every drawing program does when asked to fill
//! one.
//!
//! # Why the fill is written out rather than borrowed
//!
//! `imageproc` can fill a polygon, but only with hard edges and only from
//! integer points. A mask needs neither: the edge has to be smooth or it shows
//! as stair steps the moment the adjustment behind it is strong, and the
//! anchors are fractional because they come from a zoomed canvas.
//!
//! So the fill is a scanline over four sub-rows per pixel, and each span adds
//! its exact horizontal cover to the two pixels it ends inside. Four rows and
//! exact ends is enough: the remaining error is under half a level of 255 and
//! is invisible under the blur that feathering applies anyway.

use image::{GrayImage, Luma};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One point of the path, in the photo's pixels.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
#[serde(rename_all = "camelCase")]
pub struct PenPoint {
    pub x: f64,
    pub y: f64,
}

/// An anchor and the two handles that shape the segments either side of it.
///
/// Both handles are optional and absolute. `handle_in` shapes the segment
/// arriving at this anchor, `handle_out` the one leaving it, which is the way
/// every vector editor names them.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
#[serde(rename_all = "camelCase")]
pub struct PenAnchor {
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub handle_in: Option<PenPoint>,
    #[serde(default)]
    pub handle_out: Option<PenPoint>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct PenMaskParameters {
    #[serde(default)]
    pub points: Vec<PenAnchor>,
    /// Whether the path was joined up by hand. It does not change the fill,
    /// which always closes, only what the overlay draws while you work.
    #[serde(default)]
    pub closed: bool,
    /// Still being placed. The fill is skipped until the path has an area, so
    /// a half-drawn path does not flash a wedge across the photo.
    #[serde(default)]
    pub is_drawing: bool,
    #[serde(default)]
    pub grow: f32,
    #[serde(default)]
    pub feather: f32,
}

/// How finely a curve is chopped into straight pieces.
///
/// One piece per this many pixels of the curve's rough length, which keeps a
/// small curve cheap and a sweeping one smooth. The bounds stop a degenerate
/// curve from producing either a visible kink or a million pieces.
const PIXELS_PER_SEGMENT: f64 = 3.0;
const MIN_SEGMENTS: usize = 6;
const MAX_SEGMENTS: usize = 240;

/// Sub-rows per pixel row. Four is the point where more stops being visible.
const SUB_ROWS: usize = 4;

fn cubic_at(p0: (f64, f64), c1: (f64, f64), c2: (f64, f64), p1: (f64, f64), t: f64) -> (f64, f64) {
    let mt = 1.0 - t;
    let a = mt * mt * mt;
    let b = 3.0 * mt * mt * t;
    let c = 3.0 * mt * t * t;
    let d = t * t * t;
    (
        a * p0.0 + b * c1.0 + c * c2.0 + d * p1.0,
        a * p0.1 + b * c1.1 + c * c2.1 + d * p1.1,
    )
}

/// The control polygon's length, which is always at least the curve's own and
/// never more than about a third over it. Close enough to choose a step count.
fn rough_length(p0: (f64, f64), c1: (f64, f64), c2: (f64, f64), p1: (f64, f64)) -> f64 {
    let d = |a: (f64, f64), b: (f64, f64)| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
    d(p0, c1) + d(c1, c2) + d(c2, p1)
}

/// Turns the path into a closed polygon in canvas pixels.
///
/// `scale` and `crop_offset` are applied here rather than in the fill, so the
/// fill only ever sees the pixels it is writing to.
pub fn flatten(points: &[PenAnchor], scale: f32, crop_offset: (f32, f32)) -> Vec<(f64, f64)> {
    let place = |x: f64, y: f64| -> (f64, f64) {
        (
            x * scale as f64 - crop_offset.0 as f64,
            y * scale as f64 - crop_offset.1 as f64,
        )
    };

    let n = points.len();
    if n < 3 {
        return Vec::new();
    }

    let mut out: Vec<(f64, f64)> = Vec::with_capacity(n * 8);

    for i in 0..n {
        let a = &points[i];
        let b = &points[(i + 1) % n];

        let p0 = place(a.x, a.y);
        let p1 = place(b.x, b.y);

        // A segment is straight unless one of its two handles is present, and
        // a missing one falls back to its own anchor so a curve can meet a
        // corner without a handle having to be invented for it.
        let has_curve = a.handle_out.is_some() || b.handle_in.is_some();

        out.push(p0);

        if !has_curve {
            continue;
        }

        let c1 = a.handle_out.map(|h| place(h.x, h.y)).unwrap_or(p0);
        let c2 = b.handle_in.map(|h| place(h.x, h.y)).unwrap_or(p1);

        let steps = ((rough_length(p0, c1, c2, p1) / PIXELS_PER_SEGMENT).ceil() as usize)
            .clamp(MIN_SEGMENTS, MAX_SEGMENTS);

        // From 1, not 0: the anchor itself has just been pushed. To `steps`
        // exclusive: the next anchor is pushed by the next turn of the loop,
        // and pushing it twice would leave a zero-length edge for the fill to
        // trip over.
        for s in 1..steps {
            let t = s as f64 / steps as f64;
            out.push(cubic_at(p0, c1, c2, p1, t));
        }
    }

    out
}

/// Adds the cover of one horizontal span to a row of accumulated cover.
///
/// The two pixels the span ends inside get the fraction they actually hold;
/// everything between them gets a whole unit. A span that starts and ends in
/// the same pixel gets its width once, not twice.
fn add_span(row: &mut [f32], x0: f64, x1: f64, weight: f32) {
    let width = row.len();
    if width == 0 {
        return;
    }

    let left = x0.max(0.0);
    let right = x1.min(width as f64);
    if right <= left {
        return;
    }

    let first = left.floor() as usize;
    let last = (right.ceil() as usize).min(width);

    for px in first..last {
        let lo = (px as f64).max(left);
        let hi = ((px + 1) as f64).min(right);
        if hi > lo {
            row[px] += weight * (hi - lo) as f32;
        }
    }
}

/// Fills a closed polygon into a fresh mask, smooth at the edges.
///
/// Even-odd, so a path that crosses itself leaves a hole rather than a solid
/// blob. That is what a vector editor does and what makes a figure-of-eight
/// worth drawing at all.
pub fn fill_polygon(polygon: &[(f64, f64)], width: u32, height: u32) -> GrayImage {
    let mut mask = GrayImage::new(width, height);
    if polygon.len() < 3 || width == 0 || height == 0 {
        return mask;
    }

    // Only the rows the shape actually covers are worth walking. On a 45
    // megapixel frame a path around a window is a small part of the picture,
    // and the rest of the scanlines would find nothing.
    let mut min_y = f64::MAX;
    let mut max_y = f64::MIN;
    for &(_, y) in polygon {
        if y < min_y {
            min_y = y;
        }
        if y > max_y {
            max_y = y;
        }
    }
    let row_start = (min_y.floor().max(0.0)) as u32;
    let row_end = (max_y.ceil().min(height as f64)) as u32;
    if row_start >= row_end {
        return mask;
    }

    let sub_weight = 1.0 / SUB_ROWS as f32;
    let mut cover = vec![0f32; width as usize];
    let mut crossings: Vec<f64> = Vec::with_capacity(16);

    for y in row_start..row_end {
        cover.iter_mut().for_each(|c| *c = 0.0);

        for sub in 0..SUB_ROWS {
            let sample_y = y as f64 + (sub as f64 + 0.5) / SUB_ROWS as f64;

            crossings.clear();
            for i in 0..polygon.len() {
                let (x0, y0) = polygon[i];
                let (x1, y1) = polygon[(i + 1) % polygon.len()];

                // Half-open in y: an edge counts at its top end and not at its
                // bottom. Without that rule a sample line passing exactly
                // through a shared anchor counts both edges and the fill flips
                // inside out for the rest of that row.
                if (y0 <= sample_y && y1 > sample_y) || (y1 <= sample_y && y0 > sample_y) {
                    let t = (sample_y - y0) / (y1 - y0);
                    crossings.push(x0 + t * (x1 - x0));
                }
            }

            if crossings.len() < 2 {
                continue;
            }
            crossings.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

            for pair in crossings.chunks_exact(2) {
                add_span(&mut cover, pair[0], pair[1], sub_weight);
            }
        }

        for (x, c) in cover.iter().enumerate() {
            if *c > 0.0 {
                let v = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
                mask.put_pixel(x as u32, y, Luma([v]));
            }
        }
    }

    mask
}

/// The whole job: parameters in, mask out.
///
/// Grow and feather are left to the caller, which already has the one helper
/// every other mask type shares, so a pen mask feathers by exactly the same
/// rule as a radial one.
pub fn generate_pen_bitmap(
    params_value: &Value,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
) -> (GrayImage, f32, f32) {
    let params: PenMaskParameters =
        serde_json::from_value(params_value.clone()).unwrap_or_default();

    let polygon = flatten(&params.points, scale, crop_offset);
    (
        fill_polygon(&polygon, width, height),
        params.grow,
        params.feather,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(x: f64, y: f64) -> PenAnchor {
        PenAnchor {
            x,
            y,
            handle_in: None,
            handle_out: None,
        }
    }

    #[test]
    fn fewer_than_three_anchors_have_no_area() {
        assert!(flatten(&[anchor(0.0, 0.0), anchor(10.0, 10.0)], 1.0, (0.0, 0.0)).is_empty());
    }

    #[test]
    fn a_square_fills_solid_inside_and_empty_outside() {
        let square = vec![
            anchor(10.0, 10.0),
            anchor(30.0, 10.0),
            anchor(30.0, 30.0),
            anchor(10.0, 30.0),
        ];
        let mask = fill_polygon(&flatten(&square, 1.0, (0.0, 0.0)), 40, 40);

        assert_eq!(mask.get_pixel(20, 20)[0], 255, "the middle is filled");
        assert_eq!(mask.get_pixel(5, 20)[0], 0, "outside the left edge is empty");
        assert_eq!(mask.get_pixel(35, 20)[0], 0, "outside the right edge is empty");
        assert_eq!(mask.get_pixel(20, 5)[0], 0, "above the shape is empty");
        assert_eq!(mask.get_pixel(20, 35)[0], 0, "below the shape is empty");
    }

    #[test]
    fn scale_and_crop_move_the_shape_the_way_the_other_masks_move() {
        let square = vec![
            anchor(10.0, 10.0),
            anchor(20.0, 10.0),
            anchor(20.0, 20.0),
            anchor(10.0, 20.0),
        ];
        // Twice the size, then shifted up and left by the crop: a corner that
        // was at (10,10) lands at (10*2 - 5, 10*2 - 5) = (15,15).
        let polygon = flatten(&square, 2.0, (5.0, 5.0));
        assert_eq!(polygon[0], (15.0, 15.0));

        let mask = fill_polygon(&polygon, 60, 60);
        assert_eq!(mask.get_pixel(25, 25)[0], 255, "inside the scaled square");
        assert_eq!(mask.get_pixel(10, 10)[0], 0, "outside it");
    }

    #[test]
    fn a_half_covered_pixel_is_about_half_lit() {
        // An edge down the middle of column 20: x from 10.0 to 20.5.
        let shape = vec![
            anchor(10.0, 10.0),
            anchor(20.5, 10.0),
            anchor(20.5, 30.0),
            anchor(10.0, 30.0),
        ];
        let mask = fill_polygon(&flatten(&shape, 1.0, (0.0, 0.0)), 40, 40);

        let edge = mask.get_pixel(20, 20)[0];
        assert!(
            (100..=155).contains(&edge),
            "the half-covered pixel should be near 128, was {edge}"
        );
        assert_eq!(mask.get_pixel(19, 20)[0], 255, "the pixel before it is full");
        assert_eq!(mask.get_pixel(21, 20)[0], 0, "the pixel after it is empty");
    }

    #[test]
    fn a_handle_bends_the_edge_outwards() {
        // A square whose top edge is pulled upwards by two handles. The bulge
        // has to put ink above the straight line the anchors alone would give.
        let mut bowed = vec![
            anchor(10.0, 20.0),
            anchor(30.0, 20.0),
            anchor(30.0, 40.0),
            anchor(10.0, 40.0),
        ];
        bowed[0].handle_out = Some(PenPoint { x: 15.0, y: 5.0 });
        bowed[1].handle_in = Some(PenPoint { x: 25.0, y: 5.0 });

        let straight = fill_polygon(
            &flatten(
                &[
                    anchor(10.0, 20.0),
                    anchor(30.0, 20.0),
                    anchor(30.0, 40.0),
                    anchor(10.0, 40.0),
                ],
                1.0,
                (0.0, 0.0),
            ),
            50,
            50,
        );
        let curved = fill_polygon(&flatten(&bowed, 1.0, (0.0, 0.0)), 50, 50);

        assert_eq!(straight.get_pixel(20, 14)[0], 0, "no bulge without handles");
        assert!(
            curved.get_pixel(20, 14)[0] > 200,
            "the handle should carry the edge up past y=14"
        );
    }

    #[test]
    fn an_open_path_still_fills() {
        // Three anchors and `closed` never set: the fill joins the ends itself,
        // because a mask is an area and an unjoined one would be nothing.
        let triangle = vec![anchor(10.0, 10.0), anchor(30.0, 10.0), anchor(20.0, 30.0)];
        let mask = fill_polygon(&flatten(&triangle, 1.0, (0.0, 0.0)), 40, 40);
        assert!(mask.get_pixel(20, 15)[0] > 200, "the triangle is filled");
    }

    #[test]
    fn a_path_that_crosses_itself_leaves_a_hole() {
        // A square with a smaller square wound inside it in the same list, so
        // the even-odd rule has to cut the middle out.
        let ring = vec![
            anchor(0.0, 0.0),
            anchor(40.0, 0.0),
            anchor(40.0, 40.0),
            anchor(0.0, 40.0),
            anchor(0.0, 0.0),
            anchor(10.0, 10.0),
            anchor(10.0, 30.0),
            anchor(30.0, 30.0),
            anchor(30.0, 10.0),
            anchor(10.0, 10.0),
        ];
        let mask = fill_polygon(&flatten(&ring, 1.0, (0.0, 0.0)), 40, 40);
        assert_eq!(mask.get_pixel(20, 20)[0], 0, "the middle is cut out");
        assert_eq!(mask.get_pixel(5, 20)[0], 255, "the ring itself is filled");
    }
}
