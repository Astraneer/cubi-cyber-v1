//! Mekanism у клиента (C7 = Ф4): окно машины из виджетов (данные — таблица машин world/mek, значения — снимок
//! Msg::WinData в своей копии сущности: пиксели и подсказки считаются тем же кодом Rust, JS только рисует), модели
//! машин и кабелей для прохода моделей (web/render/chest.js), свечение кабелей по Msg::NetGlow, звук-петли машин
//! (список излучателей в радиусе 16 каждый тик — интерфейс для команды звука).

use std::collections::HashSet;

use crate::abi::{Ev, Sfx, SlotView};
use crate::cell::{block_of, offset, state_of, Cell};
use crate::game::client::ClientSim;
use crate::game::screen::{Menu, MenuKind};
use crate::proto::Act;
use crate::registry::blocks::{bdef, BeKind, MekKind, ST_LIT};
use crate::world::grid::{arm, arms_of, tkind, Kind, ARM_NONE};
use crate::world::mek::{mek_def, MekBe, Role};
use crate::world::WorldData;

/// Радиус звук-петли машины (ванильное линейное затухание, MEK-01 §7).
pub(crate) const LOOP_R: f64 = 16.0;

/// Вид слота машины для JS (рамка по роли).
pub(crate) fn mek_slot_view(r: Role) -> i32 {
    (match r {
        Role::In => SlotView::MekIn,
        Role::Out => SlotView::MekOut,
        Role::Energy | Role::Discharge => SlotView::MekEnergy,
        Role::Extra => SlotView::MekExtra,
        Role::Fuel => SlotView::MekFuel,
        Role::Charge => SlotView::MekCharge,
        Role::Upgrade => SlotView::MekExtra,
        Role::Fluid => SlotView::MekIn,
    }) as i32
}

impl ClientSim {
    /// Окно машины в клетке p по виду её блока (WinOpen с сервера).
    pub(crate) fn mek_menu(&self, p: [i32; 3]) -> Option<Menu> {
        let b = self.world.d.borrow_mut().get_block(p[0], p[1], p[2]);
        match bdef(b).be {
            Some(BeKind::Mek(k)) => Some(Menu::mek(p, k)),
            _ => None,
        }
    }

    /// Машина открытого окна: (клетка, вид).
    fn mek_open(&self) -> Option<([i32; 3], MekKind)> {
        match self.ctl.screen.as_ref()?.kind {
            MenuKind::Mek(p, k) => Some((p, k)),
            _ => None,
        }
    }
    fn mek_with<R>(&self, f: impl FnOnce(&MekBe) -> R) -> Option<R> {
        let (p, _) = self.mek_open()?;
        let w = self.world.d.borrow();
        w.be().mek(p).map(f)
    }

    /// Заголовок окна машины.
    pub(crate) fn screen_title(&self) -> String {
        self.mek_open().map_or(String::new(), |(_, k)| mek_def(k).title.to_string())
    }
    /// Виджеты окна машины: [число, затем на виджет: вид (abi Widget), x, y, w, h].
    pub(crate) fn screen_widgets(&self) -> Vec<i32> {
        let Some((_, k)) = self.mek_open() else { return Vec::new() };
        let d = mek_def(k);
        let mut v = vec![d.widgets.len() as i32];
        for w in d.widgets {
            v.extend_from_slice(&[w.kind as i32, w.x as i32, w.y as i32, w.w as i32, w.h as i32]);
        }
        v
    }
    /// Значения виджетов (раз в кадр): на виджет [пиксели, флаги] (MekBe::widget_values).
    pub(crate) fn screen_values(&self) -> Vec<i32> {
        self.mek_with(|m| m.widget_values()).unwrap_or_default()
    }
    /// Подсказка или текст виджета i.
    pub(crate) fn screen_tip(&self, i: usize) -> String {
        self.mek_with(|m| m.tip(i)).unwrap_or_default()
    }
    /// Кнопка окна (виджет i): у себя сразу (предсказание), серверу — Act::WinButton.
    pub(crate) fn screen_button(&mut self, i: usize) {
        let Some((p, _)) = self.mek_open() else { return };
        // Витрина на Перекрёстке (G13): кнопки окна только у сервера и только вне зоны — там окно лишь смотрят.
        if crate::game::interact::protected(&self.world.d.borrow(), p) {
            return;
        }
        if let Some(m) = self.world.d.borrow_mut().be_mut().mek_mut(p) {
            m.press(i);
        }
        self.act(Act::WinButton { win: self.win, widget: i as u8 });
    }

