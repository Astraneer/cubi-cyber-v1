// Each cascade owns a256-byte record; atomic and read layouts have identical
// field offsets. Only the first112bytes contain state, the tail is alignment.
struct ShadowFootprintAtomic {
    new_static:array<atomic<u32>,4>,static_previous:vec4<u32>,static_draw:vec4<u32>,
    new_combined:array<atomic<u32>,4>,combined_previous:vec4<u32>,combined_draw:vec4<u32>,metrics:vec4<u32>,
};
struct ShadowFootprintRead {
    new_static:vec4<u32>,static_previous:vec4<u32>,static_draw:vec4<u32>,
    new_combined:vec4<u32>,combined_previous:vec4<u32>,combined_draw:vec4<u32>,metrics:vec4<u32>,
};
fn shadow_rect_valid(r:vec4<u32>)->bool{return r.x<r.z&&r.y<r.w;}
fn shadow_rect_union(a:vec4<u32>,b:vec4<u32>)->vec4<u32>{
    if !shadow_rect_valid(a){return b;}if !shadow_rect_valid(b){return a;}
    return vec4(min(a.xy,b.xy),max(a.zw,b.zw));
}
fn shadow_rect_pixels(r:vec4<u32>)->u32{
    if !shadow_rect_valid(r){return 0u;}return (r.z-r.x)*(r.w-r.y);
}
// Orthographic shadow projection: extrema of an AABB are attained at corners.
// Two screen texels conservatively cover raster/PCF support beyond its hull.
fn project_shadow_rect(matrix:mat4x4<f32>,resolution:f32,minimum:vec3<f32>,maximum:vec3<f32>)->vec4<u32>{
    var low=vec2(1e30);var high=vec2(-1e30);var zmin=1e30;var zmax=-1e30;
    for(var i=0u;i<8u;i++){
        let p=matrix*vec4(select(minimum,maximum,vec3((i&1u)!=0u,(i&2u)!=0u,(i&4u)!=0u)),1.);
        let uv=vec2(p.x*.5+.5,.5-p.y*.5);low=min(low,uv);high=max(high,uv);zmin=min(zmin,p.z);zmax=max(zmax,p.z);
    }
    if zmax<0.||zmin>1.{return vec4(0u);}
    let lo=vec2<u32>(clamp(floor(low*resolution)-vec2(2.),vec2(0.),vec2(resolution)));
    let hi=vec2<u32>(clamp(ceil(high*resolution)+vec2(2.),vec2(0.),vec2(resolution)));
    let r=vec4(lo,hi);return select(vec4(0u),r,shadow_rect_valid(r));
}
