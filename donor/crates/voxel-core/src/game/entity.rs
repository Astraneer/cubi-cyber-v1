//! Предметы в мире (ASP-07 ItemEntity): появление из блока и из рук игрока, физика тика (ход раз в 4 тика у лежащего,
//! выталкивание из блока, плавучесть), слияние, деспаун 6000, подбор — отдельной фазой после тика сущностей (задержки
//! ровно 10/40, решение после ASP-07), частичный подбор с видимым остатком, анимация втягивания (3 тика). f64 —
//! позиция и скорость (double в Java), f32 — там, где Java во float.
//!
//! Тикают только предметы в активной зоне (E13, world/ticks.rs) — куски вокруг игроков; дальше они замирают, возраст
//! стоит (как у не тикающих кусков в Minecraft). Номера сущностей — общий счётчик (game::Ids): игроки, предметы,
//! падающие блоки.
//! Здесь же — рассыпание содержимого сундука и печи (ItemScatterer, ASP-10 §4.5) и падающие блоки (FallingBlockEntity,
//! ASP-05 §5; их тик — фаза симуляции, impl Sim ниже).

use crate::cell::{block_of, Cell, H};
use crate::game::fluids::fluid_height;
use crate::game::inventory::Inventory;
use crate::game::physics::{collide, space_empty, Bufs, Solids};
use crate::game::sim::Sim;
use crate::game::view::Events;
use crate::game::{GameRng, Ids};
use crate::registry::blocks::{falls_through, is_full_cube, replaceable_by};
use crate::registry::ids::WATER;
use crate::registry::items::{block_item, ItemId};
use crate::registry::shapes::Aabb;
use crate::world::stack::ItemStack;
use crate::world::ticks::Zone;
use crate::world::WorldData;
pub(crate) const ITEM_W: f64 = 0.25; // коробка 0.25 × 0.25 × 0.25, позиция — низ, центр по X/Z
pub(crate) const DESPAWN: i32 = 6000;
pub(crate) const NEVER_DESPAWN: i32 = -32768;
pub(crate) const NO_PICKUP: i16 = 32767;
pub(crate) const DELAY_BLOCK: i16 = 10;
pub(crate) const DELAY_THROWN: i16 = 40;

#[derive(Clone, Debug)]
pub(crate) struct ItemEntity {
    /// Номер появления (Entity.getId): фаза «раз в 4 тика», порядок обхода.
    pub(crate) id: u32,
    pub(crate) stack: ItemStack,
    pub(crate) pos: [f64; 3],
    pub(crate) prev: [f64; 3],
    pub(crate) vel: [f64; 3],
    pub(crate) on_ground: bool,
    pub(crate) no_clip: bool,
    /// Entity.age: +1 перед каждым тиком (первый тик — с age = 1).
    pub(crate) age: u32,
    /// Возраст для деспауна; слияние берёт min.
    pub(crate) item_age: i32,
    /// Только отрисовка (вращение и покачивание): слияние не трогает.
    pub(crate) spin_age: u32,
    pub(crate) pickup_delay: i16,
    /// Бросил игрок (thrower; на подбор не влияет).
    pub(crate) thrown: bool,
    /// uniqueOffset ∈ [0, 2π): сдвиг вращения и покачивания.
    pub(crate) offset: f32,
    pub(crate) in_water: bool,
    pub(crate) fluid_h: f64,
    pub(crate) removed: bool,
}

impl ItemEntity {
    pub(crate) fn bbox(&self) -> Aabb {
        let [x, y, z] = self.pos;
        let r = ITEM_W / 2.0;
        Aabb { lo: [x - r, y, z - r], hi: [x + r, y + ITEM_W, z + r] }
    }
    /// canMerge(): жив, подбор не запрещён навсегда, не вечный, не старый, стек не полон.
    pub(crate) fn mergeable(&self) -> bool {
        !self.removed && self.pickup_delay != NO_PICKUP && self.item_age != NEVER_DESPAWN && self.item_age < DESPAWN && self.stack.n() < self.stack.max()
    }

    /// Вода (Entity.updateMovementInFluid): коробка, сжатая на 0.001; верх воды клетки — 8/9 у источника под воздухом,
    /// 1.0 под водой. Течения у нас пока нет (ASP-15) — толчка нет.
    pub(crate) fn update_water(&mut self, w: &mut WorldData) {
        let b = self.bbox().contract(0.001);
        let (mut touching, mut h) = (false, 0.0f64);
        for x in b.lo[0].floor() as i32..b.hi[0].ceil() as i32 {
            for y in b.lo[1].floor() as i32..b.hi[1].ceil() as i32 {
                for z in b.lo[2].floor() as i32..b.hi[2].ceil() as i32 {
                    let c = w.get_cell(x, y, z);
                    if block_of(c) != WATER {
                        continue;
                    }
                    let fh = fluid_height(c, w.get_cell(x, y + 1, z)) as f32; // уровень течения (ASP-15), источник — 8/9
                    let top = (y as f32 + fh) as f64;
                    if top >= b.lo[1] {
                        touching = true;
                        h = h.max(top - b.lo[1]);
                    }
                }
            }
        }
        self.in_water = touching;
        self.fluid_h = h;
    }

