//! Активная зона (E13) и очередь запланированных тиков блоков (E12) — данные мира; сами тики (что делает блок) —
//! game/blockticks.rs.
//!
//! Активная зона — множество кусков (ревью P1-9): объединение квадратов 9×9 вокруг кусков всех игроков (решение после
//! ASP-19, MP-0), позже — якоря (Mekanism). В ней идут запланированные и случайные тики, тикают предметы, падающие блоки
//! и печи; снаружи всё замирает и потом не догоняет. Каждый кусок — один раз, сколько бы игроков рядом ни было.
//! Запланированные тики (WorldAccess.scheduleBlockTick): (клетка, блок, время); дубль (клетка, блок) не ставится — остаётся
//! самый ранний; срабатывают по (время, порядок), только в зоне (остальные ждут в очереди своего куска) и только если в
//! клетке всё ещё этот блок. Песок и гравий — через 2 тика, кактус — через 1.

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap, HashMap, HashSet};

use crate::cell::{BlockId, CS};

/// Радиус квадрата игрока в кусках: 4 → 9×9.
pub(crate) const ZONE_R: i32 = 4;

/// Активная зона: множество кусков. Обход (случайные тики) — по z, затем по x: для одного игрока это прежний порядок
/// квадрата, ГСЧ тиков расходуется так же.
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct Zone {
    /// (cz, cx) — ключ упорядочен для обхода.
    chunks: BTreeSet<(i32, i32)>,
}

impl Zone {
    /// Зона одного игрока: квадрат вокруг куска точки (ступни).
    pub(crate) fn around(p: [f64; 3]) -> Zone {
        let mut z = Zone::default();
        z.add_around(p);
        z
    }
    /// Квадрат вокруг куска (cx, cz).
    pub(crate) fn square(cx: i32, cz: i32) -> Zone {
        let mut z = Zone::default();
        z.add_square(cx, cz, ZONE_R);
        z
    }
    /// Добавить квадрат игрока в точке p.
    pub(crate) fn add_around(&mut self, p: [f64; 3]) {
        let (cx, cz) = ((p[0].floor() as i32).div_euclid(CS), (p[2].floor() as i32).div_euclid(CS));
        self.add_square(cx, cz, ZONE_R);
    }
    /// Добавить квадрат радиуса r вокруг куска (cx, cz) (якорь).
    pub(crate) fn add_square(&mut self, cx: i32, cz: i32, r: i32) {
        for dz in -r..=r {
            for dx in -r..=r {
                self.chunks.insert((cz + dz, cx + dx));
            }
        }
    }
    pub(crate) fn has_chunk(&self, cx: i32, cz: i32) -> bool {
        self.chunks.contains(&(cz, cx))
    }
    /// Клетка (x, z) в зоне.
    pub(crate) fn has(&self, x: i32, z: i32) -> bool {
        self.has_chunk(x.div_euclid(CS), z.div_euclid(CS))
    }
    /// Точка сущности в зоне.
    pub(crate) fn has_pos(&self, p: [f64; 3]) -> bool {
        self.has(p[0].floor() as i32, p[2].floor() as i32)
    }
    /// Куски (cx, cz) по z, затем по x.
    pub(crate) fn chunks(&self) -> impl Iterator<Item = (i32, i32)> + '_ {
        self.chunks.iter().map(|&(z, x)| (x, z))
    }
    pub(crate) fn len(&self) -> usize {
        self.chunks.len()
    }
}

/// Очередь запланированных тиков блоков: по кускам — кучи (время, порядковый номер, клетка, блок).
#[derive(Default)]
pub(crate) struct TickQueue {
    by_chunk: HashMap<(i32, i32), BinaryHeap<Reverse<(u64, u64, [i32; 3], BlockId)>>>,
    queued: HashSet<([i32; 3], BlockId)>,
    seq: u64,
}

impl TickQueue {
    /// Тик клетки p для блока b через delay тиков от времени now (дубль — остаётся прежний).
    pub(crate) fn schedule(&mut self, now: u64, p: [i32; 3], b: BlockId, delay: u32) {
        if !self.queued.insert((p, b)) {
            return;
        }
        self.seq += 1;
        let c = (p[0].div_euclid(CS), p[2].div_euclid(CS));
        self.by_chunk.entry(c).or_default().push(Reverse((now + delay as u64, self.seq, p, b)));
    }
    /// Созревшие (время ≤ now) в кусках зоны — по (время, номер); снимаются с очереди до запуска первого.
    pub(crate) fn take_due(&mut self, now: u64, zone: &Zone) -> Vec<([i32; 3], BlockId)> {
        let mut due = Vec::new();
        for (&(cx, cz), heap) in self.by_chunk.iter_mut() {
            if !zone.has_chunk(cx, cz) {
                continue;
            }
            while let Some(&Reverse((t, n, p, b))) = heap.peek() {
                if t > now {
                    break;
                }
                heap.pop();
                due.push((t, n, p, b));
            }
        }
        self.by_chunk.retain(|_, h| !h.is_empty());
        due.sort_by_key(|e| (e.0, e.1));
        for &(_, _, p, b) in &due {
            self.queued.remove(&(p, b));
        }
        due.into_iter().map(|(_, _, p, b)| (p, b)).collect()
    }
    pub(crate) fn len(&self) -> usize {
        self.queued.len()
    }

