//! Рука и предмет от первого лица (ASP-11) и экструзия спрайта предмета (ASP-11 §4.1, ASP-07 §8.4) — представление.
//!
//! Логика — по тикам (HandState у клиента, ClientSim): опускание и подъём (updateHeldItems), перезарядка атаки (lastAttackedTicks),
//! отставание за мышью (renderYaw/Pitch), еда. Матрица кадра — чистая функция (ClientSim::hand_pose): JS только умножает
//! её на свою проекцию руки (FOV 70, near 0.05) и рисует (web/render/hand.js).
//! Соглашения ASP-11: пространство вида +X вправо, +Y вверх, взгляд по −Z, единица — блок; углы — градусы;
//! шаги матрицы — в порядке вызовов Java (M ← M·X, последний шаг применяется к вершине первым).
//! Матрица 4×4 f32 по столбцам (m[столбец·4 + строка]), как m4 в JS.
//!
//! «Идентичность» стека (ASP-11 §2.1, §11.1; решение после ASP-11). В Java рука сравнивает нарисованный стек с
//! реальным: тот же объект или равное значение (areEqual). Правки «на месте» (ставление −1, Q, доел −1) объект не
//! меняют — нырка нет; прочность и подбор приходят от сервера новым объектом — нырок. У нас сервера нет, и у стека
//! нет поля-идентичности: HandState сам выдаёт стеку руки номер (uid) по наблюдению от тика к тику — та же ячейка
//! и только уменьшение количества (тот же предмет и износ) — номер прежний («на месте»); любая другая правка или
//! смена ячейки — новый номер. Исключение: инструмент исчез при неизменной ячейке — поломка (новый объект, нырок).
//! Сигналы Java, которые наблюдением не видны, ставит код игры явно: resetEquipProgress — `hand.reequip(рука)`,
//! resetLastAttackedTicks — `hand.reset_attack()`, правка на месте без нырка в фазе ввода (Q) — `hand.quiet(рука)`.
//! Ставление блока, клик в воздух и отпускание ЛКМ при ломании распознаются в ClientSim::hand_tick сами.

use crate::abi::P_ATTACK;
use crate::game::inventory::OFFHAND;
use crate::game::view::item_icon;
use crate::game::client::ClientSim;
use crate::game::interact::eyes_in_water;
use crate::registry::blocks::{bdef, Place, AXE, HOE, PICKAXE, SHOVEL};
use crate::registry::items::{idef, ItemId, ItemKind};
use crate::registry::ids::AIR;
use crate::world::stack::ItemStack;

pub(crate) type M4 = [f32; 16];

pub(crate) const M4_ID: M4 = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

pub(crate) fn m4_mul(a: &M4, b: &M4) -> M4 {
    let mut o = [0.0f32; 16];
    for c in 0..4 {
        for r in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += a[k * 4 + r] * b[c * 4 + k];
            }
            o[c * 4 + r] = s;
        }
    }
    o
}
pub(crate) fn m4_chain(ms: &[M4]) -> M4 {
    ms.iter().fold(M4_ID, |m, x| m4_mul(&m, x))
}
pub(crate) fn m4_t(x: f32, y: f32, z: f32) -> M4 {
    let mut m = M4_ID;
    m[12] = x;
    m[13] = y;
    m[14] = z;
    m
}
pub(crate) fn m4_s(x: f32, y: f32, z: f32) -> M4 {
    let mut m = M4_ID;
    m[0] = x;
    m[5] = y;
    m[10] = z;
    m
}
/// Поворот на deg градусов вокруг оси (0 — X, 1 — Y, 2 — Z), правило правой руки (RotationAxis.POSITIVE_*).
pub(crate) fn m4_r(axis: usize, deg: f32) -> M4 {
    let (s, c) = (deg * (std::f32::consts::PI / 180.0)).sin_cos();
    let mut m = M4_ID;
    let (i, j) = match axis {
        0 => (1, 2),
        1 => (2, 0),
        _ => (0, 1),
    };
    m[i * 4 + i] = c;
    m[j * 4 + j] = c;
    m[i * 4 + j] = s; // столбец i, строка j
    m[j * 4 + i] = -s;
    m
}
pub(crate) fn rx(d: f32) -> M4 {
    m4_r(0, d)
}
pub(crate) fn ry(d: f32) -> M4 {
    m4_r(1, d)
}
pub(crate) fn rz(d: f32) -> M4 {
    m4_r(2, d)
}

// ---------------------------------------------------------------- display-трансформы (ASP-11 §3.6)

/// display(rot, tr, sc) модели: поворот XYZ (градусы), сдвиг в 1/16, масштаб.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Disp {
    pub(crate) rot: [f32; 3],
    pub(crate) tr: [f32; 3],
    pub(crate) sc: f32,
}
/// item/generated и item/handheld (плоские предметы и инструменты — в первом лице одинаково).
pub(crate) const D_FLAT: Disp = Disp { rot: [0.0, -90.0, 25.0], tr: [1.13, 3.2, 1.13], sc: 0.68 };
/// block/block — все кубы.
pub(crate) const D_BLOCK: Disp = Disp { rot: [0.0, 45.0, 0.0], tr: [0.0; 3], sc: 0.40 };
/// block/orientable_with_bottom — печь лицом к игроку; наш блочный сундук — так же (решение ASP-11 §7 п. 4).
pub(crate) const D_FACING: Disp = Disp { rot: [0.0, 135.0, 0.0], tr: [0.0; 3], sc: 0.40 };