    /// Шаги 1–7 тика ItemEntity (ASP-07 §3); слияние, возраст и деспаун — в Items::tick.
    pub(crate) fn tick_physics(&mut self, w: &mut WorldData, rng: &mut GameRng, bufs: &mut Bufs) {
        self.prev = self.pos;
        if self.stack.is_empty() {
            self.removed = true;
            return;
        }
        self.update_water(w); // baseTick
        if self.pos[1] < -64.0 {
            self.removed = true; // пустота: дно 0 − 64 (страховка; ниже 0 у нас твёрдо)
            return;
        }
        if self.pickup_delay > 0 && self.pickup_delay != NO_PICKUP {
            self.pickup_delay -= 1;
        }
        if self.in_water && self.fluid_h > 0.1f32 as f64 {
            // applyWaterBuoyancy: гравитации в воде нет.
            self.vel[0] *= 0.99f32 as f64;
            self.vel[2] *= 0.99f32 as f64;
            if self.vel[1] < 0.06f32 as f64 {
                self.vel[1] += 5.0e-4f32 as f64;
            }
        } else {
            self.vel[1] -= 0.04;
        }
        self.no_clip = !space_empty(w, &self.bbox().contract(1e-7), &mut bufs.a);
        if self.no_clip {
            self.push_out(w, rng);
        }
        let h2 = self.vel[0] * self.vel[0] + self.vel[2] * self.vel[2];
        if !self.on_ground || h2 > 1.0e-5f32 as f64 || self.age.wrapping_add(self.id) % 4 == 0 {
            self.move_self(w, bufs);
            let f = if self.on_ground {
                // Блок трения — на 0.999999 ниже низа (getPosWithYOffset у предмета).
                let p = self.pos;
                (w.slip(p[0].floor() as i32, (p[1] - 0.999999f32 as f64).floor() as i32, p[2].floor() as i32) * 0.98f32) as f64
            } else {
                0.98f32 as f64
            };
            self.vel = [self.vel[0] * f, self.vel[1] * 0.98, self.vel[2] * f];
            if self.on_ground && self.vel[1] < 0.0 {
                self.vel[1] *= -0.5; // срабатывает только в noClip (onEntityLand уже обнулил vy)
            }
        }
    }

    /// Entity.move: noClip — перенос сквозь блоки; иначе та же коллизия, что у игрока (Y, потом бо́льшая из X/Z),
    /// без ступеньки. Упёрлись по оси — эта скорость 0; по вертикали — onEntityLand (vy = 0, пол и потолок).
    pub(crate) fn move_self(&mut self, w: &mut WorldData, bufs: &mut Bufs) {
        let m = self.vel;
        if self.no_clip {
            self.pos = [self.pos[0] + m[0], self.pos[1] + m[1], self.pos[2] + m[2]];
            return;
        }
        let a = collide(w, &self.bbox(), m, self.on_ground, 0.0, bufs);
        if a[0] * a[0] + a[1] * a[1] + a[2] * a[2] > 1e-7 {
            self.pos = [self.pos[0] + a[0], self.pos[1] + a[1], self.pos[2] + a[2]];
        }
        let near = |p: f64, q: f64| (p - q).abs() < 1.0e-5f32 as f64;
        let vertical = m[1] != a[1];
        self.on_ground = vertical && m[1] < 0.0;
        if !near(m[0], a[0]) {
            self.vel[0] = 0.0;
        }
        if !near(m[2], a[2]) {
            self.vel[2] = 0.0;
        }
        if vertical {
            self.vel[1] = 0.0;
        }
        // checkBlockCollision (кактус: 5 касаний) — со срезом 4, вместе с формой кактуса.
    }

    /// Entity.pushOutOfBlocks: ближайшая открытая сторона клетки середины коробки (N, S, W, E, UP; вниз — никогда),
    /// годится сосед, который не полный куб; скорость ×0.75, по выбранной оси — 0.1…0.3 наружу.
    pub(crate) fn push_out(&mut self, w: &mut WorldData, rng: &mut GameRng) {
        let b = self.bbox();
        let p = [self.pos[0], (b.lo[1] + b.hi[1]) * 0.5, self.pos[2]];
        let c = [p[0].floor() as i32, p[1].floor() as i32, p[2].floor() as i32];
        let l = [p[0] - c[0] as f64, p[1] - c[1] as f64, p[2] - c[2] as f64];
        // (ось, знак) по порядку N (−Z), S (+Z), W (−X), E (+X), UP.
        let mut best = (1usize, 1.0f64, f64::MAX);
        for (axis, sign) in [(2usize, -1.0f64), (2, 1.0), (0, -1.0), (0, 1.0), (1, 1.0)] {
            let mut n = c;
            n[axis] += sign as i32;
            if is_full_cube(w.get_block(n[0], n[1], n[2])) {
                continue;
            }
            let d = if sign > 0.0 { 1.0 - l[axis] } else { l[axis] };
            if d < best.2 {
                best = (axis, sign, d);
            }
        }
        let g = rng.next_f32() * 0.2 + 0.1;
        self.vel = [self.vel[0] * 0.75, self.vel[1] * 0.75, self.vel[2] * 0.75];
        self.vel[best.0] = (best.1 as f32 * g) as f64;
    }
}

