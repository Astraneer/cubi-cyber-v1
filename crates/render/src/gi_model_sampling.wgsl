@group(0) @binding(15) var<storage,read> gi_receiver_world:array<u32>;
fn model_receiver_known(world_position:vec3<f32>,world_normal:vec3<f32>)->bool {
    let cell=vec3<i32>(floor(world_position+world_normal*0.08));
    let page=(cell>>vec3(4u))-gi_params.world_origin.xyz;
    if any(page<vec3(0))||any(vec3<u32>(page)>=gi_params.world_counts.xyz){return false;}
    let counts=gi_params.world_counts.xyz;let ring_index=gi_params.world_counts.w;
    let ring=vec3(ring_index%counts.x,ring_index/(counts.x*counts.z),ring_index/counts.x%counts.z);
    let p=(vec3<u32>(page)+ring)%counts;let local=vec3<u32>(cell)&vec3(15u);
    let page_index=p.x+counts.x*(p.z+counts.z*p.y);
    let index=page_index*4096u+local.x+16u*(local.z+16u*local.y);
    return (gi_receiver_world[index]&0x80000000u)!=0u;
}
fn model_reflected_irradiance(world_position:vec3<f32>,world_normal:vec3<f32>)->vec3<f32>{
    if !model_receiver_known(world_position,world_normal){return vec3(0.);}
    return reflected_irradiance(world_position,world_normal);
}
