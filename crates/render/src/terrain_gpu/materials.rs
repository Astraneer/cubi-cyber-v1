//! Optional local pack variant. Shader preparation belongs to loading; replacements
//! only publish already-uploaded textures and one bind group, never compile.
use super::*;
use crate::material_pack::MaterialPackGpu;

pub(super) struct LightingBindings {
    shadows: wgpu::Buffer,
    depth: wgpu::TextureView,
    compare: wgpu::Sampler,
    lights: wgpu::Buffer,
    gi: Option<(wgpu::Buffer, wgpu::Buffer)>,
}
impl LightingBindings {
    pub(super) fn new(
        shadows: &crate::shadows::ShadowGpu,
        lights: &crate::light_volume::LightVolumeGpu,
        gi: Option<&crate::gi::GiGpu>,
    ) -> Self {
        Self {
            shadows: shadows.sampling_buffer().clone(),
            depth: shadows.depth_view().clone(),
            compare: shadows.sampler().clone(),
            lights: lights.buffer().clone(),
            gi: gi.map(|value| {
                (
                    value.sampling_buffer().clone(),
                    value.probe_buffer().clone(),
                )
            }),
        }
    }
}
pub(super) struct MaterialState {
    pack: MaterialPackGpu,
    control: wgpu::Buffer,
    eye: [f32; 3],
    steps: u32,
    enabled: bool,
}
impl MaterialState {
    fn write(&self, queue: &wgpu::Queue) {
        let words = [
            self.eye[0].to_bits(),
            self.eye[1].to_bits(),
            self.eye[2].to_bits(),
            0,
            u32::from(self.enabled),
            self.steps,
            0,
            0,
        ];
        queue.write_buffer(&self.control, 0, bytemuck::cast_slice(&words));
    }
}
impl TerrainGpu {
    /// Prepare the optional material shader before any frame. An initially
    /// disabled bank permits later OS-picker imports without runtime compilation.
    pub fn prepare_material_pack(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pack: MaterialPackGpu,
        enabled: bool,
    ) -> Result<(), TerrainError> {
        if self.last_render.is_some() {
            return Err(TerrainError::LatePreparation);
        }
        if pack.layers != self.atlas_layers || pack.reserved_metals {
            return Err(TerrainError::InvalidMaterial);
        }
        let control = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("local material camera and bounded POM"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uploaded = pack.upload_bytes;
        self.local_materials = Some(MaterialState {
            pack,
            control,
            eye: [0.0; 3],
            steps: 0,
            enabled,
        });
        self.local_materials.as_ref().unwrap().write(queue);
        self.rebuild_material_pipelines(device);
        self.work.record(telemetry::WorkCounts {
            upload_bytes: uploaded + 32,
            ..telemetry::WorkCounts::ZERO
        });
        Ok(())
    }
    /// Publish a bank during explicit import loading. Existing light/GI bindings
    /// and every prepared pipeline survive this operation.
    pub fn replace_material_pack(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pack: MaterialPackGpu,
        enabled: bool,
    ) -> Result<(), TerrainError> {
        if pack.layers != self.atlas_layers || pack.reserved_metals {
            return Err(TerrainError::InvalidMaterial);
        }
        let state = self
            .local_materials
            .as_mut()
            .ok_or(TerrainError::LatePreparation)?;
        let uploaded = pack.upload_bytes;
        state.pack = pack;
        state.enabled = enabled;
        state.write(queue);
        self.rebuild_material_binding(device);
        self.work.record(telemetry::WorkCounts {
            upload_bytes: uploaded + 32,
            ..telemetry::WorkCounts::ZERO
        });
        self.invalidate();
        Ok(())
    }
    /// POM stays off unless the host selects a C/D near-material profile. The
    /// shader clamps even hostile step budgets to 16 and fades after 4 blocks.
    pub fn set_material_view(
        &mut self,
        queue: &wgpu::Queue,
        eye: [f32; 3],
        pom_steps: u32,
    ) -> Result<u64, TerrainError> {
        if eye.iter().any(|v| !v.is_finite()) {
            return Err(TerrainError::InvalidTransform);
        }
        let Some(state) = self.local_materials.as_mut() else {
            return Ok(0);
        };
        let steps = pom_steps.min(16);
        if state.eye == eye && state.steps == steps {
            return Ok(0);
        }
        state.eye = eye;
        state.steps = steps;
        state.write(queue);
        self.work.record(telemetry::WorkCounts {
            upload_bytes: 32,
            ..telemetry::WorkCounts::ZERO
        });
        self.invalidate();
        Ok(32)
    }
    pub fn material_pack_revision(&self) -> Option<u64> {
        self.local_materials
            .as_ref()
            .filter(|s| s.enabled)
            .map(|s| s.pack.revision)
    }
    fn rebuild_material_binding(&mut self, device: &wgpu::Device) {
        let active = self.local_materials.as_ref().filter(|state| state.enabled);
        let color = active.map_or(&self.colors, |state| &state.pack.albedo);
        let tint = active.map_or(&self.tint_masks, |state| &state.pack.tint);
        let sampler = active.map_or(&self.sampler, |state| &state.pack.sampler);
        let mut entries = vec![
            buffer_binding(0, &self.cameras[0]),
            buffer_binding(1, &self.objects),
            buffer_binding(2, &self.quads),
            buffer_binding(3, &self.visible),
            buffer_binding(4, &self.materials),
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(color),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::TextureView(tint),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            buffer_binding(8, &self.edit_masks),
            buffer_binding(16, &self.edit_neighbors),
        ];
        if let Some(light) = &self.lighting_bindings {
            entries.extend([
                buffer_binding(9, &light.shadows),
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wgpu::BindingResource::TextureView(&light.depth),
                },
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::Sampler(&light.compare),
                },
                buffer_binding(12, &light.lights),
            ]);
            if let Some((sampling, probes)) = &light.gi {
                entries.extend([buffer_binding(13, sampling), buffer_binding(14, probes)]);
            }
        }
        if let Some(state) = &self.local_materials {
            entries.extend([
                wgpu::BindGroupEntry {
                    binding: 17,
                    resource: wgpu::BindingResource::TextureView(&state.pack.normal),
                },
                wgpu::BindGroupEntry {
                    binding: 18,
                    resource: wgpu::BindingResource::TextureView(&state.pack.specular),
                },
                buffer_binding(19, &state.control),
            ]);
        }
        if let Some(lighting) = &self.atmosphere_lighting {
            entries.push(buffer_binding(20, lighting));
        }
        self.render_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("preserved terrain light GI and optional local material bindings"),
            layout: &self.render_layout,
            entries: &entries,
        });
    }
    pub(super) fn rebuild_material_pipelines(&mut self, device: &wgpu::Device) {
        let lit = self.lighting_bindings.is_some();
        let gi = self
            .lighting_bindings
            .as_ref()
            .is_some_and(|light| light.gi.is_some());
        self.render_layout = terrain_render_layout(
            device,
            lit,
            gi,
            self.local_materials.is_some(),
            self.atmosphere_lighting.is_some(),
        );
        self.rebuild_material_binding(device);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("prepared terrain lighting/material variant"),
            bind_group_layouts: &[&self.render_layout],
            push_constant_ranges: &[],
        });
        let atmosphere = self.atmosphere_lighting.is_some();
        let source = if atmosphere {
            environment_terrain_shader(lit, gi)
        } else if lit {
            lit_terrain_shader(gi)
        } else {
            include_str!("../terrain.wgsl").to_owned()
        };
        let source = if self.local_materials.is_some() {
            shader(&source, lit, gi, atmosphere)
        } else {
            source
        };
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("prepared terrain optional material variant"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        self.opaque_pipeline = terrain_pipeline(
            device,
            &layout,
            &shader,
            false,
            false,
            false,
            self.sample_count,
        );
        self.cutout_pipeline = terrain_pipeline(
            device,
            &layout,
            &shader,
            true,
            false,
            false,
            self.sample_count,
        );
        self.masked_opaque_pipeline = terrain_pipeline(
            device,
            &layout,
            &shader,
            false,
            false,
            true,
            self.sample_count,
        );
        self.masked_cutout_pipeline = terrain_pipeline(
            device,
            &layout,
            &shader,
            true,
            false,
            true,
            self.sample_count,
        );
        if self.config.occlusion {
            self.depth_pipeline = Some(terrain_pipeline(
                device,
                &layout,
                &shader,
                false,
                true,
                false,
                self.sample_count,
            ));
            self.masked_depth_pipeline = Some(terrain_pipeline(
                device,
                &layout,
                &shader,
                true,
                true,
                true,
                self.sample_count,
            ));
        }
        self.work.record(telemetry::WorkCounts {
            pipeline_compilations: if self.config.occlusion { 6 } else { 4 },
            ..telemetry::WorkCounts::ZERO
        });
        self.invalidate();
    }
}

