// CRT treatment (PLAN §5.5): chroma split, scanlines, bloom and vignette over
// the finished scene. Fixed to the screen, so torn and rolled pictures move
// under the scanlines rather than with them. With every amount at 0 this is
// a straight copy.

@group(0) @binding(1) var scene_tex: texture_2d<f32>;
@group(0) @binding(2) var bloom_tex: texture_2d<f32>;
@group(0) @binding(3) var crt_smp: sampler;

// Red/blue offset at chroma = 1, as a fraction of the frame height.
const CHROMA_MAX: f32 = 0.004;
// Bloom blur radius in bloom texels (each 8 scene pixels).
const BLOOM_SPREAD: f32 = 1.5;

fn scene_at(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(scene_tex, crt_smp, uv, 0.0).rgb;
}

// 3×3 tent over the downsampled bright parts: a soft glow, not a halo.
fn bloom_at(uv: vec2<f32>) -> vec3<f32> {
    let texel = BLOOM_SPREAD / vec2<f32>(textureDimensions(bloom_tex));
    var sum = vec3<f32>(0.0);
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let w = f32((2 - abs(x)) * (2 - abs(y)));
            sum += w * textureSampleLevel(bloom_tex, crt_smp, uv + vec2<f32>(f32(x), f32(y)) * texel, 0.0).rgb;
        }
    }
    return sum / 16.0;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let aspect = globals.resolution.x / globals.resolution.y;

    var c = scene_at(uv);
    if (globals.chroma > 0.0) {
        let split = vec2<f32>(globals.chroma * CHROMA_MAX / aspect, 0.0);
        c = vec3<f32>(scene_at(uv + split).r, c.g, scene_at(uv - split).b);
    }
    if (globals.bloom > 0.0) {
        c += globals.bloom * bloom_at(uv);
    }

    // Scanlines: one per snow row, dark between lines. Bright lines spread
    // like a real beam, so highlights keep their brightness.
    if (globals.scanlines > 0.0) {
        let phase = fract(uv.y * globals.snow_rows);
        let line = 0.5 - 0.5 * cos(6.2831853 * phase);
        let luma = dot(to_perceptual(c), vec3<f32>(0.2126, 0.7152, 0.0722));
        c *= 1.0 - globals.scanlines * (1.0 - clamp(luma, 0.0, 1.0)) * (1.0 - line);
    }

    // Vignette, 0 at the centre to 1 in the corners.
    if (globals.vignette > 0.0) {
        let d = length((uv - 0.5) * vec2<f32>(aspect, 1.0)) / length(vec2<f32>(aspect, 1.0) * 0.5);
        c *= 1.0 - globals.vignette * smoothstep(0.35, 1.0, d);
    }
    return vec4<f32>(c, 1.0);
}
