//! Exact public Height/Proxy geometry for the optional motion pass. Geometry
//! edits reset temporal history; snapshots preserve actual presented transforms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error(pub &'static str);
fn replace(source: &str, old: &'static str, new: &str) -> Result<String, Error> {
    if !source.contains(old) {
        return Err(Error(old));
    }
    Ok(source.replacen(old, new, 1))
}
pub const COMMON: &str = r#"
struct TemporalSnapshot{model:mat4x4<f32>,metadata:vec4<u32>};
struct TemporalCamera{previous_vp:mat4x4<f32>,inverse_extent_jitter:vec4<f32>,flags:vec4<u32>,opaque_layers:array<vec4<u32>,2>};
@group(1)@binding(0)var<uniform>temporal_camera:TemporalCamera;
@group(1)@binding(1)var<storage,read>temporal_previous:array<TemporalSnapshot>;
@group(1)@binding(2)var<storage,read>temporal_generations:array<u32>;
struct TemporalOutput{@location(0)motion:vec4<f32>,@location(1)surface:vec4<u32>};
fn temporal_known(surface:vec4<u32>,previous_clip:vec4<f32>,current_clip:vec4<f32>,valid:bool,stationary:bool)->TemporalOutput{
 var output:TemporalOutput;output.motion=vec4(0.);output.surface=surface;
 if !valid||!(previous_clip.w>0.)||!(current_clip.w>0.){return output;}
 let ndc=previous_clip.xyz/previous_clip.w;let previous_uv=ndc.xy*vec2(.5,-.5)+vec2(.5);
 if !all(abs(ndc)<vec3(1e30))||!all(previous_uv>=vec2(0.))||!all(previous_uv<=vec2(1.))||!(ndc.z>=0.&&ndc.z<=1.){return output;}
 let current_uv=current_clip.xy/current_clip.w*vec2(.5,-.5)+vec2(.5)-temporal_camera.inverse_extent_jitter.zw*temporal_camera.inverse_extent_jitter.xy;
 output.motion=vec4(select(previous_uv-current_uv,vec2(0.),stationary),ndc.z,1.);return output;
}
fn temporal_layer_opaque(layer:u32)->bool{
 if layer>=256u{return false;}let word=layer/32u;
 return(temporal_camera.opaque_layers[word/4u][word%4u]&(1u<<(layer%32u)))!=0u;
}
fn temporal_face(normal:vec3<f32>)->u32{
 if abs(normal.x)>.5{return select(4u,5u,normal.x>0.);}
 if abs(normal.z)>.5{return select(2u,3u,normal.z>0.);}
 return select(0u,1u,normal.y>0.);
}
"#;

pub fn snapshot(height: bool) -> String {
    let (stride, model, flags) = if height {
        (16,"mat4x4<f32>(vec4(bitcast<f32>(source[base+3u]),0.,0.,0.),vec4(0.,1.,0.,0.),vec4(0.,0.,bitcast<f32>(source[base+3u]),0.),vec4(bitcast<f32>(source[base]),bitcast<f32>(source[base+1u]),bitcast<f32>(source[base+2u]),1.))".to_owned(),4)
    } else {
        let columns:Vec<_>=(0..4).map(|column|format!("vec4(bitcast<f32>(source[base+{}u]),bitcast<f32>(source[base+{}u]),bitcast<f32>(source[base+{}u]),bitcast<f32>(source[base+{}u]))",column*4,column*4+1,column*4+2,column*4+3)).collect();
        (64, format!("mat4x4<f32>({})", columns.join(",")), 43)
    };
    format!(
        "struct Snapshot{{model:mat4x4<f32>,metadata:vec4<u32>}};@group(0)@binding(0)var<storage,read>source:array<u32>;@group(0)@binding(1)var<storage,read>generations:array<u32>;@group(0)@binding(2)var<storage,read_write>candidate:array<Snapshot>;@compute @workgroup_size(64)fn snapshot(@builtin(global_invocation_id)i:vec3<u32>){{if i.x>=arrayLength(&generations)||i.x>=arrayLength(&candidate){{return;}}let base=i.x*{stride}u;let model={model};candidate[i.x]=Snapshot(model,vec4(generations[i.x],source[base+{flags}u],0u,0u));}}"
    )
}

