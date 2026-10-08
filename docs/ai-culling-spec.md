# AI culling — build spec

Status: spec, nothing built yet. Written to be handed to a Claude Code session and built in order.
POC scope is **Milestones 0–2** (foundations, grouping, technical pass, a minimal results UI).
Milestones 3 (personal taste model) and 4 (local vision-language model ranking) are specified so the
POC does not paint them into a corner.

---

## 1. Goal

Turn a shoot of ~1000 frames into:

1. **~300 technically usable candidates.** Faces in focus, eyes open, one or a few frames per moment.
2. **~100–150 picks.** The best frames of each moment, judged on expression, interaction and composition.

All local. No photo leaves the machine. Nothing is deleted and no user rating is overwritten without an
explicit "Apply" from the user.

### What the numbers really mean

The technical filter alone will **not** take 1000 frames to 300. In a working pro's event shoot, maybe
10–30% of frames are technically bad (missed focus, blinks, motion). Most of what culling removes is
_redundancy_: eight good frames of the same toast. So stage 1 is "technically OK **and** among the best
K of its moment". Grouping is what makes the 1000 → 300 step work, and it is why grouping is built first.

---

## 2. Decisions and their reasons

| Decision                                                                              | Why                                                                                                                                                                                                                                                                                                                                                                   |
| ------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Group with capture time + image embeddings, not with the VLM.**                     | Grouping is a similarity problem. An embedding model does it in ~10–20 ms per frame, deterministically, and the result can be re-tuned with a slider in milliseconds. A VLM would take seconds per frame and be less consistent. Keep the VLM for the one thing only it does well: comparing near-identical frames on subjective grounds.                             |
| **Analyse the camera's embedded JPEG, not BlitzRaw's rendered previews.**             | A RAW render is ~1.4 s per frame (Z9 DNG). The embedded JPEG decodes in ~0.1–0.25 s. For tens of thousands of training frames that is the difference between hours and a day. Face and eye checks also need **full resolution**: a 1920 px preview of a group of 20 people gives eyes of a few pixels. The _same_ source must be used for training and for inference. |
| **AI results are stored apart from user metadata.**                                   | Tags in `.rrdata` are written into the XMP `dc:subject` by `sync_metadata_to_xmp` (`file_management.rs` ~6176), so AI verdicts stored as tags would leak into Lightroom keywords. Results live in a cache folder. Only an explicit Apply writes ratings or labels.                                                                                                    |
| **Technical thresholds are relative (to the moment and to the shoot), not absolute.** | Laplacian sharpness swings with ISO noise, lens, face size and JPEG compression. "This face is 40% as sharp as the same face two frames earlier" is robust. "Sharpness < 100" is not.                                                                                                                                                                                 |
| **Faceless frames pass through on a different path.**                                 | Events include details, décor, venue, food and wide shots. They must never be rejected for "no face".                                                                                                                                                                                                                                                                 |
| **The pipeline core has no `AppHandle`.**                                             | So a dev CLI (`examples/cull_eval.rs`) can run it on a folder and produce an HTML report. That is how every threshold gets tuned.                                                                                                                                                                                                                                     |
| **ONNX models only, through the existing `ort` setup.**                               | `ort =2.0.0-rc.10` (`load-dynamic`) is already shipped with a bundled runtime (DirectML on Windows, CPU elsewhere). No new runtime. The VLM is the exception: it runs in a separate local server (Ollama / llama.cpp / LM Studio) spoken to over HTTP.                                                                                                                |
| **Avoid InsightFace models (SCRFD, 2d106, ArcFace).**                                 | Their pretrained weights are licensed for non-commercial research. You do paid work, and the app is distributed under AGPL.                                                                                                                                                                                                                                           |

---

## 3. Pipeline

```
 folder of RAWs
     │
     ▼
[A] analysis source ── embedded JPEG (full res) → fallback: .blitzraw-previews → fallback: render
     │
     ├──► [B] per-frame features  (cached)
     │        faces (YuNet) → eye state → eye-region sharpness → (expression, M3+)
     │        global: tile sharpness, clipping
     │        embedding (SigLIP 2 / DINOv2)
     │
     ▼
[C] grouping  ── per camera body, time order: gap + embedding similarity → "moments"      (Step 1)
     │
     ▼
[D] technical verdicts ── reject / surplus / candidate / rescued, with reasons           (Step 2)
     │                      ≈ 1000 → 300
     ▼
[E] taste score  ── personal model trained on your past ratings                          (Step 3)
     │
     ▼
[F] VLM ranking  ── within each moment, top candidates compared side by side → picks     (Step 4)
     │                      ≈ 300 → 100–150
     ▼
[G] results UI ── review, override, Apply → ratings / labels through the normal commands
```

Every stage caches its output, so re-running with new thresholds never recomputes the models.

---

## 4. Milestone 0 — Foundations

### 4.1 Analysis image source

New module `src-tauri/src/ai_cull/source.rs`.

