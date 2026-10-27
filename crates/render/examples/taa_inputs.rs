//! CPU-only dump of the exact optional temporal resolver source.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::path::Path::new(".local/taa-oracle");
    std::fs::create_dir_all(directory)?;
    for statistics in [false, true] {
        std::fs::write(
            directory.join(if statistics {
                "resolve-stats.wgsl"
            } else {
                "resolve.wgsl"
            }),
            render::taa::shader_source(statistics),
        )?;
    }
    Ok(())
}