/// Transformation.apply: левая рука отражает rot.y, rot.z и tr.x. У handheld свой lefthand — то же самое числами.
pub(crate) fn display(d: &Disp, left: bool) -> M4 {
    let (ry_, rz_, tx) = if left { (-d.rot[1], -d.rot[2], -d.tr[0]) } else { (d.rot[1], d.rot[2], d.tr[0]) };
    m4_chain(&[m4_t(tx / 16.0, d.tr[1] / 16.0, d.tr[2] / 16.0), rx(d.rot[0]), ry(ry_), rz(rz_), m4_s(d.sc, d.sc, d.sc)])
}

/// Модель предмета в руке: 1 — плоский (экструзия спрайта), 2 — куб блока. Тот же признак, что у иконки HUD:
/// item_icon [1 …] — куб, остальное (свой рисунок, растение) — плоский.
pub(crate) fn item_model(id: ItemId) -> u32 {
    if id == 0 {
        return 0;
    }
    if item_icon(id).first() == Some(&1) {
        2
    } else {
        1
    }
}
pub(crate) fn item_display(id: ItemId) -> Disp {
    if item_model(id) != 2 {
        return D_FLAT;
    }
    // Блоки «лицом к игроку» (печь, сундук — таблица: place) держатся лицом, как block/orientable_with_bottom.
    match idef(id).kind {
        ItemKind::Block(b) if bdef(b).place == Place::FacePlayer => D_FACING,
        _ => D_BLOCK,
    }
}

// ---------------------------------------------------------------- позы (ASP-11 §3.3–3.5)

/// Предмет, шаги 1–7 §3.3 (без центрирования модели): s — взмах 0…1, e — опущенность (0 — поднят), sx = ±1.
pub(crate) fn item_pose_raw(s: f32, e: f32, sx: f32, d: &Disp, left: bool) -> M4 {
    let pi = std::f32::consts::PI;
    let r = s.sqrt();
    let f = (s * s * pi).sin();
    let g = (r * pi).sin();
    m4_chain(&[
        m4_t(sx * (-0.4 * g), 0.2 * (2.0 * r * pi).sin(), -0.2 * (s * pi).sin()),
        m4_t(sx * 0.56, -0.52 - 0.6 * e, -0.72), // applyEquipOffset
        ry(sx * (45.0 - 20.0 * f)),              // applySwingOffset
        rz(sx * -20.0 * g),
        rx(-80.0 * g),
        ry(sx * -45.0),
        display(d, left),
    ])
}
/// Предмет целиком: модель в координатах 0…1 (центрирование T(−0.5)).
pub(crate) fn item_pose(s: f32, e: f32, sx: f32, d: &Disp, left: bool) -> M4 {
    m4_mul(&item_pose_raw(s, e, sx, d, left), &m4_t(-0.5, -0.5, -0.5))
}

/// Еда (§3.4): f = itemUseTimeLeft − td + 1, max — maxUseTime (еда 32). Вместо взмаха.
pub(crate) fn eat_pose(f: f32, max: f32, e: f32, sx: f32, d: &Disp, left: bool) -> M4 {
    let g = f / max;
    let shake = if g < 0.8 { (f / 4.0 * std::f32::consts::PI).cos().abs() * 0.1 } else { 0.0 };
    let h = 1.0 - (g as f64).powf(27.0) as f32;
    m4_chain(&[
        m4_t(0.0, shake, 0.0),
        m4_t(h * 0.6 * sx, h * -0.5, 0.0),
        ry(sx * h * 90.0),
        rx(h * 10.0),
        rz(sx * h * 30.0),
        m4_t(sx * 0.56, -0.52 - 0.6 * e, -0.72),
        display(d, left),
        m4_t(-0.5, -0.5, -0.5),
    ])
}

/// Пустая правая рука, шаги 1–10 §3.5 (вершины кубоида — в пикселях, их /16 — в arm_pose).
pub(crate) fn arm_pose_raw(s: f32, e: f32) -> M4 {
    let pi = std::f32::consts::PI;
    let r = s.sqrt();
    let l = (r * pi).sin();
    m4_chain(&[
        m4_t(-0.3 * l + 0.64000005, 0.4 * (2.0 * r * pi).sin() - 0.6 - 0.6 * e, -0.4 * (s * pi).sin() - 0.71999997),
        ry(45.0),
        ry(l * 70.0),
        rz((s * s * pi).sin() * -20.0),
        m4_t(-1.0, 3.6, 3.5),
        rz(120.0),
        rx(200.0),
        ry(-135.0),
        m4_t(5.6, 0.0, 0.0),
        // ModelPart правой руки: pivot (−5, 2, 0) px, rotationZYX(roll 0.1 рад, yaw 0, pitch 0).
        m4_t(-5.0 / 16.0, 2.0 / 16.0, 0.0),
        rz(0.1f32.to_degrees()),
    ])
}
/// Рука целиком: вершины кубоида (−3…1, −2…10, −2…2) в пикселях.
pub(crate) fn arm_pose(s: f32, e: f32) -> M4 {
    m4_mul(&arm_pose_raw(s, e), &m4_s(1.0 / 16.0, 1.0 / 16.0, 1.0 / 16.0))
}

/// Покачивание (bobView) из вида игрока: [сдвиг x, y, крен Z°, кивок X°].
pub(crate) fn bob_matrix(b: [f64; 4]) -> M4 {
    m4_chain(&[m4_t(b[0] as f32, b[1] as f32, 0.0), rz(b[2] as f32), rx(b[3] as f32)])
}

// ---------------------------------------------------------------- перезарядка атаки (ASP-11 §2.2, TOOL-04)

