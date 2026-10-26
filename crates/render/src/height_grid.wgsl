struct Camera{view_projection:mat4x4<f32>,params:vec4<u32>,reserved:vec4<u32>};
struct Tile{origin:vec3<f32>,step:f32,params:vec4<u32>,minimum:vec4<f32>,maximum:vec4<f32>};
@group(0)@binding(0)var<uniform>camera:Camera;
@group(0)@binding(1)var<storage,read>tiles:array<Tile>;
@group(0)@binding(2)var<storage,read>data:array<u32>;
@group(0)@binding(3)var<storage,read>visible:array<u32>;
@group(0)@binding(4)var atlas:texture_2d_array<f32>;
@group(0)@binding(5)var texels:sampler;
// SHADOWS
fn known(base:u32,x:u32,z:u32)->bool{let index=x+17u*z;return (data[base+289u+index/32u]&(1u<<(index&31u)))!=0u;}
fn sample_known(base:u32,x:u32,z:u32,seams:u32)->bool{if !known(base,x,z){return false;}if ((z==0u&&(seams&4u)!=0u)||(z==16u&&(seams&8u)!=0u))&&(x&1u)!=0u{return known(base,x-1u,z)&&known(base,x+1u,z);}if ((x==0u&&(seams&1u)!=0u)||(x==16u&&(seams&2u)!=0u))&&(z&1u)!=0u{return known(base,x,z-1u)&&known(base,x,z+1u);}return true;}
fn height(base:u32,x:u32,z:u32,seams:u32)->f32{if ((z==0u&&(seams&4u)!=0u)||(z==16u&&(seams&8u)!=0u))&&(x&1u)!=0u{return (bitcast<f32>(data[base+x-1u+17u*z])+bitcast<f32>(data[base+x+1u+17u*z]))*0.5;}if ((x==0u&&(seams&1u)!=0u)||(x==16u&&(seams&2u)!=0u))&&(z&1u)!=0u{return (bitcast<f32>(data[base+x+17u*(z-1u)])+bitcast<f32>(data[base+x+17u*(z+1u)]))*0.5;}return bitcast<f32>(data[base+x+17u*z]);}
struct Out{@builtin(position)position:vec4<f32>,@location(0)uv:vec2<f32>,@location(1)normal:vec3<f32>,@location(2)world:vec3<f32>,@location(3)@interpolate(flat)layer:u32,@location(4)@interpolate(flat)tint:vec3<f32>};
@vertex fn vertex(@builtin(vertex_index)vi:u32,@builtin(instance_index)ii:u32)->Out{
 let slot=visible[ii];let tile=tiles[slot];let base=slot*819u;let cell=vi/6u;let x=cell&15u;let z=cell/16u;let corners=array<vec2<u32>,6>(vec2(0u,0u),vec2(0u,1u),vec2(1u,0u),vec2(1u,0u),vec2(0u,1u),vec2(1u,1u));let corner=corners[vi%6u];let cx=x+corner.x;let cz=z+corner.y;let h00=height(base,x,z,tile.params.y);let h10=height(base,x+1u,z,tile.params.y);let h01=height(base,x,z+1u,tile.params.y);let h11=height(base,x+1u,z+1u,tile.params.y);let normal=normalize(vec3((h00+h01-h10-h11)*0.5,tile.step,(h00+h10-h01-h11)*0.5));let p=tile.origin+vec3(f32(cx)*tile.step,height(base,cx,cz,tile.params.y),f32(cz)*tile.step);let tint=data[base+555u+cell];var out:Out;out.position=camera.view_projection*vec4(p,1.);let protect=(data[base+811u+cell/32u]&(1u<<(cell&31u)))!=0u;if protect||!sample_known(base,x,z,tile.params.y)||!sample_known(base,x+1u,z,tile.params.y)||!sample_known(base,x,z+1u,tile.params.y)||!sample_known(base,x+1u,z+1u,tile.params.y){out.position=vec4(2.,2.,2.,1.);}out.uv=vec2(f32(cx),f32(cz))*tile.step;out.world=p;out.normal=normal;out.layer=data[base+299u+cell];let rgb=vec3<f32>(f32(tint&255u),f32((tint>>8u)&255u),f32((tint>>16u)&255u))/255.;out.tint=rgb*rgb;return out;
}
fn illumination(world:vec3<f32>,normal:vec3<f32>)->vec3<f32>{// ILLUMINATION
}
@fragment fn fragment(input:Out)->@location(0)vec4<f32>{let texel=textureSample(atlas,texels,input.uv,i32(input.layer));if covered_by_near(input.world-normalize(input.normal)*0.0001){discard;}return vec4(texel.rgb*input.tint*illumination(input.world,normalize(input.normal)),1.);}