/// Loading/tooling source for independent backend audits. GI implies lighting.
pub fn material_shader_source(lighting: bool, gi: bool) -> String {
    let lighting = lighting || gi;
    let base = if lighting {
        lit_terrain_shader(gi)
    } else {
        include_str!("../terrain.wgsl").to_owned()
    };
    shader(&base, lighting, gi, false)
}
pub fn atmosphere_material_shader_source(lighting: bool, gi: bool) -> String {
    shader(
        &environment_terrain_shader(lighting || gi, gi),
        lighting || gi,
        gi,
        true,
    )
}
fn shader(source: &str, lit: bool, gi: bool, atmosphere: bool) -> String {
    let source = source.replace("fn shade(", "fn legacy_shade(")
        .replace("@location(11) world_position:vec3<f32>,", "@location(11) world_position:vec3<f32>,\n@location(12) tangent_u:vec3<f32>,\n@location(13) tangent_v:vec3<f32>,")
        .replace("input.world_position);", "input.world_position,input.tangent_u,input.tangent_v);")
        .replace("    output.uv = texture_uv;", "    output.uv = texture_uv;\nvar pbr_u=tangent_u[face];var pbr_v=tangent_v[face];switch rotation {case 1u:{pbr_u=tangent_v[face];pbr_v=-tangent_u[face];}case 2u:{pbr_u=-tangent_u[face];pbr_v=-tangent_v[face];}case 3u:{pbr_u=-tangent_v[face];pbr_v=tangent_u[face];}default:{}}\noutput.tangent_u=normalize((object.model*vec4(pbr_u,0.0)).xyz);output.tangent_v=normalize((object.model*vec4(pbr_v,0.0)).xyz);");
    let direct = if atmosphere && lit {
        "let light=sample_page_light(slot,local_position,local_normal);let sky=light.a;let sun=normalize(atmosphere_light.direction.xyz);let visibility=cascade_shadow(world_position,normal);let indirect=light.rgb+sky*atmosphere_diffuse(normal);"
    } else if atmosphere {
        "let sky=1.0;let sun=normalize(atmosphere_light.direction.xyz);let visibility=1.0;let indirect=atmosphere_diffuse(normal);"
    } else if lit {
        "let light=sample_page_light(slot,local_position,local_normal);let sky=light.a;let sun=normalize(shadow_sampling.sun.xyz);let visibility=cascade_shadow(world_position,normal);let indirect=light.rgb+vec3(sky*0.15);"
    } else {
        "let sky=1.0;let sun=normalize(vec3(0.35,0.8,0.2));let visibility=1.0;let indirect=vec3(0.65);"
    };
    let gi = if gi {
        "+reflected_irradiance(world_position,normal)*0.3183098862"
    } else {
        ""
    };
    let solar = if atmosphere {
        "atmosphere_light.sun.rgb*(sky*visibility)"
    } else {
        "vec3(0.85*3.14159265359*sky*visibility)"
    };
    format!(
        "{source}\n{}\n{}\nfn pack_light(albedo:vec3<f32>,pbr:LabPbrSample,ao:f32,normal:vec3<f32>,view:vec3<f32>,slot:u32,local_position:vec3<f32>,local_normal:vec3<f32>,world_position:vec3<f32>)->vec3<f32>{{{direct}\nlet ambient=indirect{gi};return material_brdf(albedo,pbr,normal,view,sun)*{solar}+albedo*(1.0-material_f0(albedo,pbr))*select(1.0,0.0,pbr.metal_id>=230u)*ambient*ao+albedo*pbr.emission;}}\n",
        crate::material_pack::WGSL,
        include_str!("../terrain_material.wgsl")
    )
}