/// Скорость атаки предмета в главной руке (атрибут generic.attack_speed): рука и прочие 4.0.
pub(crate) fn attack_speed(id: ItemId) -> f64 {
    match idef(id).kind {
        ItemKind::Sword { .. } => 1.6,
        ItemKind::Tool { mask, tier } => match (mask, tier.level) {
            (PICKAXE, _) => 1.2,
            (SHOVEL, _) => 1.0,
            (AXE, 2) => 0.9,
            (AXE, _) => 0.8,
            (HOE, l) => 1.0 + l as f64, // дерево 1, камень 2, железо 3
            _ => 4.0,
        },
        _ => 4.0,
    }
}

// ---------------------------------------------------------------- состояние

/// Стек руки с номером «идентичности».
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Held {
    pub(crate) stack: ItemStack,
    pub(crate) uid: u32,
}

/// ItemStack.areEqual без объекта: пустые равны; иначе тип, количество и компоненты (износ).
pub(crate) fn same_value(a: &ItemStack, b: &ItemStack) -> bool {
    (a.is_empty() && b.is_empty()) || (!a.is_empty() && !b.is_empty() && a.count == b.count && a.item == b.item && a.damage == b.damage)
}
/// Тот же объект или равное значение.
pub(crate) fn are_equal(a: &Held, b: &Held) -> bool {
    a.uid == b.uid || same_value(&a.stack, &b.stack)
}

/// Снимок перед фазой ввода (для распознавания сигналов Java по итогу фазы).
#[derive(Clone, Copy)]
pub(crate) struct HandPre {
    pub(crate) attack_cd: i32,
    pub(crate) attacks: u16,
    pub(crate) breaking: bool,
    pub(crate) mine_pos: [i32; 3],
    pub(crate) stacks: [(usize, ItemStack); 2],
}

pub(crate) struct HandState {
    /// Нарисованные стеки (могут отставать от инвентаря), 0 — главная рука, 1 — вторая.
    pub(crate) shown: [Held; 2],
    pub(crate) equip: [f32; 2],
    pub(crate) prev: [f32; 2],
    /// Реальные стеки рук на прошлом тике руки и их ячейки.
    pub(crate) real: [Held; 2],
    pub(crate) slot: [usize; 2],
    pub(crate) next_uid: u32,
    /// lastAttackedTicks (PlayerEntity) и копия стека главной руки (selectedItem).
    pub(crate) last_attacked: i32,
    pub(crate) selected: ItemStack,
    /// renderYaw/Pitch: [прошлый, текущий] (градусы Java).
    pub(crate) ryaw: [f32; 2],
    pub(crate) rpitch: [f32; 2],
    /// Правка на месте в фазе ввода без нырка (Q): ставит код выброса.
    pub(crate) quiet: [bool; 2],
    /// Использование (еда), только для отрисовки: рука, itemUseTimeLeft, maxUseTime. Таймер ведёт логика еды: в начале —
    /// start_use (с resetEquipProgress), затем каждый тик после своего tickItemStackUsage — set_use (None — не ест).
    pub(crate) using: Option<(usize, i32, i32)>,
    /// Отладка: есть предмет в руке по кругу (game.hand_debug_eat).
    pub(crate) debug_eat: bool,
}

impl HandState {
    pub(crate) fn new() -> HandState {
        HandState {
            shown: [Held::default(); 2],
            equip: [0.0; 2],
            prev: [0.0; 2],
            real: [Held::default(); 2],
            slot: [0, OFFHAND],
            next_uid: 1,
            last_attacked: 0,
            selected: ItemStack::EMPTY,
            ryaw: [0.0; 2],
            rpitch: [0.0; 2],
            quiet: [false; 2],
            using: None,
            debug_eat: false,
        }
    }
    /// resetEquipProgress: equip = 0 без prev — в кадре предмет сразу внизу.
    pub(crate) fn reequip(&mut self, hand: usize) {
        self.equip[hand] = 0.0;
    }
    /// resetLastAttackedTicks.
    pub(crate) fn reset_attack(&mut self) {
        self.last_attacked = 0;
    }
    /// Правка стека руки на месте в фазе ввода без нырка (выброс Q).
    pub(crate) fn quiet(&mut self, hand: usize) {
        self.quiet[hand] = true;
    }
    /// Начало использования (LivingEntity.setCurrentHand + resetEquipProgress в doItemUse): еда приходит снизу.
    pub(crate) fn start_use(&mut self, hand: usize, max: i32) {
        self.using = Some((hand, max, max));
        self.reequip(hand);
    }
    /// Состояние использования на этот тик: Some((рука, itemUseTimeLeft, maxUseTime)) или None.
    #[allow(dead_code)] // логика еды (позже)
    pub(crate) fn set_use(&mut self, u: Option<(usize, i32, i32)>) {
        self.using = u;
    }
    /// Телепорт, появление, резкая смена взгляда: отставание руки сбрасывается (решение после ASP-11).
    pub(crate) fn snap_look(&mut self, yaw: f32, pitch: f32) {
        self.ryaw = [yaw, yaw];
        self.rpitch = [pitch, pitch];
    }
    /// Номер «идентичности» реального стека руки (см. шапку файла). quiet — правка на месте отмечена кодом игры.
    pub(crate) fn identify(&mut self, h: usize, slot: usize, now: ItemStack, quiet: bool) -> Held {
        let last = self.real[h];
        let keep = slot == self.slot[h]
            && (now == last.stack || quiet || {
                let a = &last.stack;
                let in_place = !a.is_empty() && now.n() < a.n() && (now.is_empty() || (now.item == a.item && now.damage == a.damage));
                // Инструмент кончился сам — поломка: сервер прислал пустой стек (новый объект).
                let broke = now.is_empty() && idef(a.item).max_damage > 0;
                in_place && !broke
            });
        let uid = if keep {
            last.uid
        } else {
            self.next_uid += 1;
            self.next_uid
        };
        let held = Held { stack: now, uid };
        self.real[h] = held;
        self.slot[h] = slot;
        held
    }
    /// updateHeldItems (§2.1): cooldown — getAttackCooldownProgress(1.0) до lastAttackedTicks++ этого тика.
    pub(crate) fn tick_held(&mut self, real: [Held; 2], cooldown: f32) {
        self.prev = self.equip;
        for h in 0..2 {
            let same = are_equal(&self.shown[h], &real[h]);
            if same {
                self.shown[h] = real[h];
            }
            let target = match (same, h) {
                (false, _) => 0.0,
                (true, 0) => cooldown * cooldown * cooldown,
                _ => 1.0,
            };
            self.equip[h] += (target - self.equip[h]).clamp(-0.4, 0.4);
            if self.equip[h] < 0.1 {
                self.shown[h] = real[h];
            }
        }
    }
    /// getAttackCooldownProgress(base) для предмета главной руки.
    pub(crate) fn cooldown(&self, base: f32, item: ItemId) -> f32 {
        let per_tick = (1.0 / attack_speed(item) * 20.0) as f32;
        ((self.last_attacked as f32 + base) / per_tick).clamp(0.0, 1.0)
    }
}

