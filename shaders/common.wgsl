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
    // Per click, newest first: (seed, intensity now, click strength, unused).
    events: array<vec4<f32>, 4>,
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
