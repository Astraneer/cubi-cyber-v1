//! A bounded cavity impulse. Six visibility distances are measured off the
//! audio thread; no world traversal or mesh generation happens during mixing.
use crate::AudioError;
pub const MAX_ECHO_SECONDS: f32 = 0.45;
#[derive(Clone, Copy, Debug)]
pub struct Cavity {
    pub wall_distances: [Option<f32>; 6],
    pub absorption: f32,
}

pub(crate) struct SparseEcho {
    history: Box<[[f32; 2]]>,
    cursor: usize,
    taps: Vec<(usize, f32)>,
}
impl SparseEcho {
    pub fn prepare(cavity: Cavity, rate: u32) -> Result<Option<Self>, AudioError> {
        let impulse = cavity.impulse(rate)?;
        let taps: Vec<_> = impulse
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, gain)| *gain != 0.0)
            .collect();
        if taps.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            history: vec![[0.0; 2]; impulse.len()].into_boxed_slice(),
            cursor: 0,
            taps,
        }))
    }
    pub fn process(&mut self, output: &mut [f32]) -> u64 {
        for frame in output.chunks_exact_mut(2) {
            self.history[self.cursor] = [frame[0], frame[1]];
            let mut wet = [0.0; 2];
            for (delay, gain) in &self.taps {
                let source = (self.cursor + self.history.len() - delay) % self.history.len();
                wet[0] += self.history[source][0] * gain;
                wet[1] += self.history[source][1] * gain;
            }
            frame[0] += wet[0] * 0.25;
            frame[1] += wet[1] * 0.25;
            self.cursor = (self.cursor + 1) % self.history.len();
        }
        output.len() as u64 * self.taps.len() as u64
    }
}
impl Cavity {
    pub fn impulse(self, rate: u32) -> Result<Vec<f32>, AudioError> {
        if !(8_000..=192_000).contains(&rate)
            || !self.absorption.is_finite()
            || !(0.0..=1.0).contains(&self.absorption)
            || self
                .wall_distances
                .iter()
                .flatten()
                .any(|v| !v.is_finite() || *v < 0.0 || *v > 256.0)
        {
            return Err(AudioError::Invalid);
        }
        let mut impulse = vec![0.0; (rate as f32 * MAX_ECHO_SECONDS) as usize];
        // Four or more nearby walls certify a cavity. The plain has no wet
        // response; Unknown distances never manufacture enclosed geometry.
        let walls = self
            .wall_distances
            .iter()
            .flatten()
            .filter(|d| **d <= 48.0)
            .count();
        if walls < 4 {
            return Ok(impulse);
        }
        for (axis, distance) in self.wall_distances.into_iter().enumerate() {
            let Some(distance) = distance else {
                continue;
            };
            for bounce in 1..=3 {
                let delay = ((2.0 * distance / 343.0 * bounce as f32 + axis as f32 * 0.0007)
                    * rate as f32) as usize;
                if let Some(value) = impulse.get_mut(delay.max(1)) {
                    *value += (1.0 - self.absorption).powi(bounce) * 0.18 / bounce as f32;
                }
            }
        }
        let sum = impulse.iter().map(|v| v.abs()).sum::<f32>();
        if sum > 0.7 {
            for value in &mut impulse {
                *value *= 0.7 / sum;
            }
        }
        if let Some(last) = impulse.iter().rposition(|value| *value != 0.0) {
            impulse.truncate(last + 1);
        }
        Ok(impulse)
    }
}