impl ClientSim {
    /// Снимок до фазы ввода.
    pub(crate) fn hand_pre(&self) -> HandPre {
        HandPre {
            attack_cd: self.ctl.attack_cd,
            attacks: self.ctl.presses[P_ATTACK as usize],
            breaking: self.ctl.mine.breaking,
            mine_pos: self.ctl.mine.pos,
            stacks: [(self.ctl.inv.selected, self.ctl.inv.hand()), (OFFHAND, self.ctl.inv.offhand)],
        }
    }

    /// Между фазой ввода и тиком игрока (GameRenderer.tick → updateHeldItems): сигналы фазы ввода, затем опускание и
    /// подъём по «идентичности» стеков.
    pub(crate) fn hand_tick(&mut self, pre: HandPre) {
        // Клик в воздух (doAttack MISS): нажатие ЛКМ при задержке 0 и без блока под прицелом (цель фазы ввода).
        if pre.attacks > 0 && pre.attack_cd == 0 {
            let miss = match self.ctl.target {
                Some(h) => self.world.d.borrow_mut().get_block(h.pos[0], h.pos[1], h.pos[2]) == AIR,
                None => true,
            };
            if miss {
                self.view.hand.reset_attack();
            }
        }
        // cancelBlockBreaking: ломание шло и оборвалось, а блок цел (сломанный — не отмена).
        if pre.breaking && !self.ctl.mine.breaking {
            let p = pre.mine_pos;
            if self.world.d.borrow_mut().get_block(p[0], p[1], p[2]) != AIR {
                self.view.hand.reset_attack();
            }
        }
        let now = [(self.ctl.inv.selected, self.ctl.inv.hand()), (OFFHAND, self.ctl.inv.offhand)];
        let quiet = self.view.hand.quiet;
        for h in 0..2 {
            // Ставление (doItemUse SUCCESS, количество изменилось, стек не пуст) — resetEquipProgress.
            let (s0, a) = pre.stacks[h];
            let (s1, b) = now[h];
            let placed = s0 == s1 && !b.is_empty() && b.item == a.item && b.damage == a.damage && b.count < a.count;
            if placed && !self.view.hand.quiet[h] {
                self.view.hand.reequip(h);
            }
            self.view.hand.quiet[h] = false;
        }
        // Отладочная еда: держимый предмет «съедается» по кругу.
        if self.view.hand.debug_eat && self.view.hand.using.is_none() && !self.ctl.inv.hand().is_empty() {
            self.view.hand.start_use(0, 32);
        }
        let real = [self.view.hand.identify(0, now[0].0, now[0].1, quiet[0]), self.view.hand.identify(1, now[1].0, now[1].1, quiet[1])];
        let cd = self.view.hand.cooldown(1.0, self.ctl.inv.hand().id());
        self.view.hand.tick_held(real, cd);
    }

    /// После тика игрока: renderYaw/Pitch (tickNewAi), lastAttackedTicks++ и смена типа предмета (PlayerEntity.tick);
    /// отладочная еда — свой таймер (tickItemStackUsage: −1 за тик, на 0 — «съедено»).
    pub(crate) fn hand_after_player(&mut self) {
        let hs = &mut self.view.hand;
        let (yaw, pitch) = (self.ctl.player.yaw, self.ctl.player.pitch);
        hs.ryaw = [hs.ryaw[1], hs.ryaw[1] + (yaw - hs.ryaw[1]) * 0.5];
        hs.rpitch = [hs.rpitch[1], hs.rpitch[1] + (pitch - hs.rpitch[1]) * 0.5];
        hs.last_attacked += 1;
        let main = self.ctl.inv.hand();
        if !same_value(&hs.selected, &main) {
            if hs.selected.id() != main.id() {
                hs.last_attacked = 0;
            }
            hs.selected = main;
        }
        if let (true, Some((h, left, max))) = (hs.debug_eat, hs.using) {
            let stack = if h == 0 { main } else { self.ctl.inv.offhand };
            hs.using = if stack.is_empty() || left <= 1 { None } else { Some((h, left - 1, max)) };
        }
    }
}

