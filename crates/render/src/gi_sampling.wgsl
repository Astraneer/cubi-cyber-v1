struct GiParams {
    origin_spacing:vec4<f32>,probe_counts:vec4<u32>,world_origin:vec4<i32>,world_counts:vec4<u32>,trace:vec4<f32>,stamps:vec4<u32>,
};
struct GiProbe {
    position:vec4<f32>,sh_r:vec4<f32>,sh_g:vec4<f32>,sh_b:vec4<f32>,moments:vec4<f32>,visibility_a:vec4<f32>,visibility_b:vec4<f32>,stamps:vec4<u32>,
};
@group(0) @binding(13) var<uniform> gi_params:GiParams;
@group(0) @binding(14) var<storage,read> gi_probes:array<GiProbe>;
fn reflected_irradiance(world_position:vec3<f32>,world_normal:vec3<f32>)->vec3<f32> {
    let query=world_position+world_normal*0.08;
    let grid=(query-gi_params.origin_spacing.xyz)/gi_params.origin_spacing.w;
    let low=vec3<i32>(floor(grid));let fraction=fract(grid);
    let basis=vec4(0.2820947918,0.4886025119*world_normal.y,0.4886025119*world_normal.z,0.4886025119*world_normal.x);
    var irradiance=vec3(0.0);var weight_sum=0.0;
    for(var corner=0u;corner<8u;corner++) {
        let offset=vec3<i32>(i32(corner&1u),i32((corner>>1u)&1u),i32((corner>>2u)&1u));
        let cell=low+offset;if any(cell<vec3(0))||any(vec3<u32>(cell)>=gi_params.probe_counts.xyz){continue;}
        let p=vec3<u32>(cell);let index=p.x+gi_params.probe_counts.x*(p.z+gi_params.probe_counts.z*p.y);let probe=gi_probes[index];
        if probe.stamps.z==0u{continue;}
        let delta=query-probe.position.xyz;
        let bound=vec3(select(probe.visibility_a.x,probe.visibility_a.y,delta.x>=0.0),select(probe.visibility_a.z,probe.visibility_a.w,delta.y>=0.0),select(probe.visibility_b.x,probe.visibility_b.y,delta.z>=0.0));
        if any(abs(delta)>bound+vec3(0.04)){continue;}
        let blend=select(vec3(1.0)-fraction,fraction,offset!=vec3(0));let weight=blend.x*blend.y*blend.z;
        irradiance+=max(vec3(dot(probe.sh_r,basis),dot(probe.sh_g,basis),dot(probe.sh_b,basis)),vec3(0.0))*weight;weight_sum+=weight;
    }
    return irradiance/max(weight_sum,0.000001);
}
