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
// PORTS
fn rx(p:vec3<f32>,a:f32)->vec3<f32>{let c=cos(a);let s=sin(a);return vec3(p.x,p.y*c-p.z*s,p.y*s+p.z*c);}
fn ry(p:vec3<f32>,a:f32)->vec3<f32>{let c=cos(a);let s=sin(a);return vec3(p.x*c+p.z*s,p.y,-p.x*s+p.z*c);}
fn rz(p:vec3<f32>,a:f32)->vec3<f32>{let c=cos(a);let s=sin(a);return vec3(p.x*c-p.y*s,p.x*s+p.y*c,p.z);}
struct Out{@builtin(position)position:vec4<f32>,@location(0)normal:vec3<f32>,@location(1)uv:vec2<f32>,@location(2)world:vec3<f32>,@location(3)@interpolate(flat)layer:u32,@location(4)@interpolate(flat)tint:vec3<f32>,@location(5)@interpolate(flat)light:vec4<f32>,@location(6)@interpolate(flat)glow:f32,@location(7)@interpolate(flat)port_mode:u32,@location(8)marker_uv:vec2<f32>,@location(9)@interpolate(flat)marker_only:u32,@location(10)@interpolate(flat)connector_color:u32};
@vertex fn vertex(@builtin(vertex_index)vi:u32,@builtin(instance_index)ii:u32)->Out{
 let v=vertices[vi];let item=instances[visible[v.kind*camera.params.y+ii]];var p=v.point;var n=v.normal;
 if v.bone==1u{let angle=item.phase*1.570796327;p=rx(p-v.pivot,-angle)+v.pivot;n=rx(n,-angle);}else if v.bone==2u{p=rz(p-v.pivot,item.phase)+v.pivot;n=rz(n,item.phase);}else if v.bone==3u{p.y*=item.amount;}else if v.bone==4u{p=rx(p-v.pivot,-0.392699082)+v.pivot;n=rx(n,-0.392699082);}else if v.bone==5u{p=ry(p-v.pivot,item.phase)+v.pivot;n=ry(n,item.phase);}
 if (item.codes.y&4u)!=0u{let center=vec3(0.,.5,0.)+n*.5;p=center+(p-center)*.36+n*.0005;}
 var layer=v.layer;if (v.flags&1u)!=0u{layer=item.layer;}if (v.flags&8u)!=0u{var face=0u;if v.normal.y>0.5{face=1u;}else if v.normal.z < -0.5{face=2u;}else if v.normal.z>0.5{face=3u;}else if v.normal.x < -0.5{face=4u;}else if v.normal.x>0.5{face=5u;}layer=(item.extra[face/2u]>>(16u*(face&1u)))&65535u;}if (v.flags&4u)!=0u&&(item.codes.y&1u)!=0u{layer+=1u;}if v.frames>1u{layer+=u32(max(floor(item.phase),0.))%v.frames;}
 p=ry(p,-item.yaw)+item.position;n=ry(n,-item.yaw);var out:Out;out.position=camera.view_projection*vec4(p,1.);let direction=(v.flags>>8u)&7u;let mode=(v.flags>>12u)&3u;if direction!=0u&&((item.codes.z>>(2u*(direction-1u)))&3u)!=mode{out.position=vec4(2.,2.,2.,1.);}
 let packed=item.codes.w;let levels=vec3<f32>(f32((packed>>8u)&255u),f32((packed>>16u)&255u),f32(packed>>24u));let mc=select(pow(vec3(0.8),vec3(15.)-min(levels,vec3(15.))),vec3(0.),levels==vec3(0.));let rgb=select(mc,levels/15.,levels>vec3(15.));let sky=f32(packed&15u);out.light=vec4(rgb,select(pow(0.8,15.-sky),0.,sky==0.));let tint=vec3<f32>(f32(item.tint&255u),f32((item.tint>>8u)&255u),f32((item.tint>>16u)&255u))/255.;out.tint=tint*tint;out.normal=n;out.uv=v.uv;out.world=p;out.layer=layer;out.port_mode=7u;if v.bone==0u&&item.codes.x>=5u&&item.codes.x<=12u{out.port_mode=machine_port_mode(item.extra.w,n);}out.marker_uv=v.pivot.xy;out.marker_only=item.codes.y&4u;out.connector_color=0u;if item.codes.x==16u&&(item.extra.w&0x40000000u)!=0u{out.connector_color=item.extra.w&31u;}out.glow=select(0.,item.amount,(v.flags&2u)!=0u);return out;
}
fn illumination(world:vec3<f32>,normal:vec3<f32>,light:vec4<f32>)->vec3<f32>{// ILLUMINATION
}
@fragment fn fragment(input:Out,@builtin(front_facing)front:bool)->@location(0)vec4<f32>{let texel=textureSample(atlas,texels,input.uv,i32(input.layer));if texel.a<0.001{discard;}let n=normalize(select(-input.normal,input.normal,front));let radiance=illumination(input.world,n,input.light);if input.marker_only!=0u{if input.port_mode>5u{discard;}return vec4(machine_port_linear(input.port_mode)*radiance,.6);}let albedo=machine_port_overlay(texel.rgb*input.tint,input.port_mode,input.marker_uv);let base=texel.rgb*input.tint;let color=base*max(radiance,vec3(input.glow))+(albedo-base)*radiance;return vec4(color,texel.a);}
@fragment fn shadow_fragment(input:Out){let alpha=textureSample(atlas,texels,input.uv,i32(input.layer)).a;if input.marker_only!=0u||alpha<0.5{discard;}}

@fragment fn glass_fragment(input:Out,@builtin(front_facing)front:bool)->@location(0)vec4<f32>{
 let du=dpdx(input.uv);let dv=dpdy(input.uv);
 let layer=clamp(input.layer,166u,167u)+2u;
 let glass=textureSampleGrad(atlas,texels,input.uv,i32(layer),du,dv);
 if input.connector_color==0u||input.connector_color>18u||input.layer<166u||input.layer>167u||glass.a<.001{discard;}
 let normal=normalize(select(-input.normal,input.normal,front));
 let light=illumination(input.world,normal,input.light);
 return vec4(glass.rgb*connector_linear(input.connector_color)*light,glass.a);
}
