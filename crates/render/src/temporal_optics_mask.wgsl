struct OpticsMaskOutput {
    @location(0) motion:vec4<f32>,
    @location(1) surface:vec4<u32>,
};
fn invalid_optical_history()->OpticsMaskOutput {
    return OpticsMaskOutput(vec4(0.0),vec4(0u));
}
