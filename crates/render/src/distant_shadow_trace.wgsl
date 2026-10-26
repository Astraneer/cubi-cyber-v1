// The six rasterized box faces carry the orthographic ray. Only public opaque
// entries participate here; cutouts use ordinary quad rasterization separately.
@fragment fn fragment(input:VertexOut)->@builtin(frag_depth)f32{
 let object=objects[input.slot];
 let direction=normalize((object.inverse_model*vec4(camera.direction.xyz,0.)).xyz);
 let origin=input.local_point-direction*64.;
 var enter=-1e30;var exit=1e30;
 for(var axis=0u;axis<3u;axis++){
  if abs(direction[axis])<1e-8{
   if origin[axis]<0.||origin[axis]>16.{discard;}
  }else{
   let a=-origin[axis]/direction[axis];let b=(16.-origin[axis])/direction[axis];
   enter=max(enter,min(a,b));exit=min(exit,max(a,b));
  }
 }
 if exit<max(enter,0.){discard;}
 var t=max(enter,0.);let start=origin+direction*(t+0.0001);
 var cell=vec3<i32>(clamp(floor(start),vec3(0.),vec3(15.)));
 let stride=select(vec3(-1),vec3(1),direction>=vec3(0.));
 var delta=vec3(1e30);var next=vec3(1e30);
 for(var axis=0u;axis<3u;axis++){
  if abs(direction[axis])>=1e-8{
   delta[axis]=abs(1./direction[axis]);
   next[axis]=(f32(cell[axis]+select(0,1,direction[axis]>0.))-origin[axis])/direction[axis];
  }
 }
 for(var steps=0u;steps<46u;steps++){
  let index=u32(cell.x)+16u*(u32(cell.z)+16u*u32(cell.y));
  if (payload[object.data.x+index/32u]&(1u<<(index%32u)))!=0u{
   let palette=object.data.y+palette_index(object,index)*2u;
   if ((payload[palette]>>16u)&3u)==0u{
    let clip=camera.view_projection*object.model*vec4(origin+direction*t,1.);
    return clip.z/clip.w;
   }
  }
  let crossing=min(next.x,min(next.y,next.z));if crossing>exit{discard;}t=crossing;
  for(var axis=0u;axis<3u;axis++){
   if next[axis]<=crossing+0.000001{cell[axis]+=stride[axis];next[axis]+=delta[axis];}
  }
  if any(cell<vec3(0))||any(cell>=vec3(16)){discard;}
 }
 discard;
 return 0.;
}