/// Анимация втягивания подобранного (ItemPickupParticle): копия летит 3 тика к середине игрока.
#[derive(Clone, Debug)]
pub(crate) struct PickupAnim {
    pub(crate) stack: ItemStack,
    pub(crate) from: [f64; 3],
    pub(crate) spin_age: u32,
    pub(crate) offset: f32,
    pub(crate) t: u32,
    /// Кто подобрал: None — игрок этого клиента (летит к нему), Some(номер) — другой игрок (летит к нему, пока виден).
    pub(crate) to: Option<u32>,
}

/// Подобранное за фазу подбора: (позиция предмета, предмет, ячейка инвентаря) — звук «pop» и «нырок» руки; номер
/// сущности, стек до подбора и возраст вращения — летящая копия у клиента (proto Msg::Pickup).
pub(crate) struct Picked {
    pub(crate) pos: [f64; 3],
    pub(crate) item: ItemId,
    pub(crate) slot: usize,
    pub(crate) id: u32,
    pub(crate) stack: ItemStack,
    pub(crate) spin: u32,
}

pub(crate) struct Items {
    pub(crate) list: Vec<ItemEntity>,
    pub(crate) anims: Vec<PickupAnim>,
    pub(crate) bufs: Bufs,
}

impl Items {
    pub(crate) fn new() -> Items {
        Items { list: Vec::new(), anims: Vec::new(), bufs: Bufs::default() }
    }
    pub(crate) fn make(&mut self, id: u32, pos: [f64; 3], stack: ItemStack, rng: &mut GameRng) -> ItemEntity {
        // Конструктор ItemEntity(world, x, y, z, stack): скорость (±0.1, 0.2, ±0.1), uniqueOffset.
        let vx = rng.next_f64() * 0.2 - 0.1;
        let vz = rng.next_f64() * 0.2 - 0.1;
        let offset = rng.next_f32() * std::f32::consts::TAU;
        ItemEntity {
            id,
            stack,
            pos,
            prev: pos,
            vel: [vx, 0.2, vz],
            on_ground: false,
            no_clip: false,
            age: 0,
            item_age: 0,
            spin_age: 0,
            pickup_delay: 0,
            thrown: false,
            offset,
            in_water: false,
            fluid_h: 0.0,
            removed: false,
        }
    }

    /// Block.dropStack: центр клетки ±0.25 по осям (по Y ещё −0.125), задержка подбора 10.
    pub(crate) fn drop_from_block(&mut self, cell: [i32; 3], stack: ItemStack, rng: &mut GameRng, ids: &mut Ids) {
        if stack.is_empty() {
            return;
        }
        let mut u = || rng.next_f64() * 0.5 - 0.25;
        let p = [cell[0] as f64 + 0.5 + u(), cell[1] as f64 + 0.5 + u() - ITEM_W / 2.0, cell[2] as f64 + 0.5 + u()];
        let mut e = self.make(ids.next(), p, stack, rng);
        e.pickup_delay = DELAY_BLOCK;
        self.list.push(e);
    }

    /// Entity.dropStack (падающий блок выпал предметом): в точке сущности, обычная случайная скорость, задержка 10.
    pub(crate) fn drop_at(&mut self, pos: [f64; 3], stack: ItemStack, rng: &mut GameRng, ids: &mut Ids) {
        if stack.is_empty() {
            return;
        }
        let mut e = self.make(ids.next(), pos, stack, rng);
        e.pickup_delay = DELAY_BLOCK;
        self.list.push(e);
    }

    /// ItemScatterer.spawn (сундук и печь сломаны, ASP-10 §4.5): одна точка на слот (x + r·0.75 + 0.125, y + r·0.75,
    /// z + r·0.75 + 0.125), стек — кусками по 10…30 (nextInt(21) + 10), у куска своя скорость tri(0 | 0.2, 0.11485);
    /// задержки подбора нет (поле по умолчанию — 0).
    pub(crate) fn scatter(&mut self, cell: [i32; 3], mut stack: ItemStack, rng: &mut GameRng, ids: &mut Ids) {
        if stack.is_empty() {
            return;
        }
        let at = [
            cell[0] as f64 + rng.next_f64() * 0.75 + 0.125,
            cell[1] as f64 + rng.next_f64() * 0.75,
            cell[2] as f64 + rng.next_f64() * 0.75 + 0.125,
        ];
        let tri = |rng: &mut GameRng, m: f64| m + 0.11485000171139836 * (rng.next_f64() - rng.next_f64());
        while !stack.is_empty() {
            let k = rng.range(10, 30).min(stack.n());
            let part = stack.with_count(k);
            stack.count -= k;
            let mut e = self.make(ids.next(), at, part, rng);
            e.vel = [tri(rng, 0.0), tri(rng, 0.2), tri(rng, 0.0)];
            e.pickup_delay = 0;
            self.list.push(e);
        }
    }

