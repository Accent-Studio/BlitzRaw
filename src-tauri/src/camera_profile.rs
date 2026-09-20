//! Camera colour calibration: the DNG recipe for turning sensor RGB into CIE XYZ.
//!
//! RapidRAW's temperature control is a linear RGB tint applied after demosaic
//! and colour conversion. It never consults the shot's own white balance or the
//! camera's calibration matrices, so a Kelvin readout would mean something
//! different on every file. This module supplies the missing half: the camera
//! profile carried in the file, and the arithmetic that turns a temperature in
//! Kelvin into the matrix that actually belongs to that temperature.
//!
//! The recipe is chapter 6 of the Adobe DNG specification, "Mapping Camera
//! Color Space to CIE XYZ Space", and the Kelvin/tint conversion is Robertson's
//! method on the CIE 1960 UCS diagram, which is what Adobe's own
//! `dng_temperature` uses and therefore what Lightroom's Temp slider reports.
//!
//! Two calibrations are stored per camera, one per illuminant, typically
//! Standard A (2850K) and D65 (6500K). Picking a temperature interpolates
//! between them in reciprocal Kelvin. That interpolation is the whole point: it
//! is why the same slider position means the same colour on every file.
//!
//! Nothing here touches the pipeline. It reads files and does arithmetic.

use std::collections::HashMap;

use rawler::formats::tiff::GenericTiffReader;
use rawler::formats::tiff::reader::TiffReader;
use rawler::imgop::xyz::Illuminant;
use rawler::rawimage::RawImage;
use rawler::tags::DngTag;

/// Row-major 3x3. `m[row][col]`, so `m[0]` is the first row.
pub type Matrix3 = [[f32; 3]; 3];

pub const IDENTITY: Matrix3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// D50 is the DNG profile connection space white point.
pub const D50_XY: (f32, f32) = (0.3457, 0.3585);

/// CIE XYZ under D50 to linear sRGB, Bradford-adapted to the sRGB D65 white.
/// The pipeline downstream of us works in linear sRGB primaries, so this is
/// where the profile connection space is handed over to it.
pub const XYZ_D50_TO_LINEAR_SRGB: Matrix3 = [
    [3.133_856, -1.6168667, -0.4906146],
    [-0.9787684, 1.9161415, 0.0334540],
    [0.0719453, -0.2289914, 1.4052427],
];

/// The linearised Bradford cone response, used to move a white point.
const BRADFORD: Matrix3 = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];

// ---------------------------------------------------------------------------
// Small matrix helpers. Deliberately plain: these run once per render, not per
// pixel, and reading them against the spec matters more than speed.
// ---------------------------------------------------------------------------

pub fn mat_mul(a: &Matrix3, b: &Matrix3) -> Matrix3 {
    let mut out = [[0.0f32; 3]; 3];
    for (r, row) in out.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            *cell = a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c];
        }
    }
    out
}

pub fn mat_apply(m: &Matrix3, v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// Inverse of a 3x3, or `None` when it is singular. A camera matrix that cannot
/// be inverted means a broken file, and the caller should fall back rather than
/// render nonsense.
pub fn mat_invert(m: &Matrix3) -> Option<Matrix3> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);

    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let inv_det = 1.0 / det;

    Some([
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv_det,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv_det,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv_det,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv_det,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv_det,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv_det,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv_det,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv_det,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv_det,
        ],
    ])
}

pub fn mat_diag(v: [f32; 3]) -> Matrix3 {
    [[v[0], 0.0, 0.0], [0.0, v[1], 0.0], [0.0, 0.0, v[2]]]
}

fn mat_lerp(a: &Matrix3, b: &Matrix3, weight_a: f32) -> Matrix3 {
    let weight_b = 1.0 - weight_a;
    let mut out = [[0.0f32; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            out[r][c] = a[r][c] * weight_a + b[r][c] * weight_b;
        }
    }
    out
}

/// xy chromaticity to XYZ, normalised to Y = 1.
pub fn xy_to_xyz(x: f32, y: f32) -> [f32; 3] {
    let y = if y < 1e-6 { 1e-6 } else { y };
    [x / y, 1.0, (1.0 - x - y) / y]
}

pub fn xyz_to_xy(xyz: [f32; 3]) -> (f32, f32) {
    let sum = xyz[0] + xyz[1] + xyz[2];
    if sum.abs() < 1e-9 {
        return D50_XY;
    }
    (xyz[0] / sum, xyz[1] / sum)
}

/// Bradford adaptation carrying `from` white to `to` white.
pub fn map_white_matrix(from: (f32, f32), to: (f32, f32)) -> Matrix3 {
    let w1 = mat_apply(&BRADFORD, xy_to_xyz(from.0, from.1));
    let w2 = mat_apply(&BRADFORD, xy_to_xyz(to.0, to.1));

    // Adobe pins the per-cone scaling; a wild ratio here means a nonsense white
    // point, and clamping keeps one bad file from producing an unusable image.
    let ratio = |a: f32, b: f32| (b / a.max(1e-6)).clamp(0.1, 10.0);
    let scale = mat_diag([
        ratio(w1[0], w2[0]),
        ratio(w1[1], w2[1]),
        ratio(w1[2], w2[2]),
    ]);

    match mat_invert(&BRADFORD) {
        Some(inv) => mat_mul(&inv, &mat_mul(&scale, &BRADFORD)),
        None => IDENTITY,
    }
}

// ---------------------------------------------------------------------------
// Kelvin and tint, by Robertson's method.
// ---------------------------------------------------------------------------

/// Isotemperature lines along the Planckian locus in CIE 1960 UCS, from
/// Wyszecki & Stiles. `r` is reciprocal megakelvin, `u`/`v` the locus point and
/// `t` the slope of the isotherm through it. Adobe's `dng_temperature` uses
/// this same table, which is why the Kelvin we report matches Lightroom's.
const ROBERTSON: [(f32, f32, f32, f32); 31] = [
    (0.0, 0.18006, 0.26352, -0.24341),
    (10.0, 0.18066, 0.26589, -0.25479),
    (20.0, 0.18133, 0.26846, -0.26876),
    (30.0, 0.18208, 0.27119, -0.28539),
    (40.0, 0.18293, 0.27407, -0.30470),
    (50.0, 0.18388, 0.27709, -0.32675),
    (60.0, 0.18494, 0.28021, -0.35156),
    (70.0, 0.18611, 0.28342, -0.37915),
    (80.0, 0.18740, 0.28668, -0.40955),
    (90.0, 0.18880, 0.28997, -0.44278),
    (100.0, 0.19032, 0.29326, -0.47888),
    (125.0, 0.19462, 0.30141, -0.58204),
    (150.0, 0.19962, 0.30921, -0.70471),
    (175.0, 0.20525, 0.31647, -0.84901),
    (200.0, 0.21142, 0.32312, -1.0182),
    (225.0, 0.21807, 0.32909, -1.2168),
    (250.0, 0.22511, 0.33439, -1.4512),
    (275.0, 0.23247, 0.33904, -1.7298),
    (300.0, 0.24010, 0.34308, -2.0637),
    (325.0, 0.24792, 0.34655, -2.4681),
    (350.0, 0.25591, 0.34951, -2.9641),
    (375.0, 0.26400, 0.35200, -3.5814),
    (400.0, 0.27218, 0.35407, -4.3633),
    (425.0, 0.28039, 0.35577, -5.3762),
    (450.0, 0.28863, 0.35714, -6.7262),
    (475.0, 0.29685, 0.35823, -8.5955),
    (500.0, 0.30505, 0.35907, -11.324),
    (525.0, 0.31320, 0.35968, -15.628),
    (550.0, 0.32129, 0.36011, -23.325),
    (575.0, 0.32931, 0.36038, -40.770),
    (600.0, 0.33724, 0.36051, -116.45),
];

/// Adobe's scaling for tint. Negative, so positive tint is magenta and negative
/// is green, matching the convention every other editor shows.
const TINT_SCALE: f32 = -3000.0;

/// The usable Kelvin range. The bottom is where the Robertson table ends; the
/// top is where the slider stops meaning anything, since the locus has nearly
/// stopped moving by then.
pub const MIN_KELVIN: f32 = 1667.0;
pub const MAX_KELVIN: f32 = 50000.0;
pub const MIN_TINT: f32 = -150.0;
pub const MAX_TINT: f32 = 150.0;

fn xy_to_uv(x: f32, y: f32) -> (f32, f32) {
    let denom = 1.5 - x + 6.0 * y;
    if denom.abs() < 1e-9 {
        return (0.0, 0.0);
    }
    (2.0 * x / denom, 3.0 * y / denom)
}

fn uv_to_xy(u: f32, v: f32) -> (f32, f32) {
    let denom = u - 4.0 * v + 2.0;
    if denom.abs() < 1e-9 {
        return D50_XY;
    }
    (1.5 * u / denom, v / denom)
}