```rust
pub enum SourceOrigin { EmbeddedJpeg, CachedPreview, Rendered, DirectDecode /* JPEG/HEIF/TIFF originals */ }

pub struct AnalysisImage {
    pub image: image::RgbImage,   // orientation applied
    pub origin: SourceOrigin,
    pub native_long_edge: u32,
}

pub fn load_analysis_image(path: &Path, min_long_edge: u32) -> Result<AnalysisImage>;
```

Order of attempts:

1. **Non-RAW files:** decode directly.
2. **Embedded JPEG:** reuse `image_loader::largest_tiff_jpeg_preview` (`image_loader.rs:218`, walks IFDs and SubIFDs and takes the largest JPEG). Make it `pub(crate)`. Fall back to `rawler::analyze::extract_preview_pixels` (as `embedded_preview_fallback` at `image_loader.rs:311` already does). Apply EXIF orientation. Use the panic-safe wrapper pattern of `safe_embedded_preview_fallback` (`:336`). Accept if `long_edge >= min_long_edge` (default 2500).
3. **Cached preview:** `preview_cache::newest_preview_for(path)` (`preview_cache.rs:283`). This is 1920 px by default, which is fine for grouping and embeddings but **marks faces as low-confidence** for eye and sharpness checks.
4. **Render:** `file_management::generate_thumbnail_data(path, gpu, None, app, Some(2560))`. Slow. Last resort only. Needs the app context, so it is the one fallback the CLI skips.

Record `origin` per frame in the cache. The UI shows a warning when a shoot falls back to render for more than 10% of its frames.

**Known risk — Adobe DNG Converter previews.** BlitzRaw converts NEFs through Adobe's DNG converter. That converter embeds a 1024 px preview by default unless "JPEG Preview: Full Size" is set in its preferences. Those DNGs will fall through to the slow path. Milestone 0 includes a probe (below) that reports this. The fix is either to set the converter's preference for new conversions, or to accept the render fallback for those shoots.

**Do not** use the `.blitzraw-previews` JPEGs as the primary source. Their cache key includes the adjustments (`preview_cache.rs:194-219`), so they are _edited_ renders. Training on edited and inferring on unedited frames would skew the taste model.

### 4.2 Module layout

```
src-tauri/src/ai_cull/
  mod.rs          // pub API: analyze(), regroup(), verdicts(); no AppHandle in this layer
  source.rs       // 4.1
  models.rs       // model registry entries, session pool, preprocessing helpers
  faces.rs        // YuNet detection + landmarks
  eyes.rs         // eye-state classifier
  sharpness.rs    // eye-region and tile sharpness
  embed.rs        // image embedder
  grouping.rs     // Step 1
  verdict.rs      // Step 2
  taste.rs        // Step 3 (inference only)
  vlm.rs          // Step 4 (OpenAI-compatible HTTP client)
  store.rs        // cache read/write (4.4)
  commands.rs     // #[tauri::command]s, progress, cancellation; the only AppHandle users
src-tauri/examples/cull_eval.rs   // dev CLI (4.6)
```

Register in `lib.rs`: `mod ai_cull;`, add the commands to `generate_handler!` (`lib.rs:2912`), and add the
`Invokes` entries to `src/components/ui/AppProperties.tsx`.

### 4.3 Models and sessions

Follow the existing pattern in `ai_processing.rs`:

- Add a `*_URL`, `*_FILENAME` and `*_SHA256` constant per model.
- Load them with `download_and_verify_model` (`:444`). It emits `ai-model-download-start/finish`, which the UI already shows.
- Build sessions with `session_builder(..)` (`:33`).
- Add `cull_models: Option<Arc<CullModels>>` to `AiState`, with a `get_or_init_cull_models` using the same double-checked lock as `get_or_init_clip_models` (`:792`). Remember to add the field to **every** `AiState { .. }` literal (`:584`, `:658`, `:778`, `:851`, `:912`).
- Host the new model files in your own Hugging Face repo, with hashes pinned. Don't depend on third-party mirrors staying up.

The models are fixed in Milestone 0 after a short bake-off (4.5); the defaults below are what to try first.

| Role                         | Default                                                                                                                          | Fallback                                              | Licence (verify before shipping)                    |
| ---------------------------- | -------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------- | --------------------------------------------------- |
| Face detection + 5 landmarks | **YuNet** (OpenCV Zoo `face_detection_yunet_2023mar.onnx`, ~230 KB)                                                              | —                                                     | MIT                                                 |
| Eye state                    | **MediaPipe Face Landmarker blendshapes** (`eyeBlinkLeft/Right`) via an ONNX conversion of the face-landmark + blendshape models | `open-closed-eye-0001` (OpenVINO OMZ, 32×32 eye crop) | MediaPipe: Apache-2.0. OMZ weights: **unconfirmed** |
| Expression (Milestone 3)     | **HSEmotion** `enet_b0_8` ONNX                                                                                                   | MediaPipe blendshapes (`mouthSmile*`, `jawOpen`)      | HSEmotion: believed Apache-2.0, **unconfirmed**     |
| Image embedding              | **SigLIP 2 base** vision tower (patch16, 224 or 256)                                                                             | **DINOv2-small** (ViT-S/14, 384-d)                    | both Apache-2.0                                     |

