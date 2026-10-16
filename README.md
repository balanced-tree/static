# Static

Visualiser for **_still, charged_**: still photographs of household objects, disturbed by the static, jitter and interference of their own electromagnetic signals (recorded with an ETHER receiver).

See [PLAN.md](PLAN.md) for the full design and milestones.

## Requirements

- Rust (stable)
- `ffmpeg` on your `PATH` (only needed for video export, milestone 7)

## Usage

```sh
# Render one 4K frame headlessly to a PNG (test card if no --photo)
cargo run -- frame
cargo run -- frame --photo photos/di_box.jpg --focus 0.5,0.4 -o out/di_box.png

# Analyse a WAV → out/<name>.features.bin + out/<name>.features.svg (open in a browser)
cargo run -- analyse audio/still_charged.wav
cargo run -- analyse audio/still_charged.wav --click-threshold 3   # more clicks

# Render a frame reacting to the audio (prints the feature values and disturbance)
cargo run -- frame --photo photos/di_box.jpg --features out/still_charged.features.bin --frame 1500
# …or a run of frames: out/frame_001500.png, out/frame_001501.png, …
cargo run -- frame --features out/still_charged.features.bin --frame 1500 --count 25

# Live preview window, looping over the piece (Esc to close; no audio yet)
cargo run -- preview --photo photos/light_switch.jpg --features out/still_charged.features.bin
```

Options shared by `frame` and `preview`:

| Flag | Meaning |
|---|---|
| `--photo <path>` | Photo to show. Omit for a generated test card. |
| `--fit fill\|contain` | `fill` crops to 16:9, `contain` letterboxes on black. |
| `--focus x,y` | Crop centre for `fill`, normalised 0..1. |
| `--features <file>` | Features from `analyse`. Without it the photo stays at rest. |
| `--stillness <mode>` | `still-at-rest` (default), `always-restless` or `inverted`. |
| `--seed N` | Seed for all noise: a different but repeatable take. |
| `--snow-rows N` | Snow grid rows (default 540). Lower = chunkier snow. |

`frame` also takes `--frame N`, `--count N`, `-o/--out`, `--width` and `--height` (default 3840×2160).
`preview` renders internally at `--width`/`--height` (default 1920×1080) and scales the result to the window.

## Testing

```sh
cargo test
```

## Folders

- `photos/`: your object photos (not committed)
- `audio/`: the WAV (not committed)
- `fonts/`: TTF/OTF files for the title
- `shaders/`: WGSL shaders
- `out/`: renders (not committed)
