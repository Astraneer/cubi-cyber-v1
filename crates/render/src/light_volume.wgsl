// This buffer is independent of quads. A lamp change uploads only its dirty
// light samples. Zero represents dark/Unknown; only CPU SkyAccess certifies sky.
@group(0) @binding(12) var<storage,read> page_lights:array<u32>;
fn sample_page_light(slot:u32,local_position:vec3<f32>,local_normal:vec3<f32>)->vec4<f32> {
    let outside=vec3<i32>(floor(local_position+local_normal*0.002));
    let p=vec3<u32>(clamp(outside,vec3(-1),vec3(16))+vec3(1));
    let header=page_lights[slot*5833u];
    let light_slot=select(slot,header&0x7fffffffu,(header&0x80000000u)!=0u);
    let resolved_header=page_lights[light_slot*5833u];
    let sampled=page_lights[light_slot*5833u+1u+p.x+18u*(p.z+18u*p.y)];
    let source_class=select(select(vec3(1u),vec3(2u),p==vec3(17u)),vec3(0u),p==vec3(0u));
    let source=source_class.x+3u*source_class.z+9u*source_class.y;
    let uniform=(resolved_header&0x40000000u)!=0u;
    let revoked=(resolved_header&(1u<<source))!=0u;
    let packed=select(select(sampled,page_lights[light_slot*5833u+1u],uniform),0u,revoked);
    // Minecraft level attenuation. Block is gameplay luminance; RGB already
    // contains visible emission, so block is deliberately not added a second time.
    let rgb=vec3<f32>(f32((packed>>8u)&255u),f32((packed>>16u)&255u),f32(packed>>24u));
    let sky=f32(packed&15u);
    let mc=select(pow(vec3(0.8),vec3(15.0)-min(rgb,vec3(15.))),vec3(0.0),rgb==vec3(0.0));
    let visible_rgb=select(mc,rgb/15.,rgb>vec3(15.));
    let visible_sky=select(pow(0.8,15.0-sky),0.0,sky==0.0);
    return vec4(visible_rgb,visible_sky);
}
