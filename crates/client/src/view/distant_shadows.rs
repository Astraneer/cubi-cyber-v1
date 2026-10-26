//! Loading/guard bridge for public far directional casters. No World read, scan,
//! generation, visibility decoding or GPU upload occurs in the frame hook.
use super::horizon::ViewHorizon;
use render::{
    distant_shadows::{DistantShadowGpu, HeightShadowPolicy},
    shadows::ShadowGpu,
};
use rules::blocks::{Render, TexRule};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub enabled: bool,
    pub source_changes: u64,
    pub startup_pipelines: u32,
    pub buffer_bytes: u64,
}
pub struct ViewDistantShadows {
    pub gpu: DistantShadowGpu,
    stats: Stats,
}
impl ViewDistantShadows {
    pub fn prepare(
        device: &wgpu::Device,
        horizon: &ViewHorizon,
        shadows: &ShadowGpu,
    ) -> Result<Self, render::distant_shadows::Error> {
        let (height, proxy) = horizon.prepared_shadow_sources();
        let gpu = DistantShadowGpu::new(device, height, proxy, shadows, height_policy()?)?;
        let stats = Stats {
            startup_pipelines: gpu.startup_pipeline_compilations(),
            buffer_bytes: gpu.buffer_bytes(),
            ..Default::default()
        };
        Ok(Self { gpu, stats })
    }
    /// The exact front/near/water gate is supplied by its existing owner. A
    /// rejected resource replacement revokes this caster before returning.
    pub fn synchronize(
        &mut self,
        horizon: &ViewHorizon,
    ) -> Result<bool, render::distant_shadows::Error> {
        let old = self.gpu.source_key();
        if let Some((height, proxy)) = horizon.committed_shadow_sources() {
            if let Err(error) = self.gpu.synchronize_sources(height, proxy) {
                self.gpu.set_enabled(false);
                self.stats.enabled = false;
                return Err(error);
            }
            self.gpu.set_enabled(true);
            self.stats.enabled = true;
        } else {
            self.gpu.set_enabled(false);
            self.stats.enabled = false;
        }
        let changed = old != self.gpu.source_key();
        if changed {
            self.stats.source_changes = self.stats.source_changes.saturating_add(1);
        }
        Ok(changed)
    }
    pub fn stats(&self) -> Stats {
        self.stats
    }
}
/// Layer semantics come from the canonical typed registry. All state-dependent
/// texture rules are covered, and any alpha/water alias vetoes an opaque layer.
/// Model/None placeholder tex fields are not terrain texture consumers.
pub fn height_policy() -> Result<HeightShadowPolicy, render::distant_shadows::Error> {
    let mut opaque = [false; 256];
    let mut veto = [false; 256];
    for (id, def) in rules::all_blocks() {
        let full = def.render == Render::Cube && def.fluid == 0;
        let alpha = matches!(
            def.render,
            Render::Leaves | Render::Glass | Render::Cross | Render::Water
        ) || def.fluid != 0;
        if !full && !alpha {
            continue;
        }
        let states = match def.tex_rule {
            TexRule::Fixed => 1,
            TexRule::Axis => 4,
            TexRule::Facing { .. } => 16,
            TexRule::Crop { .. } | TexRule::Farmland => 8,
        };
        for state in 0..states {
            for face in 0..6 {
                let layer = rules::blocks::layer_cell(id, state, face).0 as usize;
                if layer >= 256 {
                    return Err(render::distant_shadows::Error::InvalidConfig);
                }
                opaque[layer] |= full;
                veto[layer] |= alpha;
            }
        }
    }
    HeightShadowPolicy::from_opaque_layers(
        (0..256u16).filter(|&i| opaque[usize::from(i)] && !veto[usize::from(i)]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_material_policy_casts_ground_but_never_water_or_cutouts() {
        let policy = height_policy().unwrap();
        for id in [rules::ids::STONE, rules::ids::DIRT, rules::ids::GRASS] {
            for face in 0..6 {
                assert!(policy.casts(rules::blocks::layer_cell(id, 0, face).0 as u16));
            }
        }
        for (id, def) in rules::all_blocks().filter(|(_, d)| {
            matches!(
                d.render,
                Render::Leaves | Render::Glass | Render::Cross | Render::Water
            )
        }) {
            for state in 0..16 {
                for face in 0..6 {
                    assert!(
                        !policy.casts(rules::blocks::layer_cell(id, state, face).0 as u16),
                        "alpha layer of {}",
                        def.mc
                    );
                }
            }
        }
        assert!(!policy.casts(255));
        assert!(!policy.casts(256));
    }
}