    /// PlayerEntity.dropItem: из глаз − 0.3; вперёд — 0.3 по взгляду, +0.1 ± 0.1 вверх, ±0.02 вбок (всё во float);
    /// случайно (смерть) — до 0.5 в случайную сторону и 0.2 вверх. Задержка 40, скорость игрока не наследуется.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn throw(&mut self, feet: [f64; 3], eye_h: f32, yaw: f32, pitch: f32, stack: ItemStack, random: bool, owned: bool, rng: &mut GameRng, ids: &mut Ids) {
        if stack.is_empty() {
            return;
        }
        let p = [feet[0], feet[1] + eye_h as f64 - 0.3f32 as f64, feet[2]];
        let mut e = self.make(ids.next(), p, stack, rng);
        e.pickup_delay = DELAY_THROWN;
        e.thrown = owned;
        let tau = std::f32::consts::TAU;
        e.vel = if random {
            let f = rng.next_f32() * 0.5;
            let g = rng.next_f32() * tau;
            [(-libm::sinf(g) * f) as f64, 0.2f32 as f64, (libm::cosf(g) * f) as f64]
        } else {
            let rad = std::f32::consts::PI / 180.0;
            let (g, h) = (libm::sinf(pitch * rad), libm::cosf(pitch * rad));
            let (i, j) = (libm::sinf(yaw * rad), libm::cosf(yaw * rad));
            let k = rng.next_f32() * tau;
            let l = 0.02 * rng.next_f32();
            let (r1, r2) = (rng.next_f32(), rng.next_f32());
            [
                (-i * h * 0.3f32) as f64 + libm::cos(k as f64) * l as f64,
                (-g * 0.3f32 + 0.1f32 + (r1 - r2) * 0.1f32) as f64,
                (j * h * 0.3f32) as f64 + libm::sin(k as f64) * l as f64,
            ]
        };
        self.list.push(e);
    }

    /// Фаза «мир → сущности» для предметов (ASP-07 §3): по снимку длины (появившиеся сейчас — со следующего тика),
    /// только в активной зоне.
    pub(crate) fn tick(&mut self, w: &mut WorldData, rng: &mut GameRng, zone: &Zone) {
        let n = self.list.len();
        let mut bufs = std::mem::take(&mut self.bufs);
        for i in 0..n {
            if self.list[i].removed {
                continue;
            }
            let it = &mut self.list[i];
            if !zone.has_pos(it.pos) {
                it.prev = it.pos; // спит: стоит и не стареет
                continue;
            }
            it.age = it.age.wrapping_add(1);
            it.tick_physics(w, rng, &mut bufs);
            if it.removed {
                continue;
            }
            let moved = (0..3).any(|k| it.prev[k].floor() != it.pos[k].floor());
            if it.age % if moved { 2 } else { 40 } == 0 {
                self.try_merge(i);
            }
            let it = &mut self.list[i];
            if it.removed {
                continue;
            }
            if it.item_age != NEVER_DESPAWN {
                it.item_age += 1;
            }
            it.spin_age = it.spin_age.wrapping_add(1);
            it.update_water(w); // второй толчок потока (у нас потока нет)
            if it.item_age >= DESPAWN {
                it.removed = true;
            }
        }
        self.bufs = bufs;
    }

    /// ItemEntity.tryMerge: соседи в коробке +0.5 / 0 / +0.5, тот же предмет и износ, сумма не больше предела (перелива
    /// нет). Больший поглощает меньший (при равенстве — другой) и остаётся на месте; задержка — max, возраст — min.
    pub(crate) fn try_merge(&mut self, i: usize) {
        if !self.list[i].mergeable() {
            return;
        }
        for j in 0..self.list.len() {
            if j == i || !self.list[j].mergeable() {
                continue;
            }
            let zone = self.list[i].bbox();
            let zone = Aabb { lo: [zone.lo[0] - 0.5, zone.lo[1], zone.lo[2] - 0.5], hi: [zone.hi[0] + 0.5, zone.hi[1], zone.hi[2] + 0.5] };
            if !self.list[j].bbox().intersects(&zone) {
                continue;
            }
            let (a, b) = (self.list[i].stack, self.list[j].stack);
            if !a.same_kind(&b) || a.n() + b.n() > b.max() {
                continue;
            }
            let (dst, src) = if b.n() < a.n() { (i, j) } else { (j, i) };
            let s = self.list[src].clone();
            let t = &mut self.list[dst];
            let k = (t.stack.max().min(64) - t.stack.n()).min(s.stack.n());
            t.stack.count += k;
            t.pickup_delay = t.pickup_delay.max(s.pickup_delay);
            t.item_age = t.item_age.min(s.item_age);
            let src_e = &mut self.list[src];
            src_e.stack.count -= k;
            if src_e.stack.is_empty() {
                src_e.removed = true;
            }
            if self.list[i].removed {
                break;
            }
        }
    }

    /// Фаза 4 — подбор (PlayerEntity.tickMovement → ItemEntity.onPlayerCollision), после тика сущностей: коробка игрока
    /// + (1.0, 0.5, 1.0), задержка 0, частичный подбор — остаток лежит с новым числом (дефект ванили не повторяем).
    /// Сначала шаг уже летящих анимаций, затем новые (видны с этого же кадра).
    pub(crate) fn pickup(&mut self, player: &Aabb, inv: &mut Inventory, out: &mut Vec<Picked>) {
        for a in &mut self.anims {
            a.t += 1;
        }
        self.anims.retain(|a| a.t < 3);
        let zone = Aabb { lo: [player.lo[0] - 1.0, player.lo[1] - 0.5, player.lo[2] - 1.0], hi: [player.hi[0] + 1.0, player.hi[1] + 0.5, player.hi[2] + 1.0] };
        for it in self.list.iter_mut() {
            if it.removed || it.pickup_delay != 0 || !it.bbox().intersects(&zone) {
                continue;
            }
            let before = it.stack;
            let had: Vec<i32> = (0..41).map(|i| inv.get(i).n()).collect();
            if inv.insert(&mut it.stack) {
                let slot = (0..41).find(|&i| inv.get(i).n() != had[i]).unwrap_or(0);
                out.push(Picked { pos: it.pos, item: before.item, slot, id: it.id, stack: before, spin: it.spin_age });
                self.anims.push(PickupAnim { stack: before, from: it.pos, spin_age: it.spin_age, offset: it.offset, t: 0, to: None });
                if it.stack.is_empty() {
                    it.removed = true;
                }
            }
        }
        self.list.retain(|e| !e.removed);
    }

    /// Данные отрисовки (рисует «Срез 2B»): по 8 чисел на предмет — x, y, z (низ, интерполяция по alpha), поворот Y в
    /// радианах ((spin_age + α)/20 + uniqueOffset), покачивание (sin((spin_age + α)/10 + offset)·0.1 + 0.1), номер
    /// предмета, число в стеке, свет (пока 1.0). Летящие копии подбора — так же, позиция lerp(((t + α)/3)², от, цель),
    /// вращение заморожено на их spin_age. Ближе 64 блоков к target (дальше режет рендер своей дальностью).
    pub(crate) fn draws(&self, alpha: f64, target: [f64; 3], view: [f64; 3]) -> Vec<f32> {
        self.draws_with(alpha, target, view, &|_| None)
    }

    /// То же, но копии чужого подбора летят к игроку, чей номер знает remote (середина тела); неизвестен — копия не рисуется.
    pub(crate) fn draws_with(&self, alpha: f64, target: [f64; 3], view: [f64; 3], remote: &dyn Fn(u32) -> Option<[f64; 3]>) -> Vec<f32> {
        let a = alpha as f32;
        let mut v = Vec::with_capacity((self.list.len() + self.anims.len()) * 8);
        let push = |p: [f64; 3], spin: u32, offset: f32, s: &ItemStack, v: &mut Vec<f32>| {
            let d2 = (p[0] - view[0]).powi(2) + (p[1] - view[1]).powi(2) + (p[2] - view[2]).powi(2);
            if d2 > 64.0 * 64.0 {
                return;
            }
            let t = spin as f32 + a;
            v.extend_from_slice(&[p[0] as f32, p[1] as f32, p[2] as f32, t / 20.0 + offset, (t / 10.0 + offset).sin() * 0.1 + 0.1, s.item as f32, s.count as f32, 1.0]);
        };
        for e in &self.list {
            if e.removed {
                continue;
            }
            let p = [e.prev[0] + (e.pos[0] - e.prev[0]) * alpha, e.prev[1] + (e.pos[1] - e.prev[1]) * alpha, e.prev[2] + (e.pos[2] - e.prev[2]) * alpha];
            push(p, e.spin_age, e.offset, &e.stack, &mut v);
        }
        for an in &self.anims {
            let to = match an.to {
                None => target,
                Some(id) => match remote(id) {
                    Some(t) => t,
                    None => continue,
                },
            };
            let f = ((an.t as f64 + alpha) / 3.0).powi(2);
            let p = [an.from[0] + (to[0] - an.from[0]) * f, an.from[1] + (to[1] - an.from[1]) * f, an.from[2] + (to[2] - an.from[2]) * f];
            push(p, an.spin_age, an.offset, &an.stack, &mut v);
        }
        v
    }
}

