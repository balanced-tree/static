// Per-frame values shared by every pass. Must match `Globals` in
// src/renderer/mod.rs.

struct Globals {
    resolution: vec2<f32>,
    frame: u32,
    fps: f32,
    // Disturbance amounts for this frame, 0..1 (see src/look.rs).
    snow: f32,
    jitter: f32,
    // Rows of the virtual snow grid; columns follow the aspect ratio. Fixed
    // in rows (not pixels) so preview and 4K render show the same pattern.
    snow_rows: f32,
    seed: u32,
    // Click-driven displacement (see src/tears.rs): overall tear amount,
    // RGB split, vertical roll (fraction of frame height) and how many of
    // `events` are valid.
    tear: f32,
    chroma: f32,
    roll: f32,
    event_count: u32,
    // CRT treatment, each 0..1 (0 = off).
    scanlines: f32,
    vignette: f32,
    bloom: f32,
    persistence: f32,
    // Scenes (see src/timeline.rs): kind 0 static, 1 black, 2 photo; the
    // transition (0 cut, 1 dissolve, 2 noise dissolve) from `from` to `to`.
    from_kind: u32,
    from_layer: u32,
    to_kind: u32,
    to_layer: u32,
    transition: u32,
    transition_seed: u32,
    progress: f32,
    soften: f32,
    // Title: visibility, whether it is resolving through the static, and how
    // much of it is part of the (disturbed) signal vs a clean overlay.
    title_alpha: f32,
    title_noise: u32,
    title_disturb: f32,
    // Colour grade; skipped entirely unless `grade_on`.
    saturation: f32,
    contrast: f32,
    black_level: f32,
    grade_on: u32,
    _pad: u32,
    // Per click, newest first: (seed, intensity now, click strength, unused).
    events: array<vec4<f32>, 4>,
    // Linear RGB.
    title_colour: vec4<f32>,
    tint: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

// PCG3D hash (Jarzynski & Olano, 2020). All randomness comes from here, keyed
// on frame/pixel/seed, so every frame is reproducible.
fn pcg3d(v_in: vec3<u32>) -> vec3<u32> {
    var v = v_in * 1664525u + 1013904223u;
    v.x += v.y * v.z;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    v ^= v >> vec3<u32>(16u);
    v.x += v.y * v.z;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    return v;
}

// Three uniform values in 0..1.
fn hash3(x: u32, y: u32, z: u32) -> vec3<f32> {
    return vec3<f32>(pcg3d(vec3<u32>(x, y, z ^ globals.seed))) / 4294967295.0;
}

// Approximate sRGB transfer, for effects that should be even to the eye.
fn to_perceptual(c: vec3<f32>) -> vec3<f32> {
    return pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2));
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    return pow(max(c, vec3<f32>(0.0)), vec3<f32>(2.2));
}

// The snow grid cell under `uv`. Wraps rather than clamps: torn and rolled
// positions leave the frame.
fn grid_cell(uv: vec2<f32>) -> vec2<u32> {
    let rows = globals.snow_rows;
    let cols = round(rows * globals.resolution.x / globals.resolution.y);
    return vec2<u32>(fract(uv) * vec2<f32>(cols, rows));
}

// How much of the title shows at `uv`: a plain fade, or (while resolving
// through the static) whole snow cells switching on in a fixed random order.
fn title_reveal(uv: vec2<f32>) -> f32 {
    let a = globals.title_alpha;
    if (globals.title_noise == 0u || a <= 0.0 || a >= 1.0) {
        return a;
    }
    let cell = grid_cell(uv);
    return select(0.0, 1.0, hash3(cell.x, cell.y, 51u).x < a);
}
