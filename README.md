# BlitzRaw

A fast RAW photo editor and library, built for getting through a lot of frames.

BlitzRaw is a fork of [RapidRAW](https://github.com/CyberTimon/RapidRAW) by CyberTimon. RapidRAW did the hard part: a GPU-accelerated, non-destructive RAW editor in Rust that starts in under a second. This fork reshapes it into something closer to a Lightroom replacement, for event photography and commercial real estate work.

**Status: alpha.** I use it for paid work, so it has to keep working, but it is still moving and things break. There are no releases yet. If you want something finished, use [RapidRAW](https://github.com/CyberTimon/RapidRAW) itself.

## What this fork adds

**Cull and adjust with the keyboard alone.** The part I built it for. After an event there are two thousand frames and the job is to get through them. Exposure, white balance and framing all move a step at a time under the keys, batched so holding a key does not queue a hundred renders behind it. Ratings, picks and range selection never need the mouse.

**Brackets grouped and merged.** Property work is bracketed, so a shoot is three times longer than it looks. Brackets are proposed from the exposure signature and shown as one item. A merge is written as a linear DNG with JPEG XL pixels, which took one real merge from 545 MB to 20 MB.

**Photos open without waiting for a decode.** A full decode of a Z9 DNG is 1.42 seconds and 545 MB. A rendered preview of 172 KB now sits on disk beside the photo and appears at once. The previews travel with the shoot rather than living in a catalog, so moving a folder to another drive does not throw them away.

**Colour from the camera's own calibration.** The upstream editor leans on rawler's calibration, which is a reasonable general guess. This reads the real colour matrices out of the file and does the DNG colour maths properly. This is the one place where the fork genuinely diverges from upstream.

**Panels on a second screen.** Scopes, metadata and the navigator detach into their own window, owned by the main one so the two minimise together. They keep off whichever screen is showing the photo.

**One door for saving.** Every write to a photo's settings goes through one place, with one lock per file and atomic writes. The history keeps step numbers and a bookmark, so you can jump back to step 12, carry on, and have the steps after it still mean something.

**Things the camera already knew.** Nikon compression mode read without decoding, in-camera star ratings out of the XMP packet the camera writes inside the file, and DNG conversion through Adobe's converter that never deletes an original.

Everything else, the editing tools, the masks, the AI features, the export pipeline, is RapidRAW's and is documented in [its README](https://github.com/CyberTimon/RapidRAW#readme).

## What it never does

- **It never writes to your original files.** Ratings, labels and edits go to sidecars beside the photo.
- **It never deletes without the Recycle Bin.** Anything removed goes there, not into nothing.
- **It never keeps a catalog you have to look after.** Everything a shoot needs travels in the shoot's own folder.

## Building it

You need [Rust](https://www.rust-lang.org/tools/install), [Node.js](https://nodejs.org/), and the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for your platform.

```bash
git clone https://github.com/Accent-Studio/BlitzRaw.git
cd BlitzRaw
npm install
npm run tauri dev
```

For a real build:

```bash
npm run tauri build
```

Tests:

```bash
npm run build                      # the front-end gate
cargo test --lib                   # from src-tauri
```

Some backend tests need real camera files and are skipped unless you point them at some. They are named in the test modules that use them.

## Where it keeps its data

Under your local app data folder, in `com.blitzraw.app`. Settings, presets, albums, LUTs, AI models and the log all live there.

The folder is chosen at startup and proved rather than assumed: each candidate is written to, read back and compared, and only then used. That exists because writes to the roaming folder silently did not stick on one machine for six days. The log says which folder won on every start.

Set `BLITZRAW_DATA_DIR` to put it somewhere else. Creating a folder called `BlitzRaw-Data` beside the executable makes it a portable install.

Beside your photos it writes two things: a `.blitzraw-previews` folder of rendered previews, and a `.blitzraw-stacks.json` file recording which frames belong to which bracket. Both are safe to delete, and both are rebuilt. Anything copying a shoot elsewhere should skip the previews folder.

## Licence

**AGPL-3.0**, inherited from RapidRAW and permanent. See [LICENSE](LICENSE).

That means you can use, study, change and share this, and anything you build on it stays under the same licence. If you run a modified version as a network service, you have to offer its source to the people using it.

## Thanks

To **CyberTimon** for [RapidRAW](https://github.com/CyberTimon/RapidRAW), which is the foundation this is built on and most of the code here.

And to the projects RapidRAW itself builds on, credited in full inside the app under Settings: rawler, lensfun, darktable, LaMa, SAM 2, U2-Net, Depth Anything and NIND.