#[cfg(test)]
mod entity_tests {
    use super::*;
    use crate::registry::ids::*;
    use crate::registry::items::*;
    use crate::world::World;

    /// Плита камня 9×9 на y = 100 (верх — 101) высоко над рельефом.
    fn floor() -> World {
        let w = World::new(1337);
        for x in -4..=4 {
            for z in -4..=4 {
                w.set_block(x, 100, z, STONE);
            }
        }
        w
    }
    fn item(items: &mut Items, rng: &mut GameRng, pos: [f64; 3], vel: [f64; 3], n: i32) -> usize {
        let mut e = items.make(0, pos, ItemStack::of(DIRT as ItemId, n), rng);
        e.vel = vel;
        e.id = 0;
        items.list.push(e);
        items.list.len() - 1
    }

    #[test]
    fn e1_drop_from_block_flight() {
        // Э1: y0 = 0.375 над полом, v = (0, 0.2, 0): пик на 4-м тике, посадка на 11-м.
        let w = floor();
        let mut d = w.d.borrow_mut();
        let (mut items, mut rng) = (Items::new(), GameRng(7));
        let i = item(&mut items, &mut rng, [0.5, 101.375, 0.5], [0.0, 0.2, 0.0], 1);
        let want = [0.535, 0.6518, 0.726264, 0.75923872, 0.751553946, 0.704022867, 0.617442409, 0.492593561, 0.330241690, 0.131136856, 0.0];
        for (t, y) in want.iter().enumerate() {
            items.tick(&mut d, &mut rng, &Zone::square(0, 0));
            let e = &items.list[i];
            assert!((e.pos[1] - 101.0 - y).abs() < 1e-8, "тик {}: {} ≠ {}", t + 1, e.pos[1] - 101.0, y);
            assert_eq!(e.on_ground, t == 10, "тик {}", t + 1);
        }
        assert!((items.list[i].vel[1]).abs() < 1e-12, "отскока нет");
    }