The existing `clip_model.onnx` **cannot** be reused for embeddings. It is a combined text+image graph that returns similarity logits only (`tagging.rs:206`).

Sessions are `Mutex<Session>`, so a rayon pool would serialise on them. Use a **producer/consumer** pipeline instead:

- a rayon pool decodes the sources and crops the faces;
- a bounded channel (capacity ~16) feeds one inference thread;
- that thread batches the inputs: eye crops per frame, and embeddings in batches of 8–16.

Run all of it under `spawn_blocking`. **Never hold the `ai_state` mutex during inference.** The existing mask commands do (`ai_commands.rs:241-266`); don't copy that.

### 4.4 Cache store

Stored beside the shoot, inside the folder BlitzRaw already treats as disposable:

```
<shoot>/.blitzraw-previews/ai/
  features.v1.json          // per file: fingerprint, source origin + dims, model ids, faces[], global{}
  embed.<model-id>.f16      // row-major f16 matrix, L2-normalised
  embed.<model-id>.idx.json // { "fileName": row, ... } plus dim
  run.<settings-hash>.json  // grouping, verdicts, taste scores, VLM results for one settings set
```

- **Fingerprint** = file name + size + mtime (whole seconds). It is the same identity `preview_cache` uses. On mismatch, recompute that frame only.
- **Model ids** are in every record (e.g. `yunet-2023mar`, `siglip2-b16-256`). Changing a model invalidates only that model's fields.
- Writes are atomic (`.part` then rename), like `stacks::save` (`stacks.rs:182`).
- Virtual copies share their original's features (key by the real file name). Bracket stacks are analysed by **leader only** (`stacks::leaders_by_stack_id`, `stacks.rs:227`). RAW+JPEG pairs (`group_id`, `file_management.rs:788`) are analysed once, on the RAW.

`features.v1.json` per frame (sketch):

```json
{
  "_DSC1234.NEF": {
    "fp": { "size": 61234567, "mtime": 1720000000 },
    "source": { "origin": "EmbeddedJpeg", "w": 8256, "h": 5504 },
    "camera": { "body": "NIKON Z 9#3012345", "t": 1720000123.42, "ev": 0.0 },
    "models": { "face": "yunet-2023mar", "eye": "mp-blend-v2", "embed": "siglip2-b16-256" },
    "faces": [
      { "box": [0.41, 0.22, 0.09, 0.13], "score": 0.97, "lm5": [[..],[..],[..],[..],[..]],
        "eyeL": 0.04, "eyeR": 0.06, "sharp": 812.5, "native_face_px": 742, "low_conf": false }
    ],
    "global": { "tile_sharp_p95": 640.2, "clip_lo": 0.003, "clip_hi": 0.011 }
  }
}
```

### 4.5 Bake-off and labelled sets (you produce these, once)

Milestone 0 ships a CLI that dumps material to label. You sort it in a file manager.

- **Eye set:** ~300 face crops from 3 of your shoots, sorted into `open/`, `closed/` and `ambiguous/` folders. Include candid laughs, people looking down, glasses and profiles. Score each eye-state candidate on this set.
- **Grouping set:** one 300–500 frame shoot where you put each moment in its own subfolder. Score grouping with pairwise F1 and adjusted Rand index.
- **Rated shoots:** 3–5 past shoots with your final ratings, used as held-out evaluation for every milestone. **Never train on these** (Milestone 3).

Also in Milestone 0, a **source probe**: `cull_eval probe <folder>` prints, per extension and body, the embedded-JPEG sizes found and the share of frames that would fall back. Run it on your NEF folders and on your Adobe-converted DNG folders before writing anything else.

### 4.6 Dev CLI and HTML report

`cargo run --release --example cull_eval -- <command> <folder> [--models-dir DIR] [--config cfg.json]`

Commands:

- `probe` — 4.5.
- `analyze` — features plus cache.
- `group` / `verdicts` — run from the cache.
- `eval --ratings` — score against your ratings (metrics per milestone below).
- `report` — writes `cull-report.html`: one row per moment, thumbnails (from the analysis source, 256 px), badges (reject reason, surplus, candidate, pick) and per-frame numbers on hover.

The report is how thresholds get tuned. Iterating on it is the work of Milestones 1–2.

Follow the repo's existing pattern for tests that need real files (`RAPIDRAW_TEST_*` env vars, skipped when unset). Use `RAPIDRAW_TEST_CULL_DIR` and `RAPIDRAW_TEST_MODELS_DIR`.

**Milestone 0 is done when:** the probe runs on your folders; `analyze` fills the cache for a 1000-frame shoot; a second run is a no-op; the model bake-off has picked the eye-state model.

---

## 5. Milestone 1 — Grouping into moments (Step 1)

### Inputs per frame

