//! Loading-only temporal shader variants. Both entries use the exact existing
//! model-pose body; default color shaders do not call this module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error(pub &'static str);
pub fn entity_vertex_attributes() -> [wgpu::VertexAttribute; 9] {
    use wgpu::VertexFormat::*;
    let formats = [
        Float32x3, Float32x3, Float32x2, Float32x3, Uint32, Uint32, Uint32, Float32, Uint32,
    ];
    let offsets = [0, 12, 24, 32, 44, 48, 52, 56, 60];
    std::array::from_fn(|i| wgpu::VertexAttribute {
        format: formats[i],
        offset: offsets[i],
        shader_location: i as u32,
    })
}
fn replace(source: &str, old: &'static str, new: &str) -> Result<String, Error> {
    if !source.contains(old) {
        return Err(Error(old));
    }
    Ok(source.replacen(old, new, 1))
}
const ENTITY_ENTRY: &str =
    "@vertex fn vertex(input:VertexIn,@builtin(instance_index)instance:u32)->VertexOut{";
const ENTITY_SELECT: &str =
    "let index=visible[input.kind*camera.params.y+instance];let entity=entities[index];";
const BLOCK_ENTRY: &str =
    "@vertex fn vertex(@builtin(vertex_index)vi:u32,@builtin(instance_index)ii:u32)->Out{";
const BLOCK_SELECT: &str =
    "let v=vertices[vi];let item=instances[visible[v.kind*camera.params.y+ii]];";

