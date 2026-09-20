use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use image::{DynamicImage, GrayImage};
use serde::{Deserialize, Serialize};
use sysinfo::Disks;
use tokio::sync::Mutex as TokioMutex;
use tokio::task::JoinHandle;
use wgpu::{Texture, TextureView};

use crate::ai_processing::AiState;
use crate::cache_utils::DecodedImageCache;
use crate::gpu_processing::GpuProcessor;
use crate::image_processing::GpuContext;
use crate::launch_request::ExternalEditSession;
use crate::lens_correction::LensDatabase;
use crate::lut_processing::Lut;

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct WindowState {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
    pub maximized: bool,
    pub fullscreen: bool,
}

#[derive(Clone)]
pub struct LoadedImage {
    pub path: String,
    pub image: Arc<DynamicImage>,
    pub is_raw: bool,
}

#[derive(Clone)]
pub struct CachedPreview {
    pub image: Arc<DynamicImage>,
    pub small_image: Arc<DynamicImage>,
    pub transform_hash: u64,
    pub scale: f32,
    pub unscaled_crop_offset: (f32, f32),
    pub preview_dim: u32,
    pub interactive_divisor: f32,
}

pub struct GpuImageCache {
    pub texture: Texture,
    pub texture_view: TextureView,
    pub width: u32,
    pub height: u32,
    pub transform_hash: u64,
}

pub struct GpuProcessorState {
    pub processor: GpuProcessor,
    pub width: u32,
    pub height: u32,
}

pub struct PreviewJob {
    pub adjustments: serde_json::Value,
    pub is_interactive: bool,
    pub target_resolution: Option<u32>,
    pub roi: Option<(f32, f32, f32, f32)>,
    pub compute_waveform: bool,
    pub active_waveform_channel: Option<String>,
    pub responder: tokio::sync::oneshot::Sender<Vec<u8>>,
}

pub struct AnalyticsJob {
    pub path: String,
    pub image: Arc<DynamicImage>,
    pub compute_waveform: bool,
    pub active_waveform_channel: Option<String>,
}

pub struct AnalyticsConfig {
    pub path: String,
    pub compute_waveform: bool,
    pub active_waveform_channel: Option<String>,
    pub sender: Sender<AnalyticsJob>,
}

pub struct ThumbnailProgressTracker {
    pub total: usize,
    pub completed: usize,
}

/// BLITZRAW: a photo whose picture has changed and needs rendering again.
///
/// Two times rather than one. A run of key presses re-marks the same photo
/// every few tens of milliseconds, and rendering on each of them is the storm
/// this queue exists to stop, so the render waits for the marking to go quiet.
/// A finger held down would push that off forever, so the first mark also sets
/// a deadline: quiet for a moment, or a second and a half since it went dirty,
/// whichever comes first.
pub struct DirtyPicture {
    pub first_marked: Instant,
    pub last_marked: Instant,
}

pub struct ThumbnailManager {
    /// What the grid has scrolled into view and wants to see.
    pub queue: Mutex<VecDeque<String>>,
    pub cvar: Condvar,
    pub processing_now: Mutex<HashSet<String>>,
    pub rotational_disk: AtomicBool,
    pub io_gate: Mutex<()>,
    /// BLITZRAW: photos whose pictures have changed, waiting to settle.
    ///
    /// A separate lane from `queue`, and served before it, for two reasons.
    /// Scrolling clears and rewrites `queue` as the view moves, which would
    /// throw away a rebuild the user is waiting on. And an edit is worth more
    /// than a thumbnail that has scrolled past: it is what the user just did.
    ///
    /// A map rather than a list, so ten presses on one photo are one rebuild.
    pub dirty: Mutex<HashMap<String, DirtyPicture>>,
}

impl ThumbnailManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::new()),
            cvar: Condvar::new(),
            processing_now: Mutex::new(HashSet::new()),
            rotational_disk: AtomicBool::new(false),
            io_gate: Mutex::new(()),
            dirty: Mutex::new(HashMap::new()),
        })
    }
}

pub struct PendingMetadata {
    pub virtual_path: String,
    pub image_path: PathBuf,
    pub sidecar_path: PathBuf,
}

pub struct MetadataManager {
    pub queue: Mutex<VecDeque<PendingMetadata>>,
    pub cvar: Condvar,
    pub pending: Mutex<HashSet<PathBuf>>,
}

impl MetadataManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::new()),
            cvar: Condvar::new(),
            pending: Mutex::new(HashSet::new()),
        })
    }
}

pub type TransformedImageCache = (u64, Arc<DynamicImage>, (f32, f32));