- Capture time with sub-seconds and EV. Reuse `auto_stack::read_frame` / `parse_capture_time` (`auto_stack.rs:381`, `:346`) and make them `pub(crate)`. They mmap and read EXIF only, with no decode.
- Body id: Make + Model + BodySerialNumber. Read them in the same EXIF pass.
- Embedding `e` (L2-normalised).

### Algorithm

1. Split frames **by body**. Two bodies have independent clocks and different focal lengths, so the same moment from two angles is two groups. This is wanted: they are different pictures.
2. Sort each body's frames by capture time and walk them in order. The current group `G` has a running centroid `c` (mean of members' `e`, re-normalised). For frame `i`, with `gap` = time since the previous frame:
   - `gap > T_hard` (default **20 s**) → new group.
   - `gap <= T_burst` (default **1.0 s**, matching `BurstParams.max_gap_seconds`) → join `G` unless `cos(e_i, c) < τ_cut` (default **0.55**, a scene cut mid-burst).
   - otherwise → join `G` if `cos(e_i, c) >= τ` (default **0.85**, to be calibrated), else start a new group.
3. Cap the group size at `N_max` (default **40**). Split an oversized group at its largest internal similarity drop.
4. Return `Vec<Moment { id, body, frames: Vec<FrameRef>, span_seconds }>`. The id is blake3 of the sorted member names, the same scheme as `stacks::derive_stack_id` (`stacks.rs:237`), so ids are stable across runs.

### Calibration

Sweep `τ` from 0.70 to 0.95 on the grouping set; keep the best F1. Expose a single "coarser ↔ finer" slider in the UI that maps to `τ`. Regrouping runs from the cache: **target < 200 ms for 1000 frames**.

This is independent of `.blitzraw-stacks.json`. Moments are not stacks and are never written to that file. A later "Stack this moment" action could reuse `stacks::set_stacks`, but that is not in scope.

**Milestone 1 is done when:** pairwise F1 ≥ 0.85 on the grouping set at the chosen `τ`, and the HTML report shows moments that look right to you on two other shoots.

---

## 6. Milestone 2 — Technical pass (Step 2)

### 6.1 Per-face measurements

1. **Detect** on the analysis image downscaled to a long edge of 1280 (letterboxed to the model input). Keep faces with score ≥ 0.75.
2. **Prominence:** `w = face_area / largest_face_area` in the frame. _Main faces_ are those with `w >= 0.25` and face width ≥ 2.5% of the long edge.
3. **Crop from the full-resolution source.** Box = bbox expanded ×1.3, resampled with area averaging so the face is 160 px wide. **Never upscale.** If the native face is under 160 px wide, measure at native size and set `low_conf = true`.
4. **Eye state.** Get `p_closed` per eye from the chosen model. Frame-level closed = both eyes `p_closed > 0.8`, or one eye > 0.9 while the other is unreadable (profile).
5. **Eye-region sharpness.** Take the band between the two eye landmarks, padded by 0.25 face widths and covering the upper half of the face box. Apply a Gaussian blur (σ 0.8, to suppress ISO noise), then compute the variance of the Laplacian. Compare against Tenengrad in the bake-off and keep whichever separates your labelled soft/sharp examples better.
6. **Head pose (cheap).** Yaw is estimated from the asymmetry of the 5 landmarks. Profiles (|yaw| > 50°) skip the eye check.

### 6.2 Per-frame global measurements

- **Tile sharpness:** split the frame into an 8×8 grid and compute the Laplacian variance per tile on the 1280 px image; keep the **95th percentile**. This handles shallow depth of field, where most of the frame is meant to be soft. It is what the current culler's `center_focus_metric` tries to do.
- **Clipping:** share of pixels < 5 and > 250 on the analysis JPEG. A soft signal only. The RAW holds more latitude than the JPEG shows, so clipping never rejects on its own.

### 6.3 Normalisation

For each measure, compute both:

- **within-moment:** `rel = value / max(value in moment)`, for the same role (main face sharpness against the best main-face sharpness in the moment);
- **within-shoot:** the percentile rank across the whole shoot.

### 6.4 Verdict rules (`verdict.rs`)

Status per frame: `Reject(reasons)`, `Surplus`, `Candidate`, `Rescued`. All thresholds are config values; these are starting points.

| Rule                   | Condition                                                                         | Reason code                                                           |
| ---------------------- | --------------------------------------------------------------------------------- | --------------------------------------------------------------------- |
| Face soft              | largest main face: `sharp_rel < 0.55` **and** shoot percentile < 20%, `!low_conf` | `face_soft`                                                           |
| Blink (portrait)       | largest main face closed, and `w`-weighted closed share ≥ 0.5                     | `eyes_closed`                                                         |
| Blink (group)          | ≥ 3 main faces and > 20% of them closed                                           | `group_blink`                                                         |
| Missed focus (no face) | no main face, `tile_sharp_p95` shoot percentile < 10% **and** `rel < 0.5`         | `soft`                                                                |
| Exposure               | `clip_lo > 0.5` or `clip_hi > 0.35`                                               | `exposure` (flag only, never alone a reject unless `strict_exposure`) |