pub fn entity_color(source: &str) -> Result<String, Error> {
    let source = replace(
        source,
        "@builtin(position)position:vec4<f32>",
        "@builtin(position)@invariant position:vec4<f32>",
    )?;
    let source = replace(
        &source,
        ENTITY_ENTRY,
        "fn temporal_entity_pose(input:VertexIn,index:u32,entity:Entity)->VertexOut{",
    )?;
    let source = replace(&source, ENTITY_SELECT, "")?;
    Ok(format!(
        "{source}\n{ENTITY_ENTRY}{ENTITY_SELECT}return temporal_entity_pose(input,index,entity);}}"
    ))
}
pub fn block_color(source: &str) -> Result<String, Error> {
    let source = replace(
        source,
        "@builtin(position)position:vec4<f32>",
        "@builtin(position)@invariant position:vec4<f32>",
    )?;
    let source = replace(
        &source,
        BLOCK_ENTRY,
        "fn temporal_block_pose(v:Vertex,item:Instance)->Out{",
    )?;
    let source = replace(&source, BLOCK_SELECT, "")?;
    Ok(format!(
        "{source}\n{BLOCK_ENTRY}{BLOCK_SELECT}return temporal_block_pose(v,item);}}"
    ))
}
const COMMON: &str = r#"
struct TemporalCamera { previous_vp:mat4x4<f32>, params:vec4<f32> };
struct TemporalMatch { surface:vec4<u32>, previous_index:u32, valid:u32, reserved:vec2<u32> };
@group(1)@binding(0)var<uniform>temporal_camera:TemporalCamera;
@group(1)@binding(2)var<storage,read>temporal_matches:array<TemporalMatch>;
struct TemporalVertex {
 @builtin(position)@invariant position:vec4<f32>,
 @location(0)uv:vec2<f32>,
 @location(1)@interpolate(flat)layer:u32,
 @location(2)@interpolate(flat)kind:u32,
 @location(3)previous_clip:vec4<f32>,
 @location(4)@interpolate(flat)surface:vec4<u32>,
 @location(5)@interpolate(flat)valid:u32,
 @location(6)@interpolate(flat)marker_only:u32,
 @location(7)@interpolate(flat)port_mode:u32,
 @location(8)@interpolate(flat)connector_color:u32,
 @location(9)current_clip:vec4<f32>,
};
struct TemporalOutput { @location(0)motion:vec4<f32>, @location(1)surface:vec4<u32> };
fn temporal_result(input:TemporalVertex,supported:bool)->TemporalOutput {
 var output:TemporalOutput;
 output.motion=vec4(0.);output.surface=vec4(0u);
 if !supported||(input.surface.x>>24u)==0u||input.surface.w==0u{return output;}
 // Current identity seeds history even when no previous pose exists yet.
 output.surface=input.surface;
 if input.valid==0u||!(input.previous_clip.w>0.)||!(input.current_clip.w>0.)||!all(abs(input.previous_clip)<vec4(1e30))||!all(abs(input.current_clip)<vec4(1e30)){return output;}
 let previous_ndc=input.previous_clip.xyz/input.previous_clip.w;
 let previous_uv=previous_ndc.xy*vec2(.5,-.5)+vec2(.5);
 if !all(previous_uv>=vec2(0.))||!all(previous_uv<=vec2(1.))||!(previous_ndc.z>=0.&&previous_ndc.z<=1.){return output;}
 // Both clips describe the same rasterized surface point. Subtracting the
 // pixel center here would inject fixed-point raster snapping into velocity,
 // even for a completely stationary model. Remove only the current jitter;
 // previous_vp is already unjittered.
 let current_uv=input.current_clip.xy/input.current_clip.w*vec2(.5,-.5)+vec2(.5)-temporal_camera.params.zw*temporal_camera.params.xy;
 output.motion=vec4(previous_uv-current_uv,previous_ndc.z,1.);
 return output;
}
"#;
const ENTITY: &str = r#"
@group(1)@binding(1)var<storage,read>temporal_previous_entities:array<Entity>;
@vertex fn temporal_vertex(input:VertexIn,@builtin(instance_index)instance:u32,@builtin(vertex_index)vertex_index:u32)->TemporalVertex {
 let index=visible[input.kind*camera.params.y+instance];
 let current=temporal_entity_pose(input,index,entities[index]);
 let matched=temporal_matches[index];
 var output:TemporalVertex;
 output.position=current.position;output.current_clip=current.position;output.uv=current.uv;output.layer=current.layer;output.kind=current.kind;
 output.previous_clip=vec4(0.);output.valid=matched.valid;output.surface=matched.surface;
 output.surface.x|=vertex_index/3u;
 output.marker_only=0u;output.port_mode=0u;output.connector_color=0u;
 if matched.valid!=0u&&matched.previous_index<arrayLength(&temporal_previous_entities){
   let previous=temporal_entity_pose(input,index,temporal_previous_entities[matched.previous_index]);
   output.previous_clip=temporal_camera.previous_vp*vec4(previous.world,1.);
 }else{output.valid=0u;}
 return output;
}
@fragment fn temporal_fragment(input:TemporalVertex)->TemporalOutput {
 let du=dpdx(input.uv);let dv=dpdy(input.uv);var alpha:f32;
 if input.kind<6u{alpha=textureSampleGrad(mob_faces,texels,input.uv,i32(input.layer),du,dv).a;}
 else if input.kind==6u{alpha=textureSampleGrad(block_faces,texels,input.uv,i32(input.layer),du,dv).a;}
 else{alpha=textureSampleGrad(item_faces,texels,input.uv,i32(input.layer),du,dv).a;}
 if alpha<.001{discard;}
 return temporal_result(input,true);
}
"#;
const BLOCK: &str = r#"
@group(1)@binding(1)var<storage,read>temporal_previous_blocks:array<Instance>;
@vertex fn temporal_vertex(@builtin(vertex_index)vertex_index:u32,@builtin(instance_index)instance_index:u32)->TemporalVertex {
 let v=vertices[vertex_index];let index=visible[v.kind*camera.params.y+instance_index];
 let current=temporal_block_pose(v,instances[index]);let matched=temporal_matches[index];
 var output:TemporalVertex;
 output.position=current.position;output.current_clip=current.position;output.uv=current.uv;output.layer=current.layer;output.kind=v.kind;
 output.previous_clip=vec4(0.);output.valid=matched.valid;output.surface=matched.surface;
 output.surface.x|=vertex_index/3u;
 output.marker_only=current.marker_only;output.port_mode=current.port_mode;output.connector_color=current.connector_color;
 if matched.valid!=0u&&matched.previous_index<arrayLength(&temporal_previous_blocks){
   let previous=temporal_block_pose(v,temporal_previous_blocks[matched.previous_index]);
   output.previous_clip=temporal_camera.previous_vp*vec4(previous.world,1.);
 }else{output.valid=0u;}
 return output;
}
@fragment fn temporal_fragment(input:TemporalVertex)->TemporalOutput {
 let alpha=textureSample(atlas,texels,input.uv,i32(input.layer)).a;
 if alpha<.001{discard;}
 if input.marker_only!=0u&&input.port_mode>5u{discard;}
 return temporal_result(input,input.marker_only==0u&&alpha>=.999);
}
@fragment fn temporal_glass_fragment(input:TemporalVertex)->TemporalOutput {
 let du=dpdx(input.uv);let dv=dpdy(input.uv);let layer=clamp(input.layer,166u,167u)+2u;
 let alpha=textureSampleGrad(atlas,texels,input.uv,i32(layer),du,dv).a;
 if input.connector_color==0u||input.connector_color>18u||input.layer<166u||input.layer>167u||alpha<.001{discard;}
 return temporal_result(input,false);
}
"#;
pub fn entity_motion(source: &str) -> Result<String, Error> {
    Ok(format!("{}\n{COMMON}\n{ENTITY}", entity_color(source)?))
}
pub fn block_motion(source: &str) -> Result<String, Error> {
    Ok(format!("{}\n{COMMON}\n{BLOCK}", block_color(source)?))
}
