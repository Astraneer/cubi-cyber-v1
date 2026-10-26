//! Loading-only optional model receiver. The light field remains owned by GI.
use crate::gi::GiGpu;
pub(crate) fn layout(entries: &mut Vec<wgpu::BindGroupLayoutEntry>) {
    for (binding, uniform) in [(13, true), (14, false), (15, false)] {
        entries.push(wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: if uniform {
                    wgpu::BufferBindingType::Uniform
                } else {
                    wgpu::BufferBindingType::Storage { read_only: true }
                },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        });
    }
}
pub(crate) fn bindings<'a>(entries: &mut Vec<wgpu::BindGroupEntry<'a>>, gi: &'a GiGpu) {
    for (binding, buffer) in [
        (13, gi.sampling_buffer()),
        (14, gi.probe_buffer()),
        (15, gi.voxel_buffer()),
    ] {
        entries.push(wgpu::BindGroupEntry {
            binding,
            resource: buffer.as_entire_binding(),
        });
    }
}
pub(crate) fn shader(source: String) -> String {
    let illumination = "return light.rgb+light.a*(";
    assert!(
        source.contains(illumination),
        "model GI requires the shared physical atmospheric receiver"
    );
    format!(
        "{}\n{}\n{}",
        source.replacen(
            illumination,
            "return model_reflected_irradiance(world,normal)*0.3183098862+light.rgb+light.a*(",
            1
        ),
        include_str!("gi_sampling.wgsl"),
        include_str!("gi_model_sampling.wgsl")
    )
}