    // ---------------------------------------------------------------- сохранение (save/)

    /// Куски, у которых есть тики в очереди.
    pub(crate) fn chunks(&self) -> impl Iterator<Item = (i32, i32)> + '_ {
        self.by_chunk.keys().copied()
    }
    /// Тики куска по (время, номер): (время, номер, клетка, блок).
    pub(crate) fn chunk_entries(&self, cx: i32, cz: i32) -> Vec<(u64, u64, [i32; 3], BlockId)> {
        let Some(h) = self.by_chunk.get(&(cx, cz)) else { return Vec::new() };
        let mut v: Vec<_> = h.iter().map(|r| r.0).collect();
        v.sort_unstable_by_key(|e| (e.0, e.1));
        v
    }
    /// Номер последнего поставленного тика (порядок при равном времени).
    pub(crate) fn seq(&self) -> u64 {
        self.seq
    }
    pub(crate) fn set_seq(&mut self, n: u64) {
        self.seq = self.seq.max(n);
    }
    /// Тик из хранилища — с прежними временем и номером (дубль (клетка, блок) не ставится).
    pub(crate) fn restore(&mut self, t: u64, n: u64, p: [i32; 3], b: BlockId) {
        if !self.queued.insert((p, b)) {
            return;
        }
        self.seq = self.seq.max(n);
        let c = (p[0].div_euclid(CS), p[2].div_euclid(CS));
        self.by_chunk.entry(c).or_default().push(Reverse((t, n, p, b)));
    }
    #[cfg(test)]
    pub(crate) fn contains(&self, p: [i32; 3], b: BlockId) -> bool {
        self.queued.contains(&(p, b))
    }
}

#[cfg(test)]
mod ticks_tests {
    use super::*;
    use crate::registry::ids::SAND;

    #[test]
    fn queue_dedup_order_zone() {
        let mut q = TickQueue::default();
        let z = Zone::square(0, 0);
        q.schedule(10, [1, 64, 1], SAND, 2);
        q.schedule(11, [1, 64, 1], SAND, 2); // дубль — остаётся время 12
        q.schedule(10, [2, 64, 1], SAND, 1);
        q.schedule(10, [500, 64, 1], SAND, 1); // вне зоны — ждёт
        assert_eq!(q.len(), 3);
        assert!(q.take_due(10, &z).is_empty());
        assert_eq!(q.take_due(11, &z), vec![([2, 64, 1], SAND)]);
        assert_eq!(q.take_due(12, &z), vec![([1, 64, 1], SAND)]);
        assert!(q.take_due(13, &z).is_empty());
        assert!(q.contains([500, 64, 1], SAND));
        assert_eq!(q.take_due(13, &Zone::square(31, 0)), vec![([500, 64, 1], SAND)]);
        assert_eq!(q.len(), 0);
        // После снятия — можно поставить снова.
        q.schedule(20, [1, 64, 1], SAND, 2);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn zone_is_9x9() {
        let z = Zone::around([-0.5, 70.0, 15.9]);
        assert_eq!(z.len(), 81);
        assert!(z.has_chunk(-1, 0));
        assert!(z.has_chunk(3, 4) && z.has_chunk(-5, -4) && !z.has_chunk(4, 0) && !z.has_chunk(-1, 5));
        assert!(z.has(-80, 79) && !z.has(-81, 0) && !z.has(64, 0));
        // Обход — по z, затем по x (порядок прежнего квадрата).
        let order: Vec<(i32, i32)> = z.chunks().take(3).collect();
        assert_eq!(order, vec![(-5, -4), (-4, -4), (-3, -4)]);
    }

    #[test]
    fn zone_union_counts_chunk_once() {
        // Два игрока в 6 кусках друг от друга: квадраты перекрываются на 3 столбца — 9·9·2 − 3·9 кусков.
        let mut z = Zone::around([0.5, 70.0, 0.5]);
        z.add_around([6.0 * 16.0 + 0.5, 70.0, 0.5]);
        assert_eq!(z.len(), 81 * 2 - 27);
        assert!(z.has_chunk(10, 0) && z.has_chunk(-4, 4) && !z.has_chunk(11, 0));
    }
}
