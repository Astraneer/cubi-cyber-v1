struct ShadowSampling {
    matrices:array<mat4x4<f32>,3>,
    sun:vec4<f32>,
    params:vec4<u32>,
};
@group(0) @binding(9) var<uniform> shadow_sampling:ShadowSampling;
@group(0) @binding(10) var shadow_depth:texture_depth_2d_array;
@group(0) @binding(11) var shadow_compare:sampler_comparison;
fn cascade_shadow(world_position:vec3<f32>,world_normal:vec3<f32>)->f32 {
    let direction=normalize(shadow_sampling.sun.xyz);
    let bias=shadow_sampling.sun.w*(1.0+2.0*(1.0-max(dot(world_normal,direction),0.0)));
    for(var cascade=0u;cascade<3u;cascade++) {
        if (shadow_sampling.params.x&(1u<<cascade))==0u {continue;}
        let p=shadow_sampling.matrices[cascade]*vec4(world_position,1.0);
        if abs(p.x)>0.96||abs(p.y)>0.96||p.z<0.0||p.z>1.0 {continue;}
        let uv=vec2(p.x*0.5+0.5,0.5-p.y*0.5);
        let texel=1.0/f32(shadow_sampling.params.y);
        var visibility=0.0;
        for(var y=0;y<2;y++){for(var x=0;x<2;x++){
            let offset=(vec2<f32>(f32(x),f32(y))-vec2(0.5))*texel;
            visibility+=textureSampleCompareLevel(shadow_depth,shadow_compare,uv+offset,i32(cascade),p.z+bias);
        }}
        return visibility*0.25;
    }
    return 1.0;
}
