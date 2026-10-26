// LabPBR 1.3 channel projection. Normal Y follows texture V (DirectX Y-down).
// Metal IDs remain explicit; this file does not invent a conductor RGB table.
struct LabPbrSample {
    normal_dx:vec3<f32>, ao:f32, height:f32, roughness:f32,
    dielectric_f0:f32, metal_id:u32, metal_reserved:u32,
    porosity:f32, subsurface:f32, emission:f32,
};
fn decode_labpbr(normal_map:vec4<f32>,specular_map:vec4<f32>)->LabPbrSample {
    let xy=normal_map.rg*2.0-vec2(1.0);
    let normal=normalize(vec3(xy,sqrt(max(1.0-dot(xy,xy),0.0))));
    let green=u32(round(specular_map.g*255.0));let blue=round(specular_map.b*255.0);let alpha=round(specular_map.a*255.0);
    var sample:LabPbrSample;sample.normal_dx=normal;sample.ao=normal_map.b;sample.height=normal_map.a;
    sample.roughness=(1.0-specular_map.r)*(1.0-specular_map.r);
    sample.dielectric_f0=select(specular_map.g,0.0,green>=230u);
    sample.metal_id=select(0u,green,green>=230u);sample.metal_reserved=select(0u,1u,green>=238u&&green<=254u);
    sample.porosity=select(0.0,blue/64.0,blue<=64.0);sample.subsurface=select(0.0,(blue-65.0)/190.0,blue>=65.0);
    sample.emission=select(alpha/254.0,0.0,alpha>=255.0);return sample;
}
fn labpbr_world_normal(sample:LabPbrSample,tangent_u:vec3<f32>,tangent_v_down:vec3<f32>,geometric_normal:vec3<f32>)->vec3<f32>{
    return normalize(tangent_u*sample.normal_dx.x+tangent_v_down*sample.normal_dx.y+geometric_normal*sample.normal_dx.z);
}
// A host enables POM only for C/D near materials. Hard bounds remain in shader:
// at most 16 samples, fading from 4 to 8 blocks, no grazing-angle runaway.
fn labpbr_parallax_uv(uv:vec2<f32>,view_dx:vec3<f32>,distance:f32,height_scale:f32,step_budget:u32,normal_map:texture_2d_array<f32>,texels:sampler,layer:i32)->vec2<f32>{
    if distance>=8.0||height_scale<=0.0||view_dx.z<=0.05||step_budget==0u{return uv;}
    let steps=min(step_budget,16u);let fade=clamp((8.0-distance)/4.0,0.0,1.0);
    let offset=view_dx.xy/max(view_dx.z,0.2)*min(height_scale,0.1)*fade/f32(steps);
    var result=uv;var depth=0.0;
    for(var i=0u;i<16u;i=i+1u){if i>=steps||depth>=1.0-textureSampleLevel(normal_map,texels,result,layer,0.0).a{break;}result-=offset;depth+=1.0/f32(steps);}
    return result;
}
