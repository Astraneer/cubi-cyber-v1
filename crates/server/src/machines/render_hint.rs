//! Public model state only. The lookup visits one indexed component; item,
//! reservoir and immutable payload state never enter this projection.
use super::*;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderHint {
    /// World Face6 order, three bits per side (None/Input/Output/Both/Energy/Extra).
    pub modes: u32,
    pub active: bool,
    /// Radians at the supplied epoch and radians per 20 Hz simulation tick.
    pub rotor_phase: f32,
    pub rotor_velocity: f32,
}
impl MachineRuntime {
    pub fn render_hint(
        &self,
        world: &World,
        anchor: BlockPos,
        _now: Tick,
    ) -> Result<Option<RenderHint>, MachineError> {
        let Some(&id) = self.positions.get(&anchor) else {
            return Ok(None);
        };
        let meta = self.machine_meta(id).ok_or(MachineError::Missing)?;
        let machine = read_machine(world, id)?;
        let CellRead::Known(cell) = world.read(anchor) else {
            return Err(MachineError::Unknown);
        };
        let facing = Face::ALL[blocks::facing_of(cell.raw()) as usize];
        let modes = Face::ALL.into_iter().fold(0, |bits, face| {
            bits | (machine.sides[sides::relative(facing, face) as usize] as u32)
                << (3 * face as u32)
        });
        let active = meta.active && machine.active.shown;
        // Authored visual period: one turn per eighty active wind ticks. This
        // does not feed energy production or any other gameplay calculation.
        let speed = std::f32::consts::TAU / 80.0;
        Ok(Some(RenderHint {
            modes,
            active,
            rotor_phase: if machine.kind == MekKind::Wind {
                (machine.ticker % 80) as f32 * speed
            } else {
                0.0
            },
            rotor_velocity: if machine.kind == MekKind::Wind && active {
                speed
            } else {
                0.0
            },
        }))
    }
}
