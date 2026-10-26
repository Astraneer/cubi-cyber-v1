struct Parameters {threshold:f32,contrast:f32,search:u32,diagonal:u32};
@group(0) @binding(0) var color:texture_2d<f32>;
@group(0) @binding(1) var<uniform> params:Parameters;
@vertex fn vertex(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[i],0.,1.);}
fn perceptual(c:vec3<f32>)->vec3<f32>{return select(1.055*pow(max(c,vec3(0.)),vec3(1./2.4))-vec3(.055),c*12.92,c<=vec3(.0031308));}
fn luma(p:vec2<i32>)->f32 {let size=vec2<i32>(textureDimensions(color));return dot(perceptual(textureLoad(color,clamp(p,vec2(0),size-vec2(1)),0).rgb),vec3(.2126,.7152,.0722));}
@fragment fn fragment(@builtin(position)position:vec4<f32>)->@location(0)vec2<f32>{
 let p=vec2<i32>(position.xy);let c=luma(p);let west=luma(p-vec2(1,0));let north=luma(p-vec2(0,1));let left=abs(c-west);let top=abs(c-north);
 let direct=max(max(left,top),max(abs(c-luma(p+vec2(1,0))),abs(c-luma(p+vec2(0,1)))));
 let contrast=max(direct,max(abs(west-luma(p-vec2(2,0))),abs(north-luma(p-vec2(0,2)))));
 return vec2(select(0.,1.,left>=params.threshold && left*params.contrast>=contrast && p.x>0),select(0.,1.,top>=params.threshold && top*params.contrast>=contrast && p.y>0));
}
