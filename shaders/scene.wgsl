// Composites the scene layers (PLAN §5) in one pass: displacement and
// jitter, the scene(s) and their transition, the title, then snow. Snow is
// sampled at the displaced position too, so the interference tears and moves
// with the picture as one signal. Writes linear colour to a float target for
// the CRT pass, with alpha = how much of the disturbance each pixel carries
// (the CRT pass scales its chroma split by it).

@group(0) @binding(3) var title_tex: texture_2d<f32>;
@group(0) @binding(4) var title_smp: sampler;

// Brightness of the static scene at no / full snow: it never goes fully
// dark, but energy makes it fiercer.
const STATIC_DIM: f32 = 0.45;
// Width of the band of static that a noise dissolve sweeps through, in
// progress units.
const DISSOLVE_EDGE: f32 = 0.2;

// A photo over the (black) background. Each part takes its share of the
// displacement: the photo `react_image`, the background `react_background`
// and both at least `react_edge` near the outline, so a photo that moves
// while the background holds tears and shakes across it.
fn photo_colour(layer: u32, screen: vec2<f32>, uv: vec2<f32>) -> vec4<f32> {
    let near = 1.0 - smoothstep(0.0, globals.edge_width, abs(outline_distance(screen, layer)));
    let edge = globals.react_edge * near;
    let image = max(globals.react_image, edge);
    let background = max(globals.react_background, edge);
    // Shortest way round, so a partial share of a roll doesn't jump across
    // the frame.
    var offset = uv - screen;
    var pos = screen + offset * image;
    if (globals.roll > 0.0) {
        offset.y -= round(offset.y);
        pos = screen + offset * image;
        pos.y = fract(pos.y);
    }
    let p = sample_photo(pos, layer);
    return vec4<f32>(p.rgb * p.a, mix(background, image, p.a));
}

// Colour and disturbance share of one scene.
fn scene_colour(kind: u32, layer: u32, screen: vec2<f32>, uv: vec2<f32>) -> vec4<f32> {
    switch kind {
        case 0u: {
            let n = snow_value(uv) * mix(STATIC_DIM, 1.0, globals.snow);
            return vec4<f32>(to_linear(vec3<f32>(n)), 1.0);
        }
        case 1u: {
            return vec4<f32>(0.0, 0.0, 0.0, globals.react_background);
        }
        default: {
            return photo_colour(layer, screen, uv);
        }
    }
}

fn base_colour(screen: vec2<f32>, uv: vec2<f32>) -> vec4<f32> {
    let p = globals.progress;
    if (p >= 1.0 || globals.transition == 0u) {
        return scene_colour(globals.to_kind, globals.to_layer, screen, uv);
    }
    let a = scene_colour(globals.from_kind, globals.from_layer, screen, uv);
    let b = scene_colour(globals.to_kind, globals.to_layer, screen, uv);
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
        return vec4<f32>(to_linear(vec3<f32>(snow_value(uv))), 1.0);
    }
    return a;
}

// The title as part of the signal (scaled by `title_disturb`; the rest is
// drawn clean in the CRT pass). It always carries the full disturbance.
fn with_title(c: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {
    let a = textureSampleLevel(title_tex, title_smp, uv, 0.0).r * title_reveal(uv) * globals.title_disturb;
    return vec4<f32>(mix(c.rgb, globals.title_colour.rgb, a), mix(c.a, 1.0, a));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let d = displace(in.uv);
    let uv = jittered_uv(d.uv);
    let c = with_title(base_colour(in.uv, uv), uv);
    let share = c.a;
    return vec4<f32>(apply_snow(c.rgb, uv, share) * mix(1.0, d.dim, share), share);
}
