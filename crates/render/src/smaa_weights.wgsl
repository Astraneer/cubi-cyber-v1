struct Parameters {threshold:f32,contrast:f32,search:u32,diagonal:u32};
@group(0) @binding(0) var edges:texture_2d<f32>;
@group(0) @binding(1) var<storage,read> areas:array<vec2<f32>>;
@group(0) @binding(2) var<uniform> params:Parameters;
@vertex fn vertex(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[i],0.,1.);}
fn inside(p:vec2<i32>)->bool {return all(p>=vec2(0)) && all(p<vec2<i32>(textureDimensions(edges)));}
fn edge(p:vec2<i32>)->vec2<f32>{if !inside(p){return vec2(0.);}return textureLoad(edges,p,0).rg;}
fn crossing(a:f32,b:f32)->i32 {if a>.5 && b>.5{return 2;}if a>.5{return -1;}if b>.5{return 1;}return 0;}
fn sample_edge(p:vec2<i32>,vertical:bool)->f32 {let e=edge(p);return select(e.g,e.r,vertical);}
fn cross_at(p:vec2<i32>,vertical:bool)->i32 {
 if vertical {return crossing(edge(p-vec2(1,0)).g,edge(p).g);}
 return crossing(edge(p-vec2(0,1)).r,edge(p).r);
}
fn diagonal_neighbor(p:vec2<i32>,left:i32,right:i32,vertical:bool,direction:i32)->bool {
 let slope=select(-1,1,left<right);
 var q=p+vec2(direction,direction*slope);
 if vertical {q=p+vec2(direction*slope,direction);}
 let tangent=select(vec2(1,0),vec2(0,1),vertical);
 let size=vec2<i32>(textureDimensions(edges));
 if !inside(q-tangent) || !inside(q+tangent) || q.x<=0 || q.y<=0 || q.x>=size.x-1 || q.y>=size.y-1 {return false;}
 if sample_edge(q,vertical)<.5 || sample_edge(q-tangent,vertical)>.5 || sample_edge(q+tangent,vertical)>.5 {return false;}
 return cross_at(q,vertical)==left && cross_at(q+tangent,vertical)==right;
}
fn diagonal_valid(p:vec2<i32>,left:i32,right:i32,vertical:bool)->bool {
 // The previous bounded walk started with one match and accepted two. A first
 // mismatch stopped that direction, so no later step could change this result.
 // Two explicit neighbors preserve the predicate without nested FXC loops.
 if params.diagonal==0u {return false;}
 return diagonal_neighbor(p,left,right,vertical,-1) || diagonal_neighbor(p,left,right,vertical,1);
}
fn weights(p:vec2<i32>,vertical:bool)->vec2<f32> {
 if sample_edge(p,vertical)<.5 {return vec2(0.);}
 let size=vec2<i32>(textureDimensions(edges));
 let tangent=select(vec2(1,0),vec2(0,1),vertical);
 var first=p;var last=p;var d0=0u;var d1=0u;
 for(var i=0u;i<16u;i++) {if i>=params.search || !inside(first-tangent) || sample_edge(first-tangent,vertical)<.5 {break;}first-=tangent;d0++;}
 for(var i=0u;i<16u;i++) {if i>=params.search || !inside(last+tangent) || sample_edge(last+tangent,vertical)<.5 {break;}last+=tangent;d1++;}
 if first.x<=0 || first.y<=0 || last.x>=size.x-1 || last.y>=size.y-1 {return vec2(0.);}
 if sample_edge(first-tangent,vertical)>.5 || sample_edge(last+tangent,vertical)>.5 {return vec2(0.);}
 let c0=cross_at(first,vertical);let c1=cross_at(last+tangent,vertical);
 if c0==2 || c1==2 || c0*c1>0 || (c0==0 && c1==0) {return vec2(0.);}
 if d0+d1==0u && c0*c1<0 && !diagonal_valid(p,c0,c1,vertical) {return vec2(0.);}
 let x=d0+17u*u32(c0+1);let y=d1+17u*u32(c1+1);
 return areas[min(y*51u+x,2600u)];
}
@fragment fn fragment(@builtin(position)position:vec4<f32>)->@location(0)vec4<f32>{let p=vec2<i32>(position.xy);return vec4(weights(p,false),weights(p,true));}
