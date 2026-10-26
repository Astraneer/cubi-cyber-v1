struct Params { exposure:vec4<f32>, adaptation:vec4<f32>, bloom:vec4<f32>, image:vec4<u32> };
@group(0) @binding(0) var hdr:texture_2d<f32>;
@group(0) @binding(1) var<storage,read> exposure_state:vec4<f32>;
@group(0) @binding(2) var<uniform> params:Params;
// BLOOM_DECLARATIONS
struct Vertex { @builtin(position) position:vec4<f32>, @location(0) uv:vec2<f32> };
@vertex fn vertex(@builtin(vertex_index) index:u32)->Vertex {
    let positions=array<vec2<f32>,3>(vec2(-1.0,-1.0),vec2(3.0,-1.0),vec2(-1.0,3.0));
    var output:Vertex;output.position=vec4(positions[index],0.0,1.0);output.uv=positions[index]*vec2(0.5,-0.5)+vec2(0.5);return output;
}
fn linear_to_srgb(linear:vec3<f32>)->vec3<f32> {
    return select(1.055*pow(linear,vec3(1.0/2.4))-vec3(0.055),linear*12.92,linear<=vec3(0.0031308));
}
fn sample_hdr(uv:vec2<f32>, size:vec2<i32>)->vec3<f32> {
    // Derivatives describe the real output attachment, without another uniform
    // or sampler. Equal-resolution output keeps the exact original texel load.
    let footprint=fwidth(uv)*vec2<f32>(size);
    if all(footprint>=vec2(0.999)) {
        return textureLoad(hdr,clamp(vec2<i32>(uv*vec2<f32>(size)),vec2(0),size-vec2(1)),0).rgb;
    }
    let point=uv*vec2<f32>(size)-vec2(0.5);
    let low=vec2<i32>(floor(point));let weight=fract(point);
    let a=textureLoad(hdr,clamp(low,vec2(0),size-vec2(1)),0).rgb;
    let b=textureLoad(hdr,clamp(low+vec2(1,0),vec2(0),size-vec2(1)),0).rgb;
    let c=textureLoad(hdr,clamp(low+vec2(0,1),vec2(0),size-vec2(1)),0).rgb;
    let d=textureLoad(hdr,clamp(low+vec2(1,1),vec2(0),size-vec2(1)),0).rgb;
    return mix(mix(a,b,weight.x),mix(c,d,weight.x),weight.y);
}
@fragment fn fragment(input:Vertex)->@location(0)vec4<f32> {
    let size=vec2<i32>(textureDimensions(hdr));
    var color=max(sample_hdr(input.uv,size),vec3(0.0));
    // BLOOM_COMPOSITE
    let exposed=color*exposure_state.x*params.adaptation.w;
    let mapped=clamp((exposed*(2.51*exposed+vec3(0.03)))/(exposed*(2.43*exposed+vec3(0.59))+vec3(0.14)),vec3(0.0),vec3(1.0));
    // OUTPUT_TRANSFER
    return vec4(mapped,1.0);
}
