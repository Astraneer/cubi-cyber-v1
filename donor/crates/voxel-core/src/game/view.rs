//! Представление (Ф1, ARCHITECTURE-REVIEW P1-5): то, что нужно только для кадра и звука, — рука от первого лица
//! (hand.rs), таймеры HUD (имя предмета, строка действия), события тика, ГСЧ эффектов (звуки и частицы не сдвигают
//! эталоны дропа — решение после ASP-13). Живёт у клиента (client.rs: ClientSim собирает данные кадра — view, hud,
//! item_draws, model_draws, hand_pose…); описание предметов для HUD (иконка, имя, подсказка) — здесь же. События
//! с адресатом (To) — общие с сервером.

use crate::game::hand::HandState;
use crate::game::sound::Steps;
use crate::game::GameRng;
use crate::registry::blocks::{bdef, layer, tinted};
use crate::registry::ids::{BIRCH_LEAVES, LEAVES, SPRUCE_LEAVES};
use crate::registry::items::{idef, tool_attack, ItemId, ItemKind};

/// Адресат события сервера (MP-0, ревью P2 «Сеть»): всем, одному игроку или всем, кроме него (свои действия игрок уже
/// предсказал — звук и частицы у него были сразу). Радиус слышимости — MP-1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum To {
    All,
    Only(u32),
    Except(u32),
}

impl To {
    /// Доходит ли событие до игрока id.
    pub(crate) fn reaches(self, id: u32) -> bool {
        match self {
            To::All => true,
            To::Only(p) => p == id,
            To::Except(p) => p != id,
        }
    }
}

/// События тика для звуков, частиц и проверок: по 5 чисел [код Ev, x, y, z, параметр] (abi::Ev) и адресат каждого (у
/// клиента — всегда All). Не больше 1000 событий до take — хвост теряется.
#[derive(Default)]
pub(crate) struct Events {
    v: Vec<i32>,
    to: Vec<To>,
}

impl Events {
    /// Событие для всех.
    pub(crate) fn push(&mut self, code: i32, p: [i32; 3], arg: i32) {
        self.push_to(To::All, code, p, arg);
    }
    pub(crate) fn push_to(&mut self, to: To, code: i32, p: [i32; 3], arg: i32) {
        if self.v.len() < 5000 {
            self.v.extend_from_slice(&[code, p[0], p[1], p[2], arg]);
            self.to.push(to);
        }
    }
    /// Забрать накопленное (без адресатов).
    pub(crate) fn take(&mut self) -> Vec<i32> {
        self.to.clear();
        std::mem::take(&mut self.v)
    }
    /// Забрать накопленное с адресатами.
    pub(crate) fn drain(&mut self) -> Vec<(To, [i32; 5])> {
        let v = std::mem::take(&mut self.v);
        let to = std::mem::take(&mut self.to);
        to.into_iter().zip(v.chunks(5).map(|e| [e[0], e[1], e[2], e[3], e[4]])).collect()
    }
}

pub(crate) struct View {
    /// Рука от первого лица (hand.rs, ASP-11).
    pub(crate) hand: HandState,
    /// Имя предмета над хотбаром: 40 тиков (30 + 10 затухания).
    pub(crate) name_fade: i32,
    pub(crate) name_item: ItemId,
    /// Строка действия: 60 тиков.
    pub(crate) overlay: i32,
    pub(crate) overlay_msg: i32,
    /// Ступни в зоне Перекрёстка на прошлом тике (None — ещё не знаем): переход — строка над хотбаром.
    pub(crate) zone_in: Option<bool>,
    /// Ступни в проёме Врат странствий на прошлом тике.
    pub(crate) gate_in: bool,
    /// Небо по кнопке Перекрёстка (G15): выбранное время (доля суток) — только у этого клиента и только в зоне; сдвиг
    /// неба от времени мира (доли суток) сейчас и на прошлом тике — плавный переход за ~2 с.
    pub(crate) sky_pick: Option<f64>,
    pub(crate) sky_off: f64,
    pub(crate) sky_prev: f64,
    pub(crate) events: Events,
    /// Ритм шагов своего игрока (game/sound.rs).
    pub(crate) steps: Steps,
    /// Эффекты: звуки и частицы.
    pub(crate) fx_rng: GameRng,
}

