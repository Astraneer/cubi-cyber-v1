struct Camera{view_projection:mat4x4<f32>,params:vec4<u32>,reserved:vec4<u32>};
struct Entity{position:vec3<f32>,yaw:f32,angles:vec4<f32>,codes:vec4<u32>,id_high:u32,death_roll:f32,scale:f32,extra:f32};
@group(0) @binding(0)var<uniform>camera:Camera;
@group(0) @binding(1)var<storage,read>entities:array<Entity>;
@group(0) @binding(2)var<storage,read>visible:array<u32>;
@group(0) @binding(3)var<storage,read>lights:array<u32>;
@group(0) @binding(4)var mob_faces:texture_2d_array<f32>;
@group(0) @binding(5)var texels:sampler;
@group(0) @binding(6)var block_faces:texture_2d_array<f32>;
// SHADOWS
fn rx(p:vec3<f32>,a:f32)->vec3<f32>{let c=cos(a);let s=sin(a);return vec3(p.x,p.y*c-p.z*s,p.y*s+p.z*c);}
fn ry(p:vec3<f32>,a:f32)->vec3<f32>{let c=cos(a);let s=sin(a);return vec3(p.x*c+p.z*s,p.y,-p.x*s+p.z*c);}
fn rz(p:vec3<f32>,a:f32)->vec3<f32>{let c=cos(a);let s=sin(a);return vec3(p.x*c-p.y*s,p.x*s+p.y*c,p.z);}
struct VertexIn{@location(0)point:vec3<f32>,@location(1)normal:vec3<f32>,@location(2)uv:vec2<f32>,@location(3)pivot:vec3<f32>,@location(4)kind:u32,@location(5)bone:u32,@location(6)flags:u32,@location(7)base_pitch:f32,@location(8)layer:u32};
struct VertexOut{@builtin(position)position:vec4<f32>,@location(0)normal:vec3<f32>,@location(1)uv:vec2<f32>,@location(2)world:vec3<f32>,@location(3)@interpolate(flat)kind:u32,@location(4)@interpolate(flat)layer:u32,@location(5)@interpolate(flat)flags:u32,@location(6)@interpolate(flat)tint:vec3<f32>,@location(7)@interpolate(flat)light:vec4<f32>,@location(8)@interpolate(flat)part_flags:u32};
@vertex fn vertex(input:VertexIn,@builtin(instance_index)instance:u32)->VertexOut{
    let index=visible[input.kind*camera.params.y+instance];let entity=entities[index];let baby=(entity.codes.y&1u)!=0u;let phase=entity.angles.z*0.6662*select(1.,3.,baby);let swing=cos(phase)*1.4*min(entity.angles.w,1.);var pitch=input.base_pitch;var yaw=0.;var roll=0.;
    if input.bone==1u{pitch-=radians(entity.angles.y);yaw=-radians(entity.angles.x-entity.yaw);}else if input.bone==2u{pitch+=swing;}else if input.bone==3u{pitch-=swing;}else if input.bone==4u{roll=-(sin(bitcast<f32>(entity.codes.z))+1.)*entity.extra;}else if input.bone==5u{roll=(sin(bitcast<f32>(entity.codes.z))+1.)*entity.extra;}else if input.bone==6u{pitch-=swing;}else if input.bone==7u{pitch+=swing;}
    if input.kind==5u {let idle=sin(bitcast<f32>(entity.codes.z)*0.067)*0.025;if input.bone==6u{pitch=input.base_pitch-swing/1.4+idle-entity.extra;}else if input.bone==7u{pitch=input.base_pitch+swing/1.4-idle;}}
    var pivot=input.pivot;if input.kind==2u&&input.bone==1u&&(entity.codes.y&512u)!=0u{let eating=clamp(entity.extra,0.,40.);let neck=clamp(min(eating,40.-eating)/4.,0.,1.);pivot.y-=neck*0.5625;pitch=-0.62831853;if eating>=4.&&eating<=36.{pitch-=0.21991149*sin((eating-4.)/32.*28.7);}}
    var local=input.point;if baby&&input.bone==1u{local*=1.5;}var point=rz(ry(rx(local,pitch),yaw),roll)+pivot;var normal=rz(ry(rx(input.normal,pitch),yaw),roll);var body_yaw=3.141592653589793-radians(entity.yaw);var scale=entity.scale;if input.kind==5u{scale*=0.9;}
    if input.kind>=6u {body_yaw=entity.angles.z/20.+entity.extra;point.y+=0.5;point*=scale;point.y+=sin(entity.angles.z/10.+entity.extra)*0.1+0.1;point=ry(point,body_yaw);normal=ry(normal,body_yaw);}else{point*=scale;point=ry(rz(point-vec3(0.,scale*0.5,0.),radians(entity.death_roll))+vec3(0.,scale*0.5,0.),body_yaw);normal=ry(rz(normal,radians(entity.death_roll)),body_yaw);}
    if input.kind==5u&&(entity.codes.y&256u)!=0u{point.y*=0.833333333;normal.y*=1.2;}
    let world=point+entity.position;let packed=lights[index];let levels=vec3<f32>(f32(packed&15u),f32((packed>>4u)&15u),f32((packed>>8u)&15u));let rgb=select(pow(vec3(0.8),vec3(15.)-levels),vec3(0.),levels==vec3(0.));let sky=f32((packed>>12u)&15u);let dyes=array<u32,6>(0xf9fffeu,0x1d1d21u,0x474f52u,0x9d9d97u,0x835432u,0xf38baau);let dye=dyes[min(entity.codes.z,5u)];let tint=vec3<f32>(f32((dye>>16u)&255u),f32((dye>>8u)&255u),f32(dye&255u))/255.;var out:VertexOut;out.position=camera.view_projection*vec4(world,1.);if (input.flags&1u)!=0u&&(entity.codes.y&2u)!=0u{out.position=vec4(2.,2.,2.,1.);}if camera.params.z==0u&&(entity.codes.y&1024u)!=0u{out.position=vec4(2.,2.,2.,1.);}out.normal=normal;out.uv=input.uv;out.world=world;out.kind=input.kind;out.layer=select(input.layer,entity.codes.z,input.kind>=6u);out.flags=entity.codes.y;out.tint=select(vec3(1.),tint*tint,(input.flags&1u)!=0u);out.light=vec4(rgb,select(pow(0.8,15.-sky),0.,sky==0.));out.part_flags=input.flags;return out;
}
fn illumination(world:vec3<f32>,normal:vec3<f32>,light:vec4<f32>)->vec3<f32>{// ILLUMINATION
}
@fragment fn fragment(input:VertexOut)->@location(0)vec4<f32>{let du=dpdx(input.uv);let dv=dpdy(input.uv);var texel:vec4<f32>;if input.kind<6u{texel=textureSampleGrad(mob_faces,texels,input.uv,i32(input.layer),du,dv);}else{texel=textureSampleGrad(block_faces,texels,input.uv,i32(input.layer),du,dv);}if texel.a<0.001{discard;}var color=texel.rgb*input.tint;if input.kind==5u&&(input.part_flags&2u)!=0u&&color.g>color.r*1.3&&color.g>color.b*1.3{color=vec3(0.72,0.48,0.34)*(0.6+color.g*1.8);}color*=illumination(input.world,normalize(input.normal),input.light);if (input.flags&20u)!=0u{color=mix(color,vec3(1.,0.08,0.05),0.6);}if (input.flags&8u)!=0u{color+=vec3(0.2,0.03,0.);}return vec4(color,texel.a);}
@fragment fn shadow_fragment(input:VertexOut){let du=dpdx(input.uv);let dv=dpdy(input.uv);var alpha:f32;if input.kind<6u{alpha=textureSampleGrad(mob_faces,texels,input.uv,i32(input.layer),du,dv).a;}else{alpha=textureSampleGrad(block_faces,texels,input.uv,i32(input.layer),du,dv).a;}if alpha<0.5{discard;}}
