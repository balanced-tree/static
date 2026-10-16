# Static: visualiser plan

Visualiser for **_still, charged_**, a composition built from ETHER receiver recordings of the electromagnetic interference around household objects. The piece contrasts the *apparent stillness* of domestic objects with the *restless electrical activity* around them. The visualiser renders that tension: still photographs of the objects, disturbed by noise, jitter and static driven by the audio.

---

## 1. Decisions so far (from interview)

| Topic | Decision |
|---|---|
| Output | **Offline-rendered video**, 4K 16:9 (3840×2160), **25 fps** |
| Delivery | **YouTube upload** (MP4, see §9) |
| Audio source | Finished **WAV** file, analysed as a **mono** sum |
| Duration | 5–15 min |
| Structure | **Continuous drift**: no hard sections |
| Imagery | **3–5 own close-up photographs** of the recorded objects (DI boxes, light switches, …) |
| Object changes | **Timeline keyframes** in a hand-written cue file |
| Opening / ending | **Configurable in `timeline.toml`**: default is static |
| Title | **Configurable title in the opening**, set in `timeline.toml` |
| Aesthetic | **Analog static** (CRT snow, scanlines, signal tearing) **+ disturbed objects** |
| Stillness ↔ restlessness mapping | **Tunable**, explored in preview |
| Colour | **Tunable** (mono → desaturated + RGB split → full colour) |
| Audio features tracked | **Broadband hiss**, **clicks/transients** and **loudness** (louder = more snow/distortion) |
| Mains hum | 50 Hz; kept as an **optional, off-by-default** feature (probably unused) |
| Preview | **Live window with audio playback and scrubbing**; final render is offline |
| Stack | **Rust** |

---

## 2. Core idea: analyse once, render deterministically

Since the output is a rendered video and not a live performance, nothing has to happen in real time. The pipeline splits into three stages that share one feature file and one set of shaders:

```
 audio.wav ──► [analyse] ──► features.bin (per-frame feature values)
                                   │
 timeline.toml (cues, params) ─────┤
 photos/*.jpg ─────────────────────┤
 fonts/*.ttf (title) ──────────────┤
                                   ▼
                     ┌─────────────┴─────────────┐
                     ▼                           ▼
               [preview]                    [render]
        winit window, ~1080p,          headless wgpu, 4K,
        audio playback, scrub,         frame-by-frame,
        live param sliders             raw RGBA → ffmpeg stdin
                                            │
                                            ▼
                                   static.mp4 (+ audio muxed)
```

**Determinism rule:** every visual value is a pure function of `(frame_index, features, timeline, seed)`. No wall-clock time and no unseeded randomness. Noise is hashed from frame index and pixel coordinates. A frame then looks identical in preview and in the final render, so you can scrub to 7:42, tweak a parameter, and know the 4K output will match.

---

## 3. Stack

**Recommendation: plain `wgpu` + `winit` + `egui`, not nannou.**

nannou is pleasant for sketching, but it has historically trailed `wgpu` releases, and its app loop assumes a window. Here the main output is a **headless offline render**, which raw `wgpu` handles directly: render to a texture, copy it to a buffer, read it back. The preview window is a thin `winit` layer over the same renderer.

| Concern | Crate |
|---|---|
| GPU / shaders | `wgpu` (shaders in WGSL) |
| Window + input | `winit` |
| Parameter UI in preview | `egui` + `egui-wgpu` + `egui-winit` |
| WAV decoding | `hound` (simple, WAV-only; swap for `symphonia` if other formats are ever needed) |
| Audio playback (preview) | `cpal` (or `rodio` for simplicity) |
| FFT | `realfft` (on top of `rustfft`) |
| Image loading / resizing | `image` (+ `fast_image_resize` for high-quality upscaling) |
| Title text | `ab_glyph`: rasterises the title to a texture once at load, so rendering stays deterministic |
| Timeline / params | `serde` + `toml` |
| Hot reload (shaders, timeline) | `notify` |
| CLI | `clap` |
| Video encode | external `ffmpeg`, fed raw frames over stdin |

---

## 4. Audio analysis (`static analyse`)

The WAV is decoded (any common sample rate or bit depth: 16/24-bit int or 32-bit float) and summed to mono, then analysed with a short-time FFT: a 4096-sample Hann window centred on each video frame (hop = sample rate / 25). The results are written as one feature vector per video frame to `out/<wav name>.features.bin`, with an SVG plot of every feature next to it (`.features.svg`).

