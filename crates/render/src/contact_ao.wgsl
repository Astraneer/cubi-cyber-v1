// Optional same-frame screen-space ambient visibility. The ambient input is an
// explicitly authored removable color delta, never the complete HDR color.
// Signed deltas preserve max(emission) and public-port absorption exactly.
struct Params {view_projection:mat4x4<f32>,inverse_view_projection:mat4x4<f32>,image:vec4<u32>,optics:vec4<f32>,kernel:array<vec4<f32>,32>};
@group(0)@binding(0)var<uniform>params:Params;
@group(0)@binding(1)var hdr:texture_multisampled_2d<f32>;
@group(0)@binding(2)var ambient:texture_multisampled_2d<f32>;
@group(0)@binding(3)var depth:texture_depth_multisampled_2d;
// STATS
fn valid_pixel(p:vec2<i32>)->bool{return all(p>=vec2(0))&&all(p<vec2<i32>(params.image.xy));}
fn depth_at(p:vec2<i32>,sample:u32)->f32 {
 if !valid_pixel(p){return 0.;}record_load();return textureLoad(depth,p,i32(sample));
}
fn unproject(uv:vec2<f32>,value:f32)->vec3<f32>{let q=params.inverse_view_projection*vec4(uv.x*2.-1.,1.-uv.y*2.,value,1.);return q.xyz/q.w;}
fn world_at(p:vec2<i32>,fraction:vec2<f32>,d:f32)->vec3<f32>{return unproject((vec2<f32>(p)+fraction)/vec2<f32>(params.image.xy),d);}
fn visibility(position:vec4<f32>,sample:u32)->f32 {
 let pixel=vec2<i32>(position.xy);let d=depth_at(pixel,sample);if d<=0.{return 1.;}
 let fraction=fract(position.xy);let point=world_at(pixel,fraction,d);
 let left=pixel+vec2(-1,0);let right=pixel+vec2(1,0);let up=pixel+vec2(0,-1);let down=pixel+vec2(0,1);
 let dl=depth_at(left,sample);let dr=depth_at(right,sample);let du=depth_at(up,sample);let dd=depth_at(down,sample);
 if (dl<=0.&&dr<=0.)||(du<=0.&&dd<=0.) {return 1.;}
 let xl=point-world_at(left,fraction,max(dl,0.000001));let xr=world_at(right,fraction,max(dr,0.000001))-point;
 let yu=point-world_at(up,fraction,max(du,0.000001));let yd=world_at(down,fraction,max(dd,0.000001))-point;
 let dx=select(xl,xr,dl<=0.||(dr>0.&&dot(xr,xr)<dot(xl,xl)));
 let dy=select(yu,yd,du<=0.||(dd>0.&&dot(yd,yd)<dot(yu,yu)));
 let cross_normal=cross(dx,dy);if dot(cross_normal,cross_normal)<1e-14{return 1.;}
 var normal=normalize(cross_normal);let near=unproject(position.xy/vec2<f32>(params.image.xy),1.);
 if dot(normal,near-point)<0.{normal=-normal;}
 let axis=select(vec3(0.,1.,0.),vec3(1.,0.,0.),abs(normal.y)>.9);
 let tangent=normalize(cross(axis,normal));let bitangent=cross(normal,tangent);
 var blocked=0.;record_receiver();
 for(var index=0u;index<params.image.w;index++){
  let k=params.kernel[index*(32u/params.image.w)].xyz;
  let probe=point+(tangent*k.x+bitangent*k.y+normal*k.z)*params.optics.x;
  let clip=params.view_projection*vec4(probe,1.);if clip.w<=0.{continue;}
  let ndc=clip.xyz/clip.w;let uv=vec2(ndc.x*.5+.5,.5-ndc.y*.5);
  if any(uv<vec2(0.))||any(uv>=vec2(1.))||ndc.z<0.||ndc.z>1.{continue;}
  let other_pixel=vec2<i32>(uv*vec2<f32>(params.image.xy));let od=depth_at(other_pixel,sample);if od<=0.{continue;}
  let actual=world_at(other_pixel,fraction,od);let delta=actual-point;let squared=dot(delta,delta);
  // The receiver's own plane, sky/Unknown and geometry outside the explicit
  // world radius cannot occlude. Compare along the actual camera ray.
  if squared>=params.optics.x*params.optics.x||dot(delta,normal)<=params.optics.y {continue;}
  let view=normalize(near-point);
  if dot(actual-probe,view)<=params.optics.y {continue;}
  blocked+=1.-squared/(params.optics.x*params.optics.x);record_hit();
 }
 return clamp(1.-params.optics.z*blocked/f32(params.image.w),0.,1.);
}
struct ScreenVertex{@builtin(position)position:vec4<f32>,@location(0)@interpolate(linear,sample)uv:vec2<f32>};
@vertex fn vertex(@builtin(vertex_index)i:u32)->ScreenVertex{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));var out:ScreenVertex;out.position=vec4(p[i],0.,1.);out.uv=vec2(p[i].x*.5+.5,.5-p[i].y*.5);return out;}
@fragment fn fragment(input:ScreenVertex,@builtin(sample_index)sample:u32)->@location(0)vec4<f32>{
 let position=vec4(input.uv*vec2<f32>(params.image.xy),0.,1.);record_fragment();let pixel=vec2<i32>(position.xy);let original=textureLoad(hdr,pixel,i32(sample));let component=textureLoad(ambient,pixel,i32(sample)).rgb;
 var result=original;
 if !all(component==vec3(0.)){
  let ao=visibility(position,sample);
  if ao!=1.{result=vec4(max(original.rgb-component*(1.-ao),vec3(0.)),original.a);}
 }
 return result;
}
