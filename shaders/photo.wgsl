// Photos (pre-framed on the CPU, one texture layer each), displaced by jitter.

@group(0) @binding(1) var photo_tex: texture_2d_array<f32>;
@group(0) @binding(2) var photo_smp: sampler;

// Largest offsets at jitter = 1, as fractions of the frame height.
const SHAKE_MAX: f32 = 0.004;
const LINE_WOBBLE_MAX: f32 = 0.006;

// Whole-frame shake plus a per-scanline horizontal wobble, both fresh each
// frame. The wobble grows faster than the shake (j²), so low jitter reads as
// a slightly unstable picture and high jitter as a failing signal.
fn jittered_uv(uv: vec2<f32>) -> vec2<f32> {
    let j = globals.jitter;
    let aspect = globals.resolution.x / globals.resolution.y;
    let shake = (hash3(globals.frame, 0u, 11u).xy - 0.5) * 2.0 * SHAKE_MAX * j;
    let row = u32(uv.y * globals.snow_rows);
    let wobble = (hash3(globals.frame, row, 12u).x - 0.5) * 2.0 * LINE_WOBBLE_MAX * j * j;
    return uv + vec2<f32>((shake.x + wobble) / aspect, shake.y);
}

fn sample_photo(uv: vec2<f32>, layer: u32) -> vec3<f32> {
    return textureSampleLevel(photo_tex, photo_smp, uv, layer, 0.0).rgb;
}
