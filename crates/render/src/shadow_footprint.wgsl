struct Camera {matrix:mat4x4<f32>,viewport:vec4<f32>,params:vec4<u32>};
struct Object {model:mat4x4<f32>,minimum:vec4<f32>,maximum:vec4<f32>,ranges:array<vec2<u32>,3>,generation:u32,enabled:u32};
@group(0)@binding(0)var<uniform>camera:Camera;
@group(0)@binding(1)var<storage,read>objects:array<Object>;
@group(0)@binding(2)var<storage,read_write>footprint:ShadowFootprintAtomic;
@compute @workgroup_size(1) fn reset_bounds(){
    atomicStore(&footprint.new_static[0],0xffffffffu);atomicStore(&footprint.new_static[1],0xffffffffu);
    atomicStore(&footprint.new_static[2],0u);atomicStore(&footprint.new_static[3],0u);
}
@compute @workgroup_size(64) fn reduce_terrain(@builtin(global_invocation_id)id:vec3<u32>){
    if id.x>=camera.params.x{return;}let object=objects[id.x];if (object.enabled&1u)==0u{return;}
    if (object.enabled&8u)!=0u {
        let parent=object.ranges[1].x;if parent>=camera.params.x{return;}
        let base=objects[parent];if(base.enabled&1u)==0u||base.generation!=object.ranges[1].y{return;}
    }else if object.ranges[0].y==0u&&object.ranges[1].y==0u{return;}
    // Terrain's bounded owner fringe can extend one local voxel. The sum of
    // matrix column lengths bounds any transformed signed corner displacement.
    let fringe=length(object.model[0].xyz)+length(object.model[1].xyz)+length(object.model[2].xyz);
    let r=project_shadow_rect(camera.matrix,camera.viewport.x,object.minimum.xyz-vec3(fringe),object.maximum.xyz+vec3(fringe));
    if !shadow_rect_valid(r){return;}
    atomicMin(&footprint.new_static[0],r.x);atomicMin(&footprint.new_static[1],r.y);
    atomicMax(&footprint.new_static[2],r.z);atomicMax(&footprint.new_static[3],r.w);
}
@compute @workgroup_size(1) fn finish_terrain(){
    var r=vec4(atomicLoad(&footprint.new_static[0]),atomicLoad(&footprint.new_static[1]),atomicLoad(&footprint.new_static[2]),atomicLoad(&footprint.new_static[3]));
    if !shadow_rect_valid(r){r=vec4(0u);}
    footprint.static_draw=shadow_rect_union(footprint.static_previous,r);footprint.static_previous=r;
    let low=select(vec2(0xffffffffu),r.xy,shadow_rect_valid(r));
    atomicStore(&footprint.new_combined[0],low.x);atomicStore(&footprint.new_combined[1],low.y);
    atomicStore(&footprint.new_combined[2],r.z);atomicStore(&footprint.new_combined[3],r.w);
    footprint.metrics.x=shadow_rect_pixels(footprint.static_draw);footprint.metrics.z=shadow_rect_pixels(r);
}
@compute @workgroup_size(1) fn finish_combined(){
    var r=vec4(atomicLoad(&footprint.new_combined[0]),atomicLoad(&footprint.new_combined[1]),atomicLoad(&footprint.new_combined[2]),atomicLoad(&footprint.new_combined[3]));
    if !shadow_rect_valid(r){r=vec4(0u);}
    footprint.combined_draw=shadow_rect_union(footprint.combined_previous,r);footprint.combined_previous=r;
    footprint.metrics.y=shadow_rect_pixels(footprint.combined_draw);footprint.metrics.w=shadow_rect_pixels(r);
}
@compute @workgroup_size(1) fn finish_combined_full(){
    let r=vec4(0u,0u,u32(camera.viewport.x),u32(camera.viewport.y));footprint.combined_draw=r;footprint.combined_previous=r;
    footprint.metrics.y=shadow_rect_pixels(r);footprint.metrics.w=shadow_rect_pixels(r);
}
