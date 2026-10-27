struct TemporalCamera{previous_vp:mat4x4<f32>,inverse_extent_jitter:vec4<f32>,flags:vec4<u32>};
@group(1)@binding(0)var<uniform>temporal_camera:TemporalCamera;
@group(1)@binding(1)var<storage,read>temporal_previous_objects:array<Object>;

@vertex fn temporal_vertex(@builtin(vertex_index)v:u32,@builtin(instance_index)i:u32)->VertexOut {
    var output=temporal_terrain_pose(v,i);
    let current=objects[output.object_slot];
    let previous=temporal_previous_objects[output.object_slot];
    output.temporal_previous_clip=vec4(0.);
    output.temporal_identity=vec4(0x01000000u,output.object_slot,0u,current.generation);
    output.temporal_current_clip=output.position;output.temporal_static=0u;
    if temporal_camera.flags.x!=0u&&previous.generation==current.generation&&(previous.enabled&1u)!=0u {
        output.temporal_previous_clip=temporal_camera.previous_vp*(previous.model*vec4(output.local_position,1.));
        if temporal_camera.flags.z!=0u&&all(previous.model[0]==current.model[0])&&all(previous.model[1]==current.model[1])&&all(previous.model[2]==current.model[2])&&all(previous.model[3]==current.model[3]){output.temporal_static=1u;}
    }
    return output;
}
struct TemporalOutput{@location(0)motion:vec4<f32>,@location(1)surface:vec4<u32>};
@fragment fn temporal_fragment(input:VertexOut)->TemporalOutput {
    let alpha=textureSample(colors,texels,input.uv,i32(materials[input.material].x)).a;
    if input.bucket==1u&&alpha<.001 {discard;}
    if edited_owner(input){discard;}
    var output:TemporalOutput;output.motion=vec4(0.);output.surface=vec4(0u);
    // Translucent coverage and water are reactive current-only samples. Opaque
    // objects behind a later physical-water/cloud composite need its separate
    // invalidation mask as well; this pass does not claim that source yet.
    if input.bucket==2u||(input.bucket==1u&&alpha<.999) {return output;}
    let cell=vec3<u32>(clamp(floor(input.local_position-input.local_normal*.0001),vec3(0.),vec3(15.)));
    var face=select(0u,1u,input.local_normal.y>0.);
    if abs(input.local_normal.z)>.5 {face=select(2u,3u,input.local_normal.z>0.);}
    if abs(input.local_normal.x)>.5 {face=select(4u,5u,input.local_normal.x>0.);}
    output.surface=input.temporal_identity;
    output.surface.x|=(cell.x+16u*(cell.z+16u*cell.y))*8u+face;
    if !(input.temporal_previous_clip.w>0.)||!(input.temporal_current_clip.w>0.) {return output;}
    let previous_ndc=input.temporal_previous_clip.xyz/input.temporal_previous_clip.w;
    let previous_uv=previous_ndc.xy*vec2(.5,-.5)+vec2(.5);
    if !all(abs(previous_ndc)<vec3(1e30))||!all(previous_uv>=vec2(0.))||!all(previous_uv<=vec2(1.))||!(previous_ndc.z>=0.&&previous_ndc.z<=1.){return output;}
    // Both clips use the same hardware barycentric weights. Subtracting the
    // builtin pixel centre instead would inject the raster subpixel-grid error.
    let current_ndc=input.temporal_current_clip.xy/input.temporal_current_clip.w;
    let current_uv=current_ndc*vec2(.5,-.5)+vec2(.5)-temporal_camera.inverse_extent_jitter.zw*temporal_camera.inverse_extent_jitter.xy;
    output.motion=vec4(select(previous_uv-current_uv,vec2(0.),input.temporal_static!=0u),previous_ndc.z,1.);
    return output;
}