/// A white point as a chromaticity, converted to the temperature and tint that
/// name it. Tint is the signed distance from the Planckian locus along the
/// isotherm, so a point exactly on the locus has tint zero.
pub fn xy_to_temp_tint(x: f32, y: f32) -> (f32, f32) {
    let (u, v) = xy_to_uv(x, y);

    let mut last_dt = 0.0f32;
    let mut last_du = 0.0f32;
    let mut last_dv = 0.0f32;

    for index in 1..ROBERTSON.len() {
        let (r_i, u_i, v_i, t_i) = ROBERTSON[index];

        // The isotherm direction at this locus point, unit length.
        let len = (1.0 + t_i * t_i).sqrt();
        let mut du = 1.0 / len;
        let mut dv = t_i / len;

        // Signed distance from the isotherm to the sample.
        let mut dt = -(u - u_i) * dv + (v - v_i) * du;

        let last_index = index == ROBERTSON.len() - 1;
        if dt <= 0.0 || last_index {
            // The sample lies between this isotherm and the previous one.
            if dt > 0.0 {
                dt = 0.0;
            }
            dt = -dt;

            let f = if index == 1 {
                0.0
            } else {
                dt / (last_dt + dt).max(1e-12)
            };

            let (r_prev, u_prev, v_prev, _) = ROBERTSON[index - 1];
            let temperature = 1.0e6 / (r_prev * f + r_i * (1.0 - f)).max(1e-6);

            // Offset from the interpolated locus point, measured along the
            // interpolated isotherm: that distance is the tint.
            let uu = u - (u_prev * f + u_i * (1.0 - f));
            let vv = v - (v_prev * f + v_i * (1.0 - f));

            du = du * (1.0 - f) + last_du * f;
            dv = dv * (1.0 - f) + last_dv * f;
            let len = (du * du + dv * dv).sqrt().max(1e-12);
            du /= len;
            dv /= len;

            let tint = (uu * du + vv * dv) * TINT_SCALE;

            return (
                temperature.clamp(MIN_KELVIN, MAX_KELVIN),
                tint.clamp(MIN_TINT, MAX_TINT),
            );
        }

        last_dt = dt;
        last_du = du;
        last_dv = dv;
    }

    (MIN_KELVIN, 0.0)
}

/// The inverse: a temperature and tint back to the chromaticity they name.
pub fn temp_tint_to_xy(temperature: f32, tint: f32) -> (f32, f32) {
    let temperature = temperature.clamp(MIN_KELVIN, MAX_KELVIN);
    let r = 1.0e6 / temperature;
    let offset = tint.clamp(MIN_TINT, MAX_TINT) / TINT_SCALE;

    for index in 0..ROBERTSON.len() - 1 {
        let (r_i, u_i, v_i, t_i) = ROBERTSON[index];
        let (r_n, u_n, v_n, t_n) = ROBERTSON[index + 1];

        if r < r_n || index == ROBERTSON.len() - 2 {
            let span = r_n - r_i;
            let f = if span.abs() < 1e-9 {
                0.0
            } else {
                ((r_n - r) / span).clamp(0.0, 1.0)
            };

            let mut u = u_i * f + u_n * (1.0 - f);
            let mut v = v_i * f + v_n * (1.0 - f);

            let len1 = (1.0 + t_i * t_i).sqrt();
            let len2 = (1.0 + t_n * t_n).sqrt();
            let uu = (1.0 / len1) * f + (1.0 / len2) * (1.0 - f);
            let vv = (t_i / len1) * f + (t_n / len2) * (1.0 - f);
            let len3 = (uu * uu + vv * vv).sqrt().max(1e-12);

            u += (uu / len3) * offset;
            v += (vv / len3) * offset;

            return uv_to_xy(u, v);
        }
    }

    D50_XY
}

/// The temperature Adobe assigns to each DNG calibration illuminant code.
/// Matches `dng_camera_profile::IlluminantToTemperature`, so a profile
/// interpolates at the same place theirs does.
pub fn illuminant_temperature(illuminant: Illuminant) -> f32 {
    match illuminant {
        Illuminant::A | Illuminant::Tungsten => 2850.0,
        Illuminant::IsoStudioTungsten => 3200.0,
        Illuminant::WhiteFluorescent => 3450.0,
        Illuminant::CoolWhiteFluorescent | Illuminant::Fluorescent => 4200.0,
        Illuminant::D50 | Illuminant::DaylightWhiteFluorescent => 5000.0,
        Illuminant::D55
        | Illuminant::B
        | Illuminant::Daylight
        | Illuminant::FineWeather
        | Illuminant::Flash => 5500.0,
        Illuminant::DaylightFluorescent => 6400.0,
        Illuminant::D65 | Illuminant::C | Illuminant::CloudyWeather => 6500.0,
        Illuminant::D75 | Illuminant::Shade => 7500.0,
        Illuminant::Unknown => 5000.0,
    }
}

// ---------------------------------------------------------------------------
// The profile itself.
// ---------------------------------------------------------------------------

/// One illuminant's calibration.
#[derive(Debug, Clone, Copy)]
pub struct Calibration {
    pub illuminant: Illuminant,
    /// Kelvin, from the illuminant code.
    pub temperature: f32,
    /// `ColorMatrix`: CIE XYZ (D50) to camera RGB.
    pub color_matrix: Matrix3,
    /// `ForwardMatrix`: white-balanced camera RGB to CIE XYZ (D50). Optional in
    /// the spec, but present in Adobe-converted DNGs and better than inverting
    /// the colour matrix, because Adobe fits it to hold the neutral axis exact.
    pub forward_matrix: Option<Matrix3>,
    /// `CameraCalibration`: per-body variation, almost always identity.
    pub camera_calibration: Matrix3,
}

/// Everything a file carries about how its sensor sees colour.
#[derive(Debug, Clone)]
pub struct CameraProfile {
    /// One or two entries, always ordered coolest illuminant first.
    pub calibrations: Vec<Calibration>,
    /// `AsShotNeutral`: the camera-space colour the camera considered neutral.
    pub as_shot_neutral: [f32; 3],
    /// `AnalogBalance`, identity unless the file says otherwise.
    pub analog_balance: [f32; 3],
}

