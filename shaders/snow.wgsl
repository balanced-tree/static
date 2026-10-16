// TV snow on a coarse virtual grid, re-rolled every frame.

// Below COVER_START the snow is grain over the photo; from there it takes
// over, up to COVER_MAX at snow = 1. Short of 1 on purpose: like a weak TV
// signal, a ghost of the object stays visible (pure static is its own scene).
const COVER_START: f32 = 0.45;
const COVER_MAX: f32 = 0.88;
// Grain strength (perceptual units) at snow = 1, before coverage.
const GRAIN: f32 = 0.35;

fn snow_at(cell: vec2<u32>, frame: u32) -> f32 {
    return hash3(cell.x, cell.y, frame).x;
}

fn snow_value(uv: vec2<f32>) -> f32 {
    let rows = globals.snow_rows;
    let cols = round(rows * globals.resolution.x / globals.resolution.y);
    // Wrap rather than clamp: torn and rolled positions leave the frame.
    let cell = vec2<u32>(fract(uv) * vec2<f32>(cols, rows));
    var n = snow_at(cell, globals.frame);
    // Phosphor persistence: bright dots of the last two frames linger,
    // fading. Re-evaluated from their hashes rather than kept in a feedback
    // buffer, so every frame still renders on its own (seek, chunked renders).
    let p = globals.persistence;
    if (p > 0.0) {
        if (globals.frame >= 1u) {
            n = max(n, p * snow_at(cell, globals.frame - 1u));
        }
        if (globals.frame >= 2u) {
            n = max(n, p * p * snow_at(cell, globals.frame - 2u));
        }
    }
    return n;
}

fn apply_snow(linear: vec3<f32>, uv: vec2<f32>) -> vec3<f32> {
    let s = globals.snow;
    if (s <= 0.0) {
        return linear; // exactly the photo at rest
    }
    let n = snow_value(uv);
    var c = to_perceptual(linear) + (n - 0.5) * GRAIN * s;
    c = mix(c, vec3<f32>(n), COVER_MAX * smoothstep(COVER_START, 1.0, s));
    return to_linear(c);
}
