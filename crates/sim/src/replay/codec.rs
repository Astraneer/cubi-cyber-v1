use super::*;
use std::cell::Cell as Flag;

/// Validated, self-contained public movement archive. Offline allocation is
/// allowed; runtime recording uses Recorder's preallocated storage instead.
pub struct Archive {
    steps: Vec<Step>,
    complete: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayResult {
    pub steps: usize,
    pub checkpoints: usize,
    pub final_hash: u64,
    pub complete_window: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayFailure {
    pub sequence: u64,
    pub reason: &'static str,
    pub expected_hash: u64,
    pub actual_hash: u64,
}
impl Archive {
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_ARCHIVE_BYTES {
            return Err("movement archive exceeds bound");
        }
        let mut input = Reader { bytes, at: 0 };
        if input.take(6)? != b"CUBR\x01\0" {
            return Err("invalid movement archive version");
        }
        let count = usize::from(input.u16()?);
        let complete = input.boolean()?;
        if count == 0 || count > MAX_STEPS {
            return Err("invalid movement step count");
        }
        let mut steps = Vec::with_capacity(count);
        let mut previous: Option<(u64, u64, PredictionState)> = None;
        for _ in 0..count {
            let mut step = Step {
                sequence: input.u64()?,
                time_ns: input.u64()?,
                external: input.boolean()?,
                keys: input.byte()?,
                using: input.boolean()?,
                ..Step::default()
            };
            if step.keys & 128 != 0 || step.sequence == 0 {
                return Err("invalid movement input");
            }
            let before = input.state()?;
            let after = input.state()?;
            step.before = Some(before);
            step.after = Some(after);
            step.outcome = TickOutcome {
                waiting_for_world: input.boolean()?,
                unknown_boundary: input.boolean()?,
                fall_damage: input.u32()?,
                moved: input.boolean()?,
            };
            step.cell_count = input.u16()?;
            step.flow_count = input.byte()?;
            if usize::from(step.cell_count) > MAX_CELLS || usize::from(step.flow_count) > MAX_FLOWS
            {
                return Err("movement witnesses exceed bound");
            }
            if let Some((sequence, time, last_after)) = previous {
                if step.sequence
                    != sequence
                        .checked_add(1)
                        .ok_or("movement sequence overflow")?
                    || step.time_ns < time
                {
                    return Err("out-of-order movement steps");
                }
                if !step.external && before != last_after {
                    return Err("unannounced movement checkpoint");
                }
            }
            for i in 0..usize::from(step.cell_count) {
                let position = input.position()?;
                let value = if input.boolean()? {
                    CellRead::Known(Cell::from_raw(input.u32()?))
                } else {
                    CellRead::Unknown
                };
                if step.cells[..i].iter().any(|old| old.position == position) {
                    return Err("duplicate movement cell");
                }
                step.cells[i] = CellWitness { position, value };
            }
            for i in 0..usize::from(step.flow_count) {
                let position = input.position()?;
                let value = if input.boolean()? {
                    Some([
                        f64::from_bits(input.u64()?),
                        f64::from_bits(input.u64()?),
                        f64::from_bits(input.u64()?),
                    ])
                } else {
                    None
                };
                if step.flows[..i].iter().any(|old| old.position == position) {
                    return Err("duplicate movement flow");
                }
                step.flows[i] = FlowWitness { position, value };
            }
            previous = Some((step.sequence, step.time_ns, after));
            steps.push(step);
        }
        if input.at != bytes.len() {
            return Err("trailing movement archive bytes");
        }
        Ok(Self { steps, complete })
    }
    pub fn from_report(json: &str) -> Result<Self, &'static str> {
        if json.len() > MAX_ARCHIVE_BYTES * 2 + 16 * 1024 * 1024 {
            return Err("movement report exceeds bound");
        }
        let (_, tail) = json
            .split_once("\"movementReplay\":{")
            .ok_or("report has no movement replay")?;
        let (_, tail) = tail
            .split_once("\"dataHex\":\"")
            .ok_or("report has no movement bytes")?;
        let hex = tail
            .split_once('"')
            .ok_or("unterminated movement bytes")?
            .0
            .as_bytes();
        if hex.len() > MAX_ARCHIVE_BYTES * 2 || hex.len() % 2 != 0 {
            return Err("invalid movement hex length");
        }
        let nibble = |b| match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            _ => Err("invalid movement hex"),
        };
        let mut bytes = Vec::with_capacity(hex.len() / 2);
        for pair in hex.chunks_exact(2) {
            bytes.push((nibble(pair[0])? << 4) | nibble(pair[1])?);
        }
        Self::decode(&bytes)
    }
    /// Runs the real current movement engine. No fallback world/Unknown answers
    /// are accepted for queries absent from the capture. State/outcome must be
    /// bitwise equal after every tick, before any later authority checkpoint.
    pub fn replay(&self) -> Result<ReplayResult, ReplayFailure> {
        let first = &self.steps[0];
        let mut player = first
            .before
            .expect("validated before")
            .to_player()
            .map_err(|reason| failure(first, reason, 0))?;
        let mut checkpoints = 1;
        for (index, step) in self.steps.iter().enumerate() {
            if index != 0 && step.external {
                step.before
                    .expect("validated before")
                    .apply_authoritative(&mut player)
                    .map_err(|reason| failure(step, reason, 0))?;
                checkpoints += 1;
            }
            let before =
                PredictionState::capture(&player).map_err(|reason| failure(step, reason, 0))?;
            if Some(before) != step.before {
                return Err(failure(
                    step,
                    "movement initial continuation diverged",
                    before.hash(),
                ));
            }
            let query = Playback {
                step,
                missing: Flag::new(false),
            };
            let outcome = player.tick_using(&query, decode_keys(step.keys), step.using);
            let after =
                PredictionState::capture(&player).map_err(|reason| failure(step, reason, 0))?;
            if query.missing.get() {
                return Err(failure(
                    step,
                    "movement queried uncaptured world data",
                    after.hash(),
                ));
            }
            if outcome != step.outcome || Some(after) != step.after {
                return Err(failure(
                    step,
                    "movement continuation diverged",
                    after.hash(),
                ));
            }
        }
        Ok(ReplayResult {
            steps: self.steps.len(),
            checkpoints,
            final_hash: self.steps.last().unwrap().after.unwrap().hash(),
            complete_window: self.complete,
        })
    }
}
fn failure(step: &Step, reason: &'static str, actual_hash: u64) -> ReplayFailure {
    ReplayFailure {
        sequence: step.sequence,
        reason,
        expected_hash: step.after.map_or(0, |state| state.hash()),
        actual_hash,
    }
}
struct Playback<'a> {
    step: &'a Step,
    missing: Flag<bool>,
}
impl QueryWorld for Playback<'_> {
    fn cell(&self, position: [i32; 3]) -> CellRead {
        if let Some(witness) = self.step.cells[..usize::from(self.step.cell_count)]
            .iter()
            .find(|w| w.position == position)
        {
            return witness.value;
        }
        self.missing.set(true);
        CellRead::Unknown
    }
    fn flow_override(&self, position: [i32; 3]) -> Option<[f64; 3]> {
        if let Some(witness) = self.step.flows[..usize::from(self.step.flow_count)]
            .iter()
            .find(|w| w.position == position)
        {
            return witness.value;
        }
        self.missing.set(true);
        None
    }
}
pub(super) fn encode(recorder: &Recorder) -> Result<Vec<u8>, &'static str> {
    if recorder.len == 0 {
        return Err("no movement ticks captured");
    }
    if recorder
        .iter()
        .any(|step| step.invalid || step.before.is_none() || step.after.is_none())
    {
        return Err("movement capture contains incomplete steps");
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"CUBR\x01\0");
    bytes.extend_from_slice(&(recorder.len as u16).to_le_bytes());
    bytes.push(u8::from(recorder.stats().complete_window));
    for step in recorder.iter() {
        bytes.extend_from_slice(&step.sequence.to_le_bytes());
        bytes.extend_from_slice(&step.time_ns.to_le_bytes());
        bytes.extend_from_slice(&[u8::from(step.external), step.keys, u8::from(step.using)]);
        for state in [step.before.unwrap(), step.after.unwrap()] {
            bytes.extend_from_slice(&(state.as_bytes().len() as u16).to_le_bytes());
            bytes.extend_from_slice(state.as_bytes());
        }
        bytes.extend_from_slice(&[
            u8::from(step.outcome.waiting_for_world),
            u8::from(step.outcome.unknown_boundary),
        ]);
        bytes.extend_from_slice(&step.outcome.fall_damage.to_le_bytes());
        bytes.push(u8::from(step.outcome.moved));
        bytes.extend_from_slice(&step.cell_count.to_le_bytes());
        bytes.push(step.flow_count);
        for witness in &step.cells[..usize::from(step.cell_count)] {
            position(&mut bytes, witness.position);
            bytes.push(u8::from(matches!(witness.value, CellRead::Known(_))));
            if let CellRead::Known(cell) = witness.value {
                bytes.extend_from_slice(&cell.raw().to_le_bytes());
            }
        }
        for witness in &step.flows[..usize::from(step.flow_count)] {
            position(&mut bytes, witness.position);
            bytes.push(u8::from(witness.value.is_some()));
            if let Some(value) = witness.value {
                for axis in value {
                    bytes.extend_from_slice(&axis.to_bits().to_le_bytes());
                }
            }
        }
        if bytes.len() > MAX_ARCHIVE_BYTES {
            return Err("movement archive exceeds bound");
        }
    }
    Ok(bytes)
}
fn position(bytes: &mut Vec<u8>, p: [i32; 3]) {
    for axis in p {
        bytes.extend_from_slice(&axis.to_le_bytes());
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], &'static str> {
        let end = self
            .at
            .checked_add(count)
            .ok_or("movement length overflow")?;
        let bytes = self
            .bytes
            .get(self.at..end)
            .ok_or("truncated movement archive")?;
        self.at = end;
        Ok(bytes)
    }
    fn byte(&mut self) -> Result<u8, &'static str> {
        Ok(self.take(1)?[0])
    }
    fn boolean(&mut self) -> Result<bool, &'static str> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err("invalid movement boolean"),
        }
    }
    fn u16(&mut self) -> Result<u16, &'static str> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, &'static str> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, &'static str> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn position(&mut self) -> Result<[i32; 3], &'static str> {
        let p = [self.u32()? as i32, self.u32()? as i32, self.u32()? as i32];
        if p.iter().any(|axis| axis.unsigned_abs() >= 30_000_128) {
            return Err("movement witness position exceeds bound");
        }
        Ok(p)
    }
    fn state(&mut self) -> Result<PredictionState, &'static str> {
        let len = usize::from(self.u16()?);
        PredictionState::decode(self.take(len)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_witness_and_unannounced_checkpoint_cannot_silently_replay() {
        let mut recorder = Recorder::new(8).unwrap();
        let mut player = Player::new([0.5, 1.0, 0.5]);
        let world = |p: [i32; 3]| {
            CellRead::Known(Cell::from_parts(
                if p[1] < 0 {
                    rules::ids::STONE
                } else {
                    rules::ids::AIR
                },
                0,
            ))
        };
        for tick in 0..2 {
            recorder.set_time(tick * 50_000_000).unwrap();
            recorder.tick(
                &mut player,
                &world,
                Keys {
                    fwd: true,
                    ..Keys::default()
                },
                false,
            );
        }
        let bytes = recorder.encode().unwrap();
        let mut archive = Archive::decode(&bytes).unwrap();
        assert!(archive.steps[0].cell_count > 0);
        archive.steps[0].cell_count = 0;
        let failed = archive.replay().unwrap_err();
        assert_eq!(failed.sequence, 1);
        assert_eq!(failed.reason, "movement queried uncaptured world data");
        let second = (recorder.head + 1) % recorder.steps.len();
        recorder.steps[second].before = recorder.steps[recorder.head].before;
        recorder.steps[second].external = false;
        assert!(Archive::decode(&recorder.encode().unwrap()).is_err());
    }
    #[test]
    fn exceeded_cell_bound_preserves_simulation_and_refuses_incomplete_export() {
        let mut step = Step::default();
        let mut trace = Trace {
            step: &mut step,
            table: [EMPTY; TABLE],
        };
        for x in 0..=MAX_CELLS {
            trace.cell([x as i32, 0, 0], CellRead::Known(Cell::AIR));
        }
        assert_eq!(usize::from(step.cell_count), MAX_CELLS);
        assert!(step.invalid);
        let mut recorder = Recorder::new(1).unwrap();
        let mut player = Player::new([0.5, 1.0, 0.5]);
        let world = |_: [i32; 3]| CellRead::Unknown;
        recorder.tick(&mut player, &world, Keys::default(), false);
        recorder.steps[0].invalid = true;
        let mut json = "{}".to_owned();
        assert!(recorder.append_json(&mut json).is_err());
        assert_eq!(json, "{}");
    }
}
