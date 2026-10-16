// Composites the scene layers (PLAN §5) in one pass. Snow is sampled at the
// jittered position too, so the interference moves with the picture as one
// signal.

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = jittered_uv(in.uv);
    return vec4<f32>(apply_snow(sample_photo(uv), uv), 1.0);
}
