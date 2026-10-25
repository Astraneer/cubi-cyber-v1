//! Тики блоков (E12): запланированные (песок и гравий падают, кактус без опоры ломается), случайные (опадающая листва)
//! и пересчёт опада. Очередь и зона — world/ticks.rs; что делает блок — таблица (falling, support, random).
//!
//! Случайные тики (ServerWorld.tickChunk, randomTickSpeed 3): 3 случайные клетки на секцию 16³ за тик; у нас кусок —
//! 8 секций. Пока единственный блок со случайным тиком — опадающая листва (бит состояния бывает только у правок), поэтому
//! выборка идёт лишь в кусках зоны, где есть правки, и смотрит в правки: в остальных клетках она ничего бы не сделала
//! (трава → земля, таяние снега, рост посевов это изменят — тогда выборку вести по всем кускам зоны).
//! Опад листвы (решение после ASP-05): дистанцию не храним — при изменении бревна или листвы поиск в ширину по листве
//! (≤ 6 шагов) ставит или снимает бит «опадает»; опадает случайным тиком — дроп без инструмента и удаление.

use std::collections::HashMap;

use crate::cell::{block_of, cell_of, state_of, Cell, CS, DIRS, H};
use crate::game::entity::FallingBlock;
use crate::game::interact::can_place_at;
use crate::game::sim::Sim;
use crate::game::view::Events;
use crate::registry::blocks::{bdef, falls_through, is_leaves, is_log, RandomTick, Support, ST_DECAY, ST_PERSIST};
use crate::registry::ids::AIR;
use crate::world::ticks::Zone;
use crate::world::WorldData;

impl Sim {
    /// Фаза мира: запланированные тики блоков в зоне.
    pub(crate) fn run_scheduled(&mut self, w: &mut WorldData, zone: &Zone, ev: &mut Events) {
        for (p, b) in self.ticks.take_due(self.time, zone) {
            let c = w.get_cell(p[0], p[1], p[2]);
            if block_of(c) != b {
                continue; // блок сменился — тик пропадает
            }
            let def = bdef(b);
            if def.fluid != 0 {
                self.fluid_tick(w, p, ev); // текучая вода и лава (game/fluids.rs)
            } else if def.falling {
                // FallingBlock.scheduledTick: снизу проходимо — стать падающей сущностью (клетка — воздух, соседям
                // обновления: следующий песок столба планирует своё падение, факел на песке отваливается).
                if p[1] >= 0 && falls_through(w.get_cell(p[0], p[1] - 1, p[2])) {
                    self.set_world(w, p, AIR as Cell, ev);
                    let id = self.ids.next();
                    self.falling.push(FallingBlock::new(id, c, p));
                }
            } else if def.farmland {
                self.farmland_scheduled(w, p, ev);
            } else if def.support == Support::Cactus {
                // CactusBlock.scheduledTick: опоры нет — сломаться с дропом (World.breakBlock).
                if !can_place_at(w, b, p) {
                    self.break_naturally(w, p, ev);
                }
            }
        }
    }

    /// Фаза мира: случайные тики — 3 клетки на секцию 16³ в кусках зоны (см. шапку: только куски с правками); куски —
    /// по z, затем по x, каждый один раз.
    pub(crate) fn random_ticks(&mut self, w: &mut WorldData, zone: &Zone, ev: &mut Events) {
        for (cx, cz) in zone.chunks() {
            if !w.has_edits(cx, cz) {
                continue;
            }
            for sec in 0..H / 16 {
                for _ in 0..self.random_tick_speed {
                    let r = self.tick_rng.next_u64();
                    let (lx, ly, lz) = ((r & 15) as i32, (r >> 4 & 15) as i32, (r >> 8 & 15) as i32);
                    let y = sec * 16 + ly;
                    let p = [cx * CS + lx, y, cz * CS + lz];
                    let Some(c) = w.edited_cell(p[0], p[1], p[2]) else { continue };
                    self.random_tick(w, p, c, ev);
                }
            }
        }
    }