impl ClientSim {
    /// Рука в кадре (ASP-11 §3): [множитель FOV руки (вода 0.857), число рук N, затем N × 20 чисел:
    /// рука (0 главная, 1 вторая), модель (0 — рука игрока, 1 — плоский предмет, 2 — куб блока), номер предмета, 0,
    /// матрица 4×4 по столбцам — из координат модели в пространство вида (Bob · Lag · поза)]. Модель руки — кубоид
    /// (−3…1, −2…10, −2…2) в пикселях со скином 64×64; предмет — координаты 0…1 (спрайт или куб).
    /// alpha — доля тика; bob — опция «покачивание».
    pub(crate) fn hand_pose(&self, alpha: f64, bob: bool) -> Vec<f32> {
        let td = alpha as f32;
        let hs = &self.view.hand;
        let p = &self.ctl.player;
        let fov = if eyes_in_water(&mut self.world.d.borrow_mut(), &self.ctl.player) { 0.85714287 } else { 1.0 };
        let v = p.view(alpha);
        let mut base = if bob { bob_matrix([v[6], v[7], v[8], v[9]]) } else { M4_ID };
        // Отставание за мышью: взгляд сейчас (без интерполяции) против сглаженного renderYaw/Pitch.
        let dp = p.pitch - (hs.rpitch[0] + (hs.rpitch[1] - hs.rpitch[0]) * td);
        let dy = p.yaw - (hs.ryaw[0] + (hs.ryaw[1] - hs.ryaw[0]) * td);
        base = m4_chain(&[base, rx(dp * 0.1), ry(dy * 0.1)]);
        let swing = {
            let (l, c) = (p.last_swing, p.swing);
            let mut d = c - l;
            if d < 0.0 {
                d += 1.0;
            }
            l + d * td
        };
        let mut out = vec![fov, 0.0];
        for h in 0..2 {
            let item = hs.shown[h].stack.id();
            if h == 1 && item == 0 {
                continue; // пустая вторая рука не рисуется
            }
            let e = 1.0 - (hs.prev[h] + (hs.equip[h] - hs.prev[h]) * td);
            let s = if h == 0 { swing } else { 0.0 };
            let (sx, left) = if h == 0 { (1.0, false) } else { (-1.0, true) };
            let model = item_model(item);
            let pose = if item == 0 {
                arm_pose(s, e)
            } else {
                let d = item_display(item);
                match hs.using {
                    Some((uh, left_t, max)) if uh == h && left_t > 0 => eat_pose(left_t as f32 - td + 1.0, max as f32, e, sx, &d, left),
                    _ => item_pose(s, e, sx, &d, left),
                }
            };
            out.extend_from_slice(&[h as f32, model as f32, item as f32, 0.0]);
            out.extend_from_slice(&m4_mul(&base, &pose));
            out[1] += 1.0;
        }
        out
    }

    /// Отладка: «есть» держимый предмет по кругу (анимация еды без логики поедания).
    pub(crate) fn hand_debug_eat(&mut self, on: bool) {
        self.view.hand.debug_eat = on;
        if !on {
            self.view.hand.using = None;
        }
    }

    /// Отладка руки: [equip главной, prev, equip второй, prev, lastAttackedTicks, номер нарисованного предмета главной,
    /// uid реального стека главной руки].
    pub(crate) fn hand_debug(&self) -> Vec<f32> {
        let h = &self.view.hand;
        vec![h.equip[0], h.prev[0], h.equip[1], h.prev[1], h.last_attacked as f32, h.shown[0].stack.id() as f32, h.real[0].uid as f32]
    }
}

// ---------------------------------------------------------------- экструзия спрайта (ASP-11 §4.1, §11.3)

/// Сторона полоски: 0 UP (сосед y−1), 1 DOWN (y+1), 2 LEFT (x−1), 3 RIGHT (x+1); y — строка картинки сверху вниз.
pub(crate) const SIDE_UP: u8 = 0;
pub(crate) const SIDE_DOWN: u8 = 1;
pub(crate) const SIDE_LEFT: u8 = 2;
pub(crate) const SIDE_RIGHT: u8 = 3;

