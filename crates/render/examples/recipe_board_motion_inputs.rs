fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .ok_or("output directory required")?;
    std::fs::create_dir_all(&path)?;
    std::fs::write(
        path.join("color.wgsl"),
        render::recipe_boards::shader_source(false, false, false, false),
    )?;
    std::fs::write(
        path.join("motion.wgsl"),
        render::recipe_board_motion::shader_source()?,
    )?;
    Ok(())
}