Then, within each moment:

- Rank the non-rejected frames by `tech_score`, a weighted sum of the normalised measures. Weights live in config.
- The top **K_tech** become `Candidate`. Default K_tech is 1 for moments of 1–2 frames, 2 for 3–6, and 3 for more.
- The rest become `Surplus`. These are good frames that lost to a sibling; they are **not rejects**.
- If every frame in a moment is rejected, the best one becomes `Rescued` and keeps its reasons. Some moments only happen once (the ring going on, the first kiss). You decide, not the model.

**Laughing caveat.** Eyes-closed-while-laughing is often the best frame of a moment. Until expression is available (Milestone 3), `eyes_closed` on a frame whose mouth is wide open (blendshape `jawOpen` / `mouthSmile` high, if MediaPipe is the eye model) is downgraded to a flag rather than a reject.

### 6.5 Commands

```rust
#[tauri::command] async fn ai_cull_analyze(paths: Vec<String>, options: AiCullOptions, app: AppHandle, state: State<AppState>) -> Result<AiCullRun, String>;
#[tauri::command] fn       ai_cull_regroup(paths: Vec<String>, options: AiCullOptions) -> Result<AiCullRun, String>; // cache only
#[tauri::command] fn       ai_cull_cancel(state: State<AppState>) -> Result<(), String>;
#[tauri::command] async fn ai_cull_apply(decisions: Vec<AiCullDecision>, mapping: ApplyMapping, app: AppHandle) -> Result<ApplySummary, String>;
```

- Events: `ai-cull-progress` `{ stage: "source"|"faces"|"embed"|"group"|"verdict"|"vlm", current, total }`, `ai-cull-complete`, `ai-cull-error`. Actually emit the error event; the old culler's frontend listens for `culling-error`, which nothing emits.
- Cancellation: copy the export pattern. An `Arc<AtomicBool>` lives in `AppState` (cf. `export_task_token`, `app_state.rs:195`). A second concurrent run is refused (cf. `register_export_task`, `export_processing.rs:332`). An RAII guard clears it.
- `AiCullRun` returns moments with per-frame `{ path, status, reasons[], tech_score, rank_in_moment, taste_score?, vlm_note?, low_conf, origin }`.

### 6.6 Apply

The apply mapping is configurable. Defaults:

- `Candidate`/`Rescued` → no change;
- `Pick` (Milestone 4) → ★★★;
- `Reject` → nothing, or a red label if opted in;
- `Surplus` → nothing.

`ai_cull_apply` sets **explicit values** by calling the same backend paths as `set_rating_for_paths` / `set_color_label_for_paths` (`file_management.rs:4875`, `:4839`). Do **not** go through the frontend `handleRate` / `handleSetColorLabel` wrappers: they _toggle_ when the value already matches. That is a live bug in the current `CullingModal`, whose "Mark as rejected" can clear red labels instead of setting them.

- Never lower or overwrite a non-zero user rating unless `overwrite_existing` is set.
- Record the whole apply as **one** undo step, using whatever `useLibraryActions` uses for rating undo.
- With `enable_xmp_sync` on, rating changes also go into `.xmp` sidecars (existing behaviour). Mention this in the Apply dialog.

### 6.7 POC UI

Replace `CullingModal.tsx`'s internals, or add `AiCullModal.tsx` beside it. Keep the same entry point (context menu "Cull", `useAppContextMenus.ts:922`).

1. **Settings:** grouping slider, K_tech, "strict" toggle, apply mapping.
2. **Progress:** stage + count + Cancel.
3. **Results:** one horizontal strip per moment. Each frame shows a status badge and its reason chips (`eyes closed`, `face soft`, …). Keyboard:
   - ←/→ moves within a moment, ↑/↓ moves between moments;
   - `P` toggles candidate/pick, `X` toggles reject;
   - `Enter` applies.
     A counter shows `1000 → 312 candidates → 0 picks`.

A version that lives in the library is v1, after the POC proves out:

- filter chips (AI: candidates / picks / rejected);
- badges on thumbnails;
- the existing manual compare view (`src/components/panel/library/CullingView.tsx`) walking moment by moment.

**Milestone 2 is done when**, on your 3–5 held-out rated shoots:

- **false-reject rate ≤ 2%**: of the frames you rated, at most 2% are `Reject` (rescued ones count as kept);
- candidates are ≤ 35% of the shoot;
- a 1000-frame shoot analyses in **< 3 min** on your machine (Windows + DirectML), cold, excluding model download.

The false-reject rate is the number that matters. A culler that throws away a keeper costs more than it saves.

---

## 7. Milestone 3 — Personal taste model (Step 3)

### 7.1 What is actually being trained

- **Not a new network.** A frozen image encoder (the Milestone 1 embedder) produces a vector per frame.
- A **small linear model** on top learns your preferences.
- Inputs: the embedding (768-d SigLIP 2 base, or 384-d DINOv2-small), plus the technical features (eye states, sharpness rel/percentile, face count, prominence, yaw, clipping), plus moment context (moment size, `rank_in_moment` by tech score).
- Training takes seconds on a CPU.

