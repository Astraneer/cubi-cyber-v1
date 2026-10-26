//! Exact production loading variants for native/Dawn validation, not a second renderer.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .ok_or("output directory required")?;
    std::fs::create_dir_all(&path)?;
    for shadows in [false, true] {
        for atmosphere in [false, true] {
            for gi in [false, true] {
                if gi && !atmosphere {
                    continue;
                }
                for mrt in [false, true] {
                    let name = format!(
                        "board-{}{}{}{}.wgsl",
                        u8::from(shadows),
                        u8::from(atmosphere),
                        u8::from(gi),
                        u8::from(mrt)
                    );
                    std::fs::write(
                        path.join(name),
                        render::recipe_boards::shader_source(shadows, atmosphere, gi, mrt),
                    )?;
                }
            }
        }
    }
    Ok(())
}
