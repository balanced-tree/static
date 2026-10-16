// TV snow on a coarse virtual grid, re-rolled every frame.

// Below COVER_START the snow is grain over the photo; from there it takes
// over, up to COVER_MAX at snow = 1. Short of 1 on purpose: like a weak TV
// signal, a ghost of the object stays visible (pure static is its own scene).
const COVER_START: f32 = 0.45;
const COVER_MAX: f32 = 0.88;
// Grain strength (perceptual units) at snow = 1, before coverage.
const GRAIN: f32 = 0.35;

fn snow_value(uv: vec2<f32>) -> f32 {
    let rows = globals.snow_rows;
    let cols = round(rows * globals.resolution.x / globals.resolution.y);
    let cell = vec2<u32>(clamp(uv, vec2<f32>(0.0), vec2<f32>(0.9999)) * vec2<f32>(cols, rows));
    return hash3(cell.x, cell.y, globals.frame).x;
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