impl View {
    /// Тик неба по кнопке: сдвиг к выбранному времени (или к нулю без выбора) — быстро издали, мягко у цели. now — доля
    /// суток мира. Сдвиг на целые сутки не меняет неба: им держим разницу с целью в пределах полусуток (без лишнего
    /// оборота, когда цель переходит через ±0.5).
    pub(crate) fn sky_step(&mut self, now: f64) {
        self.sky_prev = self.sky_off;
        let target = self.sky_pick.map_or(0.0, |f| (f - now + 0.5).rem_euclid(1.0) - 0.5);
        let turn = (self.sky_off - target).round();
        self.sky_off -= turn;
        self.sky_prev -= turn;
        let d = target - self.sky_off;
        self.sky_off = if d.abs() < 0.002 { target } else { self.sky_off + (d * 0.15).clamp(-0.03, 0.03) };
    }
    /// Сдвиг неба на доле тика alpha.
    pub(crate) fn sky_shift(&self, alpha: f64) -> f64 {
        self.sky_prev + (self.sky_off - self.sky_prev) * alpha
    }

    pub(crate) fn new(seed: u64) -> View {
        View {
            hand: HandState::new(),
            name_fade: 0,
            name_item: 0,
            overlay: 0,
            overlay_msg: 0,
            zone_in: None,
            gate_in: false,
            sky_pick: None,
            sky_off: 0.0,
            sky_prev: 0.0,
            events: Events::default(),
            steps: Steps::new(),
            fx_rng: GameRng(seed ^ 0xEFF0_EC75),
        }
    }
}

/// Имя предмета (ру) для HUD.
pub(crate) fn item_name(id: ItemId) -> String {
    idef(id).name.to_string()
}

/// Иконка предмета для атласа HUD: [1 (куб), слои граней +X, −X, верх, низ, +Z, −Z (как в меше), маска оттенка граней,
/// вид оттенка (1 трава, 2 листва дуба, 3 берёзы, 4 ели)] | [2 (плоский спрайт блока), слой, вид оттенка] |
/// [0 (свой рисунок предмета), номер предмета].
pub(crate) fn item_icon(id: ItemId) -> Vec<u32> {
    let ItemKind::Block(b) = idef(id).kind else { return vec![0, id as u32] };
    let tint_kind = match b {
        LEAVES => 2,
        BIRCH_LEAVES => 3,
        SPRUCE_LEAVES => 4,
        _ if (0..6).any(|f| tinted(b, f)) => 1,
        _ => 0,
    };
    if bdef(b).flat_item {
        return vec![2, layer(b, 0), tint_kind]; // растения и лестница — плоский предмет (item/generated)
    }
    let mut v = vec![1];
    let mut mask = 0;
    for f in 0..6 {
        v.push(layer(b, f));
        if tinted(b, f) {
            mask |= 1 << f;
        }
    }
    v.push(mask);
    v.push(tint_kind);
    v
}

/// Подсказка предмета (ASP-08 §6.4): строки «цвет|текст» через \n; цвета Minecraft: f белый, 7 серый, 2 тёмно-зелёный,
/// 9 синий. Имя; у инструментов — пустая строка, «В ведущей руке:», урон и скорость атаки; у брони — защита.
pub(crate) fn item_tooltip(id: ItemId) -> String {
    let d = idef(id);
    let mut lines = vec![format!("f|{}", d.name)];
    let num = |v: f32| if v.fract() == 0.0 { format!("{}", v as i32) } else { format!("{}", v) };
    if let Some((dmg, speed)) = tool_attack(id) {
        lines.push("f|".into());
        lines.push("7|В ведущей руке:".into());
        lines.push(format!("2| {} урона при атаке", num(dmg)));
        lines.push(format!("2| {} скорости атаки", num(speed)));
    } else if let ItemKind::Armor { slot, points } = d.kind {
        let place = ["ступнях", "ногах", "теле", "голове"][slot as usize];
        lines.push("f|".into());
        lines.push(format!("7|Когда надето на {}:", place));
        lines.push(format!("9|+{} брони", points));
    }
    lines.join("\n")
}
