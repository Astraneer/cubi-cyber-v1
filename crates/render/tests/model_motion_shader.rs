#![cfg(not(target_arch = "wasm32"))]
use render::model_motion_shader;
#[test]
fn exact_model_pose_temporal_variants_validate_with_all_current_lighting_and_ao_layouts() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut count = 0;
    for shadows in [false, true] {
        for atmosphere in [false, true] {
            for gi in [false, true] {
                if gi && !atmosphere {
                    continue;
                }
                for ambient in [false, true] {
                    let entity = render::entity_gpu::shader_source_with_ambient(
                        shadows, atmosphere, gi, ambient,
                    )
                    .unwrap();
                    let block = render::block_models::shader_source_with_ambient(
                        shadows, atmosphere, gi, ambient,
                    )
                    .unwrap();
                    for source in [
                        model_motion_shader::entity_color(&entity).unwrap(),
                        model_motion_shader::entity_motion(&entity).unwrap(),
                        model_motion_shader::block_color(&block).unwrap(),
                        model_motion_shader::block_motion(&block).unwrap(),
                    ] {
                        let _ = gpu
                            .device
                            .create_shader_module(wgpu::ShaderModuleDescriptor {
                                label: Some("actual model temporal source validation"),
                                source: wgpu::ShaderSource::Wgsl(source.into()),
                            });
                        count += 1;
                    }
                }
            }
        }
    }
    assert_eq!(count, 48);
    assert!(pollster::block_on(gpu.device.pop_error_scope()).is_none());
    assert!(render::entity_models::ModelCatalog::new().vertices.len() / 3 < 1 << 24);
    assert!(render::block_models::BlockCatalog::new().vertices.len() / 3 < 1 << 24);
    assert!(model_motion_shader::entity_motion("invalid source").is_err());
    assert!(model_motion_shader::block_motion("invalid source").is_err());
}
