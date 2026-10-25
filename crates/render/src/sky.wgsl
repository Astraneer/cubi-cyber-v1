// M0 reference sky: procedural linear-light colors, no external assets.
// The same pipeline is used by Windows, WebGPU, and the software GPU test.
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> VertexOutput {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let p = positions[index];
    var output: VertexOutput;
    output.position = vec4<f32>(p, 0.0, 1.0);
    output.uv = vec2<f32>((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);
    return output;
}

@fragment
fn fragment(input: VertexOutput) -> @location(0) vec4<f32> {
    let zenith = vec3<f32>(0.08, 0.25, 0.60);
    let horizon = vec3<f32>(0.65, 0.82, 1.0);
    return vec4<f32>(mix(zenith, horizon, smoothstep(0.0, 1.0, input.uv.y)), 1.0);
}
