//! Shared Rust screen/input state. Every gameplay mutation is a typed intent
//! carrying the server window revision; UI never grants items or edits the world.
use crate::{
    atlas::UiAtlas,
    bindings::{Action, Bindings},
    paint::{PaintList, Painter, Rect, TextBuffer},
    settings::{Domain, Preset, SCHEMA, Section, Setting, Settings},
};
use rules::{
    ItemId,
    crafting::{RecipeBook, RecipeId},
    stack::ItemStack,
};
use std::{
    collections::{BTreeSet, VecDeque},
    fmt::Write,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Welcome,
    Loading,
    Game,
    Pause,
    Settings(Section),
    Inventory,
    Workbench,
    Container,
    Furnace,
    Machine,
    Recipe,
    Resources,
    Crossroads,
    Death,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Player,
    Admin,
    Owner,
}
impl Role {
    pub fn can_admin(self) -> bool {
        self != Self::Player
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersonalTime {
    Server,
    Day,
    Night,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerRow {
    pub name: String,
    pub role: Role,
    pub ping_ms: Option<u16>,
}
#[derive(Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub connected: bool,
    pub role: Role,
    pub creative: bool,
    pub hp: u8,
    pub food: u8,
    pub food_known: bool,
    pub armor: u8,
    pub inventory: [ItemStack; 41],
    pub inventory_revision: u64,
    pub selected: u8,
    pub cursor: ItemStack,
    pub craft: [ItemStack; 9],
    pub craft_result: ItemStack,
    pub window_id: u64,
    pub window_revision: u64,
    pub container: [ItemStack; 54],
    pub container_slots: u8,
    pub container_name: String,
    pub machine_progress: u16,
    pub furnace_fuel: u16,
    pub energy: u64,
    pub sides: [u8; 6],
    pub players: Vec<PlayerRow>,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            connected: false,
            role: Role::Player,
            creative: false,
            hp: 20,
            food: 20,
            food_known: false,
            armor: 0,
            inventory: [ItemStack::EMPTY; 41],
            inventory_revision: 0,
            selected: 0,
            cursor: ItemStack::EMPTY,
            craft: [ItemStack::EMPTY; 9],
            craft_result: ItemStack::EMPTY,
            window_id: 0,
            window_revision: 0,
            container: [ItemStack::EMPTY; 54],
            container_slots: 0,
            container_name: String::new(),
            machine_progress: 0,
            furnace_fuel: 0,
            energy: 0,
            sides: [0; 6],
            players: Vec::new(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    Connect {
        address: String,
        name: String,
    },
    Disconnect,
    Quit,
    PointerCapture(bool),
    SettingsChanged,
    SavePreferences,
    ImportPack,
    ImportMinecraft,
    OpenCredits,
    BugReport,
    SelectSlot(u8),
    OpenInventory,
    CloseWindow {
        window: u64,
        revision: u64,
        inventory_revision: u64,
    },
    Drop {
        all: bool,
    },
    Chat(String),
    SlotClick {
        window: u64,
        revision: u64,
        inventory_revision: u64,
        slot: u16,
        right: bool,
        shift: bool,
    },
    RecipeFill {
        recipe: u16,
        window: u64,
        revision: u64,
        inventory_revision: u64,
        all: bool,
    },
    TakeCraft {
        window: u64,
        revision: u64,
        inventory_revision: u64,
        all: bool,
    },
    CreativeGive {
        item: ItemId,
        count: u16,
    },
    MachineSide {
        window: u64,
        revision: u64,
        inventory_revision: u64,
        face: u8,
        mode: u8,
    },
    PersonalTime(PersonalTime),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Widget {
    Play,
    Resume,
    Settings,
    Resources,
    Disconnect,
    Quit,
    Back,
    Name,
    Address,
    Section(Section),
    Preset(Preset),
    ResetSection,
    Setting(Setting),
    Binding(Action),
    ImportPack,
    ImportMinecraft,
    Credits,
    Crossroads,
    Day,
    Night,
    ServerTime,
    Slot(u16),
    Nei(ItemId),
    NeiSearch,
    RecipePlus(u16),
    RecipeBack,
    RecipeNext,
    RecipePrev,
    TakeCraft,
    Side(u8),
    Bookmark(ItemId),
}
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub widget: Widget,
    pub rect: Rect,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    None,
    Name,
    Address,
    Chat,
    Search,
    Binding(Action),
}
#[derive(Clone, Copy)]
enum RecipeRef {
    Craft(RecipeId),
    Machine(rules::machine_recipes::MachineRecipe),
}
struct SearchItem {
    id: ItemId,
    name: String,
}
pub struct UiApp {
    pub settings: Settings,
    pub bindings: Bindings,
    pub screen: Screen,
    pub snapshot: Snapshot,
    pub name: String,
    pub address: String,
    pub chat_input: String,
    pub search: String,
    pub focus: Focus,
    pub show_debug: bool,
    pub show_hud: bool,
    pub player_list: bool,
    pub personal_time: PersonalTime,
    revision: u64,
    painted: Option<(u64, u32, u32)>,
    hits: Vec<Hit>,
    hover: Option<Widget>,
    pointer: [f32; 2],
    intents: VecDeque<Intent>,
    chat: VecDeque<String>,
    catalog: Vec<SearchItem>,
    filtered: Vec<ItemId>,
    book: RecipeBook,
    bookmarks: BTreeSet<ItemId>,
    recipes: Vec<RecipeRef>,
    recipe_page: usize,
    recipe_item: Option<ItemId>,
    recipe_uses: bool,
    scroll: usize,
    nei_page: usize,
    status: String,
    previous: Screen,
    tab_cycle: usize,
    scale: f32,
    bookmark_scroll: usize,
    profiler: crate::profiler::ProfilerSnapshot,
    pending_settings: u32,
}
impl UiApp {
    pub fn new() -> Result<Self, &'static str> {
        let catalog: Vec<_> = rules::all_items()
            .map(|(id, item)| SearchItem {
                id,
                name: format!("{} {}", item.name.to_lowercase(), item.mc),
            })
            .collect();
        let filtered = catalog.iter().map(|v| v.id).collect();
        Ok(Self {
            settings: Settings::default(),
            bindings: Bindings::default(),
            screen: Screen::Welcome,
            snapshot: Snapshot::default(),
            name: "Игрок".into(),
            address: "127.0.0.1:25565".into(),
            chat_input: String::with_capacity(256),
            search: String::with_capacity(128),
            focus: Focus::None,
            show_debug: false,
            show_hud: true,
            player_list: false,
            personal_time: PersonalTime::Server,
            revision: 1,
            painted: None,
            hits: Vec::with_capacity(1024),
            hover: None,
            pointer: [0.0; 2],
            intents: VecDeque::with_capacity(64),
            chat: VecDeque::with_capacity(64),
            catalog,
            filtered,
            book: RecipeBook::standard().map_err(|_| "recipe registry")?,
            bookmarks: BTreeSet::new(),
            recipes: Vec::with_capacity(256),
            recipe_page: 0,
            recipe_item: None,
            recipe_uses: false,
            scroll: 0,
            nei_page: 0,
            status: String::with_capacity(256),
            previous: Screen::Welcome,
            tab_cycle: 0,
            scale: 1.0,
            bookmark_scroll: 0,
            profiler: crate::profiler::ProfilerSnapshot::default(),
            pending_settings: 0,
        })
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn set_profiler(&mut self, snapshot: crate::profiler::ProfilerSnapshot) {
        if self.profiler != snapshot {
            self.profiler = snapshot;
            if self.show_debug {
                self.dirty();
            }
        }
    }
    pub fn set_pending_settings(&mut self, mask: u32) {
        let mask = mask & ((1u32 << crate::settings::COUNT) - 1);
        if mask != self.pending_settings {
            self.pending_settings = mask;
            self.dirty();
        }
    }
    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }
    pub fn hit(&self, widget: Widget) -> Option<Rect> {
        self.hits
            .iter()
            .find(|hit| hit.widget == widget)
            .map(|hit| hit.rect)
    }
    pub fn filtered_items(&self) -> &[ItemId] {
        &self.filtered
    }
    pub fn captures_keyboard(&self) -> bool {
        self.screen != Screen::Game || self.focus != Focus::None
    }
    pub fn take_intent(&mut self) -> Option<Intent> {
        self.intents.pop_front()
    }
    pub fn status(&self) -> &str {
        &self.status
    }
    pub fn encode_preferences(&self) -> Vec<u8> {
        let settings = self.settings.encode();
        let keys = self.bindings.encode();
        let mut bytes = b"UIP1".to_vec();
        for data in [&settings[..], &keys[..]] {
            bytes.extend_from_slice(&(data.len() as u16).to_le_bytes());
            bytes.extend_from_slice(data);
        }
        bytes.extend_from_slice(&(self.bookmarks.len() as u16).to_le_bytes());
        for item in &self.bookmarks {
            bytes.extend_from_slice(&item.to_le_bytes());
        }
        for text in [&self.name, &self.address] {
            bytes.extend_from_slice(&(text.len() as u16).to_le_bytes());
            bytes.extend_from_slice(text.as_bytes());
        }
        bytes
    }
    pub fn load_preferences(&mut self, bytes: &[u8]) -> Result<(), &'static str> {
        if bytes.len() > 8192 || bytes.get(..4) != Some(b"UIP1") {
            return Err("UI preferences format");
        }
        let mut offset = 4;
        fn field<'a>(bytes: &'a [u8], offset: &mut usize) -> Result<&'a [u8], &'static str> {
            let length = u16::from_le_bytes(
                bytes
                    .get(*offset..*offset + 2)
                    .ok_or("UI preferences truncated")?
                    .try_into()
                    .unwrap(),
            ) as usize;
            *offset += 2;
            let value = bytes
                .get(*offset..*offset + length)
                .ok_or("UI preferences truncated")?;
            *offset += length;
            Ok(value)
        }
        let settings = Settings::decode(field(bytes, &mut offset)?)?;
        let bindings =
            Bindings::decode(field(bytes, &mut offset)?).map_err(|_| "UI key preferences")?;
        let count = u16::from_le_bytes(
            bytes
                .get(offset..offset + 2)
                .ok_or("UI bookmark count")?
                .try_into()
                .unwrap(),
        ) as usize;
        offset += 2;
        if count > 512 {
            return Err("UI bookmarks bound");
        }
        let mut bookmarks = BTreeSet::new();
        for _ in 0..count {
            let item = u16::from_le_bytes(
                bytes
                    .get(offset..offset + 2)
                    .ok_or("UI bookmark")?
                    .try_into()
                    .unwrap(),
            );
            offset += 2;
            if rules::item(item).is_none() || !bookmarks.insert(item) {
                return Err("UI bookmark value");
            }
        }
        let name = std::str::from_utf8(field(bytes, &mut offset)?).map_err(|_| "UI name UTF8")?;
        let address =
            std::str::from_utf8(field(bytes, &mut offset)?).map_err(|_| "UI address UTF8")?;
        if name.len() > 64
            || address.len() > 256
            || name.chars().any(char::is_control)
            || address.chars().any(char::is_control)
            || offset != bytes.len()
        {
            return Err("UI profile bounds");
        }
        self.settings = settings;
        self.bindings = bindings;
        self.bookmarks = bookmarks;
        self.name = name.to_owned();
        self.address = address.to_owned();
        self.dirty();
        Ok(())
    }
    pub fn set_status(&mut self, message: &str) {
        if message.len() <= 256 && self.status != message {
            self.status.clear();
            self.status.push_str(message);
            self.dirty();
        }
    }
    fn dirty(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }
    fn intent(&mut self, intent: Intent) {
        if self.intents.len() < 64 {
            self.intents.push_back(intent);
        } else {
            self.status = "Действия ожидают подтверждения".into();
        }
        self.dirty();
    }
    pub fn open(&mut self, screen: Screen) {
        if matches!(screen, Screen::Game | Screen::Pause | Screen::Welcome)
            && matches!(
                self.screen,
                Screen::Inventory
                    | Screen::Workbench
                    | Screen::Container
                    | Screen::Furnace
                    | Screen::Machine
                    | Screen::Recipe
            )
            && self.snapshot.window_id != 0
        {
            self.intent(Intent::CloseWindow {
                window: self.snapshot.window_id,
                revision: self.snapshot.window_revision,
                inventory_revision: self.snapshot.inventory_revision,
            });
        }
        self.previous = self.screen;
        self.screen = screen;
        self.focus = Focus::None;
        self.scroll = 0;
        self.hover = None;
        self.intent(Intent::PointerCapture(screen == Screen::Game));
    }
    pub fn set_snapshot(&mut self, snapshot: Snapshot) -> Result<(), &'static str> {
        if snapshot.hp > 20
            || snapshot.food > 20
            || snapshot.armor > 20
            || snapshot.selected > 8
            || snapshot.container_slots > 54
            || snapshot.players.len() > 64
            || snapshot.container_name.len() > 128
            || snapshot.players.iter().any(|p| p.name.len() > 64)
        {
            return Err("UI snapshot bound");
        }
        if self.snapshot == snapshot {
            return Ok(());
        }
        let dead = snapshot.hp == 0;
        self.snapshot = snapshot;
        if dead && self.screen != Screen::Death {
            self.open(Screen::Death);
        }
        self.dirty();
        Ok(())
    }
    pub fn message(&mut self, message: &str) {
        if message.len() > 384 || message.chars().any(|c| c.is_control() && c != '\n') {
            return;
        }
        if self.chat.len() == 64 {
            self.chat.pop_front();
        }
        self.chat.push_back(message.to_owned());
        self.dirty();
    }
    pub fn clear_chat(&mut self) {
        if !self.chat.is_empty() {
            self.chat.clear();
            self.dirty();
        }
    }
    pub fn chat_len(&self) -> usize {
        self.chat.len()
    }
    pub fn last_chat(&self) -> Option<&str> {
        self.chat.back().map(String::as_str)
    }
    pub fn pointer(&mut self, x: f32, y: f32) {
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        self.pointer = [x, y];
        let hover = self
            .hits
            .iter()
            .rev()
            .find(|hit| hit.rect.contains(x, y))
            .map(|hit| hit.widget);
        if hover != self.hover || !self.snapshot.cursor.is_empty() || self.hovered_item().is_some()
        {
            self.hover = hover;
            self.dirty();
        }
    }
    pub fn click(&mut self, x: f32, y: f32, right: bool, modifiers: Modifiers) {
        self.pointer(x, y);
        let Some(widget) = self.hover else {
            self.focus = Focus::None;
            return;
        };
        self.activate(widget, right, modifiers);
    }
    pub fn wheel(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        if self.screen == Screen::Game && self.focus == Focus::None && self.snapshot.connected {
            self.intent(Intent::SelectSlot(
                (i32::from(self.snapshot.selected) + delta.signum()).rem_euclid(9) as u8,
            ));
            return;
        }
        if matches!(
            self.screen,
            Screen::Inventory
                | Screen::Workbench
                | Screen::Container
                | Screen::Furnace
                | Screen::Machine
        ) {
            if matches!(self.hover, Some(Widget::Bookmark(_))) {
                self.bookmark_scroll = (self.bookmark_scroll as i32 + delta.signum())
                    .clamp(0, self.bookmarks.len().saturating_sub(8) as i32)
                    as usize;
                self.dirty();
                return;
            }
            let pages = self.filtered.len().div_ceil(30);
            self.nei_page = (self.nei_page as i32 + delta.signum())
                .clamp(0, pages.saturating_sub(1) as i32) as usize;
        } else if matches!(self.screen, Screen::Settings(_)) {
            self.scroll = (self.scroll as i32 + delta.signum()).max(0) as usize;
        }
        self.dirty();
    }
    pub fn text(&mut self, text: &str) {
        if text.chars().any(char::is_control) {
            return;
        }
        let target = match self.focus {
            Focus::Name => &mut self.name,
            Focus::Address => &mut self.address,
            Focus::Chat => &mut self.chat_input,
            Focus::Search => &mut self.search,
            _ => return,
        };
        let max = if self.focus == Focus::Name {
            64
        } else if self.focus == Focus::Search {
            128
        } else {
            256
        };
        if target.len() + text.len() <= max {
            target.push_str(text);
            if self.focus == Focus::Search {
                self.filter();
            }
            self.tab_cycle = 0;
            self.dirty();
        }
    }
    pub fn key(&mut self, code: &str, pressed: bool, modifiers: Modifiers) -> bool {
        if code == "Tab" && !pressed {
            if self.player_list {
                self.player_list = false;
                self.dirty();
            }
            return self.captures_keyboard();
        }
        if !pressed {
            return self.captures_keyboard();
        }
        if let Focus::Binding(action) = self.focus {
            if code == "Escape" {
                self.focus = Focus::None;
                self.dirty();
                return true;
            }
            match self.bindings.rebind(action, code) {
                Ok(()) => {
                    self.status = "Клавиша сохранена".into();
                    self.focus = Focus::None;
                    self.intent(Intent::SavePreferences);
                }
                Err(crate::bindings::BindingError::Conflict(other)) => {
                    self.status = format!(
                        "Клавиша уже занята: {}",
                        crate::bindings::DEFAULTS[other as usize].label
                    )
                }
                Err(crate::bindings::BindingError::Fixed) => {
                    self.status = "Esc и кнопки мыши закреплены".into()
                }
                Err(_) => self.status = "Выберите физическую клавишу".into(),
            };
            self.dirty();
            return true;
        }
        if code == "Escape" {
            if self.screen == Screen::Loading {
                self.intent(Intent::Disconnect);
            } else if self.focus == Focus::Chat {
                self.focus = Focus::None;
                self.chat_input.clear();
                self.dirty();
            } else if self.screen == Screen::Game {
                self.open(Screen::Pause);
            } else if self.snapshot.connected {
                self.open(Screen::Game);
            } else {
                self.open(Screen::Welcome);
            }
            return true;
        }
        if self.focus != Focus::None {
            match code {
                "Backspace" => {
                    let target = match self.focus {
                        Focus::Name => &mut self.name,
                        Focus::Address => &mut self.address,
                        Focus::Chat => &mut self.chat_input,
                        Focus::Search => &mut self.search,
                        _ => unreachable!(),
                    };
                    target.pop();
                    if self.focus == Focus::Search {
                        self.filter();
                    }
                    self.dirty();
                    return true;
                }
                "Enter" if self.focus == Focus::Chat => {
                    if !self.chat_input.trim().is_empty() {
                        let message = self.chat_input.clone();
                        self.intent(Intent::Chat(message));
                    }
                    self.chat_input.clear();
                    self.focus = Focus::None;
                    self.dirty();
                    return true;
                }
                "Tab" if self.focus == Focus::Chat => {
                    self.complete_chat();
                    return true;
                }
                _ => return true,
            }
        }
        if let Some(action) = self.bindings.action(code, true)
            && matches!(
                self.screen,
                Screen::Inventory
                    | Screen::Workbench
                    | Screen::Container
                    | Screen::Furnace
                    | Screen::Machine
                    | Screen::Recipe
            )
        {
            if let Some(item) = self.hovered_item() {
                match action {
                    Action::Recipe => self.recipes(item, false),
                    Action::Uses => self.recipes(item, true),
                    Action::Bookmark => {
                        if !self.bookmarks.remove(&item) {
                            self.bookmarks.insert(item);
                        }
                        self.intent(Intent::SavePreferences);
                    }
                    _ => {}
                }
            }
            return true;
        }
        if let Some(action) = self.bindings.action(code, false) {
            match action {
                Action::Inventory => {
                    if !self.snapshot.connected || self.screen == Screen::Loading {
                        return true;
                    }
                    if self.screen == Screen::Game {
                        self.intent(Intent::OpenInventory);
                        self.open(Screen::Inventory);
                    } else {
                        self.open(Screen::Game);
                    }
                    return true;
                }
                Action::Settings => {
                    self.open(Screen::Settings(Section::Graphics));
                    return true;
                }
                Action::Chat | Action::Command => {
                    if !self.snapshot.connected {
                        return true;
                    }
                    self.chat_input.clear();
                    if action == Action::Command {
                        self.chat_input.push('/');
                    }
                    self.focus = Focus::Chat;
                    self.intent(Intent::PointerCapture(false));
                    return true;
                }
                Action::Players => {
                    if !self.snapshot.connected {
                        return true;
                    }
                    self.player_list = true;
                    self.dirty();
                    return true;
                }
                Action::Hud => {
                    self.show_hud = !self.show_hud;
                    self.dirty();
                    return true;
                }
                Action::Profiler => {
                    self.show_debug = !self.show_debug;
                    self.dirty();
                    return true;
                }
                Action::BugReport => {
                    self.intent(Intent::BugReport);
                    return true;
                }
                Action::Drop if self.screen == Screen::Game => {
                    self.intent(Intent::Drop {
                        all: modifiers.ctrl,
                    });
                    return true;
                }
                _ => {
                    if (action as usize) >= (Action::Hotbar1 as usize) {
                        self.intent(Intent::SelectSlot(
                            (action as usize - Action::Hotbar1 as usize) as u8,
                        ));
                        return true;
                    }
                }
            }
        }
        self.captures_keyboard()
    }
    fn filter(&mut self) {
        let query = self.search.to_lowercase();
        self.filtered.clear();
        self.filtered.extend(
            self.catalog
                .iter()
                .filter(|item| item.name.contains(&query))
                .map(|item| item.id),
        );
        self.nei_page = 0;
    }
    fn hovered_item(&self) -> Option<ItemId> {
        match self.hover {
            Some(Widget::Nei(item) | Widget::Bookmark(item)) => Some(item),
            Some(Widget::Slot(slot)) => {
                let stack = match slot {
                    0..=40 => self.snapshot.inventory[slot as usize],
                    41..=94 => self.snapshot.container[(slot - 41) as usize],
                    100..=108 => self.snapshot.craft[(slot - 100) as usize],
                    109 => self.snapshot.craft_result,
                    _ => return None,
                };
                (!stack.is_empty()).then_some(stack.item())
            }
            _ => None,
        }
    }
    /// Only the public presence of item state is shown, never private contents.
    pub fn hovered_state_label(&self) -> Option<&'static str> {
        let Some(Widget::Slot(slot)) = self.hover else {
            return None;
        };
        let stack = match slot {
            0..=40 => self.snapshot.inventory[slot as usize],
            41..=94 => self.snapshot.container[(slot - 41) as usize],
            100..=108 => self.snapshot.craft[(slot - 100) as usize],
            109 => self.snapshot.craft_result,
            _ => return None,
        };
        stack.payload()?;
        Some(if stack.item() == rules::items::I_ENERGY_TABLET {
            "Сохранённый заряд"
        } else {
            "Сохранённое состояние"
        })
    }
    fn recipes(&mut self, item: ItemId, uses: bool) {
        self.recipes.clear();
        let rows: Vec<_> = if uses {
            self.book
                .recipes_using(item)
                .map(|(id, _)| RecipeRef::Craft(id))
                .collect()
        } else {
            self.book
                .recipes_producing(item)
                .map(|(id, _)| RecipeRef::Craft(id))
                .collect()
        };
        self.recipes.extend(rows);
        for recipe in rules::machine_recipes::all() {
            let matches = if uses {
                recipe.input == item
            } else {
                recipe.product.key() == rules::machine_recipes::ProductKey::Item(item)
            };
            if matches {
                self.recipes.push(RecipeRef::Machine(recipe));
            }
        }
        self.recipe_item = Some(item);
        self.recipe_uses = uses;
        self.recipe_page = 0;
        self.open(Screen::Recipe);
    }
    fn complete_chat(&mut self) {
        let role = match self.snapshot.role {
            Role::Player => net::Role::Player,
            Role::Admin => net::Role::Admin,
            Role::Owner => net::Role::Owner,
        };
        let prefix = self.chat_input.split_whitespace().next().unwrap_or("");
        let candidates: Vec<_> = net::commands_for(role)
            .filter(|command| command.name.starts_with(prefix))
            .collect();
        if !candidates.is_empty() {
            self.chat_input = format!("{} ", candidates[self.tab_cycle % candidates.len()].name);
            self.tab_cycle += 1;
            self.dirty();
        }
    }
    fn activate(&mut self, widget: Widget, right: bool, modifiers: Modifiers) {
        match widget {
            Widget::Play => {
                if self.name.trim().is_empty() || self.address.trim().is_empty() {
                    self.status = "Укажите имя и адрес мира".into();
                } else {
                    self.intent(Intent::Connect {
                        address: self.address.clone(),
                        name: self.name.clone(),
                    });
                }
            }
            Widget::Resume => self.open(Screen::Game),
            Widget::Settings => self.open(Screen::Settings(Section::Graphics)),
            Widget::Resources => self.open(Screen::Resources),
            Widget::Disconnect => self.intent(Intent::Disconnect),
            Widget::Quit => self.intent(Intent::Quit),
            Widget::Back => self.open(if self.snapshot.connected {
                Screen::Pause
            } else {
                Screen::Welcome
            }),
            Widget::Name => self.focus = Focus::Name,
            Widget::Address => self.focus = Focus::Address,
            Widget::NeiSearch => self.focus = Focus::Search,
            Widget::Section(section) => {
                self.screen = Screen::Settings(section);
                self.scroll = 0;
            }
            Widget::Preset(preset) => {
                self.settings.apply_preset(preset);
                self.intent(Intent::SettingsChanged);
            }
            Widget::ResetSection => {
                if let Screen::Settings(section) = self.screen {
                    self.settings.reset_section(section);
                    if section == Section::Controls {
                        self.bindings.reset();
                    }
                    self.intent(Intent::SettingsChanged);
                }
            }
            Widget::Setting(id) => {
                let definition = SCHEMA[id as usize];
                let old = self.settings.get(id);
                let value = match definition.domain {
                    Domain::Choices(values) => {
                        let index = values.iter().position(|v| *v == old).unwrap();
                        values[(index + if right { values.len() - 1 } else { 1 }) % values.len()]
                    }
                    Domain::Range { step, min, max } => {
                        let proposed = old + if right { -step } else { step };
                        if proposed > max {
                            min
                        } else if proposed < min {
                            max
                        } else {
                            proposed
                        }
                    }
                };
                let _ = self.settings.set(id, value);
                self.intent(Intent::SettingsChanged);
            }
            Widget::Binding(action) => self.focus = Focus::Binding(action),
            Widget::ImportPack => self.intent(Intent::ImportPack),
            Widget::ImportMinecraft => self.intent(Intent::ImportMinecraft),
            Widget::Credits => self.intent(Intent::OpenCredits),
            Widget::Crossroads => self.open(Screen::Crossroads),
            Widget::Day => {
                self.personal_time = PersonalTime::Day;
                self.intent(Intent::PersonalTime(self.personal_time));
            }
            Widget::Night => {
                self.personal_time = PersonalTime::Night;
                self.intent(Intent::PersonalTime(self.personal_time));
            }
            Widget::ServerTime => {
                self.personal_time = PersonalTime::Server;
                self.intent(Intent::PersonalTime(self.personal_time));
            }
            Widget::Slot(slot) => self.intent(Intent::SlotClick {
                window: self.snapshot.window_id,
                revision: self.snapshot.window_revision,
                inventory_revision: self.snapshot.inventory_revision,
                slot,
                right,
                shift: modifiers.shift,
            }),
            Widget::Nei(item) => {
                if self.snapshot.creative && self.snapshot.role.can_admin() {
                    self.intent(Intent::CreativeGive {
                        item,
                        count: if right {
                            1
                        } else {
                            rules::item(item).unwrap().max_stack as u16
                        },
                    });
                } else {
                    self.recipes(item, false);
                }
            }
            Widget::Bookmark(item) => self.recipes(item, false),
            Widget::RecipePlus(recipe) => self.intent(Intent::RecipeFill {
                recipe,
                window: self.snapshot.window_id,
                revision: self.snapshot.window_revision,
                inventory_revision: self.snapshot.inventory_revision,
                all: modifiers.shift,
            }),
            Widget::TakeCraft => self.intent(Intent::TakeCraft {
                window: self.snapshot.window_id,
                revision: self.snapshot.window_revision,
                inventory_revision: self.snapshot.inventory_revision,
                all: modifiers.shift,
            }),
            Widget::RecipeBack => self.open(Screen::Inventory),
            Widget::RecipeNext => {
                if !self.recipes.is_empty() {
                    self.recipe_page = (self.recipe_page + 1) % self.recipes.len();
                }
            }
            Widget::RecipePrev => {
                if !self.recipes.is_empty() {
                    self.recipe_page =
                        (self.recipe_page + self.recipes.len() - 1) % self.recipes.len();
                }
            }
            Widget::Side(face) => self.intent(Intent::MachineSide {
                window: self.snapshot.window_id,
                revision: self.snapshot.window_revision,
                inventory_revision: self.snapshot.inventory_revision,
                face,
                mode: (self.snapshot.sides[usize::from(face)] + 1) % 6,
            }),
        }
        self.dirty();
    }
}

