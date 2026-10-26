//! Bounded observations of actual public authority and admitted durable cues.
//! Called only by CUBI_UI_DIAGNOSTICS; it never drives gameplay or UI state.
use client::network::Connection;

#[derive(Clone, Copy, PartialEq)]
struct Stamp {
    session: u64,
    tick: u64,
    health: u32,
    ui_hp: u8,
    position: [f64; 3],
    on_ground: bool,
    flying: bool,
    mode: u8,
    inventory_revision: u64,
    selected: usize,
    hotbar: [[u16; 2]; 9],
}
#[derive(Default)]
pub(super) struct Observations {
    last: Option<Stamp>,
    cue_session: Option<(u64, net::PlayerId)>,
    cue_revision: u64,
}
impl Observations {
    pub(super) fn observe(&mut self, network: &Connection, ui_hp: u8) {
        let Some(welcome) = network.welcome() else {
            return;
        };
        let session = Some((welcome.session, welcome.identity));
        if self.cue_session != session {
            self.cue_session = session;
            self.cue_revision = 0;
            self.last = None;
        }
        if let (Some((tick, motion)), Some(inventory)) =
            (network.confirmed_motion(), network.inventory())
        {
            let mut hotbar = [[0; 2]; 9];
            for (index, row) in hotbar.iter_mut().enumerate() {
                let stack = inventory.slots()[index];
                *row = [stack.item(), stack.count()];
            }
            let next = Stamp {
                session: welcome.session,
                tick: tick.0,
                health: motion.health.to_bits(),
                ui_hp,
                position: motion.position,
                on_ground: motion.on_ground,
                flying: motion.flying,
                mode: motion.mode as u8,
                inventory_revision: inventory.revision(),
                selected: inventory.selected(),
                hotbar,
            };
            if self.last != Some(next) {
                println!(
                    "NATIVE_UI_CONSUMABLE {{\"session\":{},\"tick\":{},\"welcome_tick\":{},\"welcome_position\":{:?},\"health\":{},\"ui_hp\":{},\"position\":{:?},\"on_ground\":{},\"flying\":{},\"mode\":{},\"inventory_revision\":{},\"selected\":{},\"hotbar\":{:?}}}",
                    next.session,
                    next.tick,
                    welcome.tick.0,
                    welcome.player.position,
                    motion.health,
                    next.ui_hp,
                    next.position,
                    next.on_ground,
                    next.flying,
                    next.mode,
                    next.inventory_revision,
                    next.selected,
                    next.hotbar,
                );
                self.last = Some(next);
            }
        }
        // The existing cache is bounded to64. Limit each diagnostic visit to
        // the same16-event phase bound as the actual causal audio consumer.
        for entry in network.audio_cues().after(self.cue_revision).take(16) {
            let cue = entry.cue;
            println!(
                "NATIVE_UI_CAUSAL_CUE {{\"session\":{},\"kind\":\"{:?}\",\"tick\":{},\"actor\":{},\"sequence\":{}}}",
                welcome.session, cue.kind, cue.tick.0, cue.actor, cue.sequence,
            );
            self.cue_revision = entry.revision;
        }
    }
}
