@group(0) @binding(0) var sky_view:texture_2d<f32>;
@group(0) @binding(1) var sky_filter:sampler;
struct View { inverse_view_projection:mat4x4<f32> };
@group(0) @binding(2) var<uniform> view:View;
struct SkyVertex { @builtin(position) position:vec4<f32>, @location(0) ndc:vec2<f32> };
@vertex fn vertex(@builtin(vertex_index) index:u32)->SkyVertex {
    let positions=array<vec2<f32>,3>(vec2(-1.0,-1.0),vec2(3.0,-1.0),vec2(-1.0,3.0));
    var output:SkyVertex;output.position=vec4(positions[index],0.0,1.0);output.ndc=positions[index];return output;
}
@fragment fn fragment(input:SkyVertex)->@location(0)vec4<f32> {
    let near=view.inverse_view_projection*vec4(input.ndc,1.0,1.0);
    let far=view.inverse_view_projection*vec4(input.ndc,0.0,1.0);
    let direction=normalize(far.xyz/far.w-near.xyz/near.w);
    let uv=vec2(atan2(direction.z,direction.x)/(2.0*3.141592653589793)+0.5,direction.y*0.5+0.5);
    return vec4(textureSampleLevel(sky_view,sky_filter,uv,0.0).rgb,0.0);
}
