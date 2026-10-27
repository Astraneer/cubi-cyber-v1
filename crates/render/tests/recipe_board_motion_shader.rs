#[test]
fn motion19_uses_identical_primary_card_pose_and_portable_three_vertex_storage() {
    let source = render::recipe_board_motion::shader_source().unwrap();
    let module = wgpu::naga::front::wgsl::parse_str(&source)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
    wgpu::naga::valid::Validator::new(
        wgpu::naga::valid::ValidationFlags::all(),
        wgpu::naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap();
    assert_eq!(
        module
            .global_variables
            .iter()
            .filter(|(_, v)| matches!(v.space, wgpu::naga::AddressSpace::Storage { .. }))
            .count(),
        3
    );
    assert_eq!(
        render::recipe_board_motion::RecipeBoardMotionGpu::required_buffer_bytes(9).unwrap(),
        960
    );
    assert!(render::recipe_board_motion::RecipeBoardMotionGpu::required_buffer_bytes(0).is_err());
    assert!(source.contains("out.position=current.position"));
    assert!(source.contains("0x13000000u|part"));
}
