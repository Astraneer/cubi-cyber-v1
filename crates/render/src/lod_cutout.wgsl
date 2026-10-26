struct Camera{view_projection:mat4x4<f32>,eye:vec4<f32>,direction:vec4<f32>,params:vec4<u32>};
struct Object{model:mat4x4<f32>,inverse_model:mat4x4<f32>,bounds_min:vec4<f32>,bounds_max:vec4<f32>,data:vec4<u32>,light:vec4<f32>,reserved:array<vec4<u32>,4>};
@group(0) @binding(0)var<uniform>camera:Camera;
@group(0) @binding(1)var<storage,read>objects:array<Object>;
@group(0) @binding(2)var<storage,read>payload:array<u32>;
@group(0) @binding(3)var<storage,read>visible:array<vec2<u32>>;
@group(0) @binding(4)var colors:texture_2d_array<f32>;
@group(0) @binding(5)var tint_masks:texture_2d_array<f32>;
@group(0) @binding(6)var texels:sampler;
@group(0) @binding(7)var<storage,read>materials:array<vec4<u32>>;
// SHADOWS
struct VertexOut{@builtin(position)position:vec4<f32>,@location(0)uv:vec2<f32>,@location(1)world:vec3<f32>,@location(2)normal:vec3<f32>,@location(3)@interpolate(flat)slot:u32,@location(4)@interpolate(flat)material:u32,@location(5)@interpolate(flat)tint:vec3<f32>,@location(6)ao:f32};
@vertex fn vertex(@builtin(vertex_index)corner:u32,@builtin(instance_index)instance:u32)->VertexOut{
 let entry=visible[instance];let object=objects[entry.x];let q=vec4(payload[entry.y],payload[entry.y+1u],payload[entry.y+2u],payload[entry.y+3u]);let bits=q.x;
 let origin=vec3<f32>(f32(bits&31u),f32((bits>>5u)&31u),f32((bits>>10u)&31u));let face=(bits>>15u)&7u;let width=f32(((bits>>18u)&15u)+1u);let height=f32(((bits>>22u)&15u)+1u);
 let us=array<vec3<f32>,6>(vec3(1.,0.,0.),vec3(1.,0.,0.),vec3(-1.,0.,0.),vec3(1.,0.,0.),vec3(0.,0.,1.),vec3(0.,0.,-1.));let vs=array<vec3<f32>,6>(vec3(0.,0.,1.),vec3(0.,0.,-1.),vec3(0.,1.,0.),vec3(0.,1.,0.),vec3(0.,1.,0.),vec3(0.,1.,0.));let normals=array<vec3<f32>,6>(vec3(0.,-1.,0.),vec3(0.,1.,0.),vec3(0.,0.,-1.),vec3(0.,0.,1.),vec3(-1.,0.,0.),vec3(1.,0.,0.));
 let p=origin+us[face]*f32(corner&1u)*width+vs[face]*f32(corner>>1u)*height;let world=object.model*vec4(p,1.);var out:VertexOut;out.position=camera.view_projection*world;out.world=world.xyz;out.normal=normalize((transpose(object.inverse_model)*vec4(normals[face],0.)).xyz);out.uv=vec2(dot(p,us[face]),dot(p,vs[face]));out.slot=entry.x;out.material=q.y&65535u;let tint=unpack4x8unorm(q.z).rgb;out.tint=tint*tint;out.ao=0.55+0.15*f32((q.y>>(16u+corner*2u))&3u);return out;
}
fn illumination(object:Object,world:vec3<f32>,normal:vec3<f32>)->vec3<f32>{// ILLUMINATION
}
fn ambient(object:Object,normal:vec3<f32>)->vec3<f32>{// AMBIENT
}
// OUTPUT
@fragment fn fragment(input:VertexOut)->CutoutOutput{
 let object=objects[input.slot];let material=materials[input.material];let texel=textureSample(colors,texels,input.uv,i32(material.x));let mask=textureSample(tint_masks,texels,input.uv,i32(material.x)).r;let tint=mix(vec3(1.),input.tint,select(1.,mask,(material.y&1u)!=0u));let albedo=texel.rgb*tint;let normal=normalize(input.normal);let indirect=ambient(object,normal)*input.ao;let color=albedo*(illumination(object,input.world,normal)+(input.ao-1.)*ambient(object,normal));
 // Evaluate derivatives/lighting before the pixel-rate alpha test. Hardware A2C
 // performs the actual per-sample coverage on ordinary rasterized geometry.
 if texel.a<0.5{discard;}
 // RETURN
}
