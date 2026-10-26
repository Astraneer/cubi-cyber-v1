struct Surface {faces:array<vec2<u32>,6>};
@group(0) @binding(6) var<storage,read> surfaces:array<Surface>;
fn light_factor(value:u32)->f32 {
    if value==0u{return 0.0;}
    if value>15u{return f32(value)/15.0;}
    return pow(0.8,15.0-f32(value));
}
fn surface_radiance(index:u32,packed:u32,normal:vec3<f32>,position:vec3<f32>)->vec3<f32> {
    var face=select(4u,5u,normal.x>0.0);
    if abs(normal.y)>0.5 {face=select(0u,1u,normal.y>0.0);}
    if abs(normal.z)>0.5 {face=select(2u,3u,normal.z>0.0);}
    let data=surfaces[index].faces[face];
    if (data.x&0x10000000u)==0u{return vec3(0.0);}
    let albedo=vec3<f32>(f32(data.x&255u),f32((data.x>>8u)&255u),f32((data.x>>16u)&255u))/255.0;
    let rgb=vec3(light_factor(data.y&255u),light_factor((data.y>>8u)&255u),light_factor((data.y>>16u)&255u));
    let sky=light_factor((data.x>>24u)&15u);
    let sun=normalize(atmosphere_light.direction.xyz);
    let visibility=cascade_shadow(position+normal*0.002,normal);
    // Atmosphere SH already stores E_sky/PI, RGB is the M4 diffuse term.
    // Escaped rays and direct emissive surfaces are never accumulated here.
    return albedo*(rgb+sky*(atmosphere_diffuse(normal)+atmosphere_light.sun.rgb*(max(dot(normal,sun),0.0)*visibility/3.1415926536)));
}
