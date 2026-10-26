struct Camera{view_projection:mat4x4<f32>,params:vec4<u32>};
struct Board{center:vec4<f32>,right_width:vec4<f32>,up_height:vec4<f32>,codes:vec4<u32>};
@group(0)@binding(0)var<uniform>camera:Camera;
@group(0)@binding(1)var<storage,read>boards:array<Board>;
@group(0)@binding(2)var images:texture_2d_array<f32>;
@group(0)@binding(3)var image_sampler:sampler;
// SHADOWS
struct VertexOut{@builtin(position)@invariant position:vec4<f32>,@location(0)uv:vec2<f32>,@location(1)world:vec3<f32>,@location(2)@interpolate(flat)normal:vec3<f32>,@location(3)@interpolate(flat)light:vec4<f32>,@location(4)@interpolate(flat)layer:u32};
@vertex fn vertex(@builtin(vertex_index)vertex:u32,@builtin(instance_index)instance:u32)->VertexOut{
 let item=boards[instance];let indices=array<u32,6>(0u,1u,2u,2u,1u,3u);let corner=indices[vertex];let uv=vec2(f32(corner&1u),f32(corner>>1u));
 let world=item.center.xyz+item.right_width.xyz*((uv.x-.5)*item.right_width.w)+item.up_height.xyz*((uv.y-.5)*item.up_height.w);
 let packed=item.codes.y;let levels=vec3<f32>(f32((packed>>8u)&255u),f32((packed>>16u)&255u),f32(packed>>24u));let mc=select(pow(vec3(.8),vec3(15.)-min(levels,vec3(15.))),vec3(0.),levels==vec3(0.));let rgb=select(mc,levels/15.,levels>vec3(15.));let sky=f32(packed&15u);
 var out:VertexOut;out.position=camera.view_projection*vec4(world,1.);if(item.codes.z&1u)==0u {out.position=vec4(2.,2.,2.,1.);}out.uv=vec2(uv.x,1.-uv.y);out.world=world;out.normal=cross(item.right_width.xyz,item.up_height.xyz);out.light=vec4(rgb,select(pow(.8,15.-sky),0.,sky==0.));out.layer=item.codes.x;return out;
}
fn illumination(world:vec3<f32>,normal:vec3<f32>,light:vec4<f32>)->vec3<f32>{// ILLUMINATION
}
// OUTPUT
@fragment fn fragment(input:VertexOut)->BoardOutput{
 let albedo=textureSample(images,image_sampler,input.uv,i32(input.layer)).rgb;
 let color=albedo*illumination(input.world,input.normal,input.light);
 // RETURN
}
