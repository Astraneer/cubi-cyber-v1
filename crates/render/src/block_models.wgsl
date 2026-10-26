struct Camera{view_projection:mat4x4<f32>,params:vec4<u32>,reserved:vec4<u32>};
struct Instance{position:vec3<f32>,yaw:f32,codes:vec4<u32>,phase:f32,amount:f32,tint:u32,layer:u32,extra:vec4<u32>};
struct Vertex{point:vec3<f32>,layer:u32,normal:vec3<f32>,flags:u32,uv:vec2<f32>,bone:u32,kind:u32,pivot:vec3<f32>,frames:u32};
@group(0)@binding(0)var<uniform>camera:Camera;
@group(0)@binding(1)var<storage,read>instances:array<Instance>;
@group(0)@binding(2)var<storage,read>vertices:array<Vertex>;
@group(0)@binding(3)var<storage,read>visible:array<u32>;
@group(0)@binding(4)var atlas:texture_2d_array<f32>;
@group(0)@binding(5)var texels:sampler;
// SHADOWS
fn rx(p:vec3<f32>,a:f32)->vec3<f32>{let c=cos(a);let s=sin(a);return vec3(p.x,p.y*c-p.z*s,p.y*s+p.z*c);}
fn ry(p:vec3<f32>,a:f32)->vec3<f32>{let c=cos(a);let s=sin(a);return vec3(p.x*c+p.z*s,p.y,-p.x*s+p.z*c);}
fn rz(p:vec3<f32>,a:f32)->vec3<f32>{let c=cos(a);let s=sin(a);return vec3(p.x*c-p.y*s,p.x*s+p.y*c,p.z);}
struct Out{@builtin(position)position:vec4<f32>,@location(0)normal:vec3<f32>,@location(1)uv:vec2<f32>,@location(2)world:vec3<f32>,@location(3)@interpolate(flat)layer:u32,@location(4)@interpolate(flat)tint:vec3<f32>,@location(5)@interpolate(flat)light:vec4<f32>,@location(6)@interpolate(flat)glow:f32};
@vertex fn vertex(@builtin(vertex_index)vi:u32,@builtin(instance_index)ii:u32)->Out{
 let v=vertices[vi];let item=instances[visible[v.kind*camera.params.y+ii]];var p=v.point;var n=v.normal;
 if v.bone==1u{let angle=item.phase*1.570796327;p=rx(p-v.pivot,-angle)+v.pivot;n=rx(n,-angle);}else if v.bone==2u{p=rz(p-v.pivot,item.phase)+v.pivot;n=rz(n,item.phase);}else if v.bone==3u{p.y*=item.amount;}else if v.bone==4u{p=rx(p-v.pivot,-0.392699082)+v.pivot;n=rx(n,-0.392699082);}else if v.bone==5u{p=ry(p-v.pivot,item.phase)+v.pivot;n=ry(n,item.phase);}
 var layer=v.layer;if (v.flags&1u)!=0u{layer=item.layer;}if (v.flags&8u)!=0u{var face=0u;if v.normal.y>0.5{face=1u;}else if v.normal.z < -0.5{face=2u;}else if v.normal.z>0.5{face=3u;}else if v.normal.x < -0.5{face=4u;}else if v.normal.x>0.5{face=5u;}layer=(item.extra[face/2u]>>(16u*(face&1u)))&65535u;}if (v.flags&4u)!=0u&&(item.codes.y&1u)!=0u{layer+=1u;}if v.frames>1u{layer+=u32(max(floor(item.phase),0.))%v.frames;}
 p=ry(p,-item.yaw)+item.position;n=ry(n,-item.yaw);var out:Out;out.position=camera.view_projection*vec4(p,1.);let direction=(v.flags>>8u)&7u;let mode=(v.flags>>12u)&3u;if direction!=0u&&((item.codes.z>>(2u*(direction-1u)))&3u)!=mode{out.position=vec4(2.,2.,2.,1.);}
 let packed=item.codes.w;let levels=vec3<f32>(f32((packed>>8u)&255u),f32((packed>>16u)&255u),f32(packed>>24u));let mc=select(pow(vec3(0.8),vec3(15.)-min(levels,vec3(15.))),vec3(0.),levels==vec3(0.));let rgb=select(mc,levels/15.,levels>vec3(15.));let sky=f32(packed&15u);out.light=vec4(rgb,select(pow(0.8,15.-sky),0.,sky==0.));let tint=vec3<f32>(f32(item.tint&255u),f32((item.tint>>8u)&255u),f32((item.tint>>16u)&255u))/255.;out.tint=tint*tint;out.normal=n;out.uv=v.uv;out.world=p;out.layer=layer;out.glow=select(0.,item.amount,(v.flags&2u)!=0u);return out;
}
fn illumination(world:vec3<f32>,normal:vec3<f32>,light:vec4<f32>)->vec3<f32>{// ILLUMINATION
}
@fragment fn fragment(input:Out,@builtin(front_facing)front:bool)->@location(0)vec4<f32>{let texel=textureSample(atlas,texels,input.uv,i32(input.layer));if texel.a<0.001{discard;}let n=normalize(select(-input.normal,input.normal,front));let radiance=illumination(input.world,n,input.light);return vec4(texel.rgb*input.tint*max(radiance,vec3(input.glow)),texel.a);}
@fragment fn shadow_fragment(input:Out){if textureSample(atlas,texels,input.uv,i32(input.layer)).a<0.5{discard;}}