pub struct AppState {
    pub window_setup_complete: AtomicBool,
    // ============ BLITZRAW: the window that opened small ============
    // `window_state.json` was read twice: once at setup to size the window, and
    // again in `frontend_ready` to decide whether to maximise it. Between those
    // two reads the setup's own `set_size` and `set_position` fire Resized and
    // Moved, the saver writes what it sees, and what it sees is a window that
    // is not maximised yet. On a fast start the second read happens first and
    // nothing is noticed. On a slow one, which is every start after a rebuild,
    // the file has already been overwritten with `maximized: false` and the
    // window opens small.
    //
    // So the file is read once, kept here, and the saver is not allowed to run
    // until the restore has actually happened.
    /// What `window_state.json` said when the application started.
    pub startup_window_state: Mutex<Option<WindowState>>,
    /// Set once the main window has been sized and maximised as it should be.
    /// Nothing is saved before this, because before this the window is not yet
    /// where the user left it.
    pub window_state_restored: AtomicBool,
    /// Set when the main window starts closing. Windows sends a last Resized as
    /// a window is destroyed, and a maximised window does not always still
    /// report itself as maximised by then.
    pub window_closing: AtomicBool,
    // ========== BLITZRAW END: the window that opened small ==========
    pub gpu_crash_flag_path: Mutex<Option<PathBuf>>,
    pub original_image: Mutex<Option<LoadedImage>>,
    pub cached_preview: Mutex<Option<CachedPreview>>,
    pub gpu_context: Mutex<Option<GpuContext>>,
    pub gpu_image_cache: Mutex<Option<GpuImageCache>>,
    pub gpu_processor: Mutex<Option<GpuProcessorState>>,
    pub ai_state: Mutex<Option<AiState>>,
    pub ai_init_lock: TokioMutex<()>,
    pub export_task_token: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    pub hdr_result: Arc<Mutex<Option<DynamicImage>>>,
    pub panorama_result: Arc<Mutex<Option<DynamicImage>>>,
    pub denoise_result: Arc<Mutex<Option<DynamicImage>>>,
    pub indexing_task_handle: Mutex<Option<JoinHandle<()>>>,
    pub lut_cache: Mutex<HashMap<String, Arc<Lut>>>,
    pub initial_file_path: Mutex<Option<String>>,
    pub pending_edit_session: Mutex<Option<ExternalEditSession>>,
    pub thumbnail_cancellation_token: Arc<AtomicBool>,
    pub thumbnail_progress: Mutex<ThumbnailProgressTracker>,
    pub preview_worker_tx: Mutex<Option<Sender<PreviewJob>>>,
    pub analytics_worker_tx: Mutex<Option<Sender<AnalyticsJob>>>,
    pub mask_cache: Mutex<HashMap<u64, GrayImage>>,
    pub patch_cache: Mutex<HashMap<String, serde_json::Value>>,
    pub geometry_cache: Mutex<HashMap<u64, DynamicImage>>,
    pub thumbnail_geometry_cache: Mutex<HashMap<String, (u64, DynamicImage, f32)>>,
    pub lens_db: Mutex<Option<Arc<LensDatabase>>>,
    pub load_image_generation: Arc<AtomicUsize>,
    // ============ BLITZRAW: one photo decodes at a time ============
    /// Held for the length of one editor decode.
    ///
    /// Opening a photo starts a full decode of it: on a Z9 that is 8256x5504
    /// floats, about 545 MB, and roughly a second and a half. Nothing used to
    /// stand between one of those and the next, so a cull that dropped a photo
    /// every second started a decode every second and none of them could be
    /// stopped once the demosaic had begun. Nine at once is five gigabytes, and
    /// that is what ran the window out of memory.
    ///
    /// Waiting here rather than deciding earlier is the point. A request that
    /// reaches the front of this queue and finds a newer one behind it is
    /// dropped **before it decodes anything**, so running through five photos
    /// costs one decode and four instant refusals rather than five decodes.
    pub editor_decode_slot: Arc<tokio::sync::Mutex<()>>,
    /// Whether a photo the user is looking at is being decoded right now.
    ///
    /// Read by the thumbnail workers, which stand aside while it is set. The
    /// photo on screen is what somebody is waiting for; a thumbnail four rows
    /// down is not.
    pub editor_decode_busy: Arc<std::sync::atomic::AtomicBool>,
    // ========== BLITZRAW END: one photo decodes at a time ==========
    /// BLITZRAW: which round of bulk adjustments is current.
    ///
    /// Applying adjustments across a selection writes every sidecar and then
    /// re-renders every file, and the second half is a decode apiece. A newer
    /// round covers the same files, so the older one's rendering is work whose
    /// output is about to be replaced. Only the rendering is abandoned: the
    /// sidecars are already written, and the values in a round are absolute
    /// rather than increments, so nothing is lost by dropping one.
    pub apply_adjustments_generation: Arc<AtomicUsize>,
    pub full_warped_cache: Mutex<Option<(u64, Arc<DynamicImage>)>>,
    pub full_transformed_cache: Mutex<Option<TransformedImageCache>>,
    pub decoded_image_cache: Mutex<DecodedImageCache>,
    pub thumbnail_manager: Arc<ThumbnailManager>,
    pub metadata_manager: Arc<MetadataManager>,
    pub disks_cache: Mutex<Option<Disks>>,
    pub disks_cache_refreshing: AtomicBool,
}