impl CameraProfile {
    /// Reads the profile out of a decoded `RawImage` plus the file's own TIFF.
    ///
    /// Two sources because neither is complete on its own. `RawImage` carries
    /// the colour matrices for every format `rawler` supports, including the
    /// ones it holds in its internal camera database rather than in the file.
    /// The TIFF carries `ForwardMatrix`, `CameraCalibration` and
    /// `AnalogBalance`, which `rawler` parses only when writing a DNG and never
    /// exposes on `RawImage`. Verified against real converted Z9 DNGs by the
    /// env-gated test below.
    pub fn from_raw(raw: &RawImage, file_bytes: &[u8]) -> Option<Self> {
        let tiff = GenericTiffReader::new_with_buffer(file_bytes, 0, 0, None).ok();

        let forward = tiff
            .as_ref()
            .map(read_forward_matrices)
            .unwrap_or([None, None]);
        let calibration_matrices = tiff
            .as_ref()
            .map(read_camera_calibrations)
            .unwrap_or([None, None]);
        let analog_balance = tiff
            .as_ref()
            .and_then(read_analog_balance)
            .unwrap_or([1.0, 1.0, 1.0]);

        // Which ColorMatrix slot an illuminant came from decides which
        // ForwardMatrix pairs with it, and `rawler` drops that ordering when it
        // keys its map by illuminant. Recover it from the file when we can.
        let slots = tiff.as_ref().map(read_illuminant_slots).unwrap_or_default();

        let mut calibrations: Vec<Calibration> = Vec::new();
        for (index, (illuminant, flat)) in raw.color_matrix.iter().enumerate() {
            let Some(color_matrix) = flat_to_matrix3(flat) else {
                continue;
            };
            let slot = slots.get(illuminant).copied().unwrap_or(index.min(1));

            calibrations.push(Calibration {
                illuminant: *illuminant,
                temperature: illuminant_temperature(*illuminant),
                color_matrix,
                forward_matrix: forward[slot],
                camera_calibration: calibration_matrices[slot].unwrap_or(IDENTITY),
            });
        }

        if calibrations.is_empty() {
            return None;
        }
        calibrations.sort_by(|a, b| {
            a.temperature
                .partial_cmp(&b.temperature)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        calibrations.truncate(2);

        // `rawler` stores white balance as multipliers; AsShotNeutral is their
        // reciprocal. Reading it back this way rather than from the TIFF keeps
        // non-DNG formats working, where the multipliers come from maker notes.
        let as_shot_neutral = neutral_from_wb_coeffs(raw.wb_coeffs)?;

        Some(Self {
            calibrations,
            as_shot_neutral,
            analog_balance,
        })
    }

    /// `AB * CC * CM` for a given white point: CIE XYZ (D50) to camera RGB.
    pub fn xyz_to_camera(&self, white_xy: (f32, f32)) -> Matrix3 {
        let weight = self.interpolation_weight(white_xy);
        let first = &self.calibrations[0];

        let (color_matrix, camera_calibration) = match self.calibrations.get(1) {
            Some(second) => (
                mat_lerp(&first.color_matrix, &second.color_matrix, weight),
                mat_lerp(
                    &first.camera_calibration,
                    &second.camera_calibration,
                    weight,
                ),
            ),
            None => (first.color_matrix, first.camera_calibration),
        };

        let ab = mat_diag(self.analog_balance);
        mat_mul(&ab, &mat_mul(&camera_calibration, &color_matrix))
    }

    /// Weight of the cooler calibration, interpolated in reciprocal Kelvin.
    /// The DNG spec is specific about the reciprocal: mireds are perceptually
    /// even where Kelvin is not, so a straight Kelvin lerp would skew warm.
    fn interpolation_weight(&self, white_xy: (f32, f32)) -> f32 {
        let Some(second) = self.calibrations.get(1) else {
            return 1.0;
        };
        let first = &self.calibrations[0];
        let (temperature, _) = xy_to_temp_tint(white_xy.0, white_xy.1);

        if temperature <= first.temperature {
            return 1.0;
        }
        if temperature >= second.temperature {
            return 0.0;
        }

        let inv = 1.0 / temperature;
        let inv_first = 1.0 / first.temperature;
        let inv_second = 1.0 / second.temperature;
        ((inv - inv_second) / (inv_first - inv_second)).clamp(0.0, 1.0)
    }

    /// The camera-space neutral for a chosen white point, scaled so its largest
    /// channel is 1. This is what `AsShotNeutral` would have been had the camera
    /// picked this white.
    pub fn camera_neutral(&self, white_xy: (f32, f32)) -> [f32; 3] {
        let xyz_to_cam = self.xyz_to_camera(white_xy);
        let neutral = mat_apply(&xyz_to_cam, xy_to_xyz(white_xy.0, white_xy.1));

        let max = neutral[0].max(neutral[1]).max(neutral[2]);
        let scale = if max > 1e-9 { 1.0 / max } else { 1.0 };
        [
            (neutral[0] * scale).clamp(0.001, 1.0),
            (neutral[1] * scale).clamp(0.001, 1.0),
            (neutral[2] * scale).clamp(0.001, 1.0),
        ]
    }

    /// The white point that would have produced a given camera neutral.
    ///
    /// There is no closed form: the matrix depends on the temperature, and the
    /// temperature is what we are solving for. The spec's answer is to iterate,
    /// and it settles in a handful of passes.
    pub fn neutral_to_xy(&self, neutral: [f32; 3]) -> (f32, f32) {
        const MAX_PASSES: usize = 30;
        let mut last = D50_XY;

        for pass in 0..MAX_PASSES {
            let xyz_to_cam = self.xyz_to_camera(last);
            let Some(cam_to_xyz) = mat_invert(&xyz_to_cam) else {
                return last;
            };
            let mut next = xyz_to_xy(mat_apply(&cam_to_xyz, neutral));

            if (next.0 - last.0).abs() + (next.1 - last.1).abs() < 1e-6 {
                return next;
            }
            // Failing to converge means oscillating between two answers rather
            // than diverging, so the midpoint is the honest one to return.
            if pass == MAX_PASSES - 1 {
                next = ((last.0 + next.0) * 0.5, (last.1 + next.1) * 0.5);
            }
            last = next;
        }

        last
    }

    /// The temperature and tint the camera itself chose.
    pub fn as_shot_temp_tint(&self) -> (f32, f32) {
        let (x, y) = self.neutral_to_xy(self.as_shot_neutral);
        xy_to_temp_tint(x, y)
    }

    /// The forward matrix for a white point, interpolated when both slots have
    /// one and falling back to whichever single slot does.
    fn forward_matrix_for(&self, weight: f32) -> Option<Matrix3> {
        let first = self.calibrations[0].forward_matrix;
        let second = self.calibrations.get(1).and_then(|c| c.forward_matrix);
        match (first, second) {
            (Some(a), Some(b)) => Some(mat_lerp(&a, &b, weight)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    fn camera_calibration_for(&self, weight: f32) -> Matrix3 {
        let first = &self.calibrations[0];
        match self.calibrations.get(1) {
            Some(second) => mat_lerp(
                &first.camera_calibration,
                &second.camera_calibration,
                weight,
            ),
            None => first.camera_calibration,
        }
    }

    /// Camera RGB to CIE XYZ (D50) for a chosen white point. The heart of it.
    ///
    /// With a forward matrix the spec is `FM * D * Inverse(AB * CC)`, where `D`
    /// undoes the reference neutral so that the chosen white lands exactly on
    /// D50. Without one, the colour matrix is inverted and the result
    /// chromatically adapted to D50 instead.
    pub fn camera_to_xyz_d50(&self, white_xy: (f32, f32)) -> Option<Matrix3> {
        // Normalised, and it has to be. The reference neutral is inverted into
        // the matrix, so scaling it scales the whole matrix: leaving it raw
        // makes the matrix brighter or darker depending on the temperature, and
        // the slider becomes a brightness control as well as a colour one.
        // Measured at 17.9% across a single sweep before this was pinned.
        // Adobe normalises to a largest channel of 1 for the same reason.
        let camera_neutral = self.camera_neutral(white_xy);

        let weight = self.interpolation_weight(white_xy);
        let ab = mat_diag(self.analog_balance);
        let ab_cc = mat_mul(&ab, &self.camera_calibration_for(weight));
        let ab_cc_inv = mat_invert(&ab_cc)?;

        match self.forward_matrix_for(weight) {
            Some(fm) => {
                let reference_neutral = mat_apply(&ab_cc_inv, camera_neutral);
                if reference_neutral
                    .iter()
                    .any(|c| !c.is_finite() || *c < 1e-9)
                {
                    return None;
                }
                let d = mat_diag([
                    1.0 / reference_neutral[0],
                    1.0 / reference_neutral[1],
                    1.0 / reference_neutral[2],
                ]);
                Some(mat_mul(&fm, &mat_mul(&d, &ab_cc_inv)))
            }
            None => {
                // No forward matrix, so invert the colour matrix and adapt the
                // result to D50 instead. The neutral plays no part here, which
                // is why this branch needs the matrix itself.
                let cam_to_xyz = mat_invert(&self.xyz_to_camera(white_xy))?;
                let adapt = map_white_matrix(white_xy, D50_XY);
                Some(mat_mul(&adapt, &cam_to_xyz))
            }
        }
    }

    /// Camera RGB straight to the linear sRGB the rest of the pipeline works in.
    pub fn camera_to_linear_srgb(&self, white_xy: (f32, f32)) -> Option<Matrix3> {
        let cam_to_xyz = self.camera_to_xyz_d50(white_xy)?;
        Some(mat_mul(&XYZ_D50_TO_LINEAR_SRGB, &cam_to_xyz))
    }

    /// Same, named by temperature and tint rather than chromaticity.
    pub fn camera_to_linear_srgb_at(&self, temperature: f32, tint: f32) -> Option<Matrix3> {
        self.camera_to_linear_srgb(temp_tint_to_xy(temperature, tint))
    }

    /// Whether any calibration carried a forward matrix. Without one the result
    /// is still correct, just derived the longer way round.
    pub fn has_forward_matrix(&self) -> bool {
        self.calibrations.iter().any(|c| c.forward_matrix.is_some())
    }

    /// The multipliers for the white the camera itself chose.
    pub fn as_shot_multipliers(&self) -> [f32; 3] {
        let min = self
            .as_shot_neutral
            .iter()
            .fold(f32::INFINITY, |a, b| a.min(*b));
        let max = self
            .as_shot_neutral
            .iter()
            .fold(f32::NEG_INFINITY, |a, b| a.max(*b));
        if !(min > 1e-6) || !max.is_finite() {
            return [1.0, 1.0, 1.0];
        }
        // Reciprocal of a neutral already normalised to max 1, so the smallest
        // multiplier comes out at exactly 1.
        [
            1.0 / self.as_shot_neutral[0],
            1.0 / self.as_shot_neutral[1],
            1.0 / self.as_shot_neutral[2],
        ]
    }

    /// The as-shot white point, found by inverting the camera's own neutral.
    pub fn as_shot_xy(&self) -> (f32, f32) {
        self.neutral_to_xy(self.as_shot_neutral)
    }

    /// Camera RGB to linear sRGB at the as-shot white, with the white balance
    /// multipliers divided back out.
    ///
    /// The decode applies those multipliers to the mosaic before demosaicing,
    /// which is where they belong. This matrix therefore has to expect data
    /// that is already balanced, so the scaling the DNG recipe folds into
    /// `camera_to_xyz_d50` would otherwise be applied twice.
    pub fn balanced_camera_to_linear_srgb_as_shot(&self) -> Option<Matrix3> {
        let full = self.camera_to_linear_srgb(self.as_shot_xy())?;
        let multipliers = self.as_shot_multipliers();
        Some(mat_mul(
            &full,
            &mat_diag([
                1.0 / multipliers[0],
                1.0 / multipliers[1],
                1.0 / multipliers[2],
            ]),
        ))
    }

    /// The matrix that carries an already-rendered as-shot image to a different
    /// white balance.
    ///
    /// The decode bakes in `A_as`, the camera-to-sRGB matrix at the as-shot
    /// white. Rendering at some other white wants `A_t`. Since both are plain
    /// linear maps of the same camera data, `A_t * inverse(A_as)` gets there
    /// exactly, with no need to decode the file again. That is what makes a
    /// live Kelvin slider possible at all.
    /// The matrix that carries an image already rendered at one white balance
    /// to another.
    ///
    /// `relative_correction` is this with the source fixed at as-shot, which is
    /// what a fresh decode gives. The picture on disk in `.blitzraw-previews`
    /// is not that: it was rendered at whatever Kelvin the photo was on when it
    /// was written. Nudging Kelvin on top of it therefore has to start from
    /// there, or the nudge lands on a base it was not measured from and the
    /// colour walks.
    ///
    /// Exact, for the same reason the as-shot version is: both renders are
    /// linear maps of the same sensor data, so one composed with the inverse of
    /// the other is the difference between them and nothing else.
    pub fn correction_between(
        &self,
        from_kelvin: f32,
        from_tint: f32,
        to_kelvin: f32,
        to_tint: f32,
    ) -> Option<Matrix3> {
        let from = self.camera_to_linear_srgb_at(from_kelvin, from_tint)?;
        let to = self.camera_to_linear_srgb_at(to_kelvin, to_tint)?;
        let correction = mat_mul(&to, &mat_invert(&from)?);
        if correction.iter().flatten().any(|v| !v.is_finite()) {
            return None;
        }
        Some(correction)
    }

    pub fn relative_correction(&self, kelvin: f32, tint: f32) -> Option<Matrix3> {
        let as_shot = self.camera_to_linear_srgb(self.as_shot_xy())?;
        let target = self.camera_to_linear_srgb_at(kelvin, tint)?;
        let as_shot_inverse = mat_invert(&as_shot)?;
        let correction = mat_mul(&target, &as_shot_inverse);
        if correction.iter().flatten().any(|v| !v.is_finite()) {
            return None;
        }
        Some(correction)
    }
}

// ---------------------------------------------------------------------------
// Carrying a profile onto a file that has none of its own.
// ---------------------------------------------------------------------------

/// A profile in a form that survives being written to a sidecar.
///
/// Kept separate from `CameraProfile` rather than derived on it, because the
/// illuminant is a `rawler` type we do not control and this file has to stay
/// readable by a future version. Storing the DNG illuminant code keeps it
/// self-describing.
///
/// Exists for merge outputs. An HDR merge produces a TIFF, which carries no
/// calibration of its own, but its pixels are the as-shot render of frames that
/// did. The white balance maths applies to it unchanged, provided the profile
/// travels with it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredProfile {
    pub calibrations: Vec<StoredCalibration>,
    pub as_shot_neutral: [f32; 3],
    pub analog_balance: [f32; 3],
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredCalibration {
    /// The DNG calibration illuminant code, as written in the file.
    pub illuminant: u16,
    pub temperature: f32,
    pub color_matrix: Matrix3,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward_matrix: Option<Matrix3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_calibration: Option<Matrix3>,
}

impl From<&CameraProfile> for StoredProfile {
    fn from(profile: &CameraProfile) -> Self {
        Self {
            calibrations: profile
                .calibrations
                .iter()
                .map(|c| StoredCalibration {
                    illuminant: c.illuminant as u16,
                    temperature: c.temperature,
                    color_matrix: c.color_matrix,
                    forward_matrix: c.forward_matrix,
                    camera_calibration: if c.camera_calibration == IDENTITY {
                        None
                    } else {
                        Some(c.camera_calibration)
                    },
                })
                .collect(),
            as_shot_neutral: profile.as_shot_neutral,
            analog_balance: profile.analog_balance,
        }
    }
}

impl StoredProfile {
    pub fn to_profile(&self) -> Option<CameraProfile> {
        if self.calibrations.is_empty() {
            return None;
        }
        let calibrations = self
            .calibrations
            .iter()
            .map(|c| Calibration {
                illuminant: Illuminant::try_from(c.illuminant).unwrap_or(Illuminant::Unknown),
                temperature: c.temperature,
                color_matrix: c.color_matrix,
                forward_matrix: c.forward_matrix,
                camera_calibration: c.camera_calibration.unwrap_or(IDENTITY),
            })
            .collect();
        Some(CameraProfile {
            calibrations,
            as_shot_neutral: self.as_shot_neutral,
            analog_balance: self.analog_balance,
        })
    }
}

// ---------------------------------------------------------------------------
// Remembering profiles between decode and render.
// ---------------------------------------------------------------------------

/// Profiles found during decode, keyed by file path.
///
/// The decode knows the profile; the render knows the adjustments. Nothing in
/// RapidRAW carries both, and threading one through would mean changing every
/// call site that builds adjustments. Keeping a small map here is the additive
/// way to join them, and a profile is only a handful of matrices.
static PROFILES: std::sync::OnceLock<std::sync::Mutex<HashMap<String, Option<CameraProfile>>>> =
    std::sync::OnceLock::new();

fn profiles() -> &'static std::sync::Mutex<HashMap<String, Option<CameraProfile>>> {
    PROFILES.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Bound so a long library session cannot grow the map without limit. Profiles
/// are small, but a catalogue can hold hundreds of thousands of files.
const MAX_REMEMBERED: usize = 512;

/// Strips the virtual copy suffix, so every caller keys the same file the same
/// way.
///
/// A virtual copy is addressed as `<real path>?vc=<id>`, and the two ends of
/// this map see different halves of that: the decode is handed the real path
/// it read bytes from, while the render is handed whichever path the user is
/// looking at. Normalising here rather than at each call site is the only way
/// to be sure they agree. They did not, and a virtual copy would have rendered
/// with different colour from its original.
///
/// Copies share a profile by definition: it is one file on disk.
fn profile_key(path: &str) -> &str {
    match path.rsplit_once("?vc=") {
        Some((source, _)) => source,
        None => path,
    }
}

pub fn remember(path: &str, profile: Option<CameraProfile>) {
    let key = profile_key(path);
    if let Ok(mut map) = profiles().lock() {
        if map.len() >= MAX_REMEMBERED && !map.contains_key(key) {
            // Nothing here deserves a real eviction policy: any profile can be
            // read again from the file it came from.
            map.clear();
        }
        map.insert(key.to_string(), profile);
    }
}

/// Remembers what a decode found, without overruling a profile that was
/// recorded.
///
/// A decode reads the calibration a file states about itself, which for an
/// ordinary RAW is the only answer there is. A derived file is different:
/// `hdr_dng` writes tags describing its own pixels, sRGB primaries and a
/// neutral white, while the calibration that gives its Kelvin slider a meaning
/// is the one `inherit_profile` put in the sidecar.
///
/// Letting the decode win put the file's own answer in the map, so a merge
/// opened at 6500K instead of the 4772K the scene was shot at, but only for the
/// rest of the session that made it: a restart emptied the map and `profile_for`
/// read the sidecar instead. That is the wrong way round from the failure this
/// was expected to have, and it is why it survived a fix aimed at the other one.
///
/// The sidecar is consulted rather than the map so the answer does not depend
/// on whether a merge was made in this session or found on disk in the next.
pub fn remember_decoded(path: &str, profile: Option<CameraProfile>) {
    let source = profile_key(path);
    let recorded =
        crate::exif_processing::read_camera_profile_sidecar(std::path::Path::new(source))
            .and_then(|stored| stored.to_profile());
    remember(source, recorded.or(profile));
}

/// Reads a profile straight from a file's bytes, without decoding any pixels.
/// Used when something needs the calibration but not the image.
pub fn profile_from_bytes(bytes: &[u8]) -> Option<CameraProfile> {
    use rawler::decoders::RawDecodeParams;
    use rawler::rawsource::RawSource;

    let source = RawSource::new_from_slice(bytes);
    let decoder = rawler::get_decoder(&source).ok()?;
    let raw = decoder
        .raw_image(&source, &RawDecodeParams::default(), true)
        .ok()?;
    CameraProfile::from_raw(&raw, bytes)
}

/// The profile for a file, from wherever it can be found.
///
/// Three places, in order of cost. The decode usually put one in the map
/// already. Failing that, a RAW carries its own and can be read for it. Failing
/// that, a derived file such as an HDR merge may have inherited one into its
/// sidecar: the merge output is a TIFF with no calibration of its own, but its
/// pixels are the as-shot render of frames that had one, so the same maths
/// applies to it unchanged.
///
/// The answer is remembered either way, including a negative one, so the file
/// is only opened once.
pub fn profile_for(path: &str) -> Option<CameraProfile> {
    if let Ok(map) = profiles().lock()
        && let Some(known) = map.get(profile_key(path))
    {
        return known.clone();
    }

    let source = profile_key(path);

    // A recorded profile wins over whatever the file says about itself.
    //
    // Only a derived file has one, put there by `inherit_profile` so a merge
    // keeps the calibration of the frames it was made from. Ordinary raws have
    // none and fall straight through to the file, which is where theirs lives.
    //
    // The order used to be the other way round, and it only started to matter
    // when merges became DNGs. A merge is a raw file by extension now, so it
    // was read as one: the answer came back as the sRGB primaries and neutral
    // white that `hdr_dng` writes to describe its own pixels, rather than the
    // camera calibration it inherited. The Kelvin slider would then have opened
    // a 4772K interior at 6500K, but only after a restart, since the in-memory
    // map above still held the right answer for the rest of the session.
    let found = crate::exif_processing::read_camera_profile_sidecar(std::path::Path::new(source))
        .and_then(|stored| stored.to_profile())
        .or_else(|| {
            if crate::formats::is_raw_file(source) {
                std::fs::read(source)
                    .ok()
                    .and_then(|bytes| profile_from_bytes(&bytes))
            } else {
                None
            }
        });

    remember(source, found.clone());
    found
}

/// Hands a derived file the calibration of the RAW it was made from.
///
/// Called when a merge writes its output. The profile goes to the sidecar so it
/// survives a restart, and into the map so the image that is about to appear on
/// screen already has it.
pub fn inherit_profile(source_path: &str, derived_path: &std::path::Path) {
    let Some(profile) = profile_for(source_path) else {
        return;
    };
    let stored = StoredProfile::from(&profile);
    if let Err(e) = crate::exif_processing::write_camera_profile_sidecar(derived_path, &stored) {
        log::warn!(
            "Could not record the camera profile for {}: {e}",
            derived_path.display()
        );
    }
    remember(&derived_path.to_string_lossy(), Some(profile));
}

/// What the white balance controls resolve to for one image.
#[derive(Debug, Clone, Copy)]
pub struct WhiteBalance {
    pub kelvin: f32,
    pub tint: f32,
}

/// Reads the white balance out of an adjustments object, defaulting to the
/// white the camera chose. Absent means as-shot, which is why the fields are
/// optional rather than zero-defaulted: zero Kelvin is not a white balance.
pub fn white_balance_from_json(
    js_adjustments: &serde_json::Value,
    profile: &CameraProfile,
) -> WhiteBalance {
    let (as_shot_kelvin, as_shot_tint) = profile.as_shot_temp_tint();
    let section = js_adjustments.get("whiteBalance");

    let read = |key: &str| -> Option<f32> {
        section?
            .get(key)
            .and_then(|v| v.as_f64())
            .map(|v| v as f32)
            .filter(|v| v.is_finite())
    };

    WhiteBalance {
        kelvin: read("kelvin")
            .unwrap_or(as_shot_kelvin)
            .clamp(MIN_KELVIN, MAX_KELVIN),
        tint: read("tint")
            .unwrap_or(as_shot_tint)
            .clamp(MIN_TINT, MAX_TINT),
    }
}

// ---------------------------------------------------------------------------
// Reading the tags rawler does not surface.
// ---------------------------------------------------------------------------

fn flat_to_matrix3(flat: &[f32]) -> Option<Matrix3> {
    if flat.len() < 9 {
        return None;
    }
    let mut m = [[0.0f32; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            let v = flat[r * 3 + c];
            if !v.is_finite() {
                return None;
            }
            m[r][c] = v;
        }
    }
    Some(m)
}

fn read_matrix3(tiff: &GenericTiffReader, tag: DngTag) -> Option<Matrix3> {
    let entry = tiff
        .get_entry(tag)
        .or_else(|| tiff.root_ifd().get_entry_recursive(tag))?;
    if entry.count() < 9 {
        return None;
    }
    let flat: Vec<f32> = (0..9).map(|i| entry.force_f32(i)).collect();
    flat_to_matrix3(&flat)
}

fn read_forward_matrices(tiff: &GenericTiffReader) -> [Option<Matrix3>; 2] {
    [
        read_matrix3(tiff, DngTag::ForwardMatrix1),
        read_matrix3(tiff, DngTag::ForwardMatrix2),
    ]
}

fn read_camera_calibrations(tiff: &GenericTiffReader) -> [Option<Matrix3>; 2] {
    [
        read_matrix3(tiff, DngTag::CameraCalibration1),
        read_matrix3(tiff, DngTag::CameraCalibration2),
    ]
}

fn read_analog_balance(tiff: &GenericTiffReader) -> Option<[f32; 3]> {
    let entry = tiff
        .get_entry(DngTag::AnalogBalance)
        .or_else(|| tiff.root_ifd().get_entry_recursive(DngTag::AnalogBalance))?;
    if entry.count() < 3 {
        return None;
    }
    let v = [entry.force_f32(0), entry.force_f32(1), entry.force_f32(2)];
    if v.iter().any(|c| !c.is_finite() || *c <= 0.0) {
        return None;
    }
    Some(v)
}

/// Which `ColorMatrix` slot each illuminant occupies, so a `ForwardMatrix` can
/// be paired with the right one.
fn read_illuminant_slots(tiff: &GenericTiffReader) -> HashMap<Illuminant, usize> {
    let mut slots = HashMap::new();
    let pairs = [
        (DngTag::CalibrationIlluminant1, 0usize),
        (DngTag::CalibrationIlluminant2, 1usize),
    ];
    for (tag, slot) in pairs {
        if let Some(entry) = tiff
            .get_entry(tag)
            .or_else(|| tiff.root_ifd().get_entry_recursive(tag))
            && let Ok(illuminant) = Illuminant::try_from(entry.force_u16(0))
        {
            slots.insert(illuminant, slot);
        }
    }
    slots
}

/// `rawler` normalises white balance to multipliers; the neutral is their
/// reciprocal, rescaled so the largest channel is 1.
fn neutral_from_wb_coeffs(coeffs: [f32; 4]) -> Option<[f32; 3]> {
    let mut neutral = [0.0f32; 3];
    for i in 0..3 {
        let c = coeffs[i];
        if !c.is_finite() || c <= 0.0 {
            return None;
        }
        neutral[i] = 1.0 / c;
    }
    let max = neutral[0].max(neutral[1]).max(neutral[2]);
    if max <= 1e-9 {
        return None;
    }
    Some([neutral[0] / max, neutral[1] / max, neutral[2] / max])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(a: f32, b: f32, tol: f32, what: &str) {
        assert!(
            (a - b).abs() <= tol,
            "{what}: expected {b}, got {a} (tolerance {tol})"
        );
    }

    #[test]
    fn d50_white_maps_to_neutral_srgb() {
        let white = xy_to_xyz(D50_XY.0, D50_XY.1);
        let rgb = mat_apply(&XYZ_D50_TO_LINEAR_SRGB, white);
        for channel in rgb {
            assert_close(channel, 1.0, 1e-3, "D50 white through XYZ to sRGB");
        }
    }

    /// A virtual copy is addressed as `<real path>?vc=<id>`, and the decode
    /// and the render are handed different halves of that. If the key is not
    /// normalised the lookup misses and the copy silently renders with
    /// different colour from its original.
    #[test]
    fn a_virtual_copy_shares_its_original_profile() {
        assert_eq!(
            profile_key("D:/photo/_DSC1.dng?vc=abc123"),
            "D:/photo/_DSC1.dng"
        );
        assert_eq!(profile_key("D:/photo/_DSC1.dng"), "D:/photo/_DSC1.dng");
        // A file whose own name contains the marker still resolves to itself
        // when there is no suffix after it.
        assert_eq!(profile_key(""), "");

        let profile = srgb_like_profile();
        remember("D:/photo/_DSC2.dng", Some(profile));
        assert!(
            profile_for("D:/photo/_DSC2.dng?vc=copy1").is_some(),
            "a virtual copy should find its original's profile"
        );
        assert!(
            profile_for("D:/photo/_DSC2.dng").is_some(),
            "the original should still resolve directly"
        );
    }

    /// A merge describes its own pixels, and that description is not the
    /// calibration its Kelvin slider needs.
    ///
    /// `hdr_dng` writes sRGB primaries and a neutral white, because that is
    /// honestly what the samples are. Decoding the merge therefore reports a
    /// profile that would put a 4772K interior at daylight. The one that counts
    /// is the camera calibration `inherit_profile` recorded beside it, and the
    /// decode must not push its own answer over the top.
    ///
    /// This only ever showed itself after the session that made the merge: the
    /// map was cleared by the restart and the sidecar was read instead, so the
    /// file looked right the next morning and wrong the moment it was made.
    #[test]
    fn a_decode_does_not_overrule_the_profile_a_merge_recorded() {
        let dir = std::env::temp_dir().join("blitzraw-profile-precedence");
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let merge = dir.join("_DSC1_Hdr.dng");
        // Only the sidecar beside it is read here, so the file itself can be
        // anything; writing one keeps the path honest.
        std::fs::write(&merge, b"stand-in for a merge").expect("write");

        // What inherit_profile records: the camera's own, off-neutral white.
        let mut recorded = srgb_like_profile();
        recorded.as_shot_neutral = [0.45, 1.0, 0.72];
        crate::exif_processing::write_camera_profile_sidecar(
            &merge,
            &StoredProfile::from(&recorded),
        )
        .expect("record the inherited profile");

        // What decoding the merge reports: the neutral description hdr_dng
        // wrote so the file would describe its own pixels.
        let from_the_file = srgb_like_profile();
        assert_eq!(from_the_file.as_shot_neutral, [1.0, 1.0, 1.0]);

        let path = merge.to_string_lossy().to_string();
        remember_decoded(&path, Some(from_the_file));

        let got = profile_for(&path).expect("a profile");
        assert_eq!(
            got.as_shot_neutral, recorded.as_shot_neutral,
            "the recorded calibration survives the decode that disagrees with it"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An HDR merge is a TIFF with no calibration of its own, so its profile is
    /// written to a sidecar and read back. Anything lost in that round trip
    /// shows up as the merged file rendering differently from its own frames.
    #[test]
    fn a_stored_profile_round_trips_intact() {
        let mut original = srgb_like_profile();
        original.calibrations.push(Calibration {
            illuminant: Illuminant::A,
            temperature: 2850.0,
            color_matrix: [[1.4, -0.9, 0.03], [-0.39, 1.12, 0.3], [0.002, 0.04, 0.85]],
            forward_matrix: Some([[0.42, 0.42, 0.13], [0.17, 0.79, 0.04], [0.04, 0.004, 0.79]]),
            camera_calibration: IDENTITY,
        });
        original.calibrations.sort_by(|a, b| {
            a.temperature
                .partial_cmp(&b.temperature)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        original.as_shot_neutral = [0.5657, 1.0, 0.6564];

        let json = serde_json::to_string(&StoredProfile::from(&original)).expect("serialise");
        let restored: StoredProfile = serde_json::from_str(&json).expect("deserialise");
        let restored = restored.to_profile().expect("a profile survives");

        assert_eq!(restored.calibrations.len(), original.calibrations.len());
        assert_eq!(restored.as_shot_neutral, original.as_shot_neutral);
        for (a, b) in restored
            .calibrations
            .iter()
            .zip(original.calibrations.iter())
        {
            assert_eq!(a.illuminant, b.illuminant, "illuminant code survives");
            assert_eq!(a.color_matrix, b.color_matrix);
            assert_eq!(a.forward_matrix, b.forward_matrix);
            assert_eq!(a.camera_calibration, b.camera_calibration);
        }

        // What matters is that it renders the same, not that the fields match.
        for &kelvin in &[2500.0f32, 4000.0, 6500.0] {
            let before = original.relative_correction(kelvin, 0.0).expect("before");
            let after = restored.relative_correction(kelvin, 0.0).expect("after");
            for row in 0..3 {
                for col in 0..3 {
                    assert_close(
                        after[row][col],
                        before[row][col],
                        1e-6,
                        "restored correction",
                    );
                }
            }
        }
    }

    #[test]
    fn matrix_inverse_round_trips() {
        let m: Matrix3 = [[0.7, 0.2, 0.1], [0.1, 0.8, 0.1], [0.05, 0.15, 0.8]];
        let inv = mat_invert(&m).expect("invertible");
        let product = mat_mul(&m, &inv);
        for r in 0..3 {
            for c in 0..3 {
                let expected = if r == c { 1.0 } else { 0.0 };
                assert_close(product[r][c], expected, 1e-5, "M * inverse(M)");
            }
        }
    }

    #[test]
    fn singular_matrix_has_no_inverse() {
        let singular: Matrix3 = [[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [1.0, 1.0, 1.0]];
        assert!(mat_invert(&singular).is_none());
    }

    /// A point taken from the Planckian locus itself must come back as its own
    /// temperature with no tint, which is the only part of Robertson's method
    /// with an answer known independently of the implementation.
    #[test]
    fn locus_points_round_trip_to_their_own_temperature() {
        for &(r, u, v, _) in ROBERTSON.iter().skip(1) {
            let (x, y) = uv_to_xy(u, v);
            let (temp, tint) = xy_to_temp_tint(x, y);
            let expected = 1.0e6 / r;
            if expected > MAX_KELVIN {
                continue;
            }
            assert_close(temp, expected, expected * 0.01, "locus temperature");
            assert_close(tint, 0.0, 1.0, "locus tint");
        }
    }

    #[test]
    fn temperature_and_tint_round_trip_through_chromaticity() {
        for &temperature in &[2000.0f32, 2850.0, 4000.0, 5000.0, 5500.0, 6500.0, 9000.0] {
            for &tint in &[-60.0f32, -20.0, 0.0, 20.0, 60.0] {
                let (x, y) = temp_tint_to_xy(temperature, tint);
                let (back_temp, back_tint) = xy_to_temp_tint(x, y);
                assert_close(
                    back_temp,
                    temperature,
                    temperature * 0.02,
                    "round-trip temperature",
                );
                assert_close(back_tint, tint, 2.0, "round-trip tint");
            }
        }
    }

    /// D65 sits at 6500K on the locus by construction, so this pins the
    /// convention rather than the arithmetic.
    #[test]
    fn d65_reads_as_roughly_6500k() {
        let (temp, tint) = xy_to_temp_tint(0.31271, 0.32902);
        assert_close(temp, 6500.0, 100.0, "D65 temperature");
        assert!(
            tint.abs() < 10.0,
            "D65 tint should be near zero, got {tint}"
        );
    }

    /// Temperature and tint describe the *illuminant*, not the correction, and
    /// the two run opposite. A positive tint means the light was greener than
    /// the Planckian locus, which is why the correction it produces is magenta.
    /// Getting this backwards would invert the slider, so both halves are
    /// pinned: first the illuminant, then what it does to a pixel.
    ///
    /// The sign was settled against Lightroom rather than reasoned out. Two
    /// "As Shot" sidecars written by Lightroom 18.3 for green-lit indoor
    /// venues report +11 and +2, and the same files compute positive here.
    #[test]
    fn positive_tint_means_green_light_and_a_magenta_correction() {
        let (_, y_neutral) = temp_tint_to_xy(5500.0, 0.0);
        let (_, y_positive) = temp_tint_to_xy(5500.0, 60.0);
        let (_, y_negative) = temp_tint_to_xy(5500.0, -60.0);
        assert!(
            y_positive > y_neutral,
            "positive tint should name a greener illuminant"
        );
        assert!(
            y_negative < y_neutral,
            "negative tint should name a more magenta illuminant"
        );

        // Now the correction. Hold one camera pixel and vary only the tint:
        // claiming greener light must pull green out of the render.
        let profile = srgb_like_profile();
        let pixel = [0.5f32, 0.5, 0.5];
        let render = |tint: f32| {
            let m = profile
                .camera_to_linear_srgb_at(5500.0, tint)
                .expect("conversion exists");
            let out = mat_apply(&m, pixel);
            // Green relative to the other two, so overall exposure drops out.
            out[1] / (out[0] + out[2])
        };
        assert!(
            render(60.0) < render(0.0),
            "positive tint should correct towards magenta"
        );
        assert!(
            render(-60.0) > render(0.0),
            "negative tint should correct towards green"
        );
    }

    /// A synthetic profile whose colour matrix is the real XYZ-to-sRGB matrix.
    /// The camera is then exactly an sRGB device, so every answer is known in
    /// advance and the DNG recipe has nowhere to hide.
    fn srgb_like_profile() -> CameraProfile {
        let xyz_to_srgb = mat_invert(&XYZ_D50_TO_LINEAR_SRGB)
            .map(|_| XYZ_D50_TO_LINEAR_SRGB)
            .expect("invertible");
        CameraProfile {
            calibrations: vec![Calibration {
                illuminant: Illuminant::D50,
                temperature: 5000.0,
                color_matrix: xyz_to_srgb,
                forward_matrix: None,
                camera_calibration: IDENTITY,
            }],
            as_shot_neutral: [1.0, 1.0, 1.0],
            analog_balance: [1.0, 1.0, 1.0],
        }
    }

    #[test]
    fn an_srgb_like_camera_converts_to_itself() {
        let profile = srgb_like_profile();
        let m = profile
            .camera_to_linear_srgb(D50_XY)
            .expect("conversion exists");
        for r in 0..3 {
            for c in 0..3 {
                let expected = if r == c { 1.0 } else { 0.0 };
                assert_close(m[r][c], expected, 2e-2, "sRGB-like camera to sRGB");
            }
        }
    }

    #[test]
    fn a_neutral_camera_pixel_stays_neutral() {
        let profile = srgb_like_profile();
        for &(temperature, tint) in &[(2850.0f32, 0.0f32), (5000.0, 0.0), (6500.0, 20.0)] {
            let m = profile
                .camera_to_linear_srgb_at(temperature, tint)
                .expect("conversion exists");
            // The camera neutral for this white, pushed through, must land on
            // an equal-channel colour: that is what white balance means.
            let neutral = profile.camera_neutral(temp_tint_to_xy(temperature, tint));
            let out = mat_apply(&m, neutral);
            assert_close(out[0], out[1], 1e-2, "neutral R vs G");
            assert_close(out[2], out[1], 1e-2, "neutral B vs G");
        }
    }

    #[test]
    fn neutral_to_xy_inverts_camera_neutral() {
        let profile = srgb_like_profile();
        for &(temperature, tint) in &[(3000.0f32, 0.0f32), (5000.0, 0.0), (7000.0, -30.0)] {
            let xy = temp_tint_to_xy(temperature, tint);
            let neutral = profile.camera_neutral(xy);
            let recovered = profile.neutral_to_xy(neutral);
            assert_close(recovered.0, xy.0, 2e-3, "recovered x");
            assert_close(recovered.1, xy.1, 2e-3, "recovered y");
        }
    }

    #[test]
    fn interpolation_weight_favours_the_nearer_illuminant() {
        let mut profile = srgb_like_profile();
        let cool = profile.calibrations[0];
        profile.calibrations = vec![
            Calibration {
                illuminant: Illuminant::A,
                temperature: 2850.0,
                ..cool
            },
            Calibration {
                illuminant: Illuminant::D65,
                temperature: 6500.0,
                ..cool
            },
        ];

        let at = |k: f32| profile.interpolation_weight(temp_tint_to_xy(k, 0.0));
        assert_close(at(2000.0), 1.0, 1e-4, "below the cool illuminant");
        assert_close(at(9000.0), 0.0, 1e-4, "above the warm illuminant");
        assert!(
            at(3000.0) > at(6000.0),
            "a warmer white should lean on the tungsten calibration"
        );
        // Reciprocal interpolation, not linear: the midpoint in mireds between
        // 2850K and 6500K is about 3968K, and that is where the weight is half.
        assert_close(at(3968.0), 0.5, 0.02, "mired midpoint");
    }

    /// Reads a profile and the camera name straight off a file on disk.
    /// Named apart from the module's `profile_for`, which consults the cache
    /// and the sidecar as well; these tests want the file and nothing else.
    fn read_profile_from_file(path: &std::path::Path) -> Option<(CameraProfile, String)> {
        use rawler::decoders::RawDecodeParams;
        use rawler::rawsource::RawSource;

        let bytes = std::fs::read(path).ok()?;
        let source = RawSource::new_from_slice(&bytes);
        let decoder = rawler::get_decoder(&source).ok()?;
        // `dummy` skips the pixel decode, which is the only part that fails on
        // the High Efficiency NEFs. The calibration is metadata, so it survives.
        let raw = decoder
            .raw_image(&source, &RawDecodeParams::default(), true)
            .ok()?;
        let model = format!("{} {}", raw.clean_make, raw.clean_model);
        CameraProfile::from_raw(&raw, &bytes).map(|p| (p, model))
    }

    /// Reads a Lightroom-written sidecar's Temp and Tint, but only when it was
    /// left on As Shot. Any other setting is the photographer's choice rather
    /// than the camera's, and says nothing about whether we agree.
    fn lightroom_as_shot(xmp: &std::path::Path) -> Option<(f32, f32)> {
        let text = std::fs::read_to_string(xmp).ok()?;
        if !text.contains(r#"crs:WhiteBalance="As Shot""#) {
            return None;
        }
        let field = |name: &str| -> Option<f32> {
            let needle = format!("crs:{name}=\"");
            let start = text.find(&needle)? + needle.len();
            let rest = &text[start..];
            let end = rest.find('"')?;
            rest[..end].trim_start_matches('+').parse::<f32>().ok()
        };
        Some((field("Temperature")?, field("Tint")?))
    }

    /// The one check that can prove the Kelvin readout is not a private
    /// invention: compare it to what Lightroom says about the same file.
    ///
    /// Point `RAPIDRAW_TEST_LIGHTROOM_DIR` at a tree holding camera files
    /// alongside Lightroom sidecars. Only sidecars still on As Shot count.
    /// Skips silently, and skips again if the tree has no As Shot sidecars,
    /// since that is a property of the photographer's editing, not a failure.
    #[test]
    fn kelvin_agrees_with_lightroom_on_as_shot_files() {
        let Ok(dir) = std::env::var("RAPIDRAW_TEST_LIGHTROOM_DIR") else {
            eprintln!("RAPIDRAW_TEST_LIGHTROOM_DIR unset, skipping");
            return;
        };

        let mut compared = 0;
        for entry in walkdir::WalkDir::new(&dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
        {
            let xmp = entry.path();
            if xmp.extension().and_then(|e| e.to_str()) != Some("xmp") {
                continue;
            }
            let Some((lr_temp, lr_tint)) = lightroom_as_shot(xmp) else {
                continue;
            };

            // The sidecar names the capture; find whichever raw sits with it.
            let raw_path = ["NEF", "nef", "DNG", "dng", "CR3", "cr3", "ARW", "arw"]
                .iter()
                .map(|ext| xmp.with_extension(ext))
                .find(|p| p.exists());
            let Some(raw_path) = raw_path else { continue };
            let Some((profile, model)) = read_profile_from_file(&raw_path) else {
                eprintln!("  could not profile {}", raw_path.display());
                continue;
            };

            let (temp, tint) = profile.as_shot_temp_tint();
            let drift = (temp - lr_temp).abs() / lr_temp;
            eprintln!(
                "{}  [{model}]\n  lightroom {lr_temp:.0}K {lr_tint:+.0}   ours {temp:.0}K {tint:+.1}   drift {:.1}%",
                raw_path.display(),
                drift * 100.0
            );
            compared += 1;

            // Lightroom rounds Kelvin to a slider step and applies its own
            // profile, so exact agreement is not the claim. Within a few
            // percent is: it means we are reading the same illuminant, not
            // inventing a number.
            assert!(
                drift < 0.05,
                "temperature disagrees with Lightroom by {:.1}% on {}",
                drift * 100.0,
                raw_path.display()
            );
            assert!(
                (tint - lr_tint).abs() < 10.0,
                "tint disagrees with Lightroom ({tint:.1} vs {lr_tint:.0}) on {}",
                raw_path.display()
            );
            assert_eq!(
                tint.is_sign_positive(),
                lr_tint.is_sign_positive(),
                "tint sign is inverted against Lightroom on {}",
                raw_path.display()
            );
        }

        if compared == 0 {
            eprintln!("no As Shot Lightroom sidecars found under {dir}, skipping");
        } else {
            eprintln!("compared {compared} file(s) against Lightroom");
        }
    }

    /// The eyedropper read cold in use because the geometry warp hands back
    /// `Rgb32F`, not `Rgba32F`, and the sampler only recognised the latter.
    /// Whichever float layout an image arrives in, the same patch has to give
    /// the same answer.
    #[test]
    fn the_same_patch_reads_the_same_in_either_float_layout() {
        use image::{DynamicImage, Rgb32FImage, Rgba32FImage};

        let (w, h) = (16u32, 16u32);
        let colour = [0.0671f32, 0.0673, 0.0697];

        let rgba = DynamicImage::ImageRgba32F(Rgba32FImage::from_fn(w, h, |_, _| {
            image::Rgba([colour[0], colour[1], colour[2], 1.0])
        }));
        let rgb = DynamicImage::ImageRgb32F(Rgb32FImage::from_fn(w, h, |_, _| {
            image::Rgb([colour[0], colour[1], colour[2]])
        }));

        let from_rgba = average_patch(&rgba, 0.5, 0.5).expect("rgba");
        let from_rgb = average_patch(&rgb, 0.5, 0.5).expect("rgb");

        for c in 0..3 {
            assert!(
                (from_rgba[c] - colour[c]).abs() < 1e-6,
                "rgba sampling drifted: {from_rgba:?}"
            );
            assert!(
                (from_rgb[c] - colour[c]).abs() < 1e-6,
                "rgb sampling read {from_rgb:?}, which is what made the picker read cold"
            );
        }
    }

    /// Highlights above 1.0 have to survive the sample. Reading through 8-bit
    /// clipped them to white, which is the brightest thing anyone points an
    /// eyedropper at.
    #[test]
    fn a_highlight_above_one_is_not_clipped_when_sampled() {
        use image::{DynamicImage, Rgb32FImage};

        let bright = [1.8f32, 1.4, 1.2];
        let image = DynamicImage::ImageRgb32F(Rgb32FImage::from_fn(8, 8, |_, _| {
            image::Rgb([bright[0], bright[1], bright[2]])
        }));

        let sampled = average_patch(&image, 0.5, 0.5).expect("sampled");
        for c in 0..3 {
            assert!(
                (sampled[c] - bright[c]).abs() < 1e-6,
                "a highlight was clipped on the way in: {sampled:?}"
            );
        }
    }

    /// The eyedropper reported far too cool a temperature in use. This walks
    /// the exact arithmetic it runs, on a render whose right answer is known:
    /// the as-shot neutral, which must come back as the as-shot temperature.
    #[test]
    fn picking_a_neutral_returns_the_as_shot_temperature() {
        let Ok(path) = std::env::var("RAPIDRAW_TEST_DNG") else {
            eprintln!("RAPIDRAW_TEST_DNG unset, skipping");
            return;
        };
        let path = std::path::PathBuf::from(path);
        if path.is_dir() {
            return;
        }
        let bytes = std::fs::read(&path).expect("read file");
        let profile = profile_from_bytes(&bytes).expect("profile");

        let (as_shot_kelvin, as_shot_tint) = profile.as_shot_temp_tint();
        let as_shot = profile
            .camera_to_linear_srgb(profile.as_shot_xy())
            .expect("matrix");

        // What a perfectly neutral subject looks like in the decoded image.
        let rendered = mat_apply(&as_shot, profile.as_shot_neutral);
        eprintln!("  a neutral renders as {rendered:.4?}");

        // Now the picker arithmetic, verbatim.
        let to_camera = mat_invert(&as_shot).expect("invertible");
        let camera_rgb = mat_apply(&to_camera, rendered);
        let (x, y) = profile.neutral_to_xy(camera_rgb);
        let (kelvin, tint) = xy_to_temp_tint(x, y);

        eprintln!("  as-shot   {as_shot_kelvin:.0}K {as_shot_tint:+.1}");
        eprintln!("  picked    {kelvin:.0}K {tint:+.1}");

        let drift = (kelvin - as_shot_kelvin).abs() / as_shot_kelvin;
        assert!(
            drift < 0.02,
            "picking a neutral gave {kelvin:.0}K where the camera said {as_shot_kelvin:.0}K"
        );
    }

    /// Reads real camera files and reports what calibration they carry.
    /// Skips silently when `RAPIDRAW_TEST_DNG` is unset, which may name either
    /// a single file or a folder to walk.
    #[test]
    fn reads_calibration_from_real_files() {
        use rawler::decoders::RawDecodeParams;
        use rawler::rawsource::RawSource;

        let Ok(target) = std::env::var("RAPIDRAW_TEST_DNG") else {
            eprintln!("RAPIDRAW_TEST_DNG unset, skipping");
            return;
        };

        let target = std::path::PathBuf::from(target);
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        if target.is_dir() {
            for entry in walkdir::WalkDir::new(&target)
                .max_depth(1)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_file())
            {
                let is_dng = entry
                    .path()
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("dng"))
                    .unwrap_or(false);
                if is_dng {
                    files.push(entry.path().to_path_buf());
                }
            }
            files.sort();
            files.truncate(5);
        } else {
            files.push(target);
        }

        assert!(!files.is_empty(), "no files to read");

        for path in &files {
            let bytes = std::fs::read(path).expect("read file");
            let source = RawSource::new_from_slice(&bytes);
            let decoder = rawler::get_decoder(&source).expect("decoder");
            let raw = decoder
                .raw_image(&source, &RawDecodeParams::default(), true)
                .expect("raw image");

            let profile = CameraProfile::from_raw(&raw, &bytes)
                .unwrap_or_else(|| panic!("no camera profile in {}", path.display()));

            let (temperature, tint) = profile.as_shot_temp_tint();
            eprintln!("\n{}", path.display());
            eprintln!("  {} {}", raw.clean_make, raw.clean_model);
            eprintln!("  as-shot neutral {:?}", profile.as_shot_neutral);
            eprintln!("  as-shot {temperature:.0}K tint {tint:.1}");
            eprintln!("  analog balance  {:?}", profile.analog_balance);
            for c in &profile.calibrations {
                eprintln!(
                    "  illuminant {:?} ({:.0}K) forward_matrix={} camera_calibration={}",
                    c.illuminant,
                    c.temperature,
                    c.forward_matrix.is_some(),
                    if c.camera_calibration == IDENTITY {
                        "identity"
                    } else {
                        "present"
                    }
                );
                eprintln!("    color   {:?}", c.color_matrix);
                if let Some(fm) = c.forward_matrix {
                    // The DNG spec requires each forward matrix to carry the
                    // reference neutral to the D50 white, so its rows must sum
                    // to D50's XYZ. If they do, we read the tag correctly.
                    let sums = [
                        fm[0][0] + fm[0][1] + fm[0][2],
                        fm[1][0] + fm[1][1] + fm[1][2],
                        fm[2][0] + fm[2][1] + fm[2][2],
                    ];
                    eprintln!("    forward {fm:?}");
                    eprintln!("    row sums {sums:?} (should be D50 XYZ)");
                    let d50 = xy_to_xyz(D50_XY.0, D50_XY.1);
                    for i in 0..3 {
                        assert_close(sums[i], d50[i], 5e-3, "forward matrix row sum");
                    }
                }
            }

            assert!(
                (2000.0..=12000.0).contains(&temperature),
                "as-shot temperature {temperature} is not plausible for {}",
                path.display()
            );

            // Whatever the camera called neutral must render neutral. This is
            // the single claim the whole feature rests on.
            let m = profile
                .camera_to_linear_srgb_at(temperature, tint)
                .expect("conversion exists");
            let out = mat_apply(&m, profile.as_shot_neutral);
            eprintln!("  as-shot neutral renders as {out:?}");
            assert_close(out[0], out[1], 5e-3, "as-shot neutral R vs G");
            assert_close(out[2], out[1], 5e-3, "as-shot neutral B vs G");
        }
    }
}

// ---------------------------------------------------------------------------
// What the front end needs to draw a Kelvin slider.
// ---------------------------------------------------------------------------

/// The white balance facts about one file, for the UI.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhiteBalanceInfo {
    /// False for anything with no camera calibration to read, such as a JPEG.
    /// The slider stays hidden rather than showing a Kelvin that means nothing.
    pub has_profile: bool,
    pub as_shot_kelvin: f32,
    pub as_shot_tint: f32,
    pub min_kelvin: f32,
    pub max_kelvin: f32,
    pub min_tint: f32,
    pub max_tint: f32,
    /// Whether the camera shipped forward matrices. Purely informational, but
    /// it is the difference between the good path and the fallback.
    pub has_forward_matrix: bool,
}

impl WhiteBalanceInfo {
    fn none() -> Self {
        Self {
            has_profile: false,
            as_shot_kelvin: 5500.0,
            as_shot_tint: 0.0,
            min_kelvin: MIN_KELVIN,
            max_kelvin: MAX_KELVIN,
            min_tint: MIN_TINT,
            max_tint: MAX_TINT,
            has_forward_matrix: false,
        }
    }
}

/// Reports the as-shot white balance for a file.
///
/// Answers from the profile the decode already found where possible. Falls back
/// to reading the file, because the panel can ask before a decode has finished,
/// and reading calibration needs no pixels.
#[tauri::command]
pub fn get_white_balance_info(path: String) -> WhiteBalanceInfo {
    let Some(profile) = profile_for(&path) else {
        return WhiteBalanceInfo::none();
    };
    let (kelvin, tint) = profile.as_shot_temp_tint();

    WhiteBalanceInfo {
        has_profile: true,
        as_shot_kelvin: kelvin,
        as_shot_tint: tint,
        has_forward_matrix: profile.has_forward_matrix(),
        ..WhiteBalanceInfo::none()
    }
}

/// What the eyedropper found.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhiteBalancePick {
    pub kelvin: f32,
    pub tint: f32,
}

/// Solves for the white balance that would make a picked pixel neutral.
///
/// The old picker averaged the pixel off the preview on screen and turned the
/// channel ratios into a guess at the legacy tint. That cannot work here: the
/// preview has been through exposure, tone mapping and everything else, so its
/// numbers say very little about the light in the room.
///
/// This reads the as-shot render instead, which is the sensor data and nothing
/// else, undoes the camera-to-sRGB matrix to recover camera RGB, and asks the
/// profile which illuminant would call that colour neutral. It is the same
/// question `AsShotNeutral` answers for the camera, asked about a pixel the
/// photographer chose. Lightroom's eyedropper works off the raw data for the
/// same reason.
///
/// `u` and `v` are fractions of the displayed image, which is the original
/// after crop and rotation, so the same transformations are applied here to
/// land on the pixel that was actually clicked.
#[tauri::command]
pub fn pick_white_balance(
    state: tauri::State<crate::AppState>,
    u: f32,
    v: f32,
    adjustments: serde_json::Value,
) -> Result<WhiteBalancePick, String> {
    let loaded = state
        .original_image
        .lock()
        .map_err(|_| "Image state is locked".to_string())?
        .clone()
        .ok_or_else(|| "No image loaded".to_string())?;

    let profile = profile_for(&loaded.path)
        .ok_or_else(|| "This file carries no camera calibration".to_string())?;

    let (transformed, _) =
        crate::adjustment_utils::apply_all_transformations(loaded.image.as_ref(), &adjustments);
    let average = average_patch(transformed.as_ref(), u, v)?;

    let as_shot = profile
        .camera_to_linear_srgb(profile.as_shot_xy())
        .ok_or_else(|| "Camera profile has no usable matrix".to_string())?;
    let to_camera =
        mat_invert(&as_shot).ok_or_else(|| "Camera matrix cannot be inverted".to_string())?;

    let camera_rgb = mat_apply(&to_camera, average);
    if camera_rgb.iter().any(|c| !c.is_finite() || *c <= 1e-6) {
        return Err("That area is too dark or clipped to read a white balance from".to_string());
    }

    let (x, y) = profile.neutral_to_xy(camera_rgb);
    let (kelvin, tint) = xy_to_temp_tint(x, y);
    Ok(WhiteBalancePick { kelvin, tint })
}

/// Averages a small square around a point, in linear light.
///
/// A single pixel is noise. Eleven by eleven settles it without spanning a
/// colour boundary at any sensible zoom, and is what the old picker sampled.
///
/// Every variant the pipeline can hand us is **linear**, whatever its depth.
/// That is the part worth stating, because it is what this got wrong: the
/// geometry warp returns `Rgb32F` rather than `Rgba32F`, so asking only for
/// `as_rgba32f` fell through to reading 8-bit and then running an sRGB decode
/// over data that was already linear. On a Z9 frame that exaggerated blue
/// against red by about four percent, which the solver read as bluer light and
/// answered with a colder temperature. Quantisation made it worse: a mid-tone
/// around 0.067 survives as seventeen levels out of 255, so red and green
/// collapsed onto the same number, and anything above 1.0 clipped away
/// entirely. It only showed up with lens correction switched on, since that is
/// what makes the geometry non-identity and sends the image through the warp.
fn average_patch(image: &image::DynamicImage, u: f32, v: f32) -> Result<[f32; 3], String> {
    use image::GenericImageView;

    const RADIUS: i64 = 5;

    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return Err("Image has no pixels".to_string());
    }
    if !u.is_finite() || !v.is_finite() {
        return Err("Invalid pick position".to_string());
    }

    let centre_x = (u.clamp(0.0, 1.0) * (width - 1) as f32).round() as i64;
    let centre_y = (v.clamp(0.0, 1.0) * (height - 1) as f32).round() as i64;

    let rgba32f = image.as_rgba32f();
    let rgb32f = image.as_rgb32f();
    if rgba32f.is_none() && rgb32f.is_none() {
        log::warn!(
            "White balance picked from a {:?} image; reading it at reduced precision",
            image.color()
        );
    }

    let mut totals = [0.0f64; 3];
    let mut count = 0u32;

    for dy in -RADIUS..=RADIUS {
        for dx in -RADIUS..=RADIUS {
            let x = centre_x + dx;
            let y = centre_y + dy;
            if x < 0 || y < 0 || x >= width as i64 || y >= height as i64 {
                continue;
            }
            let (x, y) = (x as u32, y as u32);

            let pixel = if let Some(buffer) = rgba32f {
                let p = buffer.get_pixel(x, y);
                [p[0], p[1], p[2]]
            } else if let Some(buffer) = rgb32f {
                let p = buffer.get_pixel(x, y);
                [p[0], p[1], p[2]]
            } else {
                // Still linear, just coarser. No gamma decode belongs here.
                let p = image.get_pixel(x, y);
                [
                    p[0] as f32 / 255.0,
                    p[1] as f32 / 255.0,
                    p[2] as f32 / 255.0,
                ]
            };

            for c in 0..3 {
                totals[c] += pixel[c] as f64;
            }
            count += 1;
        }
    }

    if count == 0 {
        return Err("Nothing to sample at that position".to_string());
    }
    Ok([
        (totals[0] / count as f64) as f32,
        (totals[1] / count as f64) as f32,
        (totals[2] / count as f64) as f32,
    ])
}
