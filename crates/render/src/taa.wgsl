// Original bounded HDR temporal resolve. History stores only presented frames;
// motion and exact surface identity are supplied by the actual geometry pass.
struct Parameters{size:vec4<u32>,jitter_blend:vec4<f32>,depth_limits:vec4<f32>,flags:vec4<u32>};
@group(0)@binding(0)var<uniform>parameters:Parameters;
@group(0)@binding(1)var current_hdr:texture_2d<f32>;
@group(0)@binding(2)var current_depth:texture_depth_2d;
@group(0)@binding(3)var motion:texture_2d<f32>;
@group(0)@binding(4)var surface:texture_2d<u32>;
@group(0)@binding(5)var history_hdr:texture_2d<f32>;
@group(0)@binding(6)var history_depth:texture_2d<f32>;
@group(0)@binding(7)var history_surface:texture_2d<u32>;
// STATISTICS
fn point(p:vec2<i32>)->vec2<i32>{return clamp(p,vec2(0),vec2<i32>(parameters.size.xy)-vec2(1));}
fn clean(value:vec4<f32>)->vec4<f32>{return select(vec4(0.),clamp(value,vec4(-65504.),vec4(65504.)),value==value);}
fn current(p:vec2<i32>)->vec4<f32>{record_loads(1u);return clean(textureLoad(current_hdr,point(p),0));}
fn spatial(pixel:vec2<f32>)->vec4<f32>{
 let at=pixel+parameters.jitter_blend.xy-vec2(.5);let base=vec2<i32>(floor(at));let f=fract(at);
 return mix(mix(current(base),current(base+vec2(1,0)),f.x),mix(current(base+vec2(0,1)),current(base+vec2(1,1)),f.x),f.y);
}
@vertex fn vertex(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[i],0.,1.);}
struct Output{@location(0)color:vec4<f32>,@location(1)depth:f32,@location(2)surface:vec4<u32>};
@fragment fn fragment(@builtin(position)pixel:vec4<f32>)->Output{
 record_fragment();let p=point(vec2<i32>(floor(pixel.xy+parameters.jitter_blend.xy)));
 record_loads(3u);let movement=textureLoad(motion,p,0);let identity=textureLoad(surface,p,0);let depth=textureLoad(current_depth,p,0);
 let value=spatial(pixel.xy);var resolved=value;
 // Depth0 is legal at the reverse-Z far plane. Identity, rather than depth>0,
 // distinguishes a known surface from sky/Unknown and unsupported effects.
 let uv=pixel.xy/vec2<f32>(parameters.size.xy)+movement.xy;
 let finite=all(movement==movement)&&all(abs(movement)<vec4(65504.));
 let domain=all(uv>=vec2(0.))&&all(uv<=vec2(1.));
 let bounded=all(abs(movement.xy)*vec2<f32>(parameters.size.xy)<=vec2(parameters.jitter_blend.w));
 if parameters.size.w!=0u&&movement.w>.5&&identity.x!=0u&&finite&&domain&&bounded&&movement.z>=0.&&movement.z<=1.{
  let at=uv*vec2<f32>(parameters.size.xy)-vec2(.5);let base=vec2<i32>(floor(at));let f=fract(at);
  var total=0.;var previous=vec3(0.);
  let tolerance=parameters.depth_limits.x+parameters.depth_limits.y*abs(movement.z)+0.000000059604645;
  for(var y=0;y<2;y++){for(var x=0;x<2;x++){
   let q=base+vec2(x,y);let weight=select(1.-f.x,f.x,x==1)*select(1.-f.y,f.y,y==1);
   if any(q<vec2(0))||any(q>=vec2<i32>(parameters.size.xy)){continue;}
   record_loads(1u);let old_identity=textureLoad(history_surface,q,0);
   if !all(old_identity==identity){record_identity_reject();continue;}
   record_loads(1u);let old_depth=textureLoad(history_depth,q,0).r;
   if abs(old_depth-movement.z)>tolerance||old_depth!=old_depth{record_depth_reject();continue;}
   record_loads(1u);previous+=clean(textureLoad(history_hdr,q,0)).rgb*weight;total+=weight;
  }}
  if total>=.5{
   var low=value.rgb;var high=value.rgb;
   for(var y=-1;y<=1;y++){for(var x=-1;x<=1;x++){let sample=current(p+vec2(x,y)).rgb;low=min(low,sample);high=max(high,sample);}}
   previous=clamp(previous/total,low,high);
   resolved=vec4(mix(value.rgb,previous,parameters.jitter_blend.z*total),value.a);record_history_accept();
  }
 }
 return Output(resolved,depth,identity);
}
