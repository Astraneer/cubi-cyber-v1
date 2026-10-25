//! Поиск пути по суше (ASP-16 §5.3–5.4, упрощённый LandPathNodeMaker + A*): узел — клетка ступней; соседи — 4 стороны
//! (тот же уровень, подъём на 1 при свободной голове, спуск до безопасной высоты) и 4 диагонали (только если обе
//! стороны проходимы и не выше); штрафы: вода и берег 8 (курице 0), рядом с кактусом 8; кактус, листва, лава —
//! непроходимы. Вес узла в куче — путь + 1.5 × расстояние до цели (жадный поиск); лимит раскрытий — follow·16, общий
//! бюджет тика — PATH_BUDGET. Не дошёл — путь к ближайшему к цели узлу.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use super::MobStats;
use crate::cell::{block_of, H};
use crate::registry::blocks::{bdef, collision_of};
use crate::registry::ids::*;
use crate::world::WorldData;

/// Что важно пути про моба.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Spec {
    pub(crate) width: f64,
    /// Клеток по высоте тела (свинья, курица — 1; корова, овца, зомби — 2).
    pub(crate) cells: i32,
    pub(crate) max_nodes: i32,
    pub(crate) range: f64,
    pub(crate) safe_fall: i32,
    pub(crate) water_penalty: f32,
    /// Плавает (путь по поверхности воды); зомби — нет: ходит по дну.
    pub(crate) swims: bool,
}

/// Тип клетки для пути (getCommonNodeType, упрощённо).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum T {
    Open,
    Water,
    Lava,
    Blocked,
    Leaves,
    Cactus,
}

pub(crate) fn cell_type(w: &mut WorldData, p: [i32; 3]) -> T {
    if p[1] < 0 {
        return T::Blocked;
    }
    if p[1] >= H {
        return T::Open;
    }
    let c = w.get_cell(p[0], p[1], p[2]);
    let b = block_of(c);
    let d = bdef(b);
    if b == CACTUS {
        return T::Cactus;
    }
    if d.leaves {
        return T::Leaves;
    }
    match d.fluid {
        crate::registry::blocks::FLUID_WATER => return T::Water,
        crate::registry::blocks::FLUID_LAVA => return T::Lava,
        _ => {}
    }
    // Низкие формы (снежный слой 1–2, ковёр) — проходимы; выше полублока — нет.
    let top = collision_of(c).iter().map(|s| s.hi[1]).fold(0.0f64, f64::max);
    if top > 0.5 {
        T::Blocked
    } else {
        T::Open
    }
}

/// Клетка твёрдая (для «вылезти из земли»).
pub(crate) fn solid_at(w: &mut WorldData, p: [i32; 3]) -> bool {
    matches!(cell_type(w, p), T::Blocked | T::Leaves | T::Cactus)
}

/// Штраф узла ступней в p для моба (None — непроходим): клетки тела Open или Water (вода — штраф, если не курица), опора
/// под ногами — твёрдое или вода (плывёт); рядом кактус — 8, рядом вода — штраф воды.
pub(crate) fn node_penalty(w: &mut WorldData, p: [i32; 3], s: &Spec) -> Option<f32> {
    let mut pen = 0.0f32;
    let mut wet = false;
    for k in 0..s.cells {
        match cell_type(w, [p[0], p[1] + k, p[2]]) {
            T::Open => {}
            T::Water => wet = true, // зомби — по дну: вода у тела допустима
            _ => return None,
        }
    }
    let below = cell_type(w, [p[0], p[1] - 1, p[2]]);
    match below {
        T::Blocked | T::Leaves => {}
        T::Water => {
            if !s.swims {
                return None; // не плавает: опора — только дно
            }
            wet = true;
        }
        T::Cactus | T::Lava => return None,
        _ => {
            if !wet {
                return None; // висит в воздухе
            }
        }
    }
    if wet {
        pen = pen.max(s.water_penalty);
    }
    // Соседи (3×3 вокруг ступней): кактус — 8, вода — «берег».
    for dz in -1..=1 {
        for dx in -1..=1 {
            if dx == 0 && dz == 0 {
                continue;
            }
            for dy in 0..=1 {
                match cell_type(w, [p[0] + dx, p[1] + dy - 1, p[2] + dz]) {
                    T::Cactus => pen = pen.max(8.0),
                    T::Water if dy == 0 => pen = pen.max(s.water_penalty),
                    T::Lava => pen = pen.max(8.0),
                    _ => {}
                }
            }
        }
    }
    Some(pen)
}