The part that is not easy is the **labels**. A frame you didn't rate is usually not a bad frame. It lost to its neighbour. A model trained on "rated vs unrated" frames as independent examples learns "near-duplicates are bad", which is nonsense. So train two heads:

1. **Pairwise (main):** within each moment, for every (rated, unrated) pair, the example is `x_rated − x_unrated` with label 1 (and the reverse with label 0). Fit a logistic regression with an L2 penalty. The weight vector `w` gives a score `s(x) = w·x`, used to **rank within a moment**. This is the standard RankNet-style trick, and it is what the data actually supports.
2. **Pointwise (secondary):** a logistic `p(keep | x)`, with class weights, for **singleton moments** and for a global "is this frame worth a look at all" signal.

Start with logistic models. Try a 1-hidden-layer MLP (256 units) only if the logistic models plateau. Fine-tuning the encoder is out of scope.

### 7.2 Getting the labels

**In Lightroom Classic,** for each past shoot:

1. Select all, then _Metadata → Save Metadata to File_ (Ctrl+S). For NEFs this writes `photo.xmp` beside the RAW; for DNGs it writes into the file.
2. **Flags are unreliable in XMP.** They were catalog-only until LR 13.2, and newer versions have a reported bug where a flag isn't saved unless another field changed. If you culled with flags, first filter _Flagged_ and give those a rating or colour label, then save.
3. Ratings and colour labels export reliably.

**Reading:**

- `.xmp` sidecar rating via `file_management::extract_xmp_rating` (`:5952`); label via `extract_xmp_label` (`:5966`).
- Embedded XMP (DNG) via `embedded_xmp::read` (`embedded_xmp.rs:73`).
- `.rrdata` `rating` for shoots culled in BlitzRaw.
- **Read the XMP directly.** Don't rely on what has been merged into `.rrdata`: `apply_xmp_rating` keeps a non-zero BlitzRaw rating over the XMP one (`:6110`).
- `keeper = max(sources) >= R_min` (default 1).
- Ignore in-camera ratings (`camera_rating`) by default. Make it a flag.

**Folder filter:** only include folders where 3–60% of frames are rated. That leaves out unculled folders, and folders where the rejects were already deleted (which invert the class balance). The extractor prints per-folder stats so you can exclude more by hand.

### 7.3 Feature extraction at scale

`ai_cull_extract_features(folders)` runs Milestone 0 stages B–C (features, embeddings, moments) over many folders. It does no VLM work.

- **Resumable:** the per-file cache in 4.4 makes a re-run skip finished frames.
- **Cancellable.**
- **No preview rendering:** it uses the embedded JPEG (4.1), which is why you don't need to rebuild previews for the old shoots.
- Estimate: 50k frames at ~5–10 frames/s ≈ 1.5–3 h, run once.

If the 4.5 probe shows your DNGs only have 1024 px previews, those shoots fall back to the render path. Expect roughly ten times slower, and consider training on the NEF shoots only.

### 7.4 Training tool

**v0 (experiment):** `tools/cull-taste/` in Python, using numpy and scikit-learn.

- It reads the `.blitzraw-previews/ai/` caches and the labels, then trains both heads.
- **Cross-validation is grouped by shoot**, never by frame. Near-duplicates across a split would leak and inflate every metric.
- It writes `taste-model.json`:

```json
{ "version": 1, "encoder": "siglip2-b16-256", "features": ["emb[0..768]", "eye_closed_max", "..."],
  "norm": { "mean": [...], "std": [...] },
  "pairwise": { "w": [...], "b": 0.0 }, "pointwise": { "w": [...], "b": 0.0 },
  "metrics": { "cv_top1_in_moment": 0.0, "cv_auc_pointwise": 0.0, "n_shoots": 0, "n_frames": 0 } }
```

Install it in the app data `models/taste/`. Inference in Rust (`taste.rs`) is a normalisation and a dot product.

**v1 (in-app):** once the feature set is settled, port training to Rust and add a "Train on my past shoots" button in Settings. This is a hand-rolled L2 logistic regression with L-BFGS or SGD, or `linfa-logistic`. Python remains only as the research harness.

### 7.5 Encoder choice

SigLIP 2 vs DINOv2 is an empirical question for _your_ taste. Extract both on ~5k frames and compare the cross-validated metrics. Keep the better one as the single embedder for grouping too, so there is one model to run.

**Milestone 3 is done when**, on the held-out rated shoots, the pairwise head's **top-1-in-moment agreement** beats the tech-score-only ranking by a clear margin. Set the margin once the baseline is measured; 10 points is a reasonable bar. If it doesn't, stop and look at the labels before adding model capacity.

---

## 8. Milestone 4 — Local VLM ranking (Step 4)

### 8.1 Runtime

A separate local server, configured by URL:

