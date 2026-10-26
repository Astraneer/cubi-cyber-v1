//! Public 20Hz day clock. Personal sky overrides never modify server time.
use core_types::Tick;
pub const DAY_TICKS: u64 = 24_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersonalTime {
    Day,
    Night,
    Fixed(u16),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockError {
    InvalidFraction,
    InvalidOverride,
}
impl std::fmt::Display for ClockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ClockError {}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunClock {
    pub public_tick: Tick,
    pub time_of_day: f64,
    pub sun_direction: [f32; 3],
    pub moon_direction: [f32; 3],
    /// Scale of the physical solar constant; planet/transmittance handles night.
    pub sun_intensity: f32,
    pub moon_phase: u8,
    pub rotation_radians: f32,
    pub animation_seconds: f32,
}
pub fn sample(
    public_tick: Tick,
    sub_tick: f64,
    personal: Option<PersonalTime>,
) -> Result<SunClock, ClockError> {
    if !sub_tick.is_finite() || !(0.0..1.0).contains(&sub_tick) {
        return Err(ClockError::InvalidFraction);
    }
    let fixed = match personal {
        None => None,
        Some(PersonalTime::Day) => Some(6000),
        Some(PersonalTime::Night) => Some(18000),
        Some(PersonalTime::Fixed(value)) => {
            if u64::from(value) >= DAY_TICKS {
                return Err(ClockError::InvalidOverride);
            }
            Some(value)
        }
    };
    let time_of_day = fixed.map_or((public_tick.0 % DAY_TICKS) as f64 + sub_tick, f64::from);
    // The vanilla dimension's smoothed solar angle. Integer modulo precedes
    // floating conversion so even a long-lived public epoch retains its phase.
    let phase = (time_of_day / DAY_TICKS as f64 - 0.25).rem_euclid(1.0);
    let eased = (2. * phase + 0.5 - 0.5 * libm::cos(std::f64::consts::PI * phase)) / 3.;
    let angle = eased * std::f64::consts::TAU;
    // World +X is east; at noon the sun is +Y, at midnight it is -Y.
    let sun_direction = [(-libm::sin(angle)) as f32, libm::cos(angle) as f32, 0.];
    Ok(SunClock {
        public_tick,
        time_of_day,
        sun_direction,
        moon_direction: sun_direction.map(|v| -v),
        sun_intensity: 1.,
        moon_phase: ((public_tick.0 / DAY_TICKS) % 8) as u8,
        rotation_radians: angle as f32,
        animation_seconds: ((public_tick.0 as f64 + sub_tick) / 20.) as f32,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn clock(tick: u64) -> SunClock {
        sample(Tick(tick), 0., None).unwrap()
    }
    #[test]
    fn vanilla_sunrise_noon_sunset_midnight_and_eight_moon_phases_are_public() {
        let sunrise = clock(0);
        let noon = clock(6000);
        let sunset = clock(12000);
        let midnight = clock(18000);
        assert!(
            sunrise.sun_direction[0] > 0.97
                && sunrise.sun_direction[1] > 0.21
                && sunrise.sun_direction[1] < 0.22
        );
        assert!(
            sunset.sun_direction[0] < -0.97
                && (sunset.sun_direction[1] - sunrise.sun_direction[1]).abs() < 0.000001
        );
        for (actual, expected) in noon.sun_direction.into_iter().zip([0., 1., 0.]) {
            assert!((actual - expected).abs() < 0.000001);
        }
        for (actual, expected) in midnight.sun_direction.into_iter().zip([0., -1., 0.]) {
            assert!((actual - expected).abs() < 0.000001);
        }
        for day in 0..16 {
            let c = clock(day * DAY_TICKS);
            assert_eq!(c.moon_phase, (day % 8) as u8);
            assert_eq!(c.moon_direction, c.sun_direction.map(|v| -v));
        }
    }
    #[test]
    fn wrap_and_fraction_are_continuous_and_large_epoch_does_not_lose_day_phase() {
        for tick in [5999, 23999] {
            let a = sample(Tick(tick), 0.999, None).unwrap();
            let b = clock(tick + 1);
            let delta = a
                .sun_direction
                .into_iter()
                .zip(b.sun_direction)
                .map(|(a, b)| (a - b).abs())
                .fold(0f32, f32::max);
            assert!(delta < 0.000001);
        }
        let high = clock(u64::MAX);
        let low = clock(u64::MAX % DAY_TICKS);
        assert_eq!(high.sun_direction, low.sun_direction);
        assert!(high.animation_seconds.is_finite());
        for tick in (0..DAY_TICKS).step_by(137) {
            let c = clock(tick);
            let length = c.sun_direction.into_iter().map(|v| v * v).sum::<f32>();
            assert!((length - 1.).abs() < 0.000001);
            assert_eq!(c.sun_intensity, 1.);
        }
        assert_eq!(sample(Tick(0), 1., None), Err(ClockError::InvalidFraction));
        assert_eq!(
            sample(Tick(0), f64::NAN, None),
            Err(ClockError::InvalidFraction)
        );
    }
    #[test]
    fn personal_day_and_night_are_visual_only_and_keep_public_moon_epoch() {
        let tick = Tick(DAY_TICKS * 11 + 1234);
        let day = sample(tick, 0.8, Some(PersonalTime::Day)).unwrap();
        let night = sample(tick, 0.8, Some(PersonalTime::Night)).unwrap();
        assert_eq!(day.public_tick, tick);
        assert_eq!(night.public_tick, tick);
        assert_eq!(day.moon_phase, 3);
        assert_eq!(night.moon_phase, 3);
        assert_eq!(day.time_of_day, 6000.);
        assert_eq!(night.time_of_day, 18000.);
        assert_eq!(day.animation_seconds, night.animation_seconds);
        assert!(day.sun_direction[1] > 0.999 && night.sun_direction[1] < -0.999);
        assert_eq!(
            sample(tick, 0., Some(PersonalTime::Fixed(24000))),
            Err(ClockError::InvalidOverride)
        );
    }
}
