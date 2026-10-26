use super::ClientView;
use crate::celestial_clock::{self, PersonalTime, SunClock};
use core_types::Tick;

#[derive(Default)]
pub(super) struct ClockState {
    public: Option<(Tick, f64)>,
    personal: Option<PersonalTime>,
    pub sun: Option<SunClock>,
}
impl ClientView {
    /// Confirmed server time, with bounded presentation interpolation supplied
    /// by the connection. Personal overrides never change this public anchor.
    pub fn set_public_clock(&mut self, tick: Tick, sub_tick: f64) -> Result<(), String> {
        let sun = celestial_clock::sample(tick, sub_tick, self.clock.personal)
            .map_err(|error| error.to_string())?;
        self.clock.public = Some((tick, sub_tick));
        self.install_sun_clock(Some(sun));
        Ok(())
    }
    pub fn set_personal_time(&mut self, personal: Option<PersonalTime>) -> Result<(), String> {
        // Validate even before a server anchor exists; no invented moon epoch
        // or public time is published while that source remains unknown.
        celestial_clock::sample(Tick(0), 0., personal).map_err(|error| error.to_string())?;
        let next = self
            .clock
            .public
            .map(|(tick, fraction)| celestial_clock::sample(tick, fraction, personal))
            .transpose()
            .map_err(|error| error.to_string())?;
        self.clock.personal = personal;
        self.install_sun_clock(next);
        Ok(())
    }
    pub fn sun_clock(&self) -> Option<SunClock> {
        self.clock.sun
    }
    fn install_sun_clock(&mut self, next: Option<SunClock>) {
        let light_key = |clock: Option<SunClock>| {
            clock.map(|c| {
                (
                    c.sun_direction,
                    c.sun_intensity,
                    c.moon_phase,
                    c.rotation_radians,
                )
            })
        };
        if light_key(self.clock.sun) != light_key(next) {
            self.invalidate();
        }
        self.clock.sun = next;
    }
}
