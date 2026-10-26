//! Retained public pipe-item render records. Current raw-view certification is
//! checked immediately before conversion, and light comes from the confirmed
//! client source; no world scans, cell generation or model vertices occur.
use crate::{
    entities::{EntityCopyWork, EntityFrame, EntityFrameError, ItemVisuals},
    network::machine_packets::PacketScene,
};
use core_types::{LocalIdx, PageId, SpaceId, Tick, Version};
use render::{entity_gpu::EntityInstance, light_volume::PackedLight};
use rules::entities::mob::Instance;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub public_rows: u64,
    pub withheld: u64,
    pub instances: u64,
    pub light_reads: u64,
    pub unknown_light_reads: u64,
    pub cpu_vertices: u64,
}
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub space: SpaceId,
    pub tick: Tick,
    pub alpha: f64,
    pub origin: [i64; 3],
}
pub struct PacketGpuSource {
    source: Vec<Instance>,
    frame: EntityFrame,
}
impl PacketGpuSource {
    pub fn new(maximum: usize) -> Result<Self, EntityFrameError> {
        if maximum == 0 || maximum > 2048 {
            return Err(EntityFrameError::Capacity);
        }
        Ok(Self {
            source: vec![Instance::default(); maximum],
            frame: EntityFrame::new(maximum),
        })
    }
    pub fn instances(&self) -> &[EntityInstance] {
        self.frame.instances()
    }
    pub fn lights(&self) -> &[PackedLight] {
        self.frame.lights()
    }
    pub fn prepare(
        &mut self,
        scene: &PacketScene,
        view: View,
        certified: &impl Fn(PageId, Version, LocalIdx) -> bool,
        items: &impl ItemVisuals,
        light_at: impl FnMut(core_types::BlockPos) -> Option<world::LightSample>,
    ) -> Result<Work, EntityFrameError> {
        let View {
            space,
            tick,
            alpha,
            origin,
        } = view;
        let count = scene
            .instances_filtered(
                tick,
                alpha,
                origin,
                &|page, view, index| page.space == space && certified(page, view, index),
                &mut self.source,
            )
            .map_err(|error| {
                if error == crate::network::machine_packets::Error::Time {
                    EntityFrameError::InvalidPosition
                } else {
                    EntityFrameError::Capacity
                }
            })?;
        let EntityCopyWork {
            instances,
            light_reads,
            unknown_light_reads,
            cpu_vertices,
        } = self
            .frame
            .prepare_sampled(&self.source[..count], space, origin, items, light_at)?;
        Ok(Work {
            public_rows: scene.len() as u64,
            withheld: scene.len().saturating_sub(count) as u64,
            instances,
            light_reads,
            unknown_light_reads,
            cpu_vertices,
        })
    }
}
