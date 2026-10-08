# Handoff prompt — AI culling POC

Paste everything below the line into a Claude Code session started in your local BlitzRaw checkout.
Fill in the folder paths first.

---

I want to build the first part of an AI culler for BlitzRaw. The full spec is in `docs/ai-culling-spec.md` on branch `claude/zealous-fermat-cc48dg`:
https://github.com/Accent-Studio/BlitzRaw/blob/claude/zealous-fermat-cc48dg/docs/ai-culling-spec.md

**Setup**

1. `git fetch origin claude/zealous-fermat-cc48dg`, then create a working branch from it: `git checkout -b ai-culling origin/claude/zealous-fermat-cc48dg`.
2. Read `docs/ai-culling-spec.md` in full before writing any code. It is the source of truth for design decisions. Its file:line references were correct when it was written but may have drifted, so check each one before relying on it.
3. Read `README.md` for the project's rules (never write to originals, never delete outside the Recycle Bin, no catalog).

**Scope: Milestones 0, 1 and 2 only** (foundations, grouping into moments, technical pass, minimal results UI). Do not build Milestones 3 (taste model) or 4 (VLM). Keep the doors open for them as the spec describes: the pipeline core has no `AppHandle`, the cache is versioned, and features are stored per frame.

**About me and my machine**

- Windows, 16 GB GPU. ONNX goes through the bundled DirectML runtime.
- My ratings: ★ = first pass (usable), ★★ = final picks (edited and delivered), ★★★+ = portfolio. Ratings come from Lightroom through `.xmp` sidecars (NEF) or embedded XMP (DNG).
- I sometimes shoot with two bodies whose clocks aren't synced. Capture times are only ever compared within one body.

**My folders**

- Recent NEF shoot (not rated): `<PATH>`
- Shoot of DNGs converted with Adobe DNG Converter: `<PATH>`
- Tuning shoots, rated (use these to tune thresholds): `<PATH>`, `<PATH>`
- Held-out shoots, rated (only for reporting the final metrics; **never tune on these**): `<PATH>`, `<PATH>`, `<PATH>`

During M0–M2, the only thing you may write inside these folders is `.blitzraw-previews/ai/`. Do not touch the RAWs, `.xmp`, `.rrdata` or `.blitzraw-stacks.json`. Ratings and labels change only when I press Apply in the UI, and Apply must set explicit values; it must never toggle (spec 6.6).

**Order and checkpoints.** Stop at each checkpoint and wait for me.

1. **M0.1:** `ai_cull::source` and `cull_eval probe`. Run the probe on all my folders and show me the embedded-preview sizes per body and extension, plus the fallback share. Checkpoint: we decide what to do about the DNGs if their previews are small.
2. **M0.2–M0.4:** model registry and session pipeline, cache store, `cull_eval analyze`, HTML report, and the face-crop dumper.
   - Before hosting or downloading any model, show me its licence. No InsightFace weights.
   - Pin SHA256 hashes. Until I've set up a Hugging Face repo for hosting, load models from a local folder (`RAPIDRAW_TEST_MODELS_DIR`).
   - Checkpoint: tell me exactly what to label (the eye-crop folders and one grouping shoot) and how many of each. Then run the eye-model bake-off on my labels and report the results.
3. **M1:** grouping. Report pairwise F1 against my hand-grouped shoot across the τ sweep, and give me the HTML report path. Checkpoint.
4. **M2.1:** technical verdicts and `cull_eval eval`. Report every Milestone 2 metric from the spec against its target: false-reject rate against ★+ and against ★★+, the share of ★★+ frames that end up as Candidate, the candidate share, and the per-stage timings. Report on the held-out shoots, with thresholds tuned only on the tuning shoots. Checkpoint.
5. **M2.2–M2.3:** Tauri commands with cancellation, settings, the results UI and Apply. Checkpoint: I try it on a fresh shoot.

**How to work**

- Match the surrounding code's style, naming and comment density. Reuse what exists (the spec names the functions).
- Run these before each commit:
  - `npm run typecheck`, `npm run lint`, `npm run format:check`;
  - `npm run i18n:check` if UI strings changed;
  - from `src-tauri`: `cargo fmt -p BlitzRaw -- --check`, `cargo clippy --all-targets --all-features -- -D warnings` and `cargo test --lib`.
- Commit locally after each step with a clear message. Ask me before pushing.
- Report numbers exactly as measured, including misses. If a target is out of reach, tell me and propose options. Never quietly loosen a threshold or tune on the held-out shoots to hit it.
- When reality differs from the spec (a model choice, probe results, a better threshold), update `docs/ai-culling-spec.md` in the same commit so it stays the source of truth.
- If something in the spec looks wrong once you're in the code, say so and argue it. Don't just work around it.