### Primary features

**Broadband hiss → "snow"**
- **Spectral flatness** (geometric mean / arithmetic mean of the magnitude spectrum): ~1 for noise-like signal, ~0 for tonal signal. This is the "how much is it *static*" measure.
- **High-band energy** (≈ 2–16 kHz), normalised over the whole piece.
- Combined into `hiss ∈ [0,1]`, smoothed with a slow attack/release envelope.

**Clicks / transients → "tears"**
- **Spectral flux** (positive change in log-magnitude) on a finer grid (1024-sample windows every 256 samples, ≈ 5 ms), then adaptive-threshold peak picking. The threshold is the local median **plus *k* × the local spread** (MAD, over ±150 ms), so clicks still register on top of steady loud hiss. *k* = `--click-threshold` (default 4; lower = more clicks). On the synthetic test material it found every isolated click with no false positives. Clicks closer together than ~50 ms merge, which is finer than one 40 ms video frame anyway.
- Outputs:
  - `click_env ∈ [0,1]`: fast attack, short exponential decay (a few frames).
  - `click_events`: list of onset times + strengths. Used to *seed* discrete glitch events (which scanline band tears, by how much), so each click looks different but stays deterministic.

**Loudness → overall disturbance**
- **RMS** over each frame's own samples, converted to dB and normalised over the piece.
- `loud ∈ [0,1]`, smoothed with a medium attack/release envelope.
- **Louder = more snow and distortion.** `loud` scales snow density, jitter amplitude and tear strength together, on top of what `hiss` and clicks contribute on their own (see §5, "Disturbance mix").

### Secondary / support features
- **Mains hum (optional, off by default, likely unused)**: energy in narrow bands at 50, 100, 150, 200 … Hz. It could drive a slow, constant shimmer or roll bar. It is computed during analysis because it's cheap, but nothing reads it unless a cue enables it (`hum = { enabled = true }`). It is low priority: build it last, or skip it entirely.

### Normalisation & smoothing
- Normalise each feature to the 5th–95th percentile across the **whole piece** (possible because analysis is offline), so quiet passages still register. Frames below −70 dBFS count as silence: every feature reads 0 there and they are left out of the percentiles.
- The file stores **unsmoothed** normalised values. Smoothing (attack/release envelopes, click decay) is applied when the file is loaded, so it can be retuned without re-analysing.
- Every feature exposes `attack`, `release`, `gain` and `curve` (gamma) in `timeline.toml`, so you can tune them per passage.

---

## 5. Visual layers (WGSL passes)

Composited from back to front:

1. **Base scene:** whatever the current cue shows: an object photo, pure static or black (see §7), crossfaded. Slow Ken Burns drift is optional (default off: stillness is the point).
   - The photos are close-ups (DI boxes, light switches…), so each cue gets `fit` (`fill` crops to 16:9, `contain` letterboxes) and a `focus` point (normalised x/y) that controls the crop.
   - Close-up surface texture (plastic grain, screws, switch edges) should hold up well under jitter and snow.
   - **Any photo resolution is accepted.** At load, each photo is cropped to its `fit`/`focus` framing, then resized once on the CPU to the output resolution (Lanczos). If the crop is smaller than 3840×2160, the loader prints a warning with the upscale factor. Up to ~1.5× is usually invisible under the snow and CRT treatment. Beyond that, a per-cue `soften` option leans into the softness deliberately (slight blur + stronger grain) instead of showing upscaling artefacts.
2. **Signal displacement ("tears"):** driven by `click_env` / `click_events`
   - horizontal scanline-band offsets (VHS tracking tear)
   - block displacement / macro-blocking on strong clicks
   - brief vertical roll on the very strongest events
3. **Jitter:** continuous sub-pixel to few-pixel position noise on the image, scaled by `hiss` and `loud`. This is the "noise reacting to the music" layer.
4. **Snow / grain:** hashed per-frame noise. `hiss` and `loud` control density, contrast and the mix with the photo. Rendered at a configurable *virtual resolution* (e.g. 960×540, upscaled nearest-neighbour) so it reads as TV snow rather than invisible 4K-pixel noise.
   - **Title layer** (opening only, see §7): composited onto the base scene *before* displacement, jitter and snow, so it tears and dissolves with the signal like the photos do. Its `disturb` setting controls how much (0 = clean overlay drawn after the CRT pass).
