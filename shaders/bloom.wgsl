// Bloom, step 1: an 8× downsampled copy of the bright parts of the scene.
// 16 bilinear taps average exactly the 8×8 source pixels under each output
// pixel; the CRT pass blurs this further as it samples it.

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_smp: sampler;

// Linear luminance where the bloom starts and where it is full.
const BLOOM_KNEE: vec2<f32> = vec2<f32>(0.35, 1.0);

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(src_tex));
    var sum = vec3<f32>(0.0);
    for (var y = 0; y < 4; y++) {
        for (var x = 0; x < 4; x++) {
            let offset = vec2<f32>(f32(x * 2 - 3), f32(y * 2 - 3)) * texel;
            sum += textureSampleLevel(src_tex, src_smp, in.uv + offset, 0.0).rgb;
        }
    }
    let c = sum / 16.0;
    let luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    return vec4<f32>(c * smoothstep(BLOOM_KNEE.x, BLOOM_KNEE.y, luma), 1.0);
}
