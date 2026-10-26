fn surface_radiance(index:u32,packed:u32,normal:vec3<f32>,position:vec3<f32>)->vec3<f32> {
    return vec3<f32>(f32(packed&1023u),f32((packed>>10u)&1023u),f32((packed>>20u)&1023u))*(32.0/1023.0);
}
