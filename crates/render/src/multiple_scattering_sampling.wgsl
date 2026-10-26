@group(0) @binding(5) var multiple_scattering_lut:texture_2d<f32>;
fn sample_multiple_scattering(point:vec3<f32>,sun:vec3<f32>,ground_radius:f32,top_radius:f32)->vec3<f32>{
 let radius=length(point);let mu=dot(point/max(radius,1e-12),sun);let uv=vec2(mu*0.5+0.5,clamp((radius-ground_radius)/(top_radius-ground_radius),0.,1.));let size=vec2<i32>(textureDimensions(multiple_scattering_lut));let p=uv*vec2<f32>(size-vec2(1));let low=vec2<i32>(floor(p));let w=fract(p);
 let a=textureLoad(multiple_scattering_lut,clamp(low,vec2(0),size-vec2(1)),0).rgb;let b=textureLoad(multiple_scattering_lut,clamp(low+vec2(1,0),vec2(0),size-vec2(1)),0).rgb;let c=textureLoad(multiple_scattering_lut,clamp(low+vec2(0,1),vec2(0),size-vec2(1)),0).rgb;let d=textureLoad(multiple_scattering_lut,clamp(low+vec2(1,1),vec2(0),size-vec2(1)),0).rgb;return mix(mix(a,b,w.x),mix(c,d,w.x),w.y);
}
