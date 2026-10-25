//! Deterministic MC step cadence. Feed fixed simulation ticks, never render
//! interpolation or frame time; silence does not advance the step accumulator.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    None,
    Stone,
    Wood,
    Gravel,
    Grass,
    Sand,
    Snow,
    Wool,
    Glass,
    Metal,
    Ladder,
}
impl Surface {
    pub fn step_event(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Stone | Self::Metal | Self::Glass => Some("block.stone.step"),
            Self::Wood => Some("block.wood.step"),
            Self::Gravel => Some("block.gravel.step"),
            Self::Grass => Some("block.grass.step"),
            Self::Sand => Some("block.sand.step"),
            Self::Snow => Some("block.snow.step"),
            Self::Wool => Some("block.cloth.step"),
            Self::Ladder => Some("block.ladder.step"),
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct StepBody {
    pub moved: [f64; 3],
    pub grounded: bool,
    pub climbing: bool,
    pub silent: bool,
    pub crouching: bool,
    pub stepping_air: bool,
    pub support: Surface,
    pub snow_above_support: bool,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StepCue {
    pub event: &'static str,
    pub gain: f32,
}
#[derive(Clone, Copy, Debug)]
pub struct Steps {
    distance: f32,
    next: f32,
}
impl Default for Steps {
    fn default() -> Self {
        Self {
            distance: 0.0,
            next: 1.0,
        }
    }
}
impl Steps {
    pub fn tick(&mut self, body: StepBody) -> [Option<StepCue>; 2] {
        let mut cues = [None; 2];
        if body.silent
            || body
                .moved
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 1024.0)
        {
            return cues;
        }
        let vertical = if body.climbing { body.moved[1] } else { 0.0 };
        self.distance += (libm::sqrt(
            body.moved[0] * body.moved[0] + vertical * vertical + body.moved[2] * body.moved[2],
        ) as f32)
            * 0.6;
        if self.distance <= self.next
            || body.stepping_air
            || !(body.grounded || body.climbing || (body.crouching && body.moved[1] == 0.0))
        {
            return cues;
        }
        self.next = libm::floorf(self.distance) + 1.0;
        if body.snow_above_support {
            cues[0] = Some(StepCue {
                event: "block.snow.step",
                gain: 0.15,
            });
            if let Some(event) = body.support.step_event() {
                cues[1] = Some(StepCue { event, gain: 0.05 });
            }
        } else if let Some(event) = body.support.step_event() {
            cues[0] = Some(StepCue { event, gain: 0.15 });
        }
        cues
    }
}
