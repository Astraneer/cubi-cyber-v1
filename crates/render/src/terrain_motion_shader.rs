//! Exact primary terrain raster plus a separate optional motion entry. No
//! camera-only approximation is used for a moved or rebased object matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error;

pub fn shader_source() -> Result<String, Error> {
    let source = include_str!("terrain.wgsl");
    let entry = "@vertex\nfn vertex(@builtin(vertex_index) vertex_id: u32, @builtin(instance_index) instance: u32)";
    let extended = "    @location(11) world_position:vec3<f32>,";
    if !source.contains(entry) || !source.contains(extended) {
        return Err(Error);
    }
    let source = source
        .replacen(entry, "fn temporal_terrain_pose(vertex_id:u32,instance:u32)", 1)
        .replacen(
            extended,
            "    @location(11) world_position:vec3<f32>,\n    @location(12) temporal_previous_clip:vec4<f32>,\n    @location(13) @interpolate(flat) temporal_identity:vec4<u32>,\n    @location(14) temporal_current_clip:vec4<f32>,\n    @location(15) @interpolate(flat) temporal_static:u32,",
            1,
        );
    Ok(format!("{source}\n{}", include_str!("terrain_motion.wgsl")))
}

/// The snapshot is encoded into the candidate bank. Only presentation switches
/// banks, so neither a skipped UI frame nor an abandoned GPU result advances it.
pub const SNAPSHOT_SHADER: &str = r#"
struct Object{model:mat4x4<f32>,minimum:vec4<f32>,maximum:vec4<f32>,ranges:array<vec2<u32>,3>,generation:u32,enabled:u32};
@group(0)@binding(0)var<storage,read>current:array<Object>;
@group(0)@binding(1)var<storage,read_write>candidate:array<Object>;
@compute @workgroup_size(64)fn snapshot(@builtin(global_invocation_id)i:vec3<u32>){if i.x<arrayLength(&current)&&i.x<arrayLength(&candidate){candidate[i.x]=current[i.x];}}
"#;
