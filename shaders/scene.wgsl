// Composites the scene layers (PLAN §5) in one pass: displacement, then
// jitter, then snow. Snow is sampled at the displaced position too, so the
// interference tears and moves with the picture as one signal. Writes
// linear colour to a float target for the CRT pass.

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let d = displace(in.uv);
    let uv = jittered_uv(d.uv);
    return vec4<f32>(apply_snow(sample_photo(uv), uv) * d.dim, 1.0);
}
