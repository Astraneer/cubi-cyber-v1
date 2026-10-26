//! Print only this project's shader for an explicit Naga/HLSL backend audit.
fn main() {
    print!(
        "{}",
        render::terrain_gpu::material_shader_source(true, true)
    );
}