    #[test]
    fn e2_throw_horizontal() {
        // Э2: Q глядя горизонтально, y0 = 1.32, v = (0, 0.1F, 0.3F).
        let w = floor();
        let mut d = w.d.borrow_mut();
        let (mut items, mut rng) = (Items::new(), GameRng(7));
        let i = item(&mut items, &mut rng, [0.5, 102.32, 0.5], [0.0, 0.1f32 as f64, 0.3f32 as f64], 1);
        let mut z = Vec::new();
        for _ in 0..24 {
            items.tick(&mut d, &mut rng, &Zone::square(0, 0));
            z.push((items.list[i].pos[1] - 101.0, items.list[i].pos[2] - 0.5));
        }
        let near = |a: f64, b: f64| (a - b).abs() < 1e-8;
        assert!(near(z[0].0, 1.380000001) && near(z[0].1, 0.300000012), "{:?}", z[0]);
        assert!(near(z[4].0, 1.216157937) && near(z[4].1, 1.441188160), "{:?}", z[4]);
        assert!(near(z[10].0, 0.0) && near(z[10].1, 2.989030137), "{:?}", z[10]);
        assert!(near(z[19].1, 3.335924990) && near(z[22].1, 3.335924990), "стоит, ход раз в 4");
        assert!(near(z[23].1, 3.337136022), "{:?}", z[23]);
    }

    #[test]
    fn e8_merge_rules() {
        let w = floor();
        let mut d = w.d.borrow_mut();
        let mut rng = GameRng(3);
        // (30, 34) → 64 у большего, остаётся на своём месте; задержка max, возраст min.
        let mut items = Items::new();
        let a = item(&mut items, &mut rng, [0.5, 101.0, 0.5], [0.0; 3], 30);
        let b = item(&mut items, &mut rng, [0.9, 101.0, 0.5], [0.0; 3], 34);
        items.list[a].pickup_delay = 40;
        items.list[a].item_age = 100;
        items.list[b].item_age = 3000;
        items.list[a].on_ground = true;
        items.list[b].on_ground = true;
        items.try_merge(a);
        assert!(items.list[a].removed && items.list[b].stack.count == 64);
        assert_eq!((items.list[b].pickup_delay, items.list[b].item_age, items.list[b].pos[0]), (40, 100, 0.9));
        // (40, 40) — нет; (32, 32) — поглощает другой; (64, 1) — нет; далеко (0.75 по X) — нет.
        for (n1, n2, merged, dx) in [(40, 40, false, 0.3), (32, 32, true, 0.3), (64, 1, false, 0.3), (2, 2, false, 0.75), (2, 2, true, 0.7499)] {
            let mut items = Items::new();
            let a = item(&mut items, &mut rng, [0.5, 101.0, 0.5], [0.0; 3], n1);
            let b = item(&mut items, &mut rng, [0.5 + dx, 101.0, 0.5], [0.0; 3], n2);
            items.try_merge(a);
            assert_eq!(items.list[a].removed, merged, "{n1}+{n2} dx {dx}");
            assert!(!items.list[b].removed);
        }
        // Изношенное с целым, инструменты — никогда.
        let mut items = Items::new();
        let a = item(&mut items, &mut rng, [0.5, 101.0, 0.5], [0.0; 3], 1);
        let b = item(&mut items, &mut rng, [0.6, 101.0, 0.5], [0.0; 3], 1);
        items.list[a].stack = ItemStack::of(I_WOODEN_PICKAXE, 1);
        items.list[b].stack = ItemStack::of(I_WOODEN_PICKAXE, 1);
        items.try_merge(a);
        assert!(!items.list[a].removed && !items.list[b].removed);
        // Пять бросков под ноги сливаются в один стек за ≤ 2 с (лежащие проверяют раз в 40 тиков).
        let mut items = Items::new();
        for k in 0..5 {
            item(&mut items, &mut rng, [0.5 + 0.05 * k as f64, 101.3, 0.5], [0.0, 0.1, 0.0], 1);
            items.list[k].id = k as u32;
        }
        for _ in 0..45 {
            items.tick(&mut d, &mut rng, &Zone::square(0, 0));
        }
        let alive: Vec<i32> = items.list.iter().filter(|e| !e.removed).map(|e| e.stack.count).collect();
        assert_eq!(alive, vec![5]);
    }

