// Composites the scene layers (PLAN §5) in one pass: displacement and
// jitter, the scene(s) and their transition, the title, then snow. Snow is
// sampled at the displaced position too, so the interference tears and moves
// with the picture as one signal. Writes linear colour to a float target for
// the CRT pass.

@group(0) @binding(3) var title_tex: texture_2d<f32>;
@group(0) @binding(4) var title_smp: sampler;

// Brightness of the static scene at no / full snow: it never goes fully
// dark, but energy makes it fiercer.
const STATIC_DIM: f32 = 0.45;
// Width of the band of static that a noise dissolve sweeps through, in
// progress units.
const DISSOLVE_EDGE: f32 = 0.2;

fn scene_colour(kind: u32, layer: u32, uv: vec2<f32>) -> vec3<f32> {
    switch kind {
        case 0u: {
            let n = snow_value(uv) * mix(STATIC_DIM, 1.0, globals.snow);
            return to_linear(vec3<f32>(n));
        }
        case 1u: {
            return vec3<f32>(0.0);
        }
        default: {
            return sample_photo(uv, layer);
        }
    }
}

fn base_colour(uv: vec2<f32>) -> vec3<f32> {
    let p = globals.progress;
    if (p >= 1.0 || globals.transition == 0u) {
        return scene_colour(globals.to_kind, globals.to_layer, uv);
    }
    let a = scene_colour(globals.from_kind, globals.from_layer, uv);
    let b = scene_colour(globals.to_kind, globals.to_layer, uv);
    if (globals.transition == 1u) {
        return mix(a, b, p);
    }
    // Noise dissolve: each snow cell flips to the new scene at its own moment,
    // flickering as static for a while first.
    let cell = grid_cell(uv);
    let h = hash3(cell.x, cell.y, 40u + globals.transition_seed * 17u).x;
    let q = p * (1.0 + DISSOLVE_EDGE);
    if (h < q - DISSOLVE_EDGE) {
        return b;
    }
    if (h < q) {
        return to_linear(vec3<f32>(snow_value(uv)));
    }
    return a;
}

// The title as part of the signal (scaled by `title_disturb`; the rest is
// drawn clean in the CRT pass).
fn with_title(c: vec3<f32>, uv: vec2<f32>) -> vec3<f32> {
    let a = textureSampleLevel(title_tex, title_smp, uv, 0.0).r * title_reveal(uv) * globals.title_disturb;
    return mix(c, globals.title_colour.rgb, a);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let d = displace(in.uv);
    let uv = jittered_uv(d.uv);
    let c = with_title(base_colour(uv), uv);
    return vec4<f32>(apply_snow(c, uv) * d.dim, 1.0);
}
