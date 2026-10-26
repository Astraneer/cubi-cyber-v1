#[test]
fn prepared_world_cards_validate_all_lighting_and_mrt_combinations() {
    for shadows in [false, true] {
        for atmosphere in [false, true] {
            for gi in [false, true] {
                if gi && !atmosphere {
                    continue;
                }
                for mrt in [false, true] {
                    let source = render::recipe_boards::shader_source(shadows, atmosphere, gi, mrt);
                    let module =
                        wgpu::naga::front::wgsl::parse_str(&source).unwrap_or_else(|error| {
                            panic!(
                                "{shadows}/{atmosphere}/{gi}/{mrt}: {}",
                                error.emit_to_string(&source)
                            )
                        });
                    wgpu::naga::valid::Validator::new(
                        wgpu::naga::valid::ValidationFlags::all(),
                        wgpu::naga::valid::Capabilities::all(),
                    )
                    .validate(&module)
                    .unwrap_or_else(|error| panic!("{shadows}/{atmosphere}/{gi}/{mrt}: {error:?}"));
                    let storage = module
                        .global_variables
                        .iter()
                        .filter(|(_, variable)| {
                            matches!(variable.space, wgpu::naga::AddressSpace::Storage { .. })
                        })
                        .count();
                    assert!(
                        storage <= 4,
                        "one vertex + at most three fragment storage buffers"
                    );
                }
            }
        }
    }
}