5. **CRT treatment:** scanlines (one per snow row; bright lines spread like a real beam), slight bloom (via an 8× downsampled pass), vignette, chroma split, optional phosphor persistence so static leaves short trails.
   - Persistence is **not** a feedback buffer: that would make each frame depend on the frames rendered before it, breaking the determinism rule and `--from` chunked renders. Snow is a pure hash of the frame, so the shader re-evaluates the previous two frames' snow and keeps the brighter, decayed value. Only the snow persists, not the photo.
6. **Colour grade:** tunable (see §6).

### Disturbance mix

Each disturbance layer takes a weighted sum of the features. The weights are keyframeable, and the defaults below are a starting point:

| Layer | `hiss` | `click_env` | `loud` |
|---|---|---|---|
| Snow density | 0.6 | 0.1 | 0.5 |
| Jitter amplitude | 0.4 | 0.2 | 0.6 |
| Tear strength | 0.0 | 0.8 | 0.4 |
| Chroma split | 0.0 | 0.6 | 0.3 |

Snow and jitter then go through the stillness mapping below. Tears and chroma split only go through its `response` curve and skip `baseline`/`invert`, because they are events rather than a resting state: an inverted look would otherwise tear constantly through silence. This all runs on the CPU per frame (`src/look.rs`, with the click events in `src/tears.rs`); the shaders only receive the final amounts.

Snow is grain over the photo at low amounts and takes over from ~0.45 upwards, but stops at ~88 % coverage. Like a weak TV signal, a ghost of the object stays visible even at full disturbance. Pure static is reserved for `scene = "static"`.

### The "stillness" control (tunable, per §1)

One shared mapping decides how audio energy becomes disturbance. It supports the three readings from the interview so they can be compared in preview:

| Mode | Behaviour |
|---|---|
| `still_at_rest` | Silence = sharp, untouched photo. Energy tears it into static. |
| `always_restless` | A floor of static is always present (`baseline > 0`). Energy pushes it into heavy interference. |
| `inverted` | Static is the default. The object *resolves* out of noise when the signal drops, or on cue. |

In practice these are a `baseline`, a `response` curve and an `invert` flag, and all three can be keyframed. Until the timeline exists, `--stillness still-at-rest|always-restless|inverted` picks a preset. The piece can therefore drift between modes over its duration.

---

## 6. Colour (tunable)

Parameters, all keyframeable:
- `saturation` (0 = monochrome)
- `chroma_split`: RGB channel offset, optionally scaled by `click_env`
- `tint`: e.g. cold phosphor green/blue vs neutral
- `contrast`, `black_level`

Presets to try in preview: **Dead channel** (mono), **Muted + split** (desaturated, chromatic aberration on peaks), **Full**.

---

## 7. Timeline / cue file (`timeline.toml`)

Hand-authored and hot-reloaded in preview. Keyframes interpolate (linear or eased), which fits the continuous-drift structure. The repo's `timeline.toml` is a commented starter covering every key.

**Timing rules** (as built):
- A cue's **scene** change starts at `at` and lasts `crossfade` (default 6 s, `noise_dissolve`).
- A cue's **parameters** start moving at `at` and arrive after `ramp` (default 4 s). `ramp = "next"` drifts all the way to the next cue (continuous drift). `ease` is `smooth` (default), `linear` or `step`. Parameters a cue doesn't mention keep their values; before any cue sets one, it has its default.
- The intro's `resolve` into the first cue's photo starts at `hold`, regardless of that cue's `at`. The cue's parameters still start at its own `at`.
- The outro has its own `crossfade` (default 6 s) for the transition at `begin`. `end_on` replaces the **final second** of the video: black, static, or `cut` (no change).
- `black` is a dead screen and `freeze` holds the last photo perfectly still. Both turn the signal's disturbance off, fading it across the transition.
- Typos are errors (unknown keys, groups or values), so a misspelt setting never silently does nothing. Missing photos, audio or fonts are warnings, so a half-finished timeline still previews.
- Parameters per cue: `stillness`, `snow_mix`, `jitter_mix`, `tear_mix`, `chroma_mix`, `hiss`/`loud` (`attack`, `release`, `gain`, `curve`), `clicks` (`decay`, `gain`), `snow` (`rows`), `crt`, `colour`. One table in `src/params.rs` defines them all.
- Not built: Ken Burns drift and the mains-hum shimmer (both optional in this plan).

Each cue shows one **scene**:
- `photo = "photos/…"`: an object photo
- `scene = "static"`: full-frame snow, still audio-reactive (`hiss` drives density, clicks still tear)
- `scene = "black"`: black frame (a dead screen)