pub fn proxy_opaque() -> Result<String, Error> {
    let source = crate::lod_gpu::shader_source(false, false, false, false);
    let source = replace(
        &source,
        "@builtin(position)position:vec4<f32>",
        "@builtin(position)@invariant position:vec4<f32>",
    )?;
    let source = replace(
        &source,
        "struct FragmentOut{@location(0)color:vec4<f32>,@builtin(frag_depth)depth:f32};",
        "struct FragmentOut{color:vec4<f32>,depth:f32,local:vec3<f32>,owner:u32,face:u32};",
    )?;
    let source = replace(
        &source,
        "@fragment fn fragment(input:VertexOut)",
        "fn temporal_unused_color(input:VertexOut)",
    )?;
    let source = source.replace(
        "FragmentOut(vec4(0.,0.,0.,1.),-1.)",
        "FragmentOut(vec4(0.,0.,0.,1.),-1.,vec3(0.),0u,0u)",
    );
    let source = replace(
        &source,
        "out.depth=clip.z/clip.w;",
        "out.local=point;out.owner=index;out.face=temporal_face(normal);out.depth=clip.z/clip.w;",
    )?;
    Ok(format!("{source}\n{COMMON}\n{}", PROXY_OPAQUE))
}
const PROXY_OPAQUE: &str = r#"
struct TemporalDepthOutput{@location(0)motion:vec4<f32>,@location(1)surface:vec4<u32>,@builtin(frag_depth)depth:f32};
@fragment fn temporal_fragment(input:VertexOut)->TemporalDepthOutput{
 let hit=trace_fragment(input);if hit.depth<0.{discard;}
 let previous=temporal_previous[input.slot];let generation=temporal_generations[input.slot];
 let identity=vec4(0x11000000u|(hit.owner*8u+hit.face),input.slot,0u,generation);
 let valid=temporal_camera.flags.x!=0u&&generation!=0u&&previous.metadata.x==generation&&previous.metadata.y!=0u;
 let previous_clip=temporal_camera.previous_vp*(previous.model*vec4(hit.local,1.));
 let current=objects[input.slot];let current_clip=camera.view_projection*(current.model*vec4(hit.local,1.));
 let stationary=temporal_camera.flags.z!=0u&&all(current.model[0]==previous.model[0])&&all(current.model[1]==previous.model[1])&&all(current.model[2]==previous.model[2])&&all(current.model[3]==previous.model[3]);
 let value=temporal_known(identity,previous_clip,current_clip,valid,stationary);
 return TemporalDepthOutput(value.motion,value.surface,hit.depth);
}
"#;

pub fn proxy_cutout() -> Result<String, Error> {
    let source = crate::lod_gpu::cutout_shader_source(false, false, false);
    let source = replace(
        &source,
        "@builtin(position)position:vec4<f32>",
        "@builtin(position)@invariant position:vec4<f32>",
    )?;
    let source = replace(
        &source,
        "@location(6)ao:f32",
        "@location(6)ao:f32,@location(7)previous_clip:vec4<f32>,@location(8)@interpolate(flat)surface:vec4<u32>,@location(9)@interpolate(flat)previous_valid:u32,@location(10)local_point:vec3<f32>,@location(11)@interpolate(flat)local_normal:vec3<f32>,@location(12)current_clip:vec4<f32>",
    )?;
    let source = replace(
        &source,
        "@vertex fn vertex(@builtin(vertex_index)corner:u32,@builtin(instance_index)instance:u32)",
        "fn temporal_cutout_pose(corner:u32,instance:u32)",
    )?;
    let source = replace(
        &source,
        "out.world=world.xyz;",
        "out.world=world.xyz;out.local_point=p;out.local_normal=normals[face];",
    )?;
    Ok(format!("{source}\n{COMMON}\n{}", PROXY_CUTOUT))
}
const PROXY_CUTOUT: &str = r#"
@vertex fn temporal_vertex(@builtin(vertex_index)corner:u32,@builtin(instance_index)instance:u32)->VertexOut{
 var output=temporal_cutout_pose(corner,instance);let previous=temporal_previous[output.slot];let generation=temporal_generations[output.slot];
 output.surface=vec4(0x12000000u,output.slot,0u,generation);output.previous_clip=vec4(0.);output.previous_valid=0u;output.current_clip=output.position;
 if temporal_camera.flags.x!=0u&&generation!=0u&&previous.metadata.x==generation&&previous.metadata.y!=0u{
  output.previous_clip=temporal_camera.previous_vp*(previous.model*vec4(output.local_point,1.));output.previous_valid=1u;
  let current=objects[output.slot];if temporal_camera.flags.z!=0u&&all(current.model[0]==previous.model[0])&&all(current.model[1]==previous.model[1])&&all(current.model[2]==previous.model[2])&&all(current.model[3]==previous.model[3]){output.previous_valid|=2u;}
 }
 return output;
}
@fragment fn temporal_fragment(input:VertexOut)->TemporalOutput{
 let alpha=textureSample(colors,texels,input.uv,i32(materials[input.material].x)).a;if alpha<.5{discard;}
 if alpha<.999{return TemporalOutput(vec4(0.),vec4(0u));}
 let point=vec3<u32>(clamp(floor(input.local_point-input.local_normal*.0001),vec3(0.),vec3(15.)));
 var identity=input.surface;identity.x|=(point.x+16u*(point.z+16u*point.y))*8u+temporal_face(input.local_normal);
 return temporal_known(identity,input.previous_clip,input.current_clip,(input.previous_valid&1u)!=0u,(input.previous_valid&2u)!=0u);
}
"#;

