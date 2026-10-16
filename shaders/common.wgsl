// Per-frame values shared by every scene pass. Must match `Globals` in
// src/renderer/mod.rs. Audio features join this struct in milestone 2.

struct Globals {
    resolution: vec2<f32>,
    frame: u32,
    fps: f32,
};

@group(0) @binding(0) var<uniform> globals: Globals;
