// M0 reference sky: procedural linear-light colors, no external assets.
// The same sky model is used by Windows, WebGPU, and the software GPU test.
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

fn sky_linear(uv: vec2<f32>) -> vec3<f32> {
    let zenith = vec3<f32>(0.08, 0.25, 0.60);
    let horizon = vec3<f32>(0.65, 0.82, 1.0);
    return mix(zenith, horizon, smoothstep(0.0, 1.0, uv.y));
}

fn linear_to_srgb(linear: vec3<f32>) -> vec3<f32> {
    let low = linear * 12.92;
    let high = 1.055 * pow(linear, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(high, low, linear <= vec3<f32>(0.0031308));
}

// Native sRGB attachments perform the output transfer; the linear probe does not.
@fragment
fn fragment(input: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(sky_linear(input.uv), 1.0);
}

// WebGPU unorm canvas: explicit final transfer avoids backend sRGB-view resolve
// inconsistencies. Selected once at startup; the shared sky remains linear.
@fragment
fn fragment_srgb_output(input: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(linear_to_srgb(sky_linear(input.uv)), 1.0);
}
