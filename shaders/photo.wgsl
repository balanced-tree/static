// Base scene: the framed photo, sampled 1:1 (it is pre-sized on the CPU).

@group(0) @binding(1) var photo_tex: texture_2d<f32>;
@group(0) @binding(2) var photo_smp: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(photo_tex, photo_smp, in.uv).rgb, 1.0);
}
