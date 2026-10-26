// The same physical atmosphere as the background, cloud and water environment.
struct AtmosphereLighting {r:vec4<f32>,g:vec4<f32>,b:vec4<f32>,sun:vec4<f32>,direction:vec4<f32>};
@group(0) @binding(20) var<storage,read> atmosphere_light:AtmosphereLighting;
fn atmosphere_diffuse(normal:vec3<f32>)->vec3<f32> {
    let n=normalize(normal);let basis=vec4(0.2820947918,0.4886025119*n.y,0.4886025119*n.z,0.4886025119*n.x);
    return max(vec3(dot(atmosphere_light.r,basis),dot(atmosphere_light.g,basis),dot(atmosphere_light.b,basis)),vec3(0.0));
}