    /// Стороны машин с сервера (Msg::MekSides) — в зеркало сущностей.
    pub(crate) fn on_mek_sides(&mut self, list: &[([i32; 3], u32)]) {
        let mut w = self.world.d.borrow_mut();
        for &(p, v) in list {
            if let Some(m) = w.be_mut().mek_mut(p) {
                crate::world::mek::sides::unpack_sides(m, v);
            }
        }
    }
    /// Подсветка граней конфигуратором (решение после MEK-08/09: все 6 граней сразу, наше «лучше мода»): пока в руке
    /// конфигуратор в режиме «предметы» — у машин в радиусе 8 по 5 чисел на грань [x, y, z, мировая грань, цвет 0xRRGGBB].
    pub(crate) fn mek_sides_overlay(&self) -> Vec<f32> {
        use crate::world::mek::sides::{dir_of, item_modes, mode_color};
        let h = self.ctl.inv.hand();
        if h.id() != crate::registry::items::I_CONFIGURATOR || h.damage % crate::game::mek_tool::CONF_MODES != crate::game::mek_tool::CONF_ITEMS {
            return Vec::new();
        }
        let p = self.ctl.player.pos;
        let mut w = self.world.d.borrow_mut();
        let list: Vec<([i32; 3], [u8; 6], MekKind)> = w
            .be()
            .iter()
            .filter_map(|(q, _, be)| match be {
                crate::world::blockentity::BlockEntity::Mek(m) => Some((q, m.sides, m.kind)),
                _ => None,
            })
            .collect();
        let mut v = Vec::new();
        for (q, sides, kind) in list {
            let d2 = (0..3).map(|k| (q[k] as f64 + 0.5 - p[k]).powi(2)).sum::<f64>();
            if d2 > 64.0 || item_modes(kind).is_empty() {
                continue;
            }
            let facing = crate::registry::blocks::facing_of(w.get_cell(q[0], q[1], q[2]));
            for (r, &m) in sides.iter().enumerate() {
                v.extend_from_slice(&[q[0] as f32, q[1] as f32, q[2] as f32, dir_of(facing, r) as f32, mode_color(m) as f32]);
            }
        }
        v
    }

    /// Параметр модели машины: вид (MekKind) | активна << 4.
    pub(crate) fn mek_model_param(&self, _w: &WorldData, _p: [i32; 3], c: Cell) -> f32 {
        let k = match bdef(block_of(c)).be {
            Some(BeKind::Mek(k)) => k as u32,
            _ => 0,
        };
        (k | if state_of(c) & ST_LIT != 0 { 16 } else { 0 }) as f32
    }
    /// Параметр модели передатчика (abi Model::Cable): 12 бит рукавов | свечение (уровень) сети << 12 | вид << 22
    /// (0 кабель, 1 транспортёр, 2 труба, 3 трубка).
    pub(crate) fn cable_model_param(&self, p: [i32; 3], c: Cell) -> f32 {
        let g = (self.cable_glow.get(&p).copied().unwrap_or(0) & 1023).min(1000) as u32;
        let kind = match tkind(c) {
            Some(Kind::Item) => 1,
            Some(Kind::Fluid) => 2,
            Some(Kind::Gas) => 3,
            _ => 0,
        };
        (arms_of(state_of(c)) as u32 | g << 12 | kind << 22) as f32
    }

    /// Свечение сетей с сервера (свечение | вещество << 10): каждому передатчику — свечение его сети (обход зеркала от
    /// члена сети по связанным передатчикам того же вида: рукав с обеих сторон).
    pub(crate) fn on_net_glow(&mut self, list: &[([i32; 3], u16)]) {
        self.cable_glow.clear();
        let mut w = self.world.d.borrow_mut();
        for &(a, g) in list {
            let Some(k) = tkind(w.get_cell(a[0], a[1], a[2])) else { continue };
            if self.cable_glow.contains_key(&a) {
                continue;
            }
            let mut seen: HashSet<[i32; 3]> = HashSet::from([a]);
            let mut q = vec![a];
            while let Some(c) = q.pop() {
                self.cable_glow.insert(c, g);
                if seen.len() > 65_536 {
                    break;
                }
                let ac = arms_of(state_of(w.get_cell(c[0], c[1], c[2])));
                for f in 0..6u8 {
                    let n = offset(c, f);
                    if seen.contains(&n) || arm(ac, f) == ARM_NONE {
                        continue;
                    }
                    let nc = w.get_cell(n[0], n[1], n[2]);
                    if tkind(nc) == Some(k) && arm(arms_of(state_of(nc)), f ^ 1) != ARM_NONE {
                        seen.insert(n);
                        q.push(n);
                    }
                }
            }
        }
    }

    /// Звук-петли машин (decisions после MEK-01; интерфейс команды звука — docs/areas/sound.md): каждый тик «клиентской»
    /// фазы — Ev::Emitter у каждой машины с битом «активна» в радиусе 16 от игрока (тепловой — пока горит топливо:
    /// Sfx::LoopGenerator; инфузор — пока работает: Sfx::LoopMachine). Не по сети: бит «активна» есть в зеркале мира.
    pub(crate) fn mek_emitters(&mut self, w: &WorldData) {
        let p = self.ctl.player.pos;
        for (q, _, be) in w.be().iter() {
            let crate::world::blockentity::BlockEntity::Mek(m) = be else { continue };
            let c = [q[0] as f64 + 0.5, q[1] as f64 + 0.5, q[2] as f64 + 0.5];
            let d2 = (c[0] - p[0]).powi(2) + (c[1] - p[1]).powi(2) + (c[2] - p[2]).powi(2);
            let Some(cell) = w.edited_cell(q[0], q[1], q[2]) else { continue };
            if d2 > LOOP_R * LOOP_R || state_of(cell) & ST_LIT == 0 {
                continue;
            }
            let s = match m.kind {
                MekKind::Heat => Sfx::LoopGenerator,
                MekKind::Infuser | MekKind::Enrich | MekKind::Smelter => Sfx::LoopMachine,
                // M4: своих петель дробилки, очистителя и сепаратора пока нет (команда звука) — общий гул машины.
                MekKind::Crusher | MekKind::Purifier | MekKind::Separator => Sfx::LoopMachine,
                // M5: тихий гул работающего насоса (decisions после MEK-13/14) — общий гул машины до своего звука.
                MekKind::Pump => Sfx::LoopMachine,
                MekKind::Cube | MekKind::Wind => continue, // звука ветра пока нет (команда звука)
            };
            self.view.events.push(Ev::Emitter as i32, q, s as i32);
        }
    }
}