    #[test]
    fn delays_and_partial_pickup() {
        // Подбор — фаза после сущностей: предмет из блока подбирается на 10-м тике, брошенный — на 40-м.
        let w = floor();
        let mut d = w.d.borrow_mut();
        let mut rng = GameRng(5);
        let player = Aabb { lo: [0.2, 101.0, 0.2], hi: [0.8, 102.8, 0.8] };
        for (delay, want) in [(DELAY_BLOCK, 10), (DELAY_THROWN, 40)] {
            let mut items = Items::new();
            let i = item(&mut items, &mut rng, [0.5, 101.0, 1.2], [0.0; 3], 3);
            items.list[i].pickup_delay = delay;
            items.list[i].on_ground = true;
            let mut inv = Inventory::new();
            let mut got = None;
            for t in 1..=60 {
                items.tick(&mut d, &mut rng, &Zone::square(0, 0));
                let mut p = Vec::new();
                items.pickup(&player, &mut inv, &mut p);
                if !p.is_empty() && got.is_none() {
                    got = Some(t);
                    assert_eq!(items.anims.len(), 1);
                }
            }
            assert_eq!(got, Some(want));
            assert_eq!(inv.main[0], ItemStack::of(DIRT as ItemId, 3));
        }
        // Граница зоны: |dx| = 1.4249 — да, 1.425 — нет (строго); низ предмета ступни − 0.75 — нет, −0.7499 — да.
        for (p, ok) in [([0.5 + 1.4249, 101.0, 0.5], true), ([0.5 + 1.425, 101.0, 0.5], false), ([0.5, 100.25, 0.5], false), ([0.5, 100.2501, 0.5], true), ([0.5, 103.2999, 0.5], true), ([0.5, 103.3, 0.5], false)] {
            let mut items = Items::new();
            item(&mut items, &mut rng, p, [0.0; 3], 1);
            let mut inv = Inventory::new();
            let mut out = Vec::new();
            items.pickup(&player, &mut inv, &mut out);
            assert_eq!(out.len() == 1, ok, "{p:?}");
        }
        // Частичный подбор: место на 4 — подобрано 4, остаток 6 лежит (виден с новым числом).
        let mut items = Items::new();
        item(&mut items, &mut rng, [0.5, 101.0, 0.5], [0.0; 3], 10);
        let mut inv = Inventory::new();
        for i in 0..36 {
            inv.main[i] = ItemStack::of(COBBLE as ItemId, 64);
        }
        inv.main[3] = ItemStack::of(DIRT as ItemId, 60);
        let mut out = Vec::new();
        items.pickup(&player, &mut inv, &mut out);
        assert_eq!((out.len(), out[0].slot, inv.main[3].count, items.list.len(), items.list[0].stack.count), (1, 3, 64, 1, 6));
        let draws = items.draws(0.0, [0.5, 101.8, 0.5], [0.5, 102.6, 0.5]);
        assert_eq!(draws.len(), 16, "остаток и летящая копия");
        assert_eq!((draws[6], draws[14]), (6.0, 10.0), "остаток 6, копия — стек до подбора");
        // Полный инвентарь — ничего, без звука и анимации.
        let mut out = Vec::new();
        items.anims.clear();
        items.pickup(&player, &mut inv, &mut out);
        assert!(out.is_empty() && items.anims.is_empty());
    }

    #[test]
    fn despawn_and_zone() {
        let w = floor();
        let mut d = w.d.borrow_mut();
        let mut rng = GameRng(9);
        let mut items = Items::new();
        item(&mut items, &mut rng, [0.5, 101.0, 0.5], [0.0; 3], 1);
        item(&mut items, &mut rng, [200.5, 101.0, 0.5], [0.0; 3], 1); // вне зоны 9×9: спит
        items.list[0].on_ground = true;
        for _ in 0..5999 {
            items.tick(&mut d, &mut rng, &Zone::square(0, 0));
        }
        assert!(!items.list[0].removed && items.list[0].item_age == 5999);
        items.tick(&mut d, &mut rng, &Zone::square(0, 0));
        assert!(items.list[0].removed, "удалён в тике, где возраст стал 6000");
        assert_eq!((items.list[1].item_age, items.list[1].age), (0, 0), "вне зоны возраст стоит");
    }

    #[test]
    fn push_out_of_block() {
        // Блок поставлен на лежащий предмет (центр в 0.2 от западной грани): выползает на запад и ложится рядом.
        let w = floor();
        let mut rng = GameRng(1);
        let mut items = Items::new();
        let i = item(&mut items, &mut rng, [0.2, 101.0, 0.5], [0.0; 3], 1);
        items.list[i].on_ground = true;
        w.set_block(0, 101, 0, STONE);
        let mut d = w.d.borrow_mut();
        for _ in 0..40 {
            items.tick(&mut d, &mut rng, &Zone::square(0, 0));
        }
        let e = &items.list[i];
        assert!(e.pos[0] < 0.0 - 0.124 && e.pos[0] > -1.0, "x {}", e.pos[0]);
        assert!(e.on_ground && (e.pos[1] - 101.0).abs() < 1e-9, "лёг на пол: {:?}", e.pos);
        // Замурован со всех сторон — всплывает вверх сквозь камень.
        drop(d);
        for (x, z) in [(2, 0), (1, 0), (3, 0), (2, 1), (2, -1)] {
            for y in 101..104 {
                w.set_block(x, y, z, STONE);
            }
        }
        let mut d = w.d.borrow_mut();
        let j = item(&mut items, &mut rng, [2.5, 101.2, 0.5], [0.0; 3], 1);
        items.list[j].on_ground = true;
        items.list[j].id = 1;
        for _ in 0..120 {
            items.tick(&mut d, &mut rng, &Zone::square(0, 0));
        }
        assert!(items.list[j].pos[1] >= 104.0 - 1e-9, "вверх ~1 блок/с сквозь камень: {}", items.list[j].pos[1]);
    }