#[derive(Clone, Copy)]
struct Open {
    f: f32,
    i: u32,
}
impl PartialEq for Open {
    fn eq(&self, o: &Self) -> bool {
        self.f == o.f && self.i == o.i
    }
}
impl Eq for Open {}
impl Ord for Open {
    fn cmp(&self, o: &Self) -> Ordering {
        o.f.partial_cmp(&self.f).unwrap_or(Ordering::Equal).then(o.i.cmp(&self.i))
    }
}
impl PartialOrd for Open {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

struct Node {
    p: [i32; 3],
    g: f32,
    from: u32,
    closed: bool,
}

fn dist(a: [i32; 3], b: [i32; 3]) -> f32 {
    (((a[0] - b[0]).pow(2) + (a[1] - b[1]).pow(2) + (a[2] - b[2]).pow(2)) as f32).sqrt()
}

/// Узел-сосед на (x, z) от узла на высоте y: тот же уровень, на 1 выше (голова свободна) или ниже до safe_fall.
fn step_to(w: &mut WorldData, cache: &mut HashMap<[i32; 3], Option<f32>>, x: i32, y: i32, z: i32, up_ok: bool, s: &Spec) -> Option<([i32; 3], f32)> {
    let mut pen = |w: &mut WorldData, p: [i32; 3]| *cache.entry(p).or_insert_with(|| node_penalty(w, p, s));
    let here = [x, y, z];
    if let Some(v) = pen(w, here) {
        return Some((here, v));
    }
    // Выше на 1: клетка на уровне занята, над ней — узел.
    if up_ok && solid_at(w, here) {
        let up = [x, y + 1, z];
        return pen(w, up).map(|v| (up, v));
    }
    if solid_at(w, here) {
        return None;
    }
    // Спуск: первая клетка ниже с опорой, не глубже safe_fall.
    for d in 1..=s.safe_fall + 1 {
        let q = [x, y - d, z];
        if q[1] < 1 {
            return None;
        }
        if let Some(v) = pen(w, q) {
            return Some((q, v));
        }
        if solid_at(w, q) || cell_type(w, q) == T::Water {
            return None;
        }
    }
    None
}

/// A* от start к goal (дошёл, если манхэттен ≤ dist). Ответ — узлы пути (с начала) и «дошёл ли»; None — старт
/// непроходим или пути нет совсем.
pub(crate) fn find(w: &mut WorldData, start: [i32; 3], goal: [i32; 3], dist_ok: i32, s: &Spec, budget: &mut i32, stats: &mut MobStats) -> Option<(Vec<[i32; 3]>, bool)> {
    let mut cache: HashMap<[i32; 3], Option<f32>> = HashMap::new();
    let mut nodes: Vec<Node> = vec![Node { p: start, g: 0.0, from: u32::MAX, closed: false }];
    let mut index: HashMap<[i32; 3], u32> = HashMap::new();
    index.insert(start, 0);
    let mut heap = BinaryHeap::new();
    heap.push(Open { f: 1.5 * dist(start, goal), i: 0 });
    let mut best = (dist(start, goal), 0u32);
    let mut reached = None;
    let mut expanded = 0;
    while let Some(Open { i, .. }) = heap.pop() {
        if nodes[i as usize].closed {
            continue;
        }
        nodes[i as usize].closed = true;
        expanded += 1;
        let p = nodes[i as usize].p;
        let man = (p[0] - goal[0]).abs() + (p[1] - goal[1]).abs() + (p[2] - goal[2]).abs();
        let dg = dist(p, goal);
        if dg < best.0 {
            best = (dg, i);
        }
        if man <= dist_ok {
            reached = Some(i);
            break;
        }
        if expanded >= s.max_nodes || expanded >= *budget {
            break;
        }
        if (dist(p, start) as f64) > s.range {
            continue;
        }
        // Голова над узлом свободна — можно подниматься.
        let up_ok = !solid_at(w, [p[0], p[1] + s.cells, p[2]]);
        let mut side: [Option<([i32; 3], f32)>; 4] = [None; 4];
        for (k, (dx, dz)) in [(0, 1), (-1, 0), (1, 0), (0, -1)].into_iter().enumerate() {
            side[k] = step_to(w, &mut cache, p[0] + dx, p[1], p[2] + dz, up_ok, s);
        }
        let mut cand: Vec<([i32; 3], f32)> = side.iter().flatten().copied().collect();
        // Диагонали: обе стороны есть и не выше узла, сама диагональ — на том же уровне или ниже.
        for (a, b, dx, dz) in [(0usize, 1usize, -1, 1), (0, 2, 1, 1), (3, 1, -1, -1), (3, 2, 1, -1)] {
            let (Some(sa), Some(sb)) = (side[a], side[b]) else { continue };
            if sa.0[1] > p[1] || sb.0[1] > p[1] {
                continue;
            }
            if let Some(d) = step_to(w, &mut cache, p[0] + dx, p[1], p[2] + dz, false, s) {
                if d.0[1] <= p[1] {
                    cand.push(d);
                }
            }
        }
        let g0 = nodes[i as usize].g;
        for (q, pen) in cand {
            let g = g0 + dist(p, q) + pen;
            if (g as f64) >= s.range * 2.0 {
                continue;
            }
            match index.get(&q) {
                Some(&j) => {
                    let n = &mut nodes[j as usize];
                    if !n.closed && g < n.g {
                        n.g = g;
                        n.from = i;
                        heap.push(Open { f: g + 1.5 * dist(q, goal), i: j });
                    }
                }
                None => {
                    let j = nodes.len() as u32;
                    nodes.push(Node { p: q, g, from: i, closed: false });
                    index.insert(q, j);
                    heap.push(Open { f: g + 1.5 * dist(q, goal), i: j });
                }
            }
        }
    }
    *budget -= expanded;
    stats.path_nodes += expanded as u64;
    stats.paths += 1;
    let end = reached.unwrap_or(best.1);
    let mut out = Vec::new();
    let mut k = end;
    while k != u32::MAX {
        out.push(nodes[k as usize].p);
        k = nodes[k as usize].from;
    }
    out.reverse();
    Some((out, reached.is_some()))
}
