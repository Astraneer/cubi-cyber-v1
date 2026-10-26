@group(0) @binding(0) var color:texture_2d<f32>;
@group(0) @binding(1) var weights:texture_2d<f32>;
@vertex fn vertex(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[i],0.,1.);}
fn linear_to_srgb(c:vec3<f32>)->vec3<f32>{return select(1.055*pow(max(c,vec3(0.)),vec3(1./2.4))-vec3(.055),c*12.92,c<=vec3(.0031308));}
fn safe(p:vec2<i32>)->vec2<i32>{return clamp(p,vec2(0),vec2<i32>(textureDimensions(color))-vec2(1));}
@fragment fn fragment(@builtin(position)position:vec4<f32>)->@location(0)vec4<f32>{
 let p=vec2<i32>(position.xy);let own=textureLoad(weights,p,0);
 var a=own.r;var b=textureLoad(weights,safe(p+vec2(0,1)),0).g;var direction=vec2(0,1);
 let left=own.b;let right=textureLoad(weights,safe(p+vec2(1,0)),0).a;
 if max(left,right)>max(a,b) {a=left;b=right;direction=vec2(1,0);}
 let total=a+b;if total>1. {a/=total;b/=total;}
 let center=textureLoad(color,p,0).rgb;
 let result=center*(1.-a-b)+textureLoad(color,safe(p-direction),0).rgb*a+textureLoad(color,safe(p+direction),0).rgb*b;
 return vec4(result,1.0);
}