    /// Случайный тик клетки: опадающая листва (LeavesBlock.randomTick) — дроп по таблице без инструмента, удаление
    /// без звука и частиц (World.removeBlock).
    pub(crate) fn random_tick(&mut self, w: &mut WorldData, p: [i32; 3], c: Cell, ev: &mut Events) {
        let (b, s) = (block_of(c), state_of(c));
        match bdef(b).random {
            RandomTick::LeafDecay if s & ST_DECAY != 0 && s & ST_PERSIST == 0 => {
                self.drop_loot(c, p, false);
                self.set_world(w, p, AIR as Cell, ev);
            }
            RandomTick::Farmland | RandomTick::Crop => self.farm_random_tick(w, p, c, ev),
            _ => {}
        }
    }

    /// Пересчёт опада после изменения клетки p (бревно или листва): поиск в ширину по листве от p до 12 шагов; листья
    /// не дальше 6 шагов от p — «кандидаты» (их дистанция до бревна могла измениться); от брёвен рядом с найденной
    /// листвой — второй поиск по ней же до 6 шагов. Кандидат держится (бит «опадает» снят), если до бревна ≤ 6 шагов
    /// по листве (LeavesBlock: DISTANCE ≤ 6), иначе бит ставится. Запись — без обновлений соседей (меш перестроится).
    pub(crate) fn leaves_recheck(&mut self, w: &mut WorldData, p: [i32; 3]) {
        let mut depth: HashMap<[i32; 3], u8> = HashMap::new();
        let mut queue = std::collections::VecDeque::new();
        let mut logs: Vec<[i32; 3]> = Vec::new();
        let here = block_of(w.get_cell(p[0], p[1], p[2]));
        if is_leaves(here) {
            depth.insert(p, 0);
            queue.push_back(p);
        } else {
            if is_log(here) {
                logs.push(p);
            }
            for d in DIRS {
                let q = [p[0] + d[0], p[1] + d[1], p[2] + d[2]];
                if is_leaves(w.get_block(q[0], q[1], q[2])) && !depth.contains_key(&q) {
                    depth.insert(q, 1);
                    queue.push_back(q);
                }
            }
        }
        while let Some(q) = queue.pop_front() {
            let k = depth[&q];
            for d in DIRS {
                let n = [q[0] + d[0], q[1] + d[1], q[2] + d[2]];
                let nb = w.get_block(n[0], n[1], n[2]);
                if is_log(nb) {
                    if !logs.contains(&n) {
                        logs.push(n);
                    }
                } else if k < 12 && is_leaves(nb) && !depth.contains_key(&n) {
                    depth.insert(n, k + 1);
                    queue.push_back(n);
                }
            }
        }
        if !depth.values().any(|&k| k <= 6) {
            return;
        }
        // Дистанция от брёвен по найденной листве (1 — лист рядом с бревном).
        let mut dist: HashMap<[i32; 3], u8> = HashMap::new();
        for l in &logs {
            for d in DIRS {
                let n = [l[0] + d[0], l[1] + d[1], l[2] + d[2]];
                if depth.contains_key(&n) && !dist.contains_key(&n) {
                    dist.insert(n, 1);
                    queue.push_back(n);
                }
            }
        }
        while let Some(q) = queue.pop_front() {
            let k = dist[&q];
            if k >= 6 {
                continue;
            }
            for d in DIRS {
                let n = [q[0] + d[0], q[1] + d[1], q[2] + d[2]];
                if depth.contains_key(&n) && !dist.contains_key(&n) {
                    dist.insert(n, k + 1);
                    queue.push_back(n);
                }
            }
        }
        let mut cand: Vec<[i32; 3]> = depth.iter().filter(|(_, &k)| k <= 6).map(|(q, _)| *q).collect();
        cand.sort(); // порядок записей не важен, но детерминирован
        for q in cand {
            let c = w.get_cell(q[0], q[1], q[2]);
            let s = state_of(c);
            let decay = !dist.contains_key(&q);
            let ns = if decay { s | ST_DECAY } else { s & !ST_DECAY };
            if ns != s {
                w.set_cell(q[0], q[1], q[2], cell_of(block_of(c), ns));
            }
        }
    }
}
