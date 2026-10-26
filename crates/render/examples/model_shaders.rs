//! Dump the exact prepared sources for local Dawn/HLSL compatibility oracles.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::Path::new(".local/model-shaders");
    std::fs::create_dir_all(root)?;
    for gi in [false, true] {
        std::fs::write(
            root.join(if gi {
                "entity-gi.wgsl"
            } else {
                "entity-no-gi.wgsl"
            }),
            render::entity_gpu::shader_source(false, true, gi)?,
        )?;
        std::fs::write(
            root.join(if gi {
                "block-gi.wgsl"
            } else {
                "block-no-gi.wgsl"
            }),
            render::block_models::shader_source(false, true, gi)?,
        )?;
    }
    for gi in [false, true] {
        let mode = if gi { "gi" } else { "no-gi" };
        std::fs::write(
            root.join(format!("entity-{mode}-mrt.wgsl")),
            render::entity_gpu::shader_source_with_ambient(false, true, gi, true)?,
        )?;
        std::fs::write(
            root.join(format!("block-{mode}-mrt.wgsl")),
            render::block_models::shader_source_with_ambient(false, true, gi, true)?,
        )?;
    }
    Ok(())
}
