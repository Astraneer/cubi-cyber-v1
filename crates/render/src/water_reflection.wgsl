// One screen-space trace per covered water sample, independent of quad overdraw.
struct Params{view_projection:mat4x4<f32>,inverse_view_projection:mat4x4<f32>,eye_time:vec4<f32>,absorption:vec4<f32>,scattering:vec4<f32>,optics:vec4<f32>,sun:vec4<f32>,image:vec4<u32>,quality:vec4<u32>,waves:vec4<f32>};
struct ReflectionConfig{steps:u32,distance:f32,tolerance:f32,padding:u32};
@group(0)@binding(0)var<uniform>params:Params;
@group(0)@binding(1)var<uniform>config:ReflectionConfig;
@group(0)@binding(2)var water_color:texture_multisampled_2d<f32>;
@group(0)@binding(3)var water_depth:texture_depth_multisampled_2d;
@group(0)@binding(4)var normal_foam:texture_multisampled_2d<f32>;
@group(0)@binding(5)var opaque_color:texture_2d<f32>;
@group(0)@binding(6)var opaque_depth:texture_depth_multisampled_2d;
@group(0)@binding(7)var sky_view:texture_2d<f32>;
// STATS
fn lookup(tex:texture_2d<f32>,uv:vec2<f32>)->vec3<f32>{
 let dimensions=vec2<i32>(textureDimensions(tex));let p=uv*vec2<f32>(dimensions)-vec2(0.5);
 let low=vec2<i32>(floor(p));let fraction=fract(p);var color=vec3(0.);
 for(var corner=0u;corner<4u;corner++){
  let d=vec2<i32>(i32(corner&1u),i32(corner>>1u));let weight=select(vec2(1.)-fraction,fraction,d!=vec2(0));
  color+=textureLoad(tex,clamp(low+d,vec2(0),dimensions-vec2(1)),0).rgb*weight.x*weight.y;
 }return color;
}
fn environment(direction:vec3<f32>)->vec3<f32>{return lookup(sky_view,vec2(atan2(direction.z,direction.x)/6.28318530718+0.5,direction.y*0.5+0.5));}
fn unproject(uv:vec2<f32>,depth:f32)->vec3<f32>{let p=params.inverse_view_projection*vec4(uv.x*2.-1.,1.-uv.y*2.,depth,1.);return p.xyz/p.w;}
fn project(point:vec3<f32>)->vec3<f32>{let clip=params.view_projection*vec4(point,1.);let ndc=clip.xyz/clip.w;return vec3(ndc.x*0.5+0.5,0.5-ndc.y*0.5,ndc.z);}
fn fresnel(cosine:f32,underwater:bool)->f32{
 let ni=select(1.,params.optics.x,underwater);let nt=select(params.optics.x,1.,underwater);
 let sine2=(ni/nt)*(ni/nt)*max(0.,1.-cosine*cosine);if sine2>=1.{return 1.;}
 let transmitted=sqrt(1.-sine2);let rs=(ni*cosine-nt*transmitted)/max(ni*cosine+nt*transmitted,0.000001);
 let rp=(ni*transmitted-nt*cosine)/max(ni*transmitted+nt*cosine,0.000001);return (rs*rs+rp*rp)*0.5;
}
@vertex fn vertex(@builtin(vertex_index)index:u32)->@builtin(position)vec4<f32>{
 let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[index],0.,1.);
}
@fragment fn fragment(@builtin(position)position:vec4<f32>,@builtin(sample_index)sample:u32)->@location(0)vec4<f32>{
 let pixel=clamp(vec2<i32>(position.xy),vec2(0),vec2<i32>(params.image.xy)-vec2(1));
 let original=textureLoad(water_color,pixel,i32(sample));let stored=textureLoad(normal_foam,pixel,i32(sample));
 if dot(stored.xyz,stored.xyz)<0.5{return original;}
 let uv=position.xy/vec2<f32>(params.image.xy);let depth=textureLoad(water_depth,pixel,i32(sample));
 if depth<=0.{return original;}
 let world=unproject(uv,depth);let view=normalize(params.eye_time.xyz-world);var normal=normalize(stored.xyz);
 if dot(normal,view)<0.{normal=-normal;}
 let ray=reflect(-view,normal);var hit=false;var hit_uv=uv;var steps=0u;
 for(var index=0u;index<config.steps;index++){
  steps++;let along=(f32(index)+1.)/f32(config.steps);let point=world+ray*(0.1+along*config.distance);
  let projected=project(point);
  if any(projected.xy<vec2(0.))||any(projected.xy>vec2(1.))||projected.z<0.||projected.z>1.{break;}
  let p=clamp(vec2<i32>(projected.xy*vec2<f32>(params.image.xy)),vec2(0),vec2<i32>(params.image.xy)-vec2(1));
  let opaque=textureLoad(opaque_depth,p,i32(sample));if opaque<=0.{continue;}
  let actual=unproject(projected.xy,opaque);let delta=distance(point,params.eye_time.xyz)-distance(actual,params.eye_time.xyz);
  if delta>=0.&&delta<config.tolerance{hit=true;hit_uv=projected.xy;break;}
 }
 record(steps,hit);
 // A miss preserves all previously shaded samples exactly, including Unknown.
 if !hit{return original;}
 let underwater=params.quality.y!=0u;let ambient=environment(vec3(0.,1.,0.));
 let previous=select(environment(ray),params.scattering.rgb*ambient,underwater);
 let reflected=lookup(opaque_color,hit_uv);let cosine=clamp(dot(normal,view),0.0001,1.);
 var weight=vec3(fresnel(cosine,underwater)*(1.-clamp(stored.w,0.,1.)));
 if underwater{weight*=exp(-(params.absorption.rgb+vec3(params.absorption.a))*min(distance(world,params.eye_time.xyz),params.optics.z));}
 return vec4(max(original.rgb+weight*(reflected-previous),vec3(0.)),original.a);
}
