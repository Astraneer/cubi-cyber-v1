// Original bounded single-scattering froxel integration. Optical coefficients
// and the solar transmittance LUT are the same resources as AtmosphereGpu.
struct Optics{radii:vec4<f32>,rayleigh:vec4<f32>,mie:vec4<f32>,ozone:vec4<f32>,sun_eye:vec4<f32>,integration:vec4<u32>};
struct View{inverse:mat4x4<f32>,eye_distance:vec4<f32>,grid:vec4<u32>,work:vec4<u32>};
@group(0)@binding(0)var<uniform>view:View;
@group(0)@binding(1)var<uniform>optics:Optics;
@group(0)@binding(2)var solar_transmittance:texture_2d<f32>;
@group(0)@binding(3)var scattering_out:texture_storage_3d<rgba16float,write>;
@group(0)@binding(4)var transmission_out:texture_storage_3d<rgba16float,write>;
// SHADOWS
fn sample_multiple_scattering(point:vec3<f32>,sun:vec3<f32>,ground_radius:f32,top_radius:f32)->vec3<f32>{return vec3(0.0);}
fn extinction(height:f32)->vec3<f32>{let h=max(height,0.);return optics.rayleigh.rgb*exp(-h/optics.radii.z)+vec3(optics.mie.x+optics.mie.y)*exp(-h/optics.radii.w)+optics.ozone.rgb*max(0.,1.-abs(h-optics.ozone.w)/15.);}
fn sunlight(point:vec3<f32>)->vec3<f32>{
 let radius=length(point);let mu=dot(point/radius,normalize(optics.sun_eye.xyz));let b=radius*mu;let ground=b*b-(radius-optics.radii.x)*(radius+optics.radii.x);if b<0.&&ground>=0.{return vec3(0.);}
 let size=vec2<i32>(textureDimensions(solar_transmittance));let uv=vec2(mu*0.5+0.5,clamp((radius-optics.radii.x)/(optics.radii.y-optics.radii.x),0.,1.));let p=uv*vec2<f32>(size)-vec2(0.5);let low=vec2<i32>(floor(p));let w=fract(p);
 let a=textureLoad(solar_transmittance,clamp(low,vec2(0),size-vec2(1)),0).rgb;let b0=textureLoad(solar_transmittance,clamp(low+vec2(1,0),vec2(0),size-vec2(1)),0).rgb;let c=textureLoad(solar_transmittance,clamp(low+vec2(0,1),vec2(0),size-vec2(1)),0).rgb;let d=textureLoad(solar_transmittance,clamp(low+vec2(1,1),vec2(0),size-vec2(1)),0).rgb;return mix(mix(a,b0,w.x),mix(c,d,w.x),w.y);
}
fn ray(ndc:vec2<f32>)->vec3<f32>{let near=view.inverse*vec4(ndc,1.,1.);let far=view.inverse*vec4(ndc,0.,1.);return normalize(far.xyz/far.w-near.xyz/near.w);}
@compute @workgroup_size(4,4,4)fn build_aerial(@builtin(global_invocation_id)id:vec3<u32>){
 if any(id>=view.grid.xyz){return;}let ndc=(vec2<f32>(id.xy)+vec2(0.5))/vec2<f32>(view.grid.xy)*2.-vec2(1.);let direction=ray(ndc*vec2(1.,-1.));let distance=(exp(log(1.+view.eye_distance.w)*f32(id.z)/f32(view.grid.z-1u))-1.)*0.001;let step=distance/f32(view.work.x);let start=vec3(0.,optics.radii.x+optics.sun_eye.w,0.);let sun=normalize(optics.sun_eye.xyz);let cosine=dot(direction,sun);let phase_r=3.*(1.+cosine*cosine)/(16.*3.141592654);let g=optics.mie.z;let phase_m=(1.-g*g)/(4.*3.141592654*pow(max(1.+g*g-2.*g*cosine,0.0001),1.5));var depth=vec3(0.);var radiance=vec3(0.);
 for(var sample=0u;sample<view.work.x;sample++){
  let point=start+direction*((f32(sample)+0.5)*step);let h=max(length(point)-optics.radii.x,0.);let sigma=extinction(h);let source=(optics.rayleigh.rgb*exp(-h/optics.radii.z)*phase_r+vec3(optics.mie.x*exp(-h/optics.radii.w)*phase_m))*sunlight(point)*optics.mie.w;
  let world=view.eye_distance.xyz+direction*((f32(sample)+0.5)*step*1000.);
  let sigma_scattering=optics.rayleigh.rgb*exp(-h/optics.radii.z)+vec3(optics.mie.x*exp(-h/optics.radii.w));let multiple=sigma_scattering*sample_multiple_scattering(point,sun,optics.radii.x,optics.radii.y)*optics.mie.w;
  radiance+=exp(-depth-sigma*step*0.5)*(source*cascade_shadow(world,sun)+multiple)*step;depth+=sigma*step;
 }
 textureStore(scattering_out,vec3<i32>(id),vec4(radiance,0.));textureStore(transmission_out,vec3<i32>(id),vec4(exp(-depth),1.));
}