- **Ollama** (`http://127.0.0.1:11434/v1`);
- **llama.cpp `llama-server`** (`http://127.0.0.1:8080/v1`);
- **LM Studio** (`http://127.0.0.1:1234/v1`).

All three speak OpenAI-style `POST /v1/chat/completions`, with images as `data:image/jpeg;base64,…` URLs. BlitzRaw does not embed an LLM runtime.

**Model bake-off on your 16 GB card.** Pick by agreement with your picks on the held-out shoots, then by speed:

| Candidate                                  | Approx. VRAM (Q4_K_M → Q8_0)   | Notes                                                                          |
| ------------------------------------------ | ------------------------------ | ------------------------------------------------------------------------------ |
| Qwen3-VL-8B-Instruct                       | ~6 → ~10 GB + vision projector | proven multi-image support in llama.cpp/Ollama; use _Instruct_, not _Thinking_ |
| Qwen3.5-9B (natively multimodal, Mar 2026) | ~6.5 → ~11 GB                  | newer; confirm the runtime's vision + multi-image support before relying on it |
| Qwen3-VL-4B-Instruct                       | ~3 → ~5 GB                     | the speed option if 8–9B is too slow                                           |
| Gemma 3 12B (Q4)                           | ~8 GB                          | an outside reference point; different licence terms                            |

Model releases move fast. Treat this table as a starting list and check the current llama.cpp/Ollama support when you get here. Leave ~3–4 GB free for context: 6 images at ~1024 px are roughly 4–8k visual tokens, depending on the model's patching.

### 8.2 What gets sent

- **Only moments with ≥ 2 candidates.** Singletons are decided by Steps 2–3.
- **At most `M` frames per request** (default 6): the top `M` of the moment by taste score, or by tech score before Milestone 3.
- **Images:** the analysis source resized to a long edge of 1024 (setting), JPEG q85, base64.
- **Moments over `M`:** a tournament. Split into chunks of ≤ `M`, take the top 2 of each, run a final round.
- **Settings:** `temperature: 0`, concurrency 1 (one GPU), timeout 120 s per request.

**Prompt (v1, versioned in code as `PROMPT_VERSION`):**

System:

> You are assisting a professional event and portrait photographer with culling. You will see N photos of the same moment, taken seconds apart. They differ in small ways. Compare them carefully against each other and choose the best. Judge, in this order: (1) expression and emotion — genuine, peak of the moment; (2) eyes and gaze — open, alive, looking where the moment needs them to; (3) interaction between people; (4) composition — framing, no awkward crops at joints, nothing distracting merging with heads; (5) technical — focus on the eyes, no motion blur. Return only JSON.

User content:
`"Image 1:"`, image, `"Image 2:"`, image, …, then
`"Pick the best {k} of these {N}. Rank all of them."`
where `k` = 1 for moments of ≤ 3 candidates, else 2 (config).

Response schema, requested with `response_format: { type: "json_schema", … }` where the server supports it, and otherwise prompt-only with a tolerant parse:

```json
{
  "ranking": [3, 1, 2],
  "picks": [3],
  "notes": { "3": "both smiling, he's looking at her", "1": "her eyes half closed" }
}
```

**Validation:** indices are 1..N, unique, and `picks ⊆ ranking`. On invalid output, retry once with the error appended. If that fails too, fall back to the taste/tech order and mark the moment `vlm_failed`.

### 8.3 Position bias

VLMs favour certain positions (often first or last).

- Shuffle the order deterministically per moment (seeded by the moment id).
- Optionally (setting `vlm_two_pass`) run a second pass in reverse order and combine the two by Borda count. If the two passes disagree on the top pick, keep both and mark `uncertain`.
- Measure the bias in the bake-off: how often does the top pick land in position 1 compared with 1/N?

### 8.4 Caching and cost

- Cache by `(sorted member fingerprints, model name, PROMPT_VERSION, k)` in `run.*.json`. Re-runs are free.
- Rough budget for 1000 frames: ~300 candidates in ~100–150 multi-candidate moments, at ~5–10 s each, is **~10–25 min**. It runs in the background, is cancellable, and shows progress per moment.

### 8.5 Combining steps 2–4

Per moment:

1. Candidates are ranked by taste score (or tech score).
2. The VLM sees the top `M` and returns the picks.
3. Final picks = the VLM picks.
4. With the VLM disabled or failed, the picks are the top `k` by taste score.

Whether the VLM, the taste model or a blend agrees best with you is measured on the held-out shoots, not assumed. Report all three in `cull_eval eval`.

**Milestone 4 is done when** VLM picks agree with your held-out ratings better than taste-only picks, and a 1000-frame shoot finishes in < 30 min.

---

## 9. Settings

Add one nested struct to `AppSettings` rather than a dozen flat fields:

