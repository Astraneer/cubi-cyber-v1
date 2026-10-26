//! Shared UI/input/audio adapter. Shells transport physical events and execute
//! returned local intents; this module never grants inventory or permissions.
pub mod quality;
pub mod sender;
use crate::{
    network::{Connection, NetworkError},
    view::{ClientView, InputKey},
};
use audio::{AudioError, Category, Mixer};
use net::{Action, ActionId};
use ui::{
    app::{Intent, Modifiers, Role, UiApp},
    bindings,
    settings::{Setting, Settings},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyRoute {
    pub captured: bool,
    pub clear_world: bool,
    pub world: Option<(InputKey, bool)>,
}
pub fn route_key(ui: &mut UiApp, code: &str, pressed: bool, modifiers: Modifiers) -> KeyRoute {
    let action = ui.bindings.action(code, false);
    let captured = ui.key(code, pressed, modifiers);
    let clear_world = ui.captures_keyboard();
    let world = action
        .and_then(world_key)
        .filter(|_| !pressed || !clear_world)
        .map(|key| (key, pressed));
    KeyRoute {
        captured,
        clear_world,
        world,
    }
}
pub fn apply_key(
    ui: &mut UiApp,
    view: &mut ClientView,
    code: &str,
    pressed: bool,
    modifiers: Modifiers,
) -> bool {
    let route = route_key(ui, code, pressed, modifiers);
    if route.clear_world {
        view.clear_keys();
    }
    if let Some((key, down)) = route.world {
        if key == InputKey::ToolMode {
            view.set_tool_mode_key(down, modifiers.shift);
        } else {
            view.set_key(key, down);
        }
    }
    route.captured
}
fn world_key(action: bindings::Action) -> Option<InputKey> {
    use bindings::Action as A;
    Some(match action {
        A::Forward => InputKey::Forward,
        A::Back => InputKey::Back,
        A::Left => InputKey::Left,
        A::Right => InputKey::Right,
        A::Jump => InputKey::Jump,
        A::Sneak => InputKey::Sneak,
        A::Sprint | A::SprintSafe => InputKey::Sprint,
        A::Flight => InputKey::Flight,
        A::DebugFlight => InputKey::Noclip,
        A::View => InputKey::Camera,
        A::ToolMode => InputKey::ToolMode,
        _ => return None,
    })
}
pub fn mouse_delta(settings: &Settings, dx: f64, dy: f64) -> Option<[f64; 2]> {
    if !dx.is_finite() || !dy.is_finite() {
        return None;
    }
    let scale = f64::from(settings.get(Setting::Sensitivity)) / 100.0;
    let invert = if settings.get(Setting::InvertY) == 1 {
        -1.0
    } else {
        1.0
    };
    Some([
        dx.clamp(-10000.0, 10000.0) * scale,
        dy.clamp(-10000.0, 10000.0) * scale * invert,
    ])
}
/// UI channels map to the same categories in CPU and WebAudio backends.
pub fn sound_levels(settings: &Settings) -> (f32, [(Category, f32); 7]) {
    let level = |id| settings.get(id) as f32 / 100.0;
    (
        level(Setting::Master),
        [
            (Category::Blocks, level(Setting::Blocks)),
            (Category::Machines, level(Setting::Blocks)),
            (Category::Players, level(Setting::Players)),
            (Category::Entities, level(Setting::Creatures)),
            (Category::Ambient, level(Setting::Ambient)),
            (Category::Music, level(Setting::Ambient)),
            (Category::Ui, level(Setting::UiVolume)),
        ],
    )
}
pub fn apply_sound(settings: &Settings, mixer: &mut Mixer) -> Result<(), AudioError> {
    let (master, categories) = sound_levels(settings);
    mixer.volume(None, master)?;
    for (category, level) in categories {
        mixer.volume(Some(category), level)?;
    }
    Ok(())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AuthorityStamp {
    connected: bool,
    session: Option<u64>,
    identity: Option<net::PlayerId>,
    inventory: Option<u64>,
    item_attributes: Option<u64>,
    health_bits: u32,
    creative: bool,
    role: Role,
    roster: u64,
    window: Option<WindowStamp>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WindowStamp {
    guard: net::window::WindowGuard,
    closed: bool,
    read_only: bool,
    progress: u16,
    fuel: u16,
    energy: u64,
    sides: [u8; 6],
}
#[derive(Default)]
pub struct UiBridge {
    authority: Option<AuthorityStamp>,
    chat: u64,
}
fn role(value: net::Role) -> Role {
    match value {
        net::Role::Player => Role::Player,
        net::Role::Admin => Role::Admin,
        net::Role::Owner => Role::Owner,
    }
}
impl UiBridge {
    /// Copies only authoritative fields and only after they change. Presence,
    /// windows and food are supplied by their independently versioned owners.
    pub fn sync_authority(
        &mut self,
        ui: &mut UiApp,
        network: &Connection,
    ) -> Result<bool, &'static str> {
        let player = network.player();
        let own_role = network.current_role().map_or(Role::Player, role);
        let next = AuthorityStamp {
            connected: network.welcome().is_some(),
            session: network.welcome().map(|welcome| welcome.session),
            identity: network.welcome().map(|welcome| welcome.identity),
            inventory: network.inventory().map(|v| v.revision()),
            item_attributes: network
                .item_attributes()
                .map(|attributes| attributes.inventory_revision),
            health_bits: player.map_or_else(
                || {
                    network
                        .welcome()
                        .map_or(20.0f32.to_bits(), |welcome| welcome.player.health.to_bits())
                },
                |p| p.health.to_bits(),
            ),
            creative: player.map_or_else(
                || {
                    network
                        .welcome()
                        .is_some_and(|welcome| welcome.player.mode == sim::GameMode::Creative as u8)
                },
                |p| p.mode == sim::GameMode::Creative,
            ),
            role: own_role,
            roster: network.presence().revision(),
            window: network.window().map(|state| WindowStamp {
                guard: state.guard,
                closed: state.closed,
                read_only: state.read_only,
                progress: state.progress,
                fuel: state.fuel,
                energy: state.energy,
                sides: state.sides,
            }),
        };
        let reset = !next.connected
            || self
                .authority
                .is_some_and(|old| old.session != next.session || old.identity != next.identity);
        if reset {
            self.chat = 0;
            ui.clear_chat();
        }
        let mut chat_changed = false;
        for line in network.presence().chats() {
            if line.sequence > self.chat {
                ui.message(&format!("{}: {}", line.name, line.text));
                self.chat = self.chat.max(line.sequence);
                chat_changed = true;
            }
        }
        if self.authority == Some(next) {
            return Ok(chat_changed);
        }
        let mut snapshot = if !reset {
            ui.snapshot.clone()
        } else {
            ui::app::Snapshot::default()
        };
        snapshot.connected = next.connected;
        snapshot.role = own_role;
        snapshot.creative = next.creative;
        snapshot.hp = f32::from_bits(next.health_bits).clamp(0.0, 20.0).ceil() as u8;
        if let Some(inventory) = network.inventory() {
            snapshot.inventory = *inventory.slots();
            snapshot.inventory_revision = inventory.revision();
            snapshot.inventory_facts =
                crate::ui_item_facts::project(network.item_attributes(), inventory);
            snapshot.selected = inventory.selected() as u8;
            snapshot.armor = inventory.armor().0;
        } else {
            snapshot.inventory = [rules::stack::ItemStack::EMPTY; 41];
            snapshot.inventory_revision = 0;
            snapshot.inventory_facts = [None; 41];
            snapshot.selected = 0;
            snapshot.armor = 0;
        }
        snapshot.players = network
            .presence()
            .roster()
            .map(|entry| ui::app::PlayerRow {
                name: entry.name.clone(),
                role: role(entry.role),
                ping_ms: entry.ping_ms,
            })
            .collect();
        if network.window().is_none() {
            snapshot.window_id = 0;
        }
        ui.set_snapshot(snapshot)?;
        if let Some(window) = network.window() {
            crate::ui_window::apply_window(ui, window)?;
        }
        self.authority = Some(next);
        Ok(true)
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum Dispatch {
    Sent(ActionId),
    Local(Intent),
    Unsupported(Intent),
}
/// Every sent operation still passes the server's action/WAL authority. Local
/// operations return to the host; an unavailable protocol route stays explicit.
pub fn dispatch(
    network: &mut Connection,
    intent: Intent,
    now_ms: u64,
) -> Result<Dispatch, NetworkError> {
    let action = match intent {
        Intent::SelectSlot(slot) => Action::SelectSlot(slot),
        Intent::Respawn => Action::Respawn,
        Intent::Chat(ref text) if text.starts_with('/') => Action::Command(text.clone()),
        Intent::CreativeGive { item, count } => Action::Command(format!("/give {item} {count}")),
        Intent::Chat(text) => Action::Chat(text),
        Intent::OpenInventory => Action::Window(net::window::WindowIntent::OpenInventory),
        Intent::CloseWindow {
            window,
            revision,
            inventory_revision,
        } => Action::Window(net::window::WindowIntent::Close {
            guard: net::window::WindowGuard {
                id: window,
                revision,
                inventory_revision,
            },
        }),
        Intent::SlotClick {
            window,
            revision,
            inventory_revision,
            slot,
            right,
            shift,
        } => Action::Window(net::window::WindowIntent::Click {
            guard: net::window::WindowGuard {
                id: window,
                revision,
                inventory_revision,
            },
            slot,
            right,
            shift,
        }),
        Intent::RecipeFill {
            window,
            revision,
            inventory_revision,
            recipe,
            all,
        } => Action::Window(net::window::WindowIntent::RecipeFill {
            guard: net::window::WindowGuard {
                id: window,
                revision,
                inventory_revision,
            },
            recipe,
            all,
        }),
        Intent::TakeCraft {
            window,
            revision,
            inventory_revision,
            all,
        } => Action::Window(net::window::WindowIntent::TakeCraft {
            guard: net::window::WindowGuard {
                id: window,
                revision,
                inventory_revision,
            },
            all,
        }),
        Intent::MachineSide {
            window,
            revision,
            inventory_revision,
            face,
            mode,
        } => Action::Window(net::window::WindowIntent::MachineSide {
            guard: net::window::WindowGuard {
                id: window,
                revision,
                inventory_revision,
            },
            face,
            mode,
        }),
        Intent::Drop { .. } => return Ok(Dispatch::Unsupported(intent)),
        _ => return Ok(Dispatch::Local(intent)),
    };
    network.action(action, false, now_ms).map(Dispatch::Sent)
}
