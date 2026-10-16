// TV snow on a coarse virtual grid, re-rolled every frame.

// Snow amount where the snow starts to take over the image, and where it has
// fully replaced it. Below the start it is grain over the photo.
const COVER_START: f32 = 0.35;
const COVER_FULL: f32 = 1.0;
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
    let n = snow_value(uv);
    var c = to_perceptual(linear) + (n - 0.5) * GRAIN * s;
    c = mix(c, vec3<f32>(n), smoothstep(COVER_START, COVER_FULL, s));
    return to_linear(c);
}