```rust
#[derive(Serialize, Deserialize, Debug, Clone)] // hand-written `impl Default` with the values in the comments
#[serde(rename_all = "camelCase", default)]
pub struct AiCullSettings {
    pub grouping_similarity: f32,   // τ, default 0.85
    pub grouping_hard_gap_s: f32,   // 20.0
    pub keep_per_moment: Vec<(u32, u32)>, // [(2,1),(6,2),(u32::MAX,3)]
    pub strict_exposure: bool,
    pub min_source_long_edge: u32,  // 2500
    pub apply: ApplyMapping,
    pub vlm_enabled: bool,
    pub vlm_base_url: String,       // "http://127.0.0.1:11434/v1"
    pub vlm_model: String,
    pub vlm_api_key: Option<String>,
    pub vlm_max_images: u32,        // 6
    pub vlm_image_long_edge: u32,   // 1024
    pub vlm_two_pass: bool,
    pub taste_enabled: bool,
}
```

- Add it as `pub ai_cull: Option<AiCullSettings>` with `#[serde(default)]` in `AppSettings` (`app_settings.rs:416`), add it to `Default` (`:609`), and mirror it in the TS `AppSettings` (`AppProperties.tsx` ~216).
- UI: a new "AI culling" card in `SettingsPanel.tsx`, in the processing category next to the AI card (~2336). It needs a **Test connection** button for the VLM server, which calls `GET {base}/models` and checks that the configured model is listed.
- Use a shared `reqwest::Client` with **connect and request timeouts** (nothing in the codebase sets one today).

---

## 10. Performance budget (1000 frames, Windows + DirectML, 16 GB GPU)

| Stage                                  | Per frame          | 1000 frames           | Notes                        |
| -------------------------------------- | ------------------ | --------------------- | ---------------------------- |
| Embedded JPEG extract + decode (45 MP) | 100–250 ms CPU     | ~20–40 s on 8 threads | dominant cost; measure in M0 |
| Downscale to 1280 + YuNet              | ~10 ms             | ~10 s                 |                              |
| Eye model + sharpness per face         | ~2–5 ms/face       | ~10–20 s              |                              |
| Embedding (batched, GPU)               | ~5–15 ms           | ~10–15 s              | CPU-only platforms: ~100 ms  |
| Grouping + verdicts                    | —                  | < 1 s                 | from cache                   |
| VLM (M4)                               | ~5–10 s per moment | ~10–25 min            | background                   |

These are estimates to measure, not promises. Milestone 0's CLI prints the real per-stage timings.

---

## 11. Licences and credits

- Before shipping, confirm the licence of each model file you actually host (table in 4.3). In particular, confirm the eye-state and HSEmotion weights.
- The VLM is run by the user's own server and is not distributed with the app.
- Add each shipped model to the Special Thanks card (`SettingsPanel.tsx:1760-1881`), with i18n keys under `settings.thanks.list.*` in all 13 locale files. CLIP and SCUNet are missing from that list today, so add them while you're there.

---

## 12. Risks and open questions

1. **Embedded preview sizes** (DNGs from Adobe's converter in particular). Resolved by the M0 probe.
2. **Two bodies and clock offset.** Grouping is per body, so an offset doesn't break grouping. It would matter only if moments from two bodies were ever merged, which this spec does not do.
3. **Looking down vs eyes closed.** A known false-positive source. Watch it in the eye set; the pose and blendshape signals help.
4. **Your rating semantics.** Do 1★ and 3★ mean different things in your workflow (e.g. 1★ = deliverable, 3★ = portfolio)? If so, train on ≥ 1★ for the cull and treat higher stars as a separate, later "hero" signal.
5. **Taste drift across genres.** Corporate events vs weddings vs portraits. Start with one model. If cross-validation shows per-genre models do better, add a "shoot type" selector.
6. **Existing culler.** Once the POC works, remove `culling.rs` / `CullingModal.tsx`. It decodes every RAW in full (`culling.rs:138`), its "reject" toggles labels, and its `rate_zero` action is labelled "1 star".

---

## 13. Build order

| #    | Deliverable                                                          | Depends on       |
| ---- | -------------------------------------------------------------------- | ---------------- |
| M0.1 | `ai_cull::source` + `cull_eval probe`                                | —                |
| M0.2 | model registry entries, `CullModels`, session pipeline               | —                |
| M0.3 | `store.rs` cache, `cull_eval analyze`                                | M0.1, M0.2       |
| M0.4 | crop dumper for the eye set, bake-off script, HTML `report`          | M0.3             |
| M1   | `grouping.rs`, `cull_eval group`, τ calibration                      | M0.3             |
| M2.1 | `sharpness.rs`, `eyes.rs`, `verdict.rs`, `cull_eval eval`            | M0.4, M1         |
| M2.2 | Tauri commands, cancellation, settings struct                        | M2.1             |
| M2.3 | POC results UI + Apply                                               | M2.2             |
| —    | **POC checkpoint:** run on 3 fresh shoots, decide go/no-go           |                  |
| M3   | `ai_cull_extract_features` bulk, `tools/cull-taste`, `taste.rs`      | M2               |
| M4   | `vlm.rs`, settings card + connection test, bake-off                  | M2 (M3 optional) |
| v1   | library integration (filters, badges, compare view), in-app training | M3, M4           |