    #[test]
    fn floats_in_water() {
        // Вода глубиной 3: предмет, брошенный сверху, тонет и потом всплывает (без гравитации в воде).
        let w = floor();
        for x in -1..=1 {
            for z in -1..=1 {
                for y in 101..104 {
                    w.set_block(x, y, z, WATER);
                }
            }
        }
        let mut d = w.d.borrow_mut();
        let mut rng = GameRng(2);
        let mut items = Items::new();
        let i = item(&mut items, &mut rng, [0.5, 104.5, 0.5], [0.0; 3], 1);
        let mut low = f64::MAX;
        for _ in 0..1200 {
            items.tick(&mut d, &mut rng, &Zone::square(0, 0));
            low = low.min(items.list[i].pos[1]);
        }
        assert!(low < 102.5, "утонул: {low}");
        let y = items.list[i].pos[1];
        assert!(y > 103.3 && y < 104.0, "качается у поверхности: {y}");
    }
}

// ---------------------------------------------------------------- падающие блоки (ASP-05 §5)

/// Падающий песок или гравий (FallingBlockEntity): коробка 0.98 × 0.98 × 0.98, позиция — низ, центр по X/Z.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FallingBlock {
    /// Номер сущности (общий счётчик, game::Ids): порядок тика; по нему же список собирается после загрузки.
    pub(crate) id: u32,
    /// Ячейка блока (с состоянием).
    pub(crate) cell: Cell,
    pub(crate) pos: [f64; 3],
    pub(crate) prev: [f64; 3],
    pub(crate) vel: [f64; 3],
    pub(crate) on_ground: bool,
    /// timeFalling.
    pub(crate) age: u32,
    pub(crate) removed: bool,
}

impl FallingBlock {
    /// spawnFromBlock: (x + 0.5, y, z + 0.5), скорость 0.
    pub(crate) fn new(id: u32, cell: Cell, p: [i32; 3]) -> FallingBlock {
        let pos = [p[0] as f64 + 0.5, p[1] as f64, p[2] as f64 + 0.5];
        FallingBlock { id, cell, pos, prev: pos, vel: [0.0; 3], on_ground: false, age: 0, removed: false }
    }
    pub(crate) fn bbox(&self) -> Aabb {
        let [x, y, z] = self.pos;
        Aabb { lo: [x - 0.49, y, z - 0.49], hi: [x + 0.49, y + 0.98, z + 0.49] }
    }
}


impl Sim {
    /// Фаза мира, сущности: падающие блоки в зоне (FallingBlockEntity.tick). Возраст +1, g 0.04, ход с коллизией блоков
    /// (игрок песок не держит); на опоре — скорость ×(0.7, −0.5, 0.7) и посадка: клетка заменяема (как пустым стеком),
    /// под ней не проходимо — блок встаёт (обычная запись с обновлениями соседей), иначе выпадает предметом самого блока
    /// (гравий — гравием); в воздухе дольше 600 тиков — тоже предмет. В конце — ×0.98.
    pub(crate) fn tick_falling(&mut self, w: &mut WorldData, zone: &Zone, ev: &mut Events) {
        let n = self.falling.len();
        for i in 0..n {
            let mut f = self.falling[i];
            if f.removed {
                continue;
            }
            f.prev = f.pos;
            if !zone.has_pos(f.pos) {
                self.falling[i] = f;
                continue;
            }
            f.age += 1;
            f.vel[1] -= 0.04;
            let m = f.vel;
            let a = collide(w, &f.bbox(), m, f.on_ground, 0.0, &mut self.fall_bufs);
            if a[0] * a[0] + a[1] * a[1] + a[2] * a[2] > 1e-7 {
                f.pos = [f.pos[0] + a[0], f.pos[1] + a[1], f.pos[2] + a[2]];
            }
            let near = |p: f64, q: f64| (p - q).abs() < 1.0e-5f32 as f64;
            let vertical = m[1] != a[1];
            f.on_ground = vertical && m[1] < 0.0;
            if !near(m[0], a[0]) {
                f.vel[0] = 0.0;
            }
            if !near(m[2], a[2]) {
                f.vel[2] = 0.0;
            }
            if vertical {
                f.vel[1] = 0.0;
            }
            let c = [f.pos[0].floor() as i32, f.pos[1].floor() as i32, f.pos[2].floor() as i32];
            let mut drop = false;
            if f.on_ground {
                f.vel = [f.vel[0] * 0.7, f.vel[1] * -0.5, f.vel[2] * 0.7];
                let here = w.get_cell(c[0], c[1], c[2]);
                let below = w.get_cell(c[0], c[1] - 1, c[2]);
                f.removed = true;
                if !(replaceable_by(here, 0) && !falls_through(below) && self.set_world(w, c, f.cell, ev)) {
                    drop = true;
                }
            } else if (f.age > 100 && (c[1] <= 0 || c[1] >= H)) || f.age > 600 {
                f.removed = true;
                drop = true;
            }
            if drop {
                let it = block_item(block_of(f.cell));
                if it != 0 {
                    self.items.drop_at(f.pos, ItemStack::of(it, 1), &mut self.rng, &mut self.ids);
                }
            }
            f.vel = [f.vel[0] * 0.98, f.vel[1] * 0.98, f.vel[2] * 0.98];
            self.falling[i] = f;
        }
        self.falling.retain(|f| !f.removed);
    }
}