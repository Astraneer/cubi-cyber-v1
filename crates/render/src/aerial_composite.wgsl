struct View{inverse:mat4x4<f32>,eye_distance:vec4<f32>,grid:vec4<u32>,work:vec4<u32>};
@group(0)@binding(0)var<uniform>view:View;
@group(0)@binding(1)var scene_color:texture_multisampled_2d<f32>;
@group(0)@binding(2)var scene_depth:texture_depth_multisampled_2d;
@group(0)@binding(3)var scattering:texture_3d<f32>;
@group(0)@binding(4)var transmission:texture_3d<f32>;
@group(0)@binding(5)var interpolation:sampler;
@vertex fn vertex(@builtin(vertex_index)id:u32)->@builtin(position)vec4<f32>{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[id],0.,1.);}
@fragment fn fragment(@builtin(position)position:vec4<f32>,@builtin(sample_index)sample:u32)->@location(0)vec4<f32>{
 let pixel=vec2<i32>(position.xy);let color=textureLoad(scene_color,pixel,i32(sample));let depth=textureLoad(scene_depth,pixel,i32(sample));if depth<=0.{return color;}
 let size=vec2<f32>(textureDimensions(scene_color));let uv=position.xy/size;let ndc=uv*2.-vec2(1.);let h=view.inverse*vec4(ndc*vec2(1.,-1.),depth,1.);let distance=length(h.xyz/h.w-view.eye_distance.xyz);let z=log(1.+min(distance,view.eye_distance.w))/log(1.+view.eye_distance.w);let coordinate=vec3(uv,(z*f32(view.grid.z-1u)+0.5)/f32(view.grid.z));
 let source=textureSampleLevel(scattering,interpolation,coordinate,0.).rgb;let t=textureSampleLevel(transmission,interpolation,coordinate,0.).rgb;return vec4(color.rgb*t+source,color.a);
}
