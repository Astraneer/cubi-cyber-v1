// Original implementation of Hillaire2020 equations5-10 (paper only).
// Isotropic order>=2, normalized incident illuminance EI=1; no source code used.
struct Optics{radii:vec4<f32>,rayleigh:vec4<f32>,mie:vec4<f32>,ozone:vec4<f32>,sun_eye:vec4<f32>,integration:vec4<u32>};
struct Settings{image_work:vec4<u32>,ground:vec4<f32>};
@group(0)@binding(0)var<uniform>optics:Optics;
@group(0)@binding(1)var transmittance:texture_2d<f32>;
@group(0)@binding(2)var psi_out:texture_storage_2d<rgba16float,write>;
@group(0)@binding(3)var feedback_out:texture_storage_2d<rgba16float,write>;
@group(0)@binding(4)var<uniform>settings:Settings;
@group(0)@binding(5)var<storage,read>directions:array<vec4<f32>,64>;
fn boundary(radius:f32,mu:f32)->vec2<f32>{let b=radius*mu;let ground=(radius-optics.radii.x)*(radius+optics.radii.x);let d=b*b-ground;if b<0.&&d>=0.{return vec2(max(-b-sqrt(d),0.),1.);}let top=(radius-optics.radii.y)*(radius+optics.radii.y);return vec2(max(-b+sqrt(max(b*b-top,0.)),0.),0.);}
fn sunlight(point:vec3<f32>,sun:vec3<f32>)->vec3<f32>{let radius=length(point);let mu=dot(point/max(radius,1e-12),sun);if boundary(radius,mu).y>0.5{return vec3(0.);}let size=vec2<i32>(textureDimensions(transmittance));let uv=vec2(mu*0.5+0.5,clamp((radius-optics.radii.x)/(optics.radii.y-optics.radii.x),0.,1.));let pixel=uv*vec2<f32>(size)-vec2(0.5);let low=vec2<i32>(floor(pixel));let w=fract(pixel);let a=textureLoad(transmittance,clamp(low,vec2(0),size-vec2(1)),0).rgb;let b=textureLoad(transmittance,clamp(low+vec2(1,0),vec2(0),size-vec2(1)),0).rgb;let c=textureLoad(transmittance,clamp(low+vec2(0,1),vec2(0),size-vec2(1)),0).rgb;let d=textureLoad(transmittance,clamp(low+vec2(1,1),vec2(0),size-vec2(1)),0).rgb;return mix(mix(a,b,w.x),mix(c,d,w.x),w.y);}
// Analytic attenuation over a homogeneous segment, with a cancellation-safe
// limit at zero extinction. This keeps feedback f_ms within physical bounds.
fn segment_integral(sigma:vec3<f32>,step:f32)->vec3<f32>{let x=sigma*step;let small=vec3(step)*(vec3(1.)-x*0.5+x*x/6.-x*x*x/24.);let exact=(vec3(1.)-exp(-x))/max(sigma,vec3(1e-20));return select(exact,small,x<vec3(0.001));}
@compute @workgroup_size(4,4)fn build_multiple_scattering(@builtin(global_invocation_id)id:vec3<u32>){
 let size=textureDimensions(psi_out);if any(id.xy>=size){return;}let uv=vec2<f32>(id.xy)/vec2<f32>(size-vec2(1u));let radius=mix(optics.radii.x,optics.radii.y,uv.y);let origin=vec3(0.,radius,0.);let mu=uv.x*2.-1.;let sun=vec3(sqrt(max(0.,1.-mu*mu)),mu,0.);var second=vec3(0.);var feedback=vec3(0.);let pu=1./12.5663706144;
 for(var ray_index=0u;ray_index<settings.image_work.z;ray_index++){
  let direction=directions[ray_index*64u/settings.image_work.z].xyz;let ray=boundary(radius,direction.y);let step=ray.x/f32(settings.image_work.w);var t=vec3(1.);var light=vec3(0.);var transfer=vec3(0.);
  for(var sample=0u;sample<settings.image_work.w;sample++){
   let point=origin+direction*((f32(sample)+0.5)*step);let h=max(length(point)-optics.radii.x,0.);let r=exp(-h/optics.radii.z);let m=exp(-h/optics.radii.w);let scattering=optics.rayleigh.rgb*r+vec3(optics.mie.x*m);let extinction=scattering+vec3(optics.mie.y*m)+optics.ozone.rgb*max(0.,1.-abs(h-optics.ozone.w)/15.);let integral=segment_integral(extinction,step);let segment=t*scattering*integral;light+=segment*sunlight(point,sun)*pu;transfer+=segment;t*=exp(-extinction*step);
  }
  if ray.y>0.5{let point=origin+direction*ray.x;let normal=point/max(length(point),1e-12);light+=t*settings.ground.rgb*sunlight(point,sun)*(max(dot(normal,sun),0.)/3.141592654);}
  second+=light;feedback+=transfer;
 }
 second/=f32(settings.image_work.z);feedback=clamp(feedback/f32(settings.image_work.z),vec3(0.),vec3(0.9999));let psi=second/(vec3(1.)-feedback);textureStore(psi_out,vec2<i32>(id.xy),vec4(psi,1.));textureStore(feedback_out,vec2<i32>(id.xy),vec4(feedback,1.));
}
