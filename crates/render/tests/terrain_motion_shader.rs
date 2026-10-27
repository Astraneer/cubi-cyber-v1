#![cfg(not(target_arch = "wasm32"))]
use render::terrain_motion_shader as source;

#[test]
fn exact_primary_terrain_and_candidate_snapshot_validate() {
    for shader in [
        source::shader_source().unwrap(),
        source::SNAPSHOT_SHADER.into(),
    ] {
        let module = wgpu::naga::front::wgsl::parse_str(&shader)
            .unwrap_or_else(|error| panic!("{}", error.emit_to_string(&shader)));
        wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
    }
}