### Opening & ending

`[intro]` and `[outro]` blocks control how the piece starts and ends. The default for both is static:

```toml
[intro]
scene = "static"      # "static" | "black" | "photo" (uses the first cue's photo)
hold = "6s"           # how long it stays before resolving
resolve = "8s"        # transition into the first cue
transition = "noise_dissolve"

[outro]
scene = "static"      # "static" | "black" | "freeze" (hold the last photo, perfectly still)
begin = "-10s"        # relative to the end of the audio
transition = "noise_dissolve"
tail = "3s"           # extra frames after the audio ends (e.g. static → cut to black)
end_on = "black"      # final frame: "black" | "static" | "cut"
```

### Title

An optional `[title]` block shows the piece's title over the opening. If the block is absent, there is no title. It is timed relative to the start of the video, so it can sit on the intro static, the first photo, or the transition between them.

```toml
[title]
text = "still, charged"
subtitle = ""                 # optional second line, e.g. your name
font = "fonts/title.ttf"      # any TTF/OTF you drop in
size = 0.045                  # cap height as a fraction of frame height
subtitle_size = 0.022
position = [0.5, 0.5]         # normalised centre point
align = "center"              # "left" | "center" | "right"
colour = "#e6e6e6"
letter_spacing = 0.08         # em; wide spacing suits lowercase titles
appear = "3s"                 # when it starts to appear
reveal = "noise"              # "noise": resolves out of the static | "fade" | "cut"
reveal_time = "2.5s"
hold = "6s"
vanish = "noise"              # "noise" | "fade" | "cut"
vanish_time = "3s"
disturb = 0.5                 # how much it tears/jitters with the signal (0 = clean)
```

`[intro]` and `[outro]` are shorthand for ordinary cues at the start and end of the timeline, so anything a cue can do (stillness, colour, feature gains) also works inside them.

### Example

```toml
[meta]
audio = "audio/still_charged_master.wav"
fps = 25
seed = 1234

[intro]
scene = "static"
hold = "10s"
resolve = "8s"

[title]
text = "still, charged"
font = "fonts/title.ttf"
appear = "2s"
hold = "5s"
reveal = "noise"
vanish = "noise"

[[cue]]
at = "0:18"
photo = "photos/di_box.jpg"
fit = "fill"
focus = [0.45, 0.55]
stillness = { mode = "still_at_rest", baseline = 0.0 }
colour = { saturation = 0.0 }

[[cue]]
at = "3:10"
photo = "photos/light_switch.jpg"
crossfade = "12s"
transition = "noise_dissolve"   # or "dissolve"
stillness = { baseline = 0.15 }

[[cue]]
at = "6:05"
colour = { saturation = 0.3, chroma_split = 0.6 }
hiss = { gain = 1.4, release = "800ms" }

[outro]
scene = "static"
begin = "-10s"
end_on = "black"
```

---

## 8. Preview app (`static preview`)

- Window at ~1080p (or a fraction of 4K) using the **same shaders** as the final render.
- Audio playback synced to the visual clock: play/pause, scrub bar, jump to cue, loop a region.
- `egui` side panel with live sliders for every parameter. Sliders show the value at the playhead; moving one sets an **override** that holds for the whole piece until cleared. A **"copy as TOML"** button turns the overrides into a cue to paste into the timeline.
- Hot reload: changes to `.wgsl` and `timeline.toml` are picked up on save.
- Overlay toggle: feature meters (hiss, click_env, loud) and a click-event marker strip to check analysis against what you hear.

## 9. Final render (`static render`)

- Headless `wgpu`, 3840×2160 @ 25 fps.
- Each frame is read back as RGBA and piped to `ffmpeg` stdin, which muxes the original audio.
- `--from` / `--to` frame ranges for test renders and **resumable chunked renders** (15 min @ 25 fps = 22,500 4K frames). Chunks use identical encoder settings, so ffmpeg's concat demuxer can join them without re-encoding.
- Optional supersampling/AA flag for the final pass. *(Not built: the snow and scanlines sit on a coarse grid, so it would mainly smooth title edges. Revisit if they look jagged at 4K.)*
- As built: chunks (default 60 s) sit on absolute frame boundaries and are renamed into place only once complete. A fingerprint (timeline, photos, audio, font, shaders, size and encoder settings) discards stale chunks. `--from`/`--to` take frames or times. Frames are converted RGB→YUV with the BT.709 matrix *and* stamped BT.709 (`setparams`); without the stamp, recent ffmpeg leaves primaries and transfer untagged. A test showed decoded frames matching the PNG render to within 0.5 levels of mean shift per channel. Measured speed: about 10 fps at 4K with x264 `slow` on this Mac (≈40 min for 15 min of video), ~110 Mbit/s on noise-heavy material.
- GOP follows YouTube's recommendation: closed, 12 frames, 2 B-frames.

