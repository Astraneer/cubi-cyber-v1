//! Pure public block-model projection over indexed anchors. The owner supplies
//! public view identity and chest-lid animation; no private component payload is
//! encoded and no page cells are scanned to discover anchors.
use crate::machines::MachineRuntime;
use core_types::{BlockPos, Cell, ComponentId, LocalIdx, PageId, Tick, Version};
use net::props::{Frame, Hint, Kind, MAX_HINTS};
use rules::blocks::{self, BeKind, MekKind};
use world::World;
#[derive(Clone, Copy, Debug)]
pub struct Stamp {
    pub page: PageId,
    pub cell_view: u64,
    pub view: u64,
    pub source: Version,
    pub tick: Tick,
}
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub anchors: usize,
    pub hints: usize,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            anchors: 4096,
            hints: 4096,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub component_anchors: u64,
    pub transmitter_anchors: u64,
    pub cell_reads: u64,
    pub machine_decodes: u64,
    pub stale: u64,
    pub withheld: u64,
    pub hints: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Bounds,
    Unknown,
    Component,
    Hint,
}
fn position(page: PageId, index: core_types::LocalIdx) -> Result<BlockPos, Error> {
    let (x, y, z) = index.xyz();
    Ok(BlockPos {
        space: page.space,
        x: page
            .x
            .checked_mul(16)
            .and_then(|p| p.checked_add(i32::from(x)))
            .ok_or(Error::Bounds)?,
        y: page
            .y
            .checked_mul(16)
            .and_then(|p| p.checked_add(i32::from(y)))
            .ok_or(Error::Bounds)?,
        z: page
            .z
            .checked_mul(16)
            .and_then(|p| p.checked_add(i32::from(z)))
            .ok_or(Error::Bounds)?,
    })
}
pub fn build(
    world: &World,
    machines: &MachineRuntime,
    stamp: Stamp,
    public_cell: &impl Fn(LocalIdx, Cell) -> bool,
    chest_lid: &impl Fn(ComponentId) -> f32,
    budget: Budget,
) -> Result<(Frame, Work), Error> {
    if budget.anchors > 4096
        || budget.hints > MAX_HINTS
        || stamp.cell_view == 0
        || stamp.view == 0
        || stamp.source.0 == 0
    {
        return Err(Error::Bounds);
    }
    let page = world.page(stamp.page).ok_or(Error::Unknown)?;
    let mut work = Work::default();
    let mut hints = Vec::new();
    for (index, id) in page.component_links() {
        if work.component_anchors + work.transmitter_anchors >= budget.anchors as u64 {
            return Err(Error::Bounds);
        }
        work.component_anchors += 1;
        work.cell_reads += 1;
        let cell = page.cell(index);
        if !public_cell(index, cell) {
            work.withheld += 1;
            continue;
        }
        let anchor = position(stamp.page, index)?;
        let record = world.component(id).ok_or(Error::Component)?;
        if record.data.anchor != anchor {
            return Err(Error::Component);
        }
        let hint = match (blocks::bdef(cell.id()).be, record.data.kind) {
            (Some(BeKind::Chest), crate::survival::CHEST_COMPONENT) => Some(
                Hint::from_visual(index, Kind::Chest, None, false, Some(chest_lid(id)), None)
                    .map_err(|_| Error::Hint)?,
            ),
            (Some(BeKind::Mek(kind)), crate::machines::MACHINE_COMPONENT) => {
                work.machine_decodes += 1;
                work.cell_reads += 1;
                if let Some(visual) = machines
                    .render_hint(world, anchor, stamp.tick)
                    .map_err(|_| Error::Component)?
                {
                    let wind = kind == MekKind::Wind;
                    Some(
                        Hint::from_visual(
                            index,
                            if wind { Kind::Wind } else { Kind::Machine },
                            Some(visual.modes),
                            visual.active,
                            None,
                            wind.then_some((visual.rotor_phase, visual.rotor_velocity)),
                        )
                        .map_err(|_| Error::Hint)?,
                    )
                } else {
                    work.stale += 1;
                    None
                }
            }
            // Furnaces and dropped-item bundles have no public model override.
            _ => None,
        };
        if let Some(hint) = hint {
            if hints.len() == budget.hints {
                return Err(Error::Bounds);
            }
            hints.push(hint);
        }
    }
    for config in machines.transmitter_configs_in_page(stamp.page) {
        if work.component_anchors + work.transmitter_anchors >= budget.anchors as u64 {
            return Err(Error::Bounds);
        }
        work.transmitter_anchors += 1;
        work.cell_reads += 1;
        let cell = page.cell(config.position.local_index());
        if !public_cell(config.position.local_index(), cell) {
            work.withheld += 1;
            continue;
        }
        if cell.id() != config.block {
            work.stale += 1;
            continue;
        }
        if hints.len() == budget.hints {
            return Err(Error::Bounds);
        }
        hints.push(
            Hint::from_visual(
                config.position.local_index(),
                Kind::Connector,
                Some(u32::from(config.modes) | (u32::from(config.color) << 12)),
                false,
                None,
                None,
            )
            .map_err(|_| Error::Hint)?,
        );
    }
    hints.sort_unstable_by_key(|hint| hint.index);
    work.hints = hints.len() as u64;
    let frame = Frame {
        page: stamp.page,
        cell_view: stamp.cell_view,
        view: stamp.view,
        source: stamp.source,
        tick: stamp.tick,
        hints,
    };
    frame.validate().map_err(|_| Error::Hint)?;
    Ok((frame, work))
}