const TEXT: u32 = 0xffece7dc;
const MUTED: u32 = 0xffa7aaa8;
const ACCENT: u32 = 0xff74c5b6;
impl UiApp {
    fn control(&mut self, p: &mut Painter<'_>, widget: Widget, rect: Rect) {
        if let Some(visible) = rect.intersection(p.clip) {
            self.hits.push(Hit {
                widget,
                rect: visible.scaled(self.scale),
            });
        }
        let hover = self.hover == Some(widget);
        p.rect(rect, if hover { 0xff3d514f } else { 0xff292f34 });
        p.border(rect, if hover { ACCENT } else { 0xff59616a });
        p.rect(
            Rect::new(rect.x + 1.0, rect.y + 1.0, rect.w - 2.0, 1.0),
            0xff6a7377,
        );
    }
    fn button(&mut self, p: &mut Painter<'_>, widget: Widget, rect: Rect, label: &str) {
        self.control(p, widget, rect);
        p.centered(rect, label, TEXT);
    }
    fn stack_count(p: &mut Painter<'_>, rect: Rect, stack: ItemStack) {
        if stack.count() > 1 {
            let mut text = TextBuffer::default();
            let _ = write!(text, "{}", stack.count());
            let width = p.text_width(text.as_str());
            p.text(
                rect.x + rect.w - width - 1.0,
                rect.y + rect.h - 16.0,
                text.as_str(),
                TEXT,
            );
        }
    }
    fn slot(&mut self, p: &mut Painter<'_>, slot: u16, rect: Rect, stack: ItemStack) {
        self.hits.push(Hit {
            widget: Widget::Slot(slot),
            rect: rect.scaled(self.scale),
        });
        p.rect(rect, 0xff171c20);
        p.border(
            rect,
            if self.hover == Some(Widget::Slot(slot)) {
                ACCENT
            } else {
                0xff62666a
            },
        );
        if !stack.is_empty() {
            p.item(
                Rect::new(rect.x + 4.0, rect.y + 4.0, 24.0, 24.0),
                stack.item(),
            );
            Self::stack_count(p, rect, stack);
            if stack.damage() > 0 {
                let max = rules::item(stack.item()).unwrap().max_damage;
                let remaining = 1.0 - f32::from(stack.damage()) / f32::from(max.max(1));
                p.rect(
                    Rect::new(rect.x + 3.0, rect.y + rect.h - 4.0, rect.w - 6.0, 2.0),
                    0xff302b2c,
                );
                p.rect(
                    Rect::new(
                        rect.x + 3.0,
                        rect.y + rect.h - 4.0,
                        (rect.w - 6.0) * remaining,
                        2.0,
                    ),
                    0xff83bf5c,
                );
            }
            if stack.payload().is_some() {
                p.rect(
                    Rect::new(rect.x + rect.w - 5.0, rect.y + 3.0, 2.0, 2.0),
                    ACCENT,
                );
            }
        }
    }
    pub fn paint(
        &mut self,
        atlas: &UiAtlas,
        list: &mut PaintList,
        width: u32,
        height: u32,
    ) -> bool {
        let stamp = (self.revision, width, height);
        if self.painted == Some(stamp) {
            return false;
        }
        list.clear(self.revision, width.max(1), height.max(1));
        self.hits.clear();
        let mut p = Painter::new(atlas, list);
        self.scale = self.settings.get(Setting::UiScale) as f32 / 100.0;
        p.scale = self.scale;
        let w = width as f32 / self.scale;
        let h = height as f32 / self.scale;
        p.clip = Rect::new(0.0, 0.0, w, h);
        if self.screen == Screen::Game {
            if self.show_hud {
                self.hud(&mut p, w, h);
            }
        } else {
            p.rect(
                Rect::new(0.0, 0.0, w, h),
                if self.snapshot.connected {
                    0xd012161c
                } else {
                    0xff141a20
                },
            );
            for y in (0..height).step_by(32) {
                p.rect(Rect::new(0.0, y as f32, w, 1.0), 0x103b4147);
            }
            match self.screen {
                Screen::Welcome => self.welcome(&mut p, w, h),
                Screen::Loading => {
                    self.title(&mut p, w, "Подключение к миру");
                    p.centered(Rect::new(20.0, h * 0.4, w - 40.0, 48.0), &self.status, TEXT);
                    self.button(
                        &mut p,
                        Widget::Disconnect,
                        Rect::new(w * 0.5 - 100.0, h * 0.65, 200.0, 32.0),
                        "Отмена",
                    );
                }
                Screen::Pause => self.pause(&mut p, w, h),
                Screen::Settings(section) => self.settings_screen(&mut p, w, h, section),
                Screen::Inventory
                | Screen::Workbench
                | Screen::Container
                | Screen::Furnace
                | Screen::Machine => self.inventory_screen(&mut p, w, h),
                Screen::Recipe => self.recipe_screen(&mut p, w, h),
                Screen::Resources => self.resource_screen(&mut p, w, h),
                Screen::Crossroads => self.crossroads_screen(&mut p, w, h),
                Screen::Death => {
                    p.centered(Rect::new(0.0, h * 0.35, w, 30.0), "Вы погибли", 0xffea8b83);
                    self.button(
                        &mut p,
                        Widget::Back,
                        Rect::new(w * 0.5 - 130.0, h * 0.5, 260.0, 34.0),
                        "Вернуться в меню",
                    );
                }
                Screen::Game => {}
            }
        }
        if self.player_list {
            self.players(&mut p, w, h);
        }
        if self.focus == Focus::Chat || (!self.chat.is_empty() && self.screen == Screen::Game) {
            self.chat_screen(&mut p, w, h);
        }
        if self.show_debug {
            self.profiler.paint(&mut p, w, h);
        }
        if !self.snapshot.cursor.is_empty()
            && matches!(
                self.screen,
                Screen::Inventory
                    | Screen::Workbench
                    | Screen::Container
                    | Screen::Furnace
                    | Screen::Machine
            )
        {
            p.item(
                Rect::new(
                    self.pointer[0] / self.scale - 12.0,
                    self.pointer[1] / self.scale - 12.0,
                    24.0,
                    24.0,
                ),
                self.snapshot.cursor.item(),
            );
            Self::stack_count(
                &mut p,
                Rect::new(
                    self.pointer[0] / self.scale - 12.0,
                    self.pointer[1] / self.scale - 12.0,
                    24.0,
                    24.0,
                ),
                self.snapshot.cursor,
            );
        }
        if let Some(item) = self.hovered_item()
            && let Some(item) = rules::item(item)
        {
            let detail = self.hovered_state_label();
            let width = (p
                .text_width(item.name)
                .max(detail.map_or(0.0, |text| p.text_width(text)))
                + 16.0)
                .min(w - 16.0);
            let height = if detail.is_some() { 44.0 } else { 26.0 };
            let x = (self.pointer[0] / self.scale).min(w - width - 8.0).max(4.0);
            let y = (self.pointer[1] / self.scale + 24.0)
                .min(h - height - 4.0)
                .max(4.0);
            p.rect(Rect::new(x, y, width, height), 0xf0181b23);
            p.border(Rect::new(x, y, width, height), 0xff6c7288);
            p.text(x + 8.0, y + 3.0, item.name, TEXT);
            if let Some(detail) = detail {
                p.text(x + 8.0, y + 21.0, detail, ACCENT);
            }
        }
        self.painted = Some(stamp);
        true
    }
    fn title(&self, p: &mut Painter<'_>, w: f32, label: &str) {
        p.centered(Rect::new(12.0, 18.0, w - 24.0, 30.0), label, TEXT);
        p.rect(Rect::new(20.0, 55.0, w - 40.0, 1.0), 0xff3c5458);
    }
    fn welcome(&mut self, p: &mut Painter<'_>, w: f32, h: f32) {
        self.title(p, w, "КУБИ · ПЕРЕКРЁСТОК");
        let x = (w * 0.5 - 180.0).max(8.0);
        let width = 360.0f32.min(w - 16.0);
        let top = (h * 0.24).max(65.0);
        p.text(x, top, "Ваше имя", MUTED);
        self.control(p, Widget::Name, Rect::new(x, top + 22.0, width, 30.0));
        p.text(x + 8.0, top + 27.0, &self.name, TEXT);
        p.text(x, top + 62.0, "Мир", MUTED);
        self.control(p, Widget::Address, Rect::new(x, top + 84.0, width, 30.0));
        p.text(x + 8.0, top + 89.0, &self.address, TEXT);
        self.button(
            p,
            Widget::Play,
            Rect::new(x, top + 128.0, width, 34.0),
            "Играть",
        );
        self.button(
            p,
            Widget::Settings,
            Rect::new(x, top + 171.0, width * 0.5 - 4.0, 32.0),
            "Настройки",
        );
        self.button(
            p,
            Widget::Resources,
            Rect::new(x + width * 0.5 + 4.0, top + 171.0, width * 0.5 - 4.0, 32.0),
            "Ресурсы",
        );
        p.text(14.0, h - 26.0, &self.status, MUTED);
    }
    fn pause(&mut self, p: &mut Painter<'_>, w: f32, h: f32) {
        self.title(p, w, "Меню");
        let x = w * 0.5 - 150.0;
        let y = (h - 270.0) * 0.5;
        for (row, widget, label) in [
            (0, Widget::Resume, "Вернуться в игру"),
            (1, Widget::Settings, "Настройки"),
            (2, Widget::Resources, "Локальные ресурсы"),
            (3, Widget::Crossroads, "Перекрёсток"),
            (4, Widget::Disconnect, "Отключиться"),
            (5, Widget::Quit, "Выйти"),
        ] {
            self.button(
                p,
                widget,
                Rect::new(x, y + row as f32 * 41.0, 300.0, 32.0),
                label,
            );
        }
    }
    fn settings_screen(&mut self, p: &mut Painter<'_>, w: f32, h: f32, section: Section) {
        self.title(p, w, "Настройки");
        let x = 24.0;
        let tab_width = (w - 48.0) / 4.0;
        for (index, target, label) in [
            (0, Section::Graphics, "Картинка"),
            (1, Section::Controls, "Управление"),
            (2, Section::Sound, "Звук"),
            (3, Section::Interface, "Интерфейс"),
        ] {
            self.button(
                p,
                Widget::Section(target),
                Rect::new(x + index as f32 * tab_width, 64.0, tab_width - 5.0, 29.0),
                label,
            );
            if section == target {
                p.rect(
                    Rect::new(x + index as f32 * tab_width, 91.0, tab_width - 5.0, 2.0),
                    ACCENT,
                );
            }
        }
        let mut row = 0usize;
        let viewport = Rect::new(16.0, 104.0, w - 32.0, (h - 174.0).max(32.0));
        let old_clip = p.clip;
        p.clip = viewport;
        for def in SCHEMA {
            if def.section != section {
                continue;
            }
            let y = 108.0 + (row as f32 - self.scroll as f32) * 36.0;
            row += 1;
            if y + 32.0 < viewport.y || y > viewport.y + viewport.h {
                continue;
            }
            p.text(25.0, y + 6.0, def.label, TEXT);
            self.control(
                p,
                Widget::Setting(def.id),
                Rect::new((w - 130.0).max(210.0), y, 102.0, 30.0),
            );
            let mut text = TextBuffer::default();
            let value = self.settings.get(def.id);
            let _ = write!(text, "{value}");
            if self.pending_settings & (1 << def.id as u32) != 0 {
                let _ = write!(text, "*");
            }
            p.centered(
                Rect::new((w - 130.0).max(210.0), y, 102.0, 30.0),
                text.as_str(),
                ACCENT,
            );
        }
        if section == Section::Controls {
            for def in crate::bindings::DEFAULTS {
                let y = 108.0 + (row as f32 - self.scroll as f32) * 36.0;
                row += 1;
                if y + 32.0 < viewport.y || y > viewport.y + viewport.h {
                    continue;
                }
                p.text(25.0, y + 6.0, def.label, TEXT);
                self.control(
                    p,
                    Widget::Binding(def.action),
                    Rect::new(w - 190.0, y, 160.0, 30.0),
                );
                p.centered(
                    Rect::new(w - 190.0, y, 160.0, 30.0),
                    if self.focus == Focus::Binding(def.action) {
                        "Нажмите клавишу"
                    } else {
                        self.bindings.code(def.action)
                    },
                    ACCENT,
                );
            }
        }
        p.clip = old_clip;
        if section == Section::Graphics {
            self.button(
                p,
                Widget::Preset(Preset::Default),
                Rect::new(24.0, h - 64.0, 170.0, 27.0),
                "По умолчанию",
            );
            self.button(
                p,
                Widget::Preset(Preset::Heavy),
                Rect::new(204.0, h - 64.0, 150.0, 27.0),
                "Тяжелее",
            );
        }
        self.button(
            p,
            Widget::ResetSection,
            Rect::new((w - 330.0).max(24.0), h - 64.0, 170.0, 27.0),
            "Сброс раздела",
        );
        self.button(
            p,
            Widget::Back,
            Rect::new(w - 145.0, h - 64.0, 120.0, 27.0),
            "Готово",
        );
        p.text(
            24.0,
            h - 28.0,
            if section == Section::Graphics && self.pending_settings != 0 && self.status.is_empty()
            {
                "* сохранено, ещё не применено"
            } else {
                &self.status
            },
            MUTED,
        );
    }
    fn hud(&mut self, p: &mut Painter<'_>, w: f32, h: f32) {
        let start = w * 0.5 - 9.0 * 18.0;
        for slot in 0..9 {
            let rect = Rect::new(start + slot as f32 * 36.0, h - 43.0, 34.0, 34.0);
            p.rect(rect, 0xc01c2127);
            p.border(
                rect,
                if slot == self.snapshot.selected as usize {
                    ACCENT
                } else {
                    0xff6a7074
                },
            );
            let stack = self.snapshot.inventory[slot];
            if !stack.is_empty() {
                p.item(
                    Rect::new(rect.x + 5.0, rect.y + 5.0, 24.0, 24.0),
                    stack.item(),
                );
                if stack.count() > 1 {
                    let mut text = TextBuffer::default();
                    let _ = write!(text, "{}", stack.count());
                    p.text(rect.x + 14.0, rect.y + 17.0, text.as_str(), TEXT);
                }
            }
        }
        for heart in 0..10 {
            let x = start + heart as f32 * 15.0;
            p.rect(Rect::new(x, h - 63.0, 11.0, 7.0), 0xff4b343b);
            p.rect(Rect::new(x + 2.0, h - 66.0, 7.0, 11.0), 0xff4b343b);
            for half in 0..2 {
                if heart * 2 + half < usize::from(self.snapshot.hp) {
                    p.rect(
                        Rect::new(x + half as f32 * 5.0, h - 63.0, 6.0, 7.0),
                        0xffc64e56,
                    );
                    p.rect(
                        Rect::new(x + 2.0 + half as f32 * 4.0, h - 66.0, 3.0, 11.0),
                        0xffc64e56,
                    );
                }
            }
            let food = if heart * 2 < usize::from(self.snapshot.food) {
                0xffd99c60
            } else {
                0xff574a3e
            };
            if self.snapshot.food_known {
                p.rect(
                    Rect::new(start + 180.0 + heart as f32 * 14.0, h - 63.0, 9.0, 9.0),
                    food,
                );
            }
            if self.snapshot.armor > 0 {
                let armor = if heart * 2 < usize::from(self.snapshot.armor) {
                    0xffb6c8cd
                } else {
                    0xff3f484f
                };
                p.rect(Rect::new(x, h - 80.0, 11.0, 5.0), armor);
                p.rect(Rect::new(x + 2.0, h - 77.0, 7.0, 7.0), armor);
            }
        }
        p.rect(
            Rect::new(w * 0.5 - 1.0, h * 0.5 - 5.0, 2.0, 10.0),
            0xddece9e2,
        );
        p.rect(
            Rect::new(w * 0.5 - 5.0, h * 0.5 - 1.0, 10.0, 2.0),
            0xddece9e2,
        );
    }
    fn inventory_screen(&mut self, p: &mut Painter<'_>, w: f32, h: f32) {
        let panel = Rect::new((w - 650.0) * 0.5, (h - 358.0) * 0.5, 650.0, 358.0);
        p.rect(panel, 0xff242a30);
        p.border(panel, 0xff667078);
        let x = panel.x + 20.0;
        let y = panel.y + 18.0;
        p.text(
            x,
            y,
            match self.screen {
                Screen::Workbench => "Верстак",
                Screen::Container => "Сундук",
                Screen::Furnace => "Печь",
                Screen::Machine => "Машина",
                _ => "Инвентарь",
            },
            TEXT,
        );
        if self.screen == Screen::Furnace {
            for (slot, dx, dy) in [(0, 98.0, 32.0), (1, 98.0, 105.0), (2, 227.0, 68.0)] {
                if slot < usize::from(self.snapshot.container_slots) {
                    self.slot(
                        p,
                        41 + slot as u16,
                        Rect::new(x + dx, y + dy, 32.0, 32.0),
                        self.snapshot.container[slot],
                    );
                }
            }
            p.rect(Rect::new(x + 109.0, y + 72.0, 10.0, 25.0), 0xff15191d);
            let fuel = f32::from(self.snapshot.furnace_fuel.min(1000)) / 1000.0;
            p.rect(
                Rect::new(x + 109.0, y + 97.0 - 25.0 * fuel, 10.0, 25.0 * fuel),
                0xffe8a756,
            );
            p.rect(Rect::new(x + 153.0, y + 80.0, 54.0, 8.0), 0xff15191d);
            p.rect(
                Rect::new(
                    x + 153.0,
                    y + 80.0,
                    54.0 * f32::from(self.snapshot.machine_progress.min(1000)) / 1000.0,
                    8.0,
                ),
                ACCENT,
            );
        } else if matches!(self.screen, Screen::Container | Screen::Machine) {
            for slot in 0..usize::from(self.snapshot.container_slots.min(54)) {
                let rect = Rect::new(
                    x + (slot % 9) as f32 * 34.0,
                    y + 30.0 + (slot / 9) as f32 * 34.0,
                    32.0,
                    32.0,
                );
                self.slot(p, slot as u16 + 41, rect, self.snapshot.container[slot]);
            }
            if self.screen == Screen::Machine {
                let mut text = TextBuffer::default();
                let _ = write!(text, "Энергия: {} Дж", self.snapshot.energy);
                p.text(x + 6.0, y + 108.0, text.as_str(), ACCENT);
                p.rect(Rect::new(x + 6.0, y + 136.0, 250.0, 8.0), 0xff12171b);
                p.rect(
                    Rect::new(
                        x + 6.0,
                        y + 136.0,
                        250.0 * f32::from(self.snapshot.machine_progress.min(1000)) / 1000.0,
                        8.0,
                    ),
                    ACCENT,
                );
                for face in 0..6 {
                    let mut label = TextBuffer::default();
                    let _ = write!(label, "{}:{}", face + 1, self.snapshot.sides[face]);
                    self.control(
                        p,
                        Widget::Side(face as u8),
                        Rect::new(x + face as f32 * 50.0, y + 151.0, 46.0, 26.0),
                    );
                    p.centered(
                        Rect::new(x + face as f32 * 50.0, y + 151.0, 46.0, 26.0),
                        label.as_str(),
                        TEXT,
                    );
                }
            }
        } else {
            let size = if self.screen == Screen::Workbench {
                3
            } else {
                2
            };
            for slot in 0..size * size {
                let rect = Rect::new(
                    x + 95.0 + (slot % size) as f32 * 34.0,
                    y + 32.0 + (slot / size) as f32 * 34.0,
                    32.0,
                    32.0,
                );
                let index = (slot / size) * 3 + slot % size;
                self.slot(p, 100 + index as u16, rect, self.snapshot.craft[index]);
            }
            p.text(x + 211.0, y + 73.0, "→", MUTED);
            self.control(
                p,
                Widget::TakeCraft,
                Rect::new(x + 259.0, y + 64.0, 38.0, 38.0),
            );
            if !self.snapshot.craft_result.is_empty() {
                p.item(
                    Rect::new(x + 267.0, y + 71.0, 24.0, 24.0),
                    self.snapshot.craft_result.item(),
                );
                Self::stack_count(
                    p,
                    Rect::new(x + 259.0, y + 64.0, 38.0, 38.0),
                    self.snapshot.craft_result,
                );
            }
        }
        let base = panel.y + 194.0;
        for row in 0..3 {
            for col in 0..9 {
                let slot = 9 + row * 9 + col;
                self.slot(
                    p,
                    slot as u16,
                    Rect::new(x + col as f32 * 34.0, base + row as f32 * 34.0, 32.0, 32.0),
                    self.snapshot.inventory[slot],
                );
            }
        }
        for slot in 0..9 {
            self.slot(
                p,
                slot as u16,
                Rect::new(x + slot as f32 * 34.0, base + 110.0, 32.0, 32.0),
                self.snapshot.inventory[slot],
            );
        }
        for (row, slot) in [39, 38, 37, 36, 40].into_iter().enumerate() {
            self.slot(
                p,
                slot as u16,
                Rect::new(x + 322.0, y + 32.0 + row as f32 * 34.0, 32.0, 32.0),
                self.snapshot.inventory[slot],
            );
        }
        if !self.bookmarks.is_empty() {
            let bx = panel.x - 46.0;
            p.rect(Rect::new(bx - 4.0, panel.y, 42.0, 318.0), 0xe0242a30);
            p.text(bx + 8.0, panel.y + 8.0, "A", ACCENT);
            for index in 0..8 {
                if let Some(item) = self
                    .bookmarks
                    .iter()
                    .nth(self.bookmark_scroll + index)
                    .copied()
                {
                    let rect = Rect::new(bx, panel.y + 36.0 + index as f32 * 34.0, 32.0, 32.0);
                    self.control(p, Widget::Bookmark(item), rect);
                    p.item(Rect::new(rect.x + 4.0, rect.y + 4.0, 24.0, 24.0), item);
                }
            }
        }
        self.nei(p, panel.x + 408.0, panel.y + 16.0);
        p.text(panel.x + 20.0, panel.y + 340.0, &self.status, MUTED);
        self.button(
            p,
            Widget::Back,
            Rect::new(panel.x + 408.0, panel.y + 315.0, 215.0, 28.0),
            "Закрыть",
        );
    }
    fn nei(&mut self, p: &mut Painter<'_>, x: f32, y: f32) {
        p.text(x, y, "Предметы · R / U / A", MUTED);
        self.control(p, Widget::NeiSearch, Rect::new(x, y + 25.0, 215.0, 28.0));
        p.text(
            x + 6.0,
            y + 29.0,
            if self.search.is_empty() {
                "Поиск…"
            } else {
                &self.search
            },
            TEXT,
        );
        for index in 0..30 {
            let at = self.nei_page * 30 + index;
            if let Some(item) = self.filtered.get(at).copied() {
                let rect = Rect::new(
                    x + (index % 5) as f32 * 42.0,
                    y + 62.0 + (index / 5) as f32 * 34.0,
                    36.0,
                    32.0,
                );
                self.control(p, Widget::Nei(item), rect);
                p.item(Rect::new(rect.x + 6.0, rect.y + 4.0, 24.0, 24.0), item);
            }
        }
        let mut text = TextBuffer::default();
        let _ = write!(
            text,
            "{}/{} · {} предметов",
            self.nei_page + 1,
            self.filtered.len().div_ceil(30).max(1),
            self.filtered.len()
        );
        p.text(x, y + 270.0, text.as_str(), MUTED);
    }
    fn recipe_screen(&mut self, p: &mut Painter<'_>, w: f32, h: f32) {
        self.title(
            p,
            w,
            if self.recipe_uses {
                "Применения"
            } else {
                "Рецепты"
            },
        );
        let x = (w - 460.0) * 0.5;
        let y = 85.0;
        if let Some(recipe) = self.recipes.get(self.recipe_page).copied() {
            match recipe {
                RecipeRef::Craft(id) => {
                    let recipe = self.book.get(id).unwrap();
                    let name = rules::item(recipe.output_item())
                        .map(|item| item.name)
                        .unwrap_or("Рецепт");
                    let output = recipe.output_item();
                    let count = recipe.output_count();
                    let kind = *recipe.kind();
                    p.text(x, y, name, TEXT);
                    let cells = match kind {
                        rules::crafting::RecipeKind::Shaped(pattern) => {
                            let mut cells = [None; 9];
                            for row in 0..pattern.height() {
                                for col in 0..pattern.width() {
                                    cells[row * 3 + col] =
                                        pattern.cells()[row * pattern.width() + col];
                                }
                            }
                            cells
                        }
                        rules::crafting::RecipeKind::Shapeless { ingredients, .. } => ingredients,
                        rules::crafting::RecipeKind::Repair => [None; 9],
                    };
                    for (index, ingredient) in cells.into_iter().enumerate() {
                        let rect = Rect::new(
                            x + (index % 3) as f32 * 42.0,
                            y + 40.0 + (index / 3) as f32 * 42.0,
                            38.0,
                            38.0,
                        );
                        p.rect(rect, 0xff1b2228);
                        p.border(rect, 0xff657078);
                        if let Some(ingredient) = ingredient {
                            p.item(
                                Rect::new(rect.x + 7.0, rect.y + 7.0, 24.0, 24.0),
                                ingredient.items()[0],
                            );
                        }
                    }
                    p.text(x + 150.0, y + 90.0, "→", ACCENT);
                    p.item(Rect::new(x + 198.0, y + 70.0, 48.0, 48.0), output);
                    let mut text = TextBuffer::default();
                    let _ = write!(text, "× {count}");
                    p.text(x + 260.0, y + 90.0, text.as_str(), TEXT);
                    self.button(
                        p,
                        Widget::RecipePlus(id.raw()),
                        Rect::new(x + 180.0, y + 145.0, 200.0, 30.0),
                        "+ в верстак",
                    );
                }
                RecipeRef::Machine(recipe) => {
                    let name = match recipe.kind {
                        rules::machine_recipes::MachineKind::Infuse => {
                            "Металлургический наполнитель"
                        }
                        rules::machine_recipes::MachineKind::Enrich => "Камера обогащения",
                        rules::machine_recipes::MachineKind::Crush => "Дробитель",
                        rules::machine_recipes::MachineKind::Purify => "Очиститель",
                        rules::machine_recipes::MachineKind::Smelt => "Печь",
                        rules::machine_recipes::MachineKind::ChemicalConversion => {
                            "Химическое преобразование"
                        }
                        rules::machine_recipes::MachineKind::EnergyConversion => {
                            "Преобразование энергии"
                        }
                    };
                    p.text(x, y, name, TEXT);
                    p.item(Rect::new(x + 30.0, y + 65.0, 48.0, 48.0), recipe.input);
                    p.text(x + 105.0, y + 78.0, "→", ACCENT);
                    match recipe.product {
                        rules::machine_recipes::Product::Item { item, count } => {
                            p.item(Rect::new(x + 160.0, y + 65.0, 48.0, 48.0), item);
                            let mut text = TextBuffer::default();
                            let _ = write!(text, "× {count}");
                            p.text(x + 225.0, y + 80.0, text.as_str(), TEXT);
                        }
                        rules::machine_recipes::Product::Substance { substance, amount } => {
                            let mut text = TextBuffer::default();
                            let _ = write!(
                                text,
                                "{}: {} мБ",
                                rules::machine_recipes::sub_name(substance),
                                amount
                            );
                            p.text(x + 160.0, y + 80.0, text.as_str(), TEXT);
                        }
                        rules::machine_recipes::Product::Energy { joules } => {
                            let mut text = TextBuffer::default();
                            let _ = write!(text, "{joules} Дж");
                            p.text(x + 160.0, y + 80.0, text.as_str(), TEXT);
                        }
                    }
                    if let Some(reagent) = recipe.reagent {
                        let mut text = TextBuffer::default();
                        let _ = write!(
                            text,
                            "{} · {} мБ",
                            rules::machine_recipes::sub_name(reagent.substance),
                            reagent.amount
                        );
                        p.text(x, y + 150.0, text.as_str(), MUTED);
                    }
                }
            }
        } else {
            p.text(x, y + 40.0, "Рецептов не найдено", MUTED);
        }
        self.button(
            p,
            Widget::RecipePrev,
            Rect::new(x, y + 220.0, 90.0, 30.0),
            "←",
        );
        self.button(
            p,
            Widget::RecipeNext,
            Rect::new(x + 110.0, y + 220.0, 90.0, 30.0),
            "→",
        );
        self.button(
            p,
            Widget::RecipeBack,
            Rect::new(x + 230.0, y + 220.0, 180.0, 30.0),
            "К предметам",
        );
        let mut text = TextBuffer::default();
        let _ = write!(
            text,
            "{} / {}",
            self.recipe_page + 1,
            self.recipes.len().max(1)
        );
        p.text(x, y + 265.0, text.as_str(), MUTED);
        p.text(
            x,
            h - 30.0,
            "Shift + «+» — заполнить на несколько изготовлений",
            MUTED,
        );
    }
    fn resource_screen(&mut self, p: &mut Painter<'_>, w: f32, h: f32) {
        self.title(p, w, "Локальные ресурсы");
        let x = (w - 500.0) * 0.5;
        p.text(x, 90.0, "Ваши файлы остаются на этом устройстве.", TEXT);
        self.button(
            p,
            Widget::ImportPack,
            Rect::new(x, 135.0, 500.0, 34.0),
            "Загрузить zip-набор текстур и звуков",
        );
        self.button(
            p,
            Widget::ImportMinecraft,
            Rect::new(x, 185.0, 500.0, 34.0),
            "Звуки из своей папки .minecraft",
        );
        self.button(
            p,
            Widget::Credits,
            Rect::new(x, 235.0, 500.0, 34.0),
            "Авторы и лицензии",
        );
        self.button(
            p,
            Widget::Back,
            Rect::new(w * 0.5 - 100.0, h - 65.0, 200.0, 32.0),
            "Готово",
        );
    }
    fn crossroads_screen(&mut self, p: &mut Painter<'_>, w: f32, h: f32) {
        self.title(p, w, "Перекрёсток");
        let x = (w - 540.0) * 0.5;
        p.text(x, 90.0, "Защищённая площадь и витрина машин", TEXT);
        p.text(x, 125.0, "Время неба — только для вас", MUTED);
        self.button(p, Widget::Day, Rect::new(x, 165.0, 160.0, 34.0), "День");
        self.button(
            p,
            Widget::Night,
            Rect::new(x + 180.0, 165.0, 160.0, 34.0),
            "Ночь",
        );
        self.button(
            p,
            Widget::ServerTime,
            Rect::new(x + 360.0, 165.0, 160.0, 34.0),
            "Как в мире",
        );
        for (index, item) in [
            rules::ids::HEAT_GENERATOR,
            rules::ids::METALLURGIC_INFUSER,
            rules::ids::ENRICHMENT_CHAMBER,
            rules::ids::CRUSHER,
            rules::ids::ENERGY_CUBE,
        ]
        .into_iter()
        .enumerate()
        {
            let rect = Rect::new(x + index as f32 * 104.0, 230.0, 80.0, 60.0);
            self.control(p, Widget::Nei(item), rect);
            p.item(Rect::new(rect.x + 20.0, rect.y + 10.0, 40.0, 40.0), item);
        }
        self.button(
            p,
            Widget::Back,
            Rect::new(w * 0.5 - 100.0, h - 65.0, 200.0, 32.0),
            "Назад",
        );
    }
    fn players(&self, p: &mut Painter<'_>, w: f32, h: f32) {
        let x = w * 0.5 - 180.0;
        p.rect(
            Rect::new(
                x,
                45.0,
                360.0,
                (self.snapshot.players.len() as f32 * 25.0 + 40.0).min(h - 60.0),
            ),
            0xee1b2128,
        );
        p.text(x + 12.0, 52.0, "Игроки", ACCENT);
        for (index, player) in self.snapshot.players.iter().take(12).enumerate() {
            p.text(x + 12.0, 82.0 + index as f32 * 25.0, &player.name, TEXT);
            let mut text = TextBuffer::default();
            if let Some(ping) = player.ping_ms {
                let _ = write!(text, "{ping} мс");
            } else {
                let _ = write!(text, "—");
            }
            p.text(x + 250.0, 82.0 + index as f32 * 25.0, text.as_str(), MUTED);
        }
    }
    fn chat_screen(&self, p: &mut Painter<'_>, w: f32, h: f32) {
        let width = (w - 24.0).min(650.0);
        let bottom = h - 90.0;
        let count = self.chat.len().min(8);
        p.rect(
            Rect::new(
                10.0,
                bottom - count as f32 * 23.0 - 5.0,
                width,
                count as f32 * 23.0 + 10.0,
            ),
            0xa610151a,
        );
        for (row, message) in self.chat.iter().rev().take(8).rev().enumerate() {
            p.text(18.0, bottom - (count - row) as f32 * 23.0, message, TEXT);
        }
        if self.focus == Focus::Chat {
            p.rect(Rect::new(10.0, h - 72.0, width, 30.0), 0xf0161c23);
            p.border(Rect::new(10.0, h - 72.0, width, 30.0), ACCENT);
            p.text(18.0, h - 66.0, &self.chat_input, TEXT);
        }
    }
}