/// Полоски рёбер спрайта w×h (alpha построчно сверху; прозрачный — ровно 0): (сторона, уровень, от, до) — уровень —
/// строка для UP/DOWN и столбец для LEFT/RIGHT; одна полоска на (сторону, уровень) от мин. до макс. края, даже через
/// разрывы (ItemModelGenerator.getFrames), в порядке обхода Java. Лицо и изнанка — всегда, их здесь нет.
pub(crate) fn extrude(alpha: &[u8], w: usize, h: usize) -> Vec<(u8, u8, u8, u8)> {
    let opaque = |x: i32, y: i32| x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h && alpha[y as usize * w + x as usize] != 0;
    let mut out: Vec<(u8, u8, u8, u8)> = Vec::new();
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            if !opaque(x, y) {
                continue;
            }
            for (side, dx, dy) in [(SIDE_UP, 0, -1), (SIDE_DOWN, 0, 1), (SIDE_LEFT, -1, 0), (SIDE_RIGHT, 1, 0)] {
                if opaque(x + dx, y + dy) {
                    continue;
                }
                let vert = side <= SIDE_DOWN;
                let (level, k) = if vert { (y as u8, x as u8) } else { (x as u8, y as u8) };
                match out.iter_mut().find(|f| f.0 == side && f.1 == level) {
                    Some(f) => {
                        f.2 = f.2.min(k);
                        f.3 = f.3.max(k);
                    }
                    None => out.push((side, level, k, k)),
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod hand_tests {
    use super::*;
    use crate::abi::*;
    use crate::registry::ids::*;
    use crate::registry::items::*;
    use crate::world::World;
    use crate::game::Game;

    /// Строки 3×4 матрицы (столбец 4 — перенос) против эталона ASP-11 §10.1.
    fn close(m: &M4, rows: [[f32; 4]; 3], eps: f32) {
        for r in 0..3 {
            for c in 0..4 {
                let got = m[c * 4 + r];
                assert!((got - rows[r][c]).abs() < eps, "строка {r} столбец {c}: {got} против {}\n{m:?}", rows[r][c]);
            }
        }
    }
    fn pt(m: &M4, p: [f32; 3]) -> [f32; 3] {
        let mut o = [0.0; 3];
        for r in 0..3 {
            o[r] = m[r] * p[0] + m[4 + r] * p[1] + m[8 + r] * p[2] + m[12 + r];
        }
        o
    }
    fn near3(a: [f32; 3], b: [f32; 3], eps: f32) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < eps)
    }
    /// NDC при 16:9, FOV 70.
    fn ndc(p: [f32; 3]) -> [f32; 2] {
        let t = (35.0f32).to_radians().tan();
        [p[0] / -p[2] / (t * 16.0 / 9.0), p[1] / -p[2] / t]
    }

    #[test]
    fn matrices_asp11_10_1() {
        close(
            &item_pose_raw(0.0, 0.0, 1.0, &D_FLAT, false),
            [[0.0, 0.0, -0.68, 0.63063], [0.28738, 0.61629, 0.0, -0.32], [0.61629, -0.28738, 0.0, -0.64937]],
            1e-4,
        );
        close(
            &item_pose_raw(1.0 / 6.0, 0.0, 1.0, &D_FLAT, false),
            [[-0.30545, -0.31664, -0.5185, 0.09313], [0.60614, -0.11961, -0.28404, -0.27572], [0.04106, -0.58977, 0.33598, -0.97725]],
            1e-4,
        );
        close(
            &item_pose_raw(0.0, 0.0, 1.0, &D_BLOCK, false),
            [[0.28284, 0.0, 0.28284, 0.56], [0.0, 0.4, 0.0, -0.52], [-0.28284, 0.0, 0.28284, -0.72]],
            1e-4,
        );
        close(
            &arm_pose_raw(0.0, 0.0),
            [[-0.3328, 0.36866, 0.86795, 0.50396], [-0.68273, 0.54071, -0.49145, -0.81857], [-0.65048, -0.75612, 0.07175, -0.66329]],
            1e-4,
        );
    }

    #[test]
    fn swing_points_asp11_10_2() {
        // Центр (8,8,8) и верх-право головки (15,15,8) плоского предмета по фазам взмаха; кулак руки (−1, 10, 0) px.
        let item = [
            ([0.631, -0.320, -0.649], [0.631, 0.075, -0.505]),
            ([0.093, -0.276, -0.977], [-0.179, -0.063, -1.217]),
            ([0.101, -0.481, -1.059], [-0.152, -0.274, -1.324]),
            ([0.214, -0.541, -1.061], [0.035, -0.257, -1.314]),
            ([0.350, -0.493, -0.970], [0.236, -0.124, -1.136]),
            ([0.479, -0.402, -0.813], [0.400, 0.010, -0.828]),
        ];
        let fist = [
            [0.755, -0.438, -1.095],
            [0.050, -0.231, -1.176],
            [0.038, -0.667, -1.330],
            [0.151, -0.913, -1.455],
            [0.339, -0.930, -1.467],
            [0.560, -0.751, -1.341],
        ];
        for k in 0..6 {
            let s = k as f32 / 6.0;
            let m = item_pose(s, 0.0, 1.0, &D_FLAT, false);
            assert!(near3(pt(&m, [0.5, 0.5, 0.5]), item[k].0, 1.5e-3), "центр s={s}: {:?}", pt(&m, [0.5, 0.5, 0.5]));
            assert!(near3(pt(&m, [15.0 / 16.0, 15.0 / 16.0, 0.5]), item[k].1, 1.5e-3), "головка s={s}");
            let a = arm_pose(s, 0.0);
            assert!(near3(pt(&a, [-1.0, 10.0, 0.0]), fist[k], 1.5e-3), "кулак s={s}: {:?}", pt(&a, [-1.0, 10.0, 0.0]));
        }
        // NDC в покое: предмет (+0.78, −0.70), головка (+1.00, +0.21); кулак (+0.55, −0.57).
        let n = ndc(pt(&item_pose(0.0, 0.0, 1.0, &D_FLAT, false), [0.5, 0.5, 0.5]));
        assert!((n[0] - 0.78).abs() < 6e-3 && (n[1] + 0.70).abs() < 6e-3, "{n:?}");
        let n = ndc(pt(&arm_pose(0.0, 0.0), [-1.0, 10.0, 0.0]));
        assert!((n[0] - 0.55).abs() < 6e-3 && (n[1] + 0.57).abs() < 6e-3, "{n:?}");
        // Опущенность: центр предмета y = −0.32 − 0.6·e.
        let c = pt(&item_pose(0.0, 1.0, 1.0, &D_FLAT, false), [0.5, 0.5, 0.5]);
        assert!((c[1] + 0.92).abs() < 1e-4);
        // Левая рука — зеркало правой по X (точка модели x ↔ 1 − x: центрирование T(−0.5) не отражается).
        let r = pt(&item_pose(0.3, 0.2, 1.0, &D_FLAT, false), [0.25, 0.75, 0.5]);
        let l = pt(&item_pose(0.3, 0.2, -1.0, &D_FLAT, true), [0.75, 0.75, 0.5]);
        assert!(near3(r, [-l[0], l[1], l[2]], 1e-5), "{r:?} {l:?}");
    }

    #[test]
    fn eat_asp11_10_4() {
        // h = 1 − g^27 по прошедшим тикам; центр у рта (h = 1, без тряски) (−0.0329, −0.3496, −0.7061).
        for (el, h) in [(0.0f32, 0.0f32), (1.0, 0.5757), (2.0, 0.8249), (3.0, 0.9299), (4.0, 0.9728), (6.0, 0.9963)] {
            let g = (32.0 - el) / 32.0;
            assert!(((1.0 - (g as f64).powf(27.0) as f32) - h).abs() < 1e-4);
        }
        let c = pt(&eat_pose(26.0, 32.0, 0.0, 1.0, &D_FLAT, false), [0.5, 0.5, 0.5]);
        assert!(near3(c, [-0.0310, -0.3496, -0.7096], 2e-3), "{c:?}");
        let c = pt(&eat_pose(1e-3, 32.0, 0.0, 1.0, &D_FLAT, false), [0.5, 0.5, 0.5]);
        assert!(near3(c, [-0.0329, -0.2496, -0.7061], 2e-3), "{c:?}"); // f≈0: тряска на максимуме
    }

    /// Последовательность equip по тикам: смена слота на другой тип в тике 1 (§10.3).
    fn switch_seq(to: ItemStack, n: usize) -> Vec<f32> {
        let mut hs = HandState::new();
        let dirt = Held { stack: ItemStack::of(DIRT as ItemId, 10), uid: 1 };
        hs.shown[0] = dirt;
        hs.real[0] = dirt;
        hs.equip = [1.0, 0.0];
        hs.selected = dirt.stack;
        hs.last_attacked = 100;
        let mut out = Vec::new();
        for _ in 0..n {
            let real = hs.identify(0, 1, to, false);
            let cd = hs.cooldown(1.0, to.id());
            hs.tick_held([real, hs.real[1]], cd);
            // тик игрока: lastAttackedTicks++ и смена типа
            hs.last_attacked += 1;
            if !same_value(&hs.selected, &to) {
                if hs.selected.id() != to.id() {
                    hs.last_attacked = 0;
                }
                hs.selected = to;
            }
            out.push((hs.equip[0] * 1e4).round() / 1e4);
        }
        out
    }

    #[test]
    fn equip_sequences_asp11_10_3() {
        assert_eq!(switch_seq(ItemStack::EMPTY, 7), vec![0.6, 0.2, 0.0, 0.216, 0.512, 0.912, 1.0]);
        let sword = ItemStack::of(I_WOODEN_PICKAXE + 4, 1);
        assert_eq!(
            switch_seq(sword, 14),
            vec![0.6, 0.2, 0.0, 0.0138, 0.0328, 0.064, 0.1106, 0.1756, 0.2621, 0.3732, 0.512, 0.6815, 0.8847, 1.0]
        );
        let pick = ItemStack::of(I_WOODEN_PICKAXE, 1);
        assert_eq!(
            switch_seq(pick, 18),
            vec![
                0.6, 0.2, 0.0, 0.0058, 0.0138, 0.027, 0.0467, 0.0741, 0.1106, 0.1575, 0.216, 0.2875, 0.3732, 0.4746, 0.5927, 0.729,
                0.8847, 1.0
            ]
        );
        let shovel = ItemStack::of(I_WOODEN_PICKAXE + 2, 1);
        assert_eq!(switch_seq(shovel, 21)[20], 1.0);
        assert!(switch_seq(shovel, 20)[19] < 1.0);
        let axe = ItemStack::of(I_WOODEN_PICKAXE + 1, 1);
        let s = switch_seq(axe, 26);
        assert_eq!((s[23], s[25]), (0.7787, 1.0));
    }

    #[test]
    fn identity_and_reequip() {
        let pick = |damage: u16| ItemStack { item: I_WOODEN_PICKAXE, count: 1, damage };
        let mut hs = HandState::new();
        hs.equip = [1.0, 0.0];
        hs.last_attacked = 100;
        let step = |hs: &mut HandState, s: ItemStack| {
            let r = hs.identify(0, 0, s, false);
            let cd = hs.cooldown(1.0, s.id());
            hs.tick_held([r, hs.real[1]], cd);
            hs.last_attacked += 1;
            (hs.equip[0] * 1e4).round() / 1e4
        };
        step(&mut hs, pick(0));
        hs.equip = [1.0, 1.0];
        hs.prev = [1.0, 1.0];
        hs.shown[0] = hs.real[0];
        // Прочность изменилась — быстрый нырок 0.6, 0.2, 0, 0.4, 0.8, 1.0 (кулдаун не сброшен).
        let seq: Vec<f32> = (0..6).map(|_| step(&mut hs, pick(1))).collect();
        assert_eq!(seq, vec![0.6, 0.2, 0.0, 0.4, 0.8, 1.0]);
        // Ставление: количество −1 на месте — тот же объект, нырка нет.
        let dirt = |n| ItemStack::of(DIRT as ItemId, n);
        let mut hs = HandState::new();
        hs.equip = [1.0, 0.0];
        hs.last_attacked = 100;
        step(&mut hs, dirt(10));
        hs.equip = [1.0, 1.0];
        hs.shown[0] = hs.real[0];
        let uid = hs.real[0].uid;
        assert_eq!(step(&mut hs, dirt(9)), 1.0);
        assert_eq!(hs.real[0].uid, uid);
        // Последний блок: стек пуст на месте — пустая рука сразу, без подъёма.
        assert_eq!(step(&mut hs, ItemStack::EMPTY), 1.0);
        assert_eq!(hs.shown[0].stack, ItemStack::EMPTY);
        // Подбор в держимый стек (+) — новый объект: нырок.
        step(&mut hs, dirt(5));
        hs.equip = [1.0, 1.0];
        hs.shown[0] = hs.real[0];
        assert_eq!(step(&mut hs, dirt(6)), 0.6);
        // Поломка инструмента: пустая рука — новым объектом, через нырок.
        let mut hs = HandState::new();
        hs.equip = [1.0, 0.0];
        step(&mut hs, pick(58));
        hs.equip = [1.0, 1.0];
        hs.shown[0] = hs.real[0];
        assert_eq!(step(&mut hs, ItemStack::EMPTY), 0.6);
        // Q последним инструментом (quiet): тот же объект — пустая рука сразу.
        let mut hs = HandState::new();
        hs.equip = [1.0, 0.0];
        step(&mut hs, pick(5));
        hs.equip = [1.0, 1.0];
        hs.shown[0] = hs.real[0];
        let r = hs.identify(0, 0, ItemStack::EMPTY, true);
        hs.tick_held([r, hs.real[1]], 1.0);
        assert_eq!((hs.equip[0], hs.shown[0].stack), (1.0, ItemStack::EMPTY));
    }

    #[test]
    fn extrude_asp11_4_1() {
        let mask = |f: &dyn Fn(usize, usize) -> bool| {
            let mut a = [0u8; 256];
            for y in 0..16 {
                for x in 0..16 {
                    if f(x, y) {
                        a[y * 16 + x] = 255;
                    }
                }
            }
            a
        };
        assert_eq!(extrude(&mask(&|_, _| true), 16, 16).len(), 4);
        assert_eq!(extrude(&mask(&|x, y| (x, y) == (7, 7)), 16, 16).len(), 4);
        assert_eq!(extrude(&mask(&|x, y| (6..=9).contains(&x) && (6..=9).contains(&y)), 16, 16).len(), 4);
        assert_eq!(extrude(&mask(&|x, y| x + y == 15), 16, 16).len(), 64);
        let row = extrude(&mask(&|x, y| y == 4 && (x == 2 || x == 10)), 16, 16);
        assert_eq!(row.len(), 6);
        assert!(row.contains(&(SIDE_UP, 4, 2, 10)) && row.contains(&(SIDE_DOWN, 4, 2, 10)));
        assert_eq!(extrude(&mask(&|_, _| true), 16, 16).len() * 4, 16);
    }

    #[test]
    fn lag_and_signals_in_game() {
        // Сцена: пол из камня, игрок на нём.
        let w = World::new(1337);
        for x in -4..=4 {
            for z in -4..=4 {
                w.set_block(x, 100, z, STONE);
            }
        }
        let mut g = Game::new(&w);
        g.teleport(0.5, 101.0, 0.5);
        g.body(|p| p.on_ground = true);
        g.set_look(0.0, 0.0);
        for _ in 0..10 {
            g.tick();
        }
        // Ровный поворот ω = 9 °/тик: в кадре разница взгляда и renderYaw — 2ω, рука — 0.2ω (§10.4).
        for _ in 0..30 {
            g.client.sim.ctl.player.yaw += 9.0;
            g.tick();
        }
        for a in [0.0f32, 0.5, 0.999] {
            let yaw_now = g.ctl().player.yaw + 9.0 * a;
            let dy = yaw_now - (g.client.sim.view.hand.ryaw[0] + (g.client.sim.view.hand.ryaw[1] - g.client.sim.view.hand.ryaw[0]) * a);
            assert!((dy - 18.0).abs() < 1e-2, "{dy}");
        }
        // Смена слота на кирку: нырок 0.6, 0.2, 0 и медленный подъём (тик 18 — 1.0).
        g.give(I_WOODEN_PICKAXE, 1);
        g.set_keys(K_LOCKED);
        g.press(P_HOTBAR + 0);
        g.tick();
        g.client.sim.view.hand.equip = [1.0, 0.0];
        g.client.sim.view.hand.shown[0] = g.client.sim.view.hand.real[0];
        g.give(DIRT as ItemId, 10); // в ячейку 1
        g.press(P_HOTBAR + 1);
        let mut seq = Vec::new();
        for _ in 0..7 {
            g.tick();
            seq.push((g.client.sim.view.hand.equip[0] * 1e4).round() / 1e4);
        }
        assert_eq!(seq, vec![0.6, 0.2, 0.0, 0.216, 0.512, 0.912, 1.0]);
        // Ставление землёй: мгновенно вниз и 0.4, 0.8, 1.0.
        g.set_look(0.0, 60.0);
        w.set_block(0, 101, 1, AIR);
        g.tick();
        g.press(P_USE);
        g.tick();
        assert_eq!(g.ctl().inv.hand().count, 9);
        assert_eq!(g.client.sim.view.hand.equip[0], 0.4);
        g.tick();
        g.tick();
        assert_eq!(g.client.sim.view.hand.equip[0], 1.0);
        // Клик в воздух — сброс перезарядки: рука ныряет по кулдауну 0.6, 0.2, 0.216 …
        g.set_look(0.0, -90.0);
        g.tick();
        g.press(P_ATTACK);
        let mut seq = Vec::new();
        for _ in 0..4 {
            g.tick();
            seq.push((g.client.sim.view.hand.equip[0] * 1e4).round() / 1e4);
        }
        assert_eq!(seq, vec![0.6, 0.2, 0.216, 0.512]);
        // Телепорт сбрасывает отставание.
        g.client.sim.ctl.player.yaw += 500.0;
        g.teleport(0.5, 101.0, 0.5);
        assert_eq!(g.client.sim.view.hand.ryaw, [g.ctl().player.yaw, g.ctl().player.yaw]);
        // Поза: пустая рука — 1 рука, модель 0.
        g.set_slot(1, ItemStack::EMPTY);
        for _ in 0..10 {
            g.tick();
        }
        let v = g.hand_pose(0.5, true);
        assert_eq!((v[1], v[3]), (1.0, 0.0));
        assert_eq!(v.len(), 2 + 20);
    }
}



