# Static

Visualiser for **_still, charged_**: still photographs of household objects, disturbed by the static, jitter and interference of their own electromagnetic signals (recorded with an ETHER receiver).

See [PLAN.md](PLAN.md) for the full design and milestones.

## Requirements

- Rust (stable)
- `ffmpeg` on your `PATH` for `render` (macOS: `brew install ffmpeg`)

## Usage

The piece is described by [`timeline.toml`](timeline.toml): audio, photos, cues, intro/outro and title (see PLAN.md §7; the file itself is commented). `frame` and `preview` use `./timeline.toml` automatically.

```sh
# Analyse a WAV → out/<name>.features.bin + out/<name>.features.svg (open in a browser)
cargo run -- analyse audio/still_charged.wav
cargo run -- analyse audio/still_charged.wav --click-threshold 3   # more clicks

# Live preview: audio playback, scrubbing, sliders, hot reload of timeline.toml and shaders/
cargo run -- preview

# Render frames headlessly to PNGs (4K by default; prints the feature values and disturbance)
cargo run -- frame --frame 1500
cargo run -- frame --frame 1500 --count 25          # out/frame_001500.png, out/frame_001501.png, …

# The video: 4K H.264 + AAC MP4 for YouTube → out/still_charged.mp4
cargo run --release -- render
cargo run --release -- render --from 3:00 --to 3:30 -o out/test.mp4   # an excerpt (frames or times)
cargo run --release -- render --preview-youtube --from 6:05           # 20 s at YouTube-like bitrates

# Without a timeline: one photo (or the test card), no intro/outro/title
cargo run -- preview --photo photos/di_box.jpg --audio audio/still_charged.wav
cargo run -- frame --photo photos/di_box.jpg --focus 0.5,0.4 -o out/di_box.png
```

Missing photos show the test card and a missing title font hides the title, with a warning, so the timeline previews before the material is in place. Typos in the timeline are errors.

**Still background.** By default the disturbance covers the whole frame. To make only the photo react, shrink it on its cue (`scale = 0.6`, optionally `position = [x, y]`) so black background shows around it, then set `react = { background = 0 }`. `react = { background = 0, image = 0, edge = 1 }` goes further: only a band along the photo's outline (`edge_width`, in frame heights) crackles and frays, and the middle stays still. Transparent PNGs and SVGs work as photos. Their outline is the shape itself, not the rectangle. The static intro and outro always fill the screen.

**Preview.** Sliders show each parameter at the playhead; moving one *overrides* it for the whole piece (× clears it). **Copy as TOML** turns the overrides into a `[[cue]]` at the playhead, ready to paste. Saving `timeline.toml` or a `.wgsl` file reloads it; errors show over the frame and the last working version stays. Keys: Space play/pause · ←/→ 5 s (⇧ one frame) · [ ] previous/next cue · Home loop start · L loop · I/O loop in/out · H hide panels · M meters · Esc quit. Click or drag the strip under the transport to seek; yellow marks are cues.

Options shared by `frame` and `preview`:

| Flag | Meaning |
|---|---|
| `--timeline <file>` | The timeline (default `./timeline.toml` if present). |
| `--audio <wav>` | Overrides `[meta] audio`. Analysed on the fly if there is no features file. |
| `--features <file>` | Overrides `[meta] features`. |
| `--seed N` | Overrides `[meta] seed`: a different but repeatable take. |
| `--photo <path>` | Without a timeline: the photo (omit for a test card). |
| `--fit fill\|contain` | Without a timeline: `fill` crops to 16:9, `contain` letterboxes. |
| `--focus x,y` | Without a timeline: crop centre for `fill`, normalised 0..1. |
| `--stillness <mode>` | Without a timeline: `still-at-rest` (default), `always-restless` or `inverted`. |

**Render.** Frames are encoded in 60 s chunks (`--chunk`) under `out/<name>.chunks/`, then joined without re-encoding and muxed with the original audio. An interrupted render picks up where it stopped when you run the same command again. If the timeline, photos, audio, font, shaders or encoder settings changed in between, it starts over rather than mixing old and new chunks. Output is H.264 `yuv420p` BT.709 (CRF 14, `--preset slow`); `--codec h265` gives a smaller file, `--crf` trades size for quality, and `--audio-copy` puts lossless PCM audio in a `.mov`. `--preview-youtube` writes `<name>.youtube-1080p.mp4` and `-2160p.mp4` at roughly YouTube's bitrates (8 and 25 Mbit/s, x264, so a little harsher than YouTube's VP9/AV1), to check that the snow doesn't collapse into mush; fewer `snow = { rows }` survives better.

`frame` also takes `--frame N`, `--count N`, `-o/--out`, `--width` and `--height` (default 3840×2160).
`preview` renders internally at `--width`/`--height` (default 1920×1080) and scales the result to the window.

## Testing

```sh
cargo test
```

## Folders

- `photos/`: your object photos: JPEG/PNG, or SVG (rendered sharp at the output size); transparency shows the background (not committed)
- `audio/`: the WAV (not committed)
- `fonts/`: TTF/OTF files for the title (`timeline.toml` expects `fonts/title.ttf`)
- `shaders/`: WGSL shaders
- `out/`: renders (not committed)
