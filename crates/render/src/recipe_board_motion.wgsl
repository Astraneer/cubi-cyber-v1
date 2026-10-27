struct PreviousCamera {vp:mat4x4<f32>,inverse_extent_jitter:vec4<f32>,flags:vec4<u32>};
struct SurfacePair {current:vec4<u32>,previous:vec4<u32>};
@group(1)@binding(0)var<uniform>previous_camera:PreviousCamera;
@group(1)@binding(1)var<storage,read>previous_boards:array<Board>;
@group(1)@binding(2)var<storage,read>surface_pairs:array<SurfacePair>;
struct MotionVertex {
 @builtin(position)@invariant position:vec4<f32>,
 @location(0)current_clip:vec4<f32>,
 @location(1)previous_clip:vec4<f32>,
 @location(2)@interpolate(flat)current_surface:vec4<u32>,
 @location(3)@interpolate(flat)previous_surface:vec4<u32>,
};
@vertex fn motion_vertex(@builtin(vertex_index)vertex:u32,@builtin(instance_index)instance:u32)->MotionVertex{
 let current=board_pose(vertex,boards[instance]);
 let previous=board_pose(vertex,previous_boards[instance]);
 let pair=surface_pairs[instance];let part=instance*2u+vertex/3u;
 var out:MotionVertex;out.position=current.position;out.current_clip=current.position;
 out.previous_clip=previous_camera.vp*vec4(previous.world,1.);
 out.current_surface=vec4(0x13000000u|part,pair.current.yzw);
 if pair.current.w==0u{out.current_surface=vec4(0u);}
 out.previous_surface=pair.previous;return out;
}
struct MotionOutput{@location(0)motion:vec4<f32>,@location(1)surface:vec4<u32>};
@fragment fn motion_fragment(input:MotionVertex)->MotionOutput{
 var out:MotionOutput;out.motion=vec4(0.);out.surface=input.current_surface;
 let current_key=input.current_surface;let old_key=input.previous_surface;
 if previous_camera.flags.x==0u||current_key.w==0u||any(current_key.yzw!=old_key.yzw)||!(input.previous_clip.w>0.)||!(input.current_clip.w>0.){return out;}
 let previous=input.previous_clip.xyz/input.previous_clip.w;
 let uv=previous.xy*vec2(.5,-.5)+vec2(.5);
 let current_uv=input.current_clip.xy/input.current_clip.w*vec2(.5,-.5)+vec2(.5)-previous_camera.inverse_extent_jitter.zw*previous_camera.inverse_extent_jitter.xy;
 if !all(abs(previous)<vec3(1e30))||!all(uv>=vec2(0.))||!all(uv<=vec2(1.))||!(previous.z>=0.&&previous.z<=1.){return out;}
 out.motion=vec4(uv-current_uv,previous.z,1.);return out;
}