### Output format: YouTube

A single upload-ready **MP4**, so no ProRes master is needed (it would be ~80 GB for 15 min at 4K):
- **Video:** H.264 (`libx264`), `yuv420p`, BT.709, 25 fps, `-preset slow -crf 14`, `-movflags +faststart`. H.265 (`libx265`) is available as a flag for a smaller file.
- **Audio:** AAC-LC 384 kbps, 48 kHz. A `--audio-copy` option can add a lossless audio track instead, if the upload ends up as `.mov`.

**Grain vs. YouTube compression.** Fine noise is the hardest thing for a video encoder, and YouTube re-encodes everything. Mitigations built into the plan:
- Upload at **4K** even if most people watch at 1080p. YouTube gives 4K uploads a higher-bitrate encode (VP9/AV1) at every resolution.
- Upload at a **high bitrate**: the CRF ~14 above, or ~60–100 Mbps.
- Render snow at a **lower virtual resolution** (§5.4). Chunky TV-snow survives compression much better than per-pixel 4K noise.
- A `render --preview-youtube` mode encodes a short excerpt at roughly YouTube-like bitrates, to check that the snow doesn't collapse into mush.

---

## 10. Project layout

```
Static/
├── Cargo.toml
├── PLAN.md
├── timeline.toml
├── audio/            (gitignored)
├── photos/           (gitignored or LFS)
├── fonts/            (title font)
├── shaders/
│   ├── common.wgsl   (hash/noise, feature uniforms)
│   ├── photo.wgsl
│   ├── displace.wgsl
│   ├── snow.wgsl
│   ├── crt.wgsl
│   ├── title.wgsl
│   └── grade.wgsl
└── src/
    ├── main.rs       (clap: analyse | preview | render)
    ├── analysis/     (decode, stft, features, envelopes)
    ├── timeline.rs   (parse + interpolate cues → params at frame N)
    ├── assets.rs     (photo load/crop/resize, title rasterisation)
    ├── renderer/     (wgpu pipeline, passes, uniforms; window-agnostic)
    ├── preview/      (winit, egui, audio playback, scrubbing)
    └── export.rs     (headless loop, readback, ffmpeg pipe)
```

---

## 11. Milestones

1. ✅ **Skeleton:** cargo project, CLI, `wgpu` renders a photo to a window and headlessly to PNG.
2. ✅ **Analysis:** decode, STFT, hiss + click + loudness (+ optional hum) features → `features.bin`, plus an SVG debug plot.
3. ✅ **Snow + jitter:** first audio-reactive shader layers on one photo, plus the disturbance mix and stillness modes.
4. ✅ **Preview app:** audio playback, scrubbing, egui sliders.
5. ✅ **Tears + CRT:** click-driven displacement, scanlines, persistence.
6. ✅ **Timeline:** cues, `[intro]`/`[outro]`, `[title]` (text rasterisation + reveal/vanish), static/black scenes, crossfades, `noise_dissolve`, keyframed params, hot reload.
7. ✅ **Export:** ffmpeg pipe, YouTube-ready MP4, chunked/resumable renders, YouTube-bitrate excerpt check.
8. **Exploration pass:** compare stillness modes and colour presets against the actual piece, then lock the timeline.
9. **Final 4K render.**

---

## 12. Open questions

### Resolved
- Frame rate: **25 fps**
- Photos: **3–5 close-ups** (DI boxes, light switches, …)
- Opening/ending: **static by default, configurable** via `[intro]` / `[outro]`
- Mains hum: **optional, off by default**
- Delivery: **YouTube-ready MP4**
- Title: **configurable `[title]` block** in the opening
- Loudness: **drives snow/distortion amount**
- Audio: **WAV**
- Photo resolution: **unknown, so any size is handled** (resize at load, upscale warning, optional `soften`)

### Still open (non-blocking)
- **Title font:** which typeface? A mono or grotesk (e.g. IBM Plex Mono, Space Grotesk) fits the clinical, electrical feel. Any TTF in `fonts/` will work.