pub fn height() -> Result<String, Error> {
    let source = crate::height_gpu::shader_source(false, false, false);
    let source = replace(
        &source,
        "@builtin(position)position:vec4<f32>",
        "@builtin(position)@invariant position:vec4<f32>",
    )?;
    let source = replace(
        &source,
        "@location(4)@interpolate(flat)tint:vec3<f32>",
        "@location(4)@interpolate(flat)tint:vec3<f32>,@location(5)previous_clip:vec4<f32>,@location(6)@interpolate(flat)surface:vec4<u32>,@location(7)@interpolate(flat)previous_valid:u32,@location(8)current_clip:vec4<f32>",
    )?;
    let source = replace(
        &source,
        "@vertex fn vertex(@builtin(vertex_index)vi:u32,@builtin(instance_index)ii:u32)",
        "fn temporal_height_pose(vi:u32,ii:u32,bucket:u32)",
    )?;
    let source = replace(
        &source,
        "let slot=visible[ii];",
        "let slot=visible[ii+bucket*camera.params.x];",
    )?;
    Ok(format!("{source}\n{COMMON}\n{}", HEIGHT))
}
const HEIGHT: &str = r#"
fn temporal_height_vertex(vi:u32,ii:u32,bucket:u32)->Out{
 var output=temporal_height_pose(vi,ii,bucket);let slot=visible[ii+bucket*camera.params.x];
 let tile=tiles[slot];let previous=temporal_previous[slot];let generation=temporal_generations[slot];
 output.surface=vec4(0x10000000u|(vi/3u),slot,0u,generation);output.previous_clip=vec4(0.);output.previous_valid=0u;output.current_clip=output.position;
 if temporal_camera.flags.x!=0u&&previous.metadata.x==generation&&generation!=0u&&previous.metadata.y!=0u{
  let local=vec3((output.world.x-tile.origin.x)/tile.step,output.world.y-tile.origin.y,(output.world.z-tile.origin.z)/tile.step);
  output.previous_clip=temporal_camera.previous_vp*(previous.model*vec4(local,1.));output.previous_valid=1u;
  if temporal_camera.flags.z!=0u&&all(previous.model[3].xyz==tile.origin)&&previous.model[0].x==tile.step&&previous.model[2].z==tile.step{output.previous_valid|=2u;}
 }
 return output;
}
@vertex fn temporal_vertex(@builtin(vertex_index)vi:u32,@builtin(instance_index)ii:u32)->Out{return temporal_height_vertex(vi,ii,0u);}
@vertex fn temporal_near_vertex(@builtin(vertex_index)vi:u32,@builtin(instance_index)ii:u32)->Out{return temporal_height_vertex(vi,ii,1u);}
@fragment fn temporal_fragment(input:Out)->TemporalOutput{
 if covered_by_near(input.world-normalize(input.normal)*.0001){discard;}
 if !temporal_layer_opaque(input.layer){return TemporalOutput(vec4(0.),vec4(0u));}
 return temporal_known(input.surface,input.previous_clip,input.current_clip,(input.previous_valid&1u)!=0u,(input.previous_valid&2u)!=0u);
}
"#;
