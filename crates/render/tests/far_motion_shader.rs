#![cfg(not(target_arch = "wasm32"))]
use render::far_motion_shader as shader;
#[test]
fn public_geometry_motion_sources_validate() {
    for source in [
        shader::height().unwrap(),
        shader::proxy_opaque().unwrap(),
        shader::proxy_cutout().unwrap(),
        shader::snapshot(true),
        shader::snapshot(false),
    ] {
        let module = wgpu::naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|error| panic!("{}", error.emit_to_string(&source)));
        wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
    }
}
