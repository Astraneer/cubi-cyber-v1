//! Shared deterministic input route. Sampling never depends on renderer speed.
use crate::view::{ClientView, InputKey};

pub const ROUTE: &str = "forest-input-v1";
pub const FRAMES: u32 = 1800;
pub const STEP_SECONDS: f64 = 1.0 / 60.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RouteInput {
    pub buttons: u32,
    pub mouse: [f64; 2],
}
impl RouteInput {
    pub fn apply(self, view: &mut ClientView) {
        for key in [
            InputKey::Forward,
            InputKey::Back,
            InputKey::Left,
            InputKey::Right,
            InputKey::Jump,
            InputKey::Sprint,
            InputKey::Sneak,
            InputKey::Flight,
            InputKey::Noclip,
            InputKey::Camera,
            InputKey::Attack,
            InputKey::Place,
        ] {
            view.set_key(key, self.buttons & (1 << key as usize) != 0);
        }
        if self.mouse != [0.0; 2] {
            view.mouse_delta(self.mouse[0], self.mouse[1]);
        }
    }
}
fn bit(key: InputKey) -> u32 {
    1 << key as usize
}
pub fn input(frame: u32) -> Option<RouteInput> {
    if frame >= FRAMES {
        return None;
    }
    let stage = frame / 300;
    let local = frame % 300;
    let buttons = match stage {
        0 => bit(InputKey::Forward),
        1 => bit(InputKey::Forward) | bit(InputKey::Sprint),
        2 => {
            bit(InputKey::Forward)
                | if local % 60 < 3 {
                    bit(InputKey::Jump)
                } else {
                    0
                }
        }
        3 => bit(InputKey::Right) | bit(InputKey::Sneak),
        4 => {
            bit(InputKey::Forward)
                | if local < 90 {
                    bit(InputKey::Jump)
                } else if local < 180 {
                    bit(InputKey::Sneak)
                } else {
                    0
                }
                | if local == 0 { bit(InputKey::Flight) } else { 0 }
        }
        _ => {
            if local == 0 {
                bit(InputKey::Flight)
            } else {
                0
            }
        }
    };
    let mouse = match stage {
        0..=2 => [8.0, 0.0],
        3 => [-1.2, 0.0],
        4 => [
            0.6,
            if local < 60 {
                -0.3
            } else if local < 120 {
                0.3
            } else {
                0.0
            },
        ],
        _ => [0.0; 2],
    };
    Some(RouteInput { buttons, mouse })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replay_is_finite_and_toggles_flight_on_two_distinct_edges() {
        let first: Vec<_> = (0..FRAMES).map(|frame| input(frame).unwrap()).collect();
        let second: Vec<_> = (0..FRAMES).map(|frame| input(frame).unwrap()).collect();
        assert_eq!(first, second);
        assert!(input(FRAMES).is_none());
        assert_eq!(
            first
                .iter()
                .filter(|i| i.buttons & bit(InputKey::Flight) != 0)
                .count(),
            2
        );
        assert!(first.iter().all(|i| i.buttons
            & (bit(InputKey::Attack) | bit(InputKey::Place) | bit(InputKey::Noclip))
            == 0));
        assert!(first.iter().any(|i| i.buttons & bit(InputKey::Jump) != 0));
        assert!(first.iter().any(|i| i.buttons & bit(InputKey::Sprint) != 0));
    }
}
