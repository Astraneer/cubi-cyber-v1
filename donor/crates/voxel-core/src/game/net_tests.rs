//! Проверки сети (только для cargo test). MP-0: петля клиент ↔ локальный сервер — главная проверка
//! path_tree_to_stone_pickaxe (game/mod.rs), здесь — что счётчики расхождений ловят расхождение. MP-1 (mp/plan.md):
//! сервер и два клиента через транспорт с задержкой и разбросом (net::Server и net::Peer — те же, что у voxel-server и
//! бота): правка и дроп видны обоим, подбор; ставление в другого игрока — отказ и откат; ходьба без поправок, «читер» —
//! Teleport назад; другой игрок рисуется с отставанием ~2 тика и без рывков.

use std::collections::VecDeque;

use super::client::Client;
use super::interact::break_block;
use super::server::ServerSim;
use super::*;
use crate::abi::*;
use crate::net::{ConnId, Peer, Server};
use crate::proto::{decode, encode, Msg, C2S, CHAT_DEATH, CHAT_DENY, CHAT_JOIN, CHAT_LEAVE, CHAT_PLAYER, CHAT_SYSTEM, S2C};
use crate::registry::items::I_STICK;

/// Пол из камня 9×9 на y = 100 высоко над рельефом, игрок на нём в (0.5, 101, 0.5).
fn scene() -> (World, Game) {
    let w = World::new(1337);
    for x in -4..=4 {
        for z in -4..=4 {
            w.set_block(x, 100, z, STONE);
        }
    }
    let mut g = Game::new(&w);
    g.teleport(0.5, 101.0, 0.5);
    g.body(|p| {
        p.on_ground = true;
        p.vel = [0.0, -0.0784, 0.0];
    });
    g.set_keys(K_LOCKED);
    g.tick();
    (w, g)
}

#[test]
fn counters_catch_divergence() {
    let (w, mut g) = scene();
    g.assert_in_sync();
    let id = g.server.pid.unwrap();
    // 1. Тело у сервера сдвинули мимо клиента: на 0.05 — правдоподобно, итог клиента принят (поправка); на 0.25 — нет:
    // Teleport в состояние сервера, клиент встаёт туда и подтверждает; дальше сходится.
    g.server.sim.player_mut(id).unwrap().player.pos[0] += 0.05;
    g.tick();
    assert_eq!((g.server.sim.stats.corrections, g.server.sim.stats.teleports), (1, 0));
    g.server.sim.player_mut(id).unwrap().player.pos[0] += 0.25;
    g.tick();
    assert_eq!(g.server.sim.stats.teleports, 1);
    assert_eq!(g.client.sim.stats.teleports, 1);
    let want = g.me().player.pos;
    assert_eq!(g.ctl().player.pos, want, "клиент — там, куда вернул сервер");
    for _ in 0..5 {
        g.tick();
    }
    assert_eq!((g.server.sim.stats.corrections, g.server.sim.stats.teleports), (1, 1), "после TeleportAck сходится");
    assert!(g.me().teleport.is_none());
    // 2. У сервера в клетке камень, клиенту не сообщённый: клиент ставит туда землю — сервер отказывает, по Ack блок
    // становится тем, что у сервера (откат предсказания).
    g.give(DIRT as ItemId, 4);
    g.set_look(0.0, 50.0);
    g.tick();
    assert_eq!(g.ctl().target.map(|h| (h.pos, h.face)), Some(([0, 100, 1], crate::cell::F_UP)));
    g.server.sim.sim.world.d.borrow_mut().set_cell(0, 101, 1, STONE as Cell);
    g.press(P_USE);
    g.tick();
    assert_eq!(g.client.sim.stats.rollbacks, 1, "предсказанная земля откатилась");
    assert_eq!(w.get_block(0, 101, 1), STONE, "у клиента — блок сервера");
    assert_eq!(g.ctl().inv.main[0].count, 4, "стек вернулся полной копией инвентаря");
    assert!(g.server.sim.stats.inv_mismatch >= 1);
    // 3. Инвентарь сервера изменили мимо клиента — хеш окна после клика разошёлся: полная копия окна и инвентаря.
    g.open_inventory();
    g.tick();
    g.server.sim.player_mut(id).unwrap().inv.main[30] = ItemStack::of(I_STICK, 5);
    g.screen_click(36, 0, A_PICKUP);
    g.tick();
    assert_eq!(g.server.sim.stats.win_mismatch, 1);
    assert_eq!(g.ctl().inv.main[30], ItemStack::of(I_STICK, 5), "копия с сервера");
    assert_eq!(g.screen_view()[..3], [DIRT as i32, 4, 0], "курсор как у сервера");
}

/// Сервер и клиенты в одном процессе без задержки: кадры — байтами.
struct Net {
    sv: ServerSim,
    cl: Vec<(Client, Option<u32>)>,
}

impl Net {
    fn pump(&mut self) {
        for (c, id) in &mut self.cl {
            for b in c.take_outbox() {
                match (decode::<C2S>(&b).unwrap(), *id) {
                    (C2S::Hello(h), None) => *id = Some(self.sv.join(&h).expect("вход")),
                    (m, Some(i)) => self.sv.handle(i, m),
                    _ => {}
                }
            }
        }
        for (c, id) in &mut self.cl {
            let Some(p) = id.and_then(|i| self.sv.player_mut(i)) else { continue };
            for m in std::mem::take(&mut p.outbox) {
                c.recv(&encode(&m)).unwrap();
            }
        }
    }
    fn tick(&mut self) {
        for (c, _) in &mut self.cl {
            c.sim.tick();
        }
        self.pump();
        self.sv.tick();
        self.pump();
        for (c, _) in &mut self.cl {
            c.sim.tick_visual();
        }
    }
}

#[test]
fn two_clients_one_server() {
    let mut sv = ServerSim::new(World::new(1337));
    sv.debug = true;
    let (wa, wb) = (World::new(1337), World::new(1337));
    let mut net = Net { sv, cl: vec![(Client::new(&wa, "A"), None), (Client::new(&wb, "B"), None)] };
    net.pump();
    let (ia, ib) = (net.cl[0].1.unwrap(), net.cl[1].1.unwrap());
    assert_eq!((ia, ib), (1, 2), "номера игроков — из общего счётчика сущностей");
    // Ник занят — отказ.
    let mut dup = Client::new(&World::new(1337), "a");
    let h = match decode::<C2S>(&dup.take_outbox()[0]).unwrap() {
        C2S::Hello(h) => h,
        _ => unreachable!(),
    };
    assert!(net.sv.join(&h).is_err());
    // B уходит на 400 блоков: зона — два квадрата; B пропал у A (дальше слежения).
    net.tick();
    assert!(net.cl[0].0.sim.remotes.contains_key(&ib), "рядом — видны друг другу");
    net.cl[1].0.sim.teleport(400.5, 120.0, 0.5);
    net.cl[1].0.sim.set_flying(true);
    net.tick();
    assert_eq!(net.sv.sim.zone.len(), 162);
    assert!(!net.cl[0].0.sim.remotes.contains_key(&ib));
    // Предмет у B (далеко от A) тикает; у никого — стоит. Номер предмета — следующий после игроков.
    let s = &mut net.sv.sim;
    s.items.drop_from_block([400, 110, 0], ItemStack::of(DIRT as ItemId, 1), &mut s.rng, &mut s.ids);
    s.items.drop_from_block([0, 110, 800], ItemStack::of(DIRT as ItemId, 1), &mut s.rng, &mut s.ids);
    assert_eq!(s.items.list.iter().map(|e| e.id).collect::<Vec<_>>(), vec![3, 4]);
    for _ in 0..5 {
        net.tick();
    }
    let age: Vec<u32> = net.sv.sim.items.list.iter().map(|e| e.age).collect();
    assert_eq!(age, vec![5, 0]);
    // Предмет у B виден B (слежение), но не A.
    assert_eq!((net.cl[0].0.sim.items.list.len(), net.cl[1].0.sim.items.list.len()), (0, 1));
    // A ломает блок у себя (решение сервера): B далеко — ни правки (кусок вне радиуса), ни звука.
    let i = net.sv.idx(ia).unwrap();
    let a = net.sv.players[i].player.pos;
    let p = [a[0].floor() as i32, a[1].floor() as i32 - 10, a[2].floor() as i32];
    let broken = |net: &mut Net, p: [i32; 3]| -> [usize; 2] {
        let i = net.sv.idx(ia).unwrap();
        net.sv.auth(i, |c| break_block(c, p));
        net.sv.flush();
        let mut fx = [0; 2];
        for (k, (c, id)) in net.cl.iter_mut().enumerate() {
            let pl = net.sv.player_mut(id.unwrap()).unwrap();
            for m in std::mem::take(&mut pl.outbox) {
                if let S2C::Bundle(b) = &m {
                    fx[k] += b.msgs.iter().filter(|m| matches!(m, Msg::Fx { code, .. } if *code as i32 == EV_BROKEN)).count();
                }
                c.recv(&encode(&m)).unwrap();
            }
        }
        fx
    };
    assert_ne!(wa.get_block(p[0], p[1], p[2]), AIR);
    assert_eq!(broken(&mut net, p), [0, 0]);
    assert_eq!((wa.get_block(p[0], p[1], p[2]), wb.get_block(p[0], p[1], p[2]) != AIR), (AIR, true), "B — вне радиуса правок");
    // B вернулся к A: при входе в радиус — правки куска целиком (ChunkEdits); звук ломания — B, не A (A предсказал).
    net.cl[1].0.sim.teleport(a[0] + 3.0, a[1] + 2.0, a[2]);
    for _ in 0..3 {
        net.tick();
    }
    assert_eq!(wb.get_block(p[0], p[1], p[2]), AIR, "кусок вошёл в радиус — правки пришли");
    let q = [p[0], p[1] - 1, p[2]];
    assert_eq!(broken(&mut net, q), [0, 1], "событие ломания — всем рядом, кроме ломавшего");
    assert_eq!((wa.get_block(q[0], q[1], q[2]), wb.get_block(q[0], q[1], q[2])), (AIR, AIR), "правка — обоим");
    // Ушёл — ждёт в parked; у A пропал.
    net.sv.leave(ib);
    net.tick();
    assert_eq!((net.sv.players.len(), net.sv.parked.len()), (1, 1));
    assert!(!net.cl[0].0.sim.remotes.contains_key(&ib) && !net.cl[0].0.sim.names.contains_key(&ib));
}

// ---------------------------------------------------------------- транспорт с задержкой (MP-1)

/// Одна сторона связи: кадры доходят через delay ± jitter мс по порядку (TCP).
#[derive(Default)]
struct Link {
    q: VecDeque<(u64, Vec<u8>)>,
    last: u64,
}

impl Link {
    fn send(&mut self, now: u64, delay: u64, jitter: i64, rng: &mut GameRng, b: Vec<u8>) {
        let j = if jitter > 0 { rng.range(-jitter as i32, jitter as i32) as i64 } else { 0 };
        let at = ((now + delay) as i64 + j).max(now as i64) as u64;
        self.last = self.last.max(at);
        self.q.push_back((self.last, b));
    }
    fn due(&mut self, now: u64) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while self.q.front().is_some_and(|(at, _)| *at <= now) {
            out.push(self.q.pop_front().unwrap().1);
        }
        out
    }
}

/// Сервер и клиенты (net::Server, net::Peer) по часам в мс: клиенты тикают в 0, 50, 100…, сервер — в 25, 75, …
pub(super) struct LagNet {
    pub(super) sv: Server,
    peers: Vec<(Peer, ConnId, Link, Link)>,
    now: u64,
    delay: u64,
    jitter: i64,
    rng: GameRng,
    /// Ходы клиентов на концах их тиков: (номер тика, ступни) — эталон для отставания.
    track: Vec<Vec<(u32, [f64; 3])>>,
    /// Клиент «вне игры» (вкладка скрыта, кадры стоят): тиков у него нет, кадры сервера он принимает.
    away: Vec<bool>,
}

impl LagNet {
    /// Сервер с площадкой из камня 16 × 28 на y = 100 (x −8…7, z −8…19), высоко над рельефом.
    pub(super) fn new(nicks: &[&str], delay: u64, jitter: i64) -> LagNet {
        let list: Vec<(&str, [u8; 16])> = nicks.iter().map(|n| (*n, [0; 16])).collect();
        LagNet::custom(Server::new(1337, true), &list, delay, jitter)
    }
    /// То же со своим сервером (без отладки, с папкой мира) и токенами игроков (G17: права — только у закреплённого ника).
    pub(super) fn custom(mut sv: Server, nicks: &[(&str, [u8; 16])], delay: u64, jitter: i64) -> LagNet {
        for x in -8..8 {
            for z in -8..20 {
                sv.sim.world_edit([x, 100, z], STONE as Cell);
            }
        }
        sv.sim.sim.world.d.borrow_mut().take_log();
        let mut net = LagNet { sv, peers: Vec::new(), now: 0, delay, jitter, rng: GameRng(77), track: Vec::new(), away: Vec::new() };
        for (n, token) in nicks {
            let c = net.sv.open_conn();
            net.peers.push((Peer::with_token(1337, n, *token), c, Link::default(), Link::default()));
            net.track.push(Vec::new());
            net.away.push(false);
        }
        net
    }
    /// 1 мс: доставка, тики клиентов (кратно 50), сервера (25 по модулю 50).
    fn step(&mut self) {
        let now = self.now;
        for (peer, conn, up, down) in &mut self.peers {
            for b in up.due(now) {
                self.sv.frame(*conn, &b);
            }
            for b in down.due(now) {
                peer.recv(&b).unwrap();
            }
        }
        if now % 50 == 0 {
            for (k, (peer, _, up, _)) in self.peers.iter_mut().enumerate() {
                if peer.ready() && !self.away[k] {
                    peer.tick();
                    self.track[k].push((peer.client.sim.n, peer.pos()));
                }
                for b in peer.take() {
                    up.send(now, self.delay, self.jitter, &mut self.rng, b);
                }
            }
        }
        if now % 50 == 25 {
            self.sv.tick();
        }
        let (out, _) = self.sv.take();
        for (c, b) in out {
            if let Some((_, _, _, down)) = self.peers.iter_mut().find(|p| p.1 == c) {
                down.send(now, self.delay, self.jitter, &mut self.rng, b);
            }
        }
        self.now += 1;
    }
    /// n тиков клиента (по 50 мс), перед каждым — f(сеть, номер тика).
    pub(super) fn ticks(&mut self, n: usize, mut f: impl FnMut(&mut LagNet, usize)) {
        for k in 0..n {
            f(self, k);
            for _ in 0..50 {
                self.step();
            }
        }
    }
    pub(super) fn until_ready(&mut self) {
        for _ in 0..200 {
            if self.peers.iter().all(|p| p.0.ready()) {
                return;
            }
            self.ticks(1, |_, _| {});
        }
        panic!("не вошли");
    }
    pub(super) fn peer(&mut self, k: usize) -> &mut Peer {
        &mut self.peers[k].0
    }
    /// Байт от сервера всем клиентам.
    pub(super) fn peers_down(&self) -> u64 {
        self.peers.iter().map(|p| p.0.client.bytes_down).sum()
    }
    pub(super) fn client(&self, k: usize) -> &super::client::ClientSim {
        &self.peers[k].0.client.sim
    }
}

/// Поставить игрока k на площадку в (x, z) и подождать, пока сервер примет.
pub(super) fn put_on(net: &mut LagNet, k: usize, x: f64, z: f64) {
    net.peer(k).teleport([x, 101.0, z]);
    net.peer(k).set_keys(0);
    net.ticks(12, |_, _| {});
}

/// Правка и дроп видны обоим, подбор — у подобравшего; без задержки — второй видит воздух не позже 2 тиков.
fn break_seen(delay: u64, jitter: i64) {
    let mut net = LagNet::new(&["A", "B"], delay, jitter);
    net.until_ready();
    put_on(&mut net, 0, 0.5, 0.5);
    put_on(&mut net, 1, 4.5, 4.5);
    // Земля перед A (запись сервера, ChunkEdits не нужен — кусок уже известен: придёт Blocks).
    net.sv.sim.world_edit([0, 101, 2], DIRT as Cell);
    net.ticks(4, |_, _| {});
    assert_eq!((net.peer(0).block(0, 101, 2), net.peer(1).block(0, 101, 2)), (DIRT, DIRT));
    net.peer(0).set_look(0.0, 35.0);
    net.ticks(2, |_, _| {});
    assert_eq!(net.peer(0).target(), Some([0, 101, 2]));
    let (mut at_a, mut at_b, mut seen) = (None, None, [0usize; 2]);
    net.ticks(60, |n, k| {
        if k == 0 {
            n.peer(0).click();
        }
        n.peer(0).set_keys(if at_a.is_none() { K_ATTACK } else { 0 });
        if at_a.is_none() && n.peer(0).block(0, 101, 2) == AIR {
            at_a = Some(k);
        }
        if at_b.is_none() && n.peer(1).block(0, 101, 2) == AIR {
            at_b = Some(k);
        }
        for p in 0..2 {
            seen[p] = seen[p].max(n.peer(p).items());
        }
    });
    let (a, b) = (at_a.expect("A сломал"), at_b.expect("B увидел"));
    let lag_ticks = (2 * delay + 25 + 50) as usize / 50 + 1;
    assert!(b >= a && b - a <= lag_ticks.max(2), "B видит воздух через {} тиков (A — {a}, B — {b})", b - a);
    assert_eq!(net.sv.status().rejected, 0, "ломание по времени и дальности принято");
    // Дроп — у обоих; A идёт к нему и подбирает: у A в инвентаре, у B предмет пропал.
    assert_eq!(seen, [1, 1], "дроп виден обоим");
    net.peer(0).set_look(0.0, 0.0);
    net.ticks(30, |n, _| n.peer(0).set_keys(K_FWD));
    net.peer(0).set_keys(0);
    net.ticks(lag_ticks + 3, |_, _| {});
    assert_eq!(net.peer(0).count(DIRT as ItemId), 1, "у A в инвентаре");
    assert_eq!((net.peer(0).items(), net.peer(1).items()), (0, 0), "у B предмет пропал");
    let st = net.sv.status();
    assert_eq!((st.corrections, st.teleports), (0, 0));
    assert_eq!((net.peer(0).stats()[0], net.peer(0).stats()[1]), (0, 0), "без откатов и телепортов");
}

#[test]
fn mp1_break_seen_by_both() {
    break_seen(0, 0);
}

#[test]
fn mp1_break_seen_by_both_lagged() {
    break_seen(75, 20);
}

/// A ставит блок туда, где стоит B: сервер отказывает (тело B), у A после Ack блок пропал, стек вернулся.
#[test]
fn mp1_place_into_other_player_rejected() {
    for (delay, jitter) in [(0, 0), (75, 20)] {
        let mut net = LagNet::new(&["A", "B"], delay, jitter);
        net.until_ready();
        put_on(&mut net, 0, 0.5, 0.5);
        put_on(&mut net, 1, 0.5, 2.5);
        net.peer(0).give(DIRT as ItemId, 5);
        net.peer(0).set_look(0.0, 46.0);
        net.ticks(3, |_, _| {});
        assert_eq!(net.peer(0).target(), Some([0, 100, 2]), "верх пола под B");
        let mut seen = false;
        net.ticks(10, |n, k| {
            if k == 0 {
                n.peer(0).press(P_USE);
            }
            if k == 1 {
                seen = n.peer(0).block(0, 101, 2) == DIRT; // предсказание — сразу
            }
        });
        assert!(seen || delay == 0, "A сначала видит свой блок (предсказание до ответа сервера)");
        assert_eq!(net.peer(0).block(0, 101, 2), AIR, "после Ack блок пропал");
        assert_eq!(net.peer(1).block(0, 101, 2), AIR);
        assert_eq!(net.peer(0).count(DIRT as ItemId), 5, "стек вернулся");
        assert_eq!(net.peer(0).stats()[0], 1, "один откат");
    }
}

/// 200 тиков ходьбы, бега, прыжков и приседа у края — ни одной поправки; «читер» сдвинулся на 5 блоков — Teleport назад.
#[test]
fn mp1_no_corrections_then_cheater() {
    let mut net = LagNet::new(&["A"], 75, 20);
    net.until_ready();
    put_on(&mut net, 0, 0.5, -6.5);
    net.ticks(200, |n, k| {
        let keys = match k {
            0..=29 => K_FWD,
            30..=59 => K_FWD | K_SPRINT,
            60..=89 => K_FWD | if k % 12 < 2 { K_JUMP } else { 0 },
            _ => K_FWD | K_SNEAK, // присед: у края площадки (z = 20) — защита края держит
        };
        n.peer(0).set_look(0.0, 0.0);
        n.peer(0).set_keys(keys);
    });
    let st = net.sv.status();
    assert_eq!((st.corrections, st.teleports), (0, 0), "теневой тик сошёлся на каждом тике");
    let p = net.peer(0).pos();
    assert!((p[1] - 101.0).abs() < 1e-9 && p[2] > 19.5 && p[2] < 20.31, "присед удержал у края: {p:?}");
    // «Читер»: +5 по X мимо физики — сервер возвращает.
    net.peer(0).set_keys(0);
    net.ticks(3, |_, _| {});
    let before = net.peer(0).pos();
    net.peer(0).cheat_move([5.0, 0.0, 0.0]);
    net.ticks(8, |_, _| {});
    assert_eq!(net.sv.status().teleports, 1);
    assert_eq!(net.peer(0).stats()[1], 1, "клиент получил Teleport");
    let after = net.peer(0).pos();
    assert!((after[0] - before[0]).abs() < 0.2 && (after[2] - before[2]).abs() < 0.2, "назад: {before:?} → {after:?}");
    net.ticks(10, |n, _| n.peer(0).set_keys(K_FWD));
    assert_eq!(net.sv.status().teleports, 1, "после TeleportAck ходы снова принимаются");
}

/// У B позиция A — та, что была у A ~2 тика назад (плюс сеть), и идёт ровно при неровной доставке.
fn remote_smooth(delay: u64, jitter: i64) {
    let mut net = LagNet::new(&["A", "B"], delay, jitter);
    net.until_ready();
    put_on(&mut net, 0, -6.5, -6.5);
    put_on(&mut net, 1, 6.5, -6.5);
    let ia = net.peer(0).id();
    let mut drawn: Vec<(f64, [f64; 3])> = Vec::new(); // (курсор B в тиках A, позиция)
    let mut lag: Vec<f64> = Vec::new();
    net.ticks(120, |n, k| {
        n.peer(0).set_look(0.0, 0.0);
        n.peer(0).set_keys(if k < 100 { K_FWD } else { 0 });
        let b = &n.peers[1].0.client.sim;
        if let Some(r) = b.remotes.get(&ia) {
            drawn.push((r.t, r.at(r.t).0));
            lag.push(n.track[0].last().map_or(0, |e| e.0) as f64 - r.t);
        }
    });
    assert!(drawn.len() > 100, "B видит A");
    // Рисуется ровно то, где A был на тике курсора (ходы A — эталон), отставание от самого A — 2 тика плюс сеть.
    let truth: std::collections::HashMap<u32, [f64; 3]> = net.track[0].iter().copied().collect();
    let net_ticks = (2 * delay) as f64 / 50.0;
    for (i, (t, p)) in drawn.iter().enumerate().skip(30).take(60) {
        let (a, b) = (truth[&(t.floor() as u32)], truth[&(t.ceil() as u32)]);
        let f = t.fract();
        let q: [f64; 3] = std::array::from_fn(|k| a[k] + (b[k] - a[k]) * f);
        assert!((0..3).all(|k| (p[k] - q[k]).abs() < 1e-4), "кадр {i}: {p:?} ≠ {q:?} (t {t:.2})");
        let l = lag[i];
        assert!(l >= 1.0 && l <= 3.5 + net_ticks + 1.0, "отставание {l:.2} тиков (сеть {net_ticks:.1})");
    }
    // Ровно: шаг курсора 0.9…1.1 тика A, шаг позиции — как у A (ходьба 0.2158 б/т), без остановок и рывков.
    let steps: Vec<f64> = drawn.windows(2).skip(30).take(60).map(|w| w[1].0 - w[0].0).collect();
    assert!(steps.iter().all(|&s| (0.89..=1.11).contains(&s)), "шаги курсора {steps:?}");
    let dz: Vec<f64> = drawn.windows(2).skip(30).take(60).map(|w| w[1].1[2] - w[0].1[2]).collect();
    assert!(dz.iter().all(|&d| d > 0.17 && d < 0.26), "шаги позиции {dz:?}");
}

#[test]
fn mp1_remote_lags_two_ticks_smooth() {
    remote_smooth(0, 0);
}

#[test]
fn mp1_remote_smooth_with_latency_and_jitter() {
    remote_smooth(75, 20);
}

/// Клик без удержания (BreakStart и BreakAbort в одном тике), потом удержание: клиент, как Minecraft, дожимает тот же
/// блок без нового начала — сервер принимает по прогрессу с начала (запись после отмены не снимается). Нашёл бот.
#[test]
fn mp1_click_then_hold_is_accepted() {
    let mut net = LagNet::new(&["A"], 75, 20);
    net.until_ready();
    put_on(&mut net, 0, 0.5, 0.5);
    net.sv.sim.world_edit([0, 101, 2], DIRT as Cell);
    net.peer(0).set_look(0.0, 35.0);
    net.ticks(6, |_, _| {});
    net.ticks(40, |n, k| {
        if k == 0 {
            n.peer(0).click();
            n.peer(0).set_keys(0); // клик в тике — без удержания: начало и отмена
        } else {
            n.peer(0).set_keys(K_ATTACK);
        }
    });
    net.ticks(6, |n, _| n.peer(0).set_keys(0));
    assert_eq!(net.peer(0).block(0, 101, 2), AIR);
    assert_eq!(net.sv.sim.sim.world.get_block(0, 101, 2), AIR, "сервер сломал");
    assert_eq!((net.sv.status().rejected, net.peer(0).stats()[0]), (0, 0), "без отказа и отката");
}

// ---------------------------------------------------------------- Away и перезаход (MP-2, N1b)

/// Состояние игрока k на сервере (тело): (скорость по Y, ступни, на земле).
fn body(net: &LagNet, k: usize) -> (f64, [f64; 3], bool) {
    let id = net.peers[k].0.id();
    let p = &net.sv.sim.player(id).expect("игрок на сервере").player;
    (p.vel[1], p.pos, p.on_ground)
}

/// Игрок ушёл (вкладка скрыта, меню заморозило кадры): без Away он висит там, где замолчал; с Away сервер тикает его с
/// пустым вводом — падает на пол, другой игрок это видит; вернулся — Resume: встаёт в серверное состояние, без
/// телепорта-нарушения и поправок, дальше идёт как обычно, а другие видят его ходы (номер тика не откатился).
fn away_case(delay: u64, jitter: i64) {
    let mut net = LagNet::new(&["A", "B", "C"], delay, jitter);
    net.until_ready();
    put_on(&mut net, 0, 0.5, 0.5);
    put_on(&mut net, 1, 4.5, 4.5);
    put_on(&mut net, 2, -4.5, 4.5);
    let ia = net.peer(0).id();
    // A и C высоко над площадкой, падают. C замолчал без Away (старое поведение), A сообщил об уходе.
    net.peer(0).teleport([0.5, 112.0, 0.5]);
    net.peer(2).teleport([-4.5, 112.0, 4.5]);
    net.ticks(3 + (2 * delay / 50) as usize, |_, _| {}); // телепорт дошёл до сервера
    net.peer(0).set_keys(0);
    net.peer(0).set_away(true);
    net.away[0] = true;
    net.away[2] = true;
    let y_start = body(&net, 0).1[1];
    assert!(y_start < 112.0 && y_start > 106.0, "падение началось: {y_start}");
    // B смотрит, как A падает: позиции A у B по тикам — вниз, без рывков и до пола.
    let mut seen: Vec<f64> = Vec::new();
    net.ticks(60, |n, _| {
        let b = &n.peers[1].0.client.sim;
        if let Some(r) = b.remotes.get(&ia) {
            seen.push(r.at(r.t).0[1]);
        }
    });
    let (vy, pos, ground) = body(&net, 0);
    assert!(ground && (pos[1] - 101.0).abs() < 1e-9 && vy.abs() < 0.1, "A упал на площадку: {pos:?}");
    assert!(seen.windows(2).all(|w| w[1] <= w[0] + 1e-6), "у B A только падает: {seen:?}");
    assert!((seen.last().unwrap() - 101.0).abs() < 0.01, "и приземлился: {:?}", seen.last());
    assert!(seen.windows(2).all(|w| w[0] - w[1] < 2.0), "без рывков");
    let c = body(&net, 2);
    assert!(c.1[1] > 110.0 && !c.2, "без Away тело висит там, где игрок замолчал: {:?}", c.1);
    // Возвращение: сервер ответил Resume; клиент стоит на площадке, как сервер.
    assert_eq!(net.peer(0).resumes(), 0);
    net.peer(0).set_away(false);
    net.away[0] = false;
    let n0 = net.peer(0).client.sim.n;
    net.ticks(8, |_, _| {});
    assert_eq!(net.peer(0).resumes(), 1);
    let (p, s) = (net.peer(0).pos(), body(&net, 0).1);
    assert!((0..3).all(|k| (p[k] - s[k]).abs() < 1e-9) && (p[1] - 101.0).abs() < 1e-9, "клиент {p:?}, сервер {s:?}");
    assert!(net.peer(0).client.sim.n > n0, "тики пошли: {} → {}", n0, net.peer(0).client.sim.n);
    let st = net.sv.status();
    assert_eq!((st.corrections, st.teleports), (0, 0), "возврат — не нарушение, ход сходится с первого тика");
    // Идёт вперёд — B видит ходы A (номера тиков не откатились).
    let at = |net: &LagNet| {
        let r = &net.peers[1].0.client.sim.remotes[&ia];
        r.at(r.t).0
    };
    let before = at(&net);
    net.peer(0).set_look(0.0, 0.0);
    net.ticks(30, |n, _| n.peer(0).set_keys(K_FWD));
    let after = at(&net);
    assert!(after[2] - before[2] > 3.0, "B видит, как A пошёл: {before:?} → {after:?}");
    let st = net.sv.status();
    assert_eq!((st.corrections, st.teleports), (0, 0));
}

#[test]
fn mp2_away_body_keeps_living() {
    away_case(0, 0);
}

#[test]
fn mp2_away_body_keeps_living_lagged() {
    away_case(75, 20);
}

/// Away { on: false } без предшествующего Away { on: true } тоже получает Resume (клиент ждёт ответа и без него не
/// тикает); тики, пришедшие после Away { on: true }, отбрасываются, а не удваивают тик тела.
#[test]
fn mp2_away_edge_cases() {
    let mut net = LagNet::new(&["A"], 0, 0);
    net.until_ready();
    put_on(&mut net, 0, 0.5, 0.5);
    net.peer(0).set_away(false);
    net.ticks(4, |_, _| {});
    assert_eq!(net.peer(0).resumes(), 1, "Resume и на «вернулся» без «ушёл»");
    // Тики после Away { on: true } (клиент ошибся и продолжает) не принимаются: номер тика растёт на один за тик сервера.
    net.peer(0).set_away(true);
    net.ticks(1, |_, _| {});
    let id = net.peer(0).id();
    let n_srv = |net: &LagNet| net.sv.sim.player(id).unwrap().n;
    let n0 = n_srv(&net);
    net.ticks(10, |_, _| {});
    let n1 = n_srv(&net);
    assert!((9..=11).contains(&(n1 - n0)), "номер тика растёт на один за тик сервера, а не вдвое: {n0} → {n1}");
    assert!(net.sv.sim.player(id).unwrap().away);
    assert_eq!(net.sv.status().teleports, 0);
}

/// Перезаход (сеть оборвалась, сервер вернулся): та же сущность и то же место; пока старое соединение ещё числится,
/// вход с тем же ником — NickTaken (клиент JS повторяет); после закрытия — принят, другие видят ходы (номера тиков не
/// откатились, хотя клиент считает с нуля).
#[test]
fn mp2_reconnect_same_player() {
    let mut net = LagNet::new(&["A", "B"], 0, 0);
    net.until_ready();
    put_on(&mut net, 0, 0.5, 0.5);
    put_on(&mut net, 1, 4.5, 4.5);
    net.peer(0).set_look(0.0, 0.0);
    net.ticks(20, |n, _| n.peer(0).set_keys(K_FWD));
    net.peer(0).set_keys(0);
    net.ticks(8, |_, _| {});
    let (ia, pos_a) = (net.peer(0).id(), net.peer(0).pos());
    assert!(net.sv.sim.player(ia).unwrap().n > 20);
    // Новое соединение с тем же ником, пока старое не закрыто: отказ.
    let dup = net.sv.open_conn();
    net.peers.push((Peer::new(1337, "A"), dup, Link::default(), Link::default()));
    net.track.push(Vec::new());
    net.away.push(false);
    net.ticks(3, |_, _| {});
    let late = net.peers.len() - 1;
    // Гость (токен из нулей) прежний сеанс не вытесняет — «ник занят» (код, текст — человеческий).
    let code = net.peers[late].0.client.sim.stats.reject;
    assert_eq!(code, Some(crate::proto::Reject::NickTaken), "{:?}", net.peers[late].0.rejected());
    // Сервер заметил обрыв старого: оно закрыто, отказанное убрано, тот же ник входит заново.
    for k in [late, 0] {
        let (_, conn, _, _) = net.peers.remove(k);
        net.track.remove(k);
        net.away.remove(k);
        net.sv.close_conn(conn);
    }
    net.ticks(2, |_, _| {});
    let conn = net.sv.open_conn();
    net.peers.push((Peer::new(1337, "A"), conn, Link::default(), Link::default()));
    net.track.push(Vec::new());
    net.away.push(false);
    net.until_ready();
    let (a, b) = (1, 0); // B осталась первой, новый A — последним
    assert_eq!(net.peers[a].0.id(), ia, "тот же номер сущности");
    let p = net.peers[a].0.pos();
    assert!((0..3).all(|k| (p[k] - pos_a[k]).abs() < 1e-9), "на том же месте: {p:?} ≠ {pos_a:?}");
    // Идёт — B видит ходы.
    let at = |net: &LagNet| {
        let r = &net.peers[b].0.client.sim.remotes[&ia];
        r.at(r.t).0
    };
    net.ticks(4, |_, _| {});
    let before = at(&net);
    net.peers[a].0.set_look(0.0, 0.0);
    net.ticks(30, |n, _| n.peers[a].0.set_keys(K_FWD));
    let after = at(&net);
    assert!(after[2] - before[2] > 3.0, "после перезахода B видит ходы A: {before:?} → {after:?}");
    assert_eq!(net.sv.status().teleports, 0);
}

/// Линия взгляда для подписей ников: стена мешает, стекло, листва и вода — нет; камера и цель внутри блока — по правилам.
#[test]
fn mp2_line_clear_walls_only() {
    let p = Peer::new(1337, "A");
    let w = &p.world;
    let sim = &p.client.sim;
    let (a, b) = ([0.5, 110.5, 0.5], [10.5, 110.5, 0.5]);
    assert!(sim.line_clear(a, b), "пустой воздух");
    for (x, id) in [(3, GLASS), (5, LEAVES), (7, WATER)] {
        w.set_block(x, 110, 0, id);
    }
    assert!(sim.line_clear(a, b), "стекло, листва и вода не мешают");
    w.set_block(4, 110, 0, STONE);
    assert!(!sim.line_clear(a, b), "камень на пути");
    assert!(!sim.line_clear(b, a), "и обратно");
    assert!(sim.line_clear(a, [3.5, 110.5, 0.5]), "до камня — свободно");
    assert!(!sim.line_clear(a, [4.5, 110.5, 0.5]), "цель внутри камня — не видна");
    assert!(sim.line_clear([4.5, 110.5, 0.5], b), "камера внутри блока: клетка начала не в счёт");
    // Косой луч поверх стены: на 3 блока выше — свободно, по диагонали сквозь неё — нет.
    assert!(sim.line_clear([0.5, 112.5, 0.5], [10.5, 112.5, 0.5]));
    assert!(!sim.line_clear([0.5, 112.5, 0.5], [9.5, 108.5, 0.5]));
    assert!(sim.line_clear(a, a) && sim.line_clear(a, [f64::NAN, 0.0, 0.0]));
}

/// Радиус правок мира следует за клиентом («блоки до»): по умолчанию 8 кусков; клиент просит больше — чужая стройка
/// дальше приходит целиком; меньше — забывается (Forget); сервер ограничивает своим пределом (32 куска).
#[test]
fn mp2_view_radius_follows_client() {
    let mut net = LagNet::new(&["A"], 0, 0);
    net.until_ready();
    put_on(&mut net, 0, 0.5, 0.5);
    // Правки чужой стройки в 12, 30 и 34 кусках от игрока (кусок = 16 блоков), высоко над рельефом.
    let far = |k: i32| [k * 16 + 3, 110, 3];
    for k in [12, 30, 34] {
        net.sv.sim.world_edit(far(k), STONE as Cell);
    }
    net.ticks(4, |_, _| {});
    let seen = |net: &mut LagNet, k: i32| net.peer(0).block(far(k)[0], 110, 3) == STONE;
    assert!(!seen(&mut net, 12), "по умолчанию — 8 кусков: стройка в 12 не видна");
    net.peer(0).set_view(16);
    net.ticks(6, |_, _| {});
    assert!(seen(&mut net, 12) && !seen(&mut net, 30), "просил 16 — пришла стройка в 12");
    net.peer(0).set_view(4);
    net.ticks(6, |_, _| {});
    assert!(!seen(&mut net, 12), "просил 4 — стройка в 12 забыта (Forget)");
    net.peer(0).set_view(200);
    net.ticks(6, |_, _| {});
    assert!(seen(&mut net, 12) && seen(&mut net, 30) && !seen(&mut net, 34), "200 — сервер ограничил 32: 30 видна, 34 нет");
}

/// Выживание по сети (срез 4): падение, смерть и возрождение при задержке 75 ± 20 мс. Урон считает сервер, клиент получает
/// Vitals; после смерти ввод молчит, кнопка возрождения — с 20-го тика, Teleport на точку появления.
#[test]
fn survival_over_lag() {
    let mut net = LagNet::new(&["A"], 75, 20);
    net.until_ready();
    put_on(&mut net, 0, 0.5, 0.5);
    net.sv.sim.players[0].hp.spawn_protect = 0;
    // Падение с 10 блоков (площадка на y = 100: ступни 101 → 111): урон 7, у клиента — те же 13.
    net.peer(0).teleport([0.5, 111.0, 0.5]);
    net.ticks(50, |_, _| {});
    assert_eq!(net.sv.sim.players[0].hp.hp, 13.0, "сервер");
    assert_eq!(net.peer(0).client.sim.vit.hp, 13.0, "клиент знает");
    assert_eq!(net.sv.sim.stats.teleports, 0, "падение без телепортов");
    // Смерть: клиент узнаёт, вещи выпали, кнопка — с 20-го тика.
    net.peer(0).give(AIR as u16 + 1, 5);
    net.ticks(4, |_, _| {});
    net.sv.sim.hurt(0, crate::game::health::DamageKind::Generic, 100.0);
    net.ticks(8, |_, _| {});
    assert!(net.peer(0).client.sim.vit.dead, "клиент: умер");
    assert_eq!(net.peer(0).client.sim.vit.cause, crate::game::health::DamageKind::Generic.code());
    net.peer(0).client.sim.respawn();
    net.ticks(2, |_, _| {});
    assert!(!net.sv.sim.players[0].hp.alive(), "раньше 20 тиков — нельзя");
    net.ticks(30, |_, _| {});
    net.peer(0).client.sim.respawn();
    net.ticks(12, |_, _| {});
    assert!(net.sv.sim.players[0].hp.alive(), "возродился");
    assert!(!net.peer(0).client.sim.vit.dead);
    let (s, c) = (net.sv.sim.players[0].player.pos, net.peer(0).pos());
    assert!((s[0] - c[0]).abs() < 0.2 && (s[2] - c[2]).abs() < 0.2 && (s[1] - c[1]).abs() < 0.5, "клиент там же, где сервер: {s:?} {c:?}");
    assert!(net.sv.sim.players[0].teleport.is_none(), "TeleportAck пришёл");
}

// ---------------------------------------------------------------- G2: Tab, чат, ники

/// Строки чата, пришедшие игроку k после первых seen (вид, текст).
fn chat_of(net: &mut LagNet, k: usize, seen: u32) -> Vec<(u8, String)> {
    net.peer(k).chat_since(seen)
}

/// Tab: оба видят обоих по нику, себя — флагом, пинг приходит после ListPing (раз в 2 с); вход и выход — строки в чате.
#[test]
fn g2_tab_list_ping_and_presence_lines() {
    let mut net = LagNet::new(&["Боб", "ann"], 0, 0);
    net.until_ready();
    net.ticks(90, |_, _| {}); // два ListPing
    for k in 0..2 {
        let tab = net.peer(k).tab();
        // Порядок — по нику без учёта регистра: «ann» раньше «Боб».
        assert_eq!(tab.iter().map(|t| t.0.as_str()).collect::<Vec<_>>(), ["ann", "Боб"], "peer {k}");
        assert_eq!(tab.iter().filter(|t| t.2).count(), 1, "свой — один");
        assert!(tab[if k == 0 { 1 } else { 0 }].2, "флаг «это вы» у своего ника");
        assert!(tab.iter().all(|t| t.1 >= 0), "пинг пришёл: {tab:?}");
    }
    // Входы: «Боб» вошёл первым и видит вход «ann»; у ann своя строка о входе.
    let bob: Vec<String> = chat_of(&mut net, 0, 0).into_iter().filter(|l| l.0 == CHAT_JOIN).map(|l| l.1).collect();
    assert!(bob.contains(&"Боб присоединился к игре".to_string()) && bob.contains(&"ann присоединился к игре".to_string()), "{bob:?}");
    let ann: Vec<String> = chat_of(&mut net, 1, 0).into_iter().filter(|l| l.0 == CHAT_JOIN).map(|l| l.1).collect();
    assert!(ann.contains(&"ann присоединился к игре".to_string()), "{ann:?}");
    // Боб вышел — у ann строка и Tab из одного.
    let seen = net.peer(1).chat_total();
    let (_, conn, _, _) = net.peers.remove(0);
    net.track.remove(0);
    net.away.remove(0);
    net.sv.close_conn(conn);
    net.ticks(4, |_, _| {});
    let left = chat_of(&mut net, 0, seen);
    assert_eq!(left, vec![(CHAT_LEAVE, "Боб вышел из игры".to_string())]);
    assert_eq!(net.peer(0).tab().len(), 1);
}

/// Чат: сообщение приходит всем (и отправителю) как «<Ник> текст», текст чистится; команды отвечают только спросившему.
#[test]
fn g2_chat_delivers_cleans_and_answers_commands() {
    let mut net = LagNet::new(&["Ann", "Bob"], 75, 20);
    net.until_ready();
    net.ticks(10, |_, _| {});
    let seen = [net.peer(0).chat_total(), net.peer(1).chat_total()];
    net.peer(0).chat_send("  привет \n  мир\u{7} ");
    net.ticks(8, |_, _| {});
    for k in 0..2 {
        assert_eq!(chat_of(&mut net, k, seen[k]), vec![(CHAT_PLAYER, "<Ann> привет мир".to_string())], "peer {k}");
    }
    // /list — ответ только Ann; для Bob тишина.
    let seen = [net.peer(0).chat_total(), net.peer(1).chat_total()];
    net.peer(0).chat_send("/list");
    net.ticks(8, |_, _| {});
    assert_eq!(chat_of(&mut net, 0, seen[0]), vec![(CHAT_SYSTEM, "В сети (2): Ann, Bob".to_string())]);
    assert!(chat_of(&mut net, 1, seen[1]).is_empty());
    net.peer(1).chat_send("/ban Ann");
    net.ticks(8, |_, _| {});
    let r = chat_of(&mut net, 1, seen[1]);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].0, CHAT_DENY);
    // Пустое (только пробелы) не уходит вовсе.
    let n = net.peer(0).chat_total();
    net.peer(0).chat_send(" \n\t ");
    net.ticks(8, |_, _| {});
    assert_eq!(net.peer(0).chat_total(), n);
}

/// Частота: подряд пять сообщений — до четырёх доходят, остальное — серая строка отказа отправителю; через 2 с снова можно.
#[test]
fn g2_chat_rate_limit() {
    let mut net = LagNet::new(&["Ann", "Bob"], 0, 0);
    net.until_ready();
    net.ticks(10, |_, _| {});
    let seen = [net.peer(0).chat_total(), net.peer(1).chat_total()];
    for i in 0..6 {
        net.peer(0).chat_send(&format!("м{i}"));
    }
    net.ticks(6, |_, _| {});
    let bob = chat_of(&mut net, 1, seen[1]);
    assert_eq!(bob.iter().map(|l| l.1.as_str()).collect::<Vec<_>>(), ["<Ann> м0", "<Ann> м1", "<Ann> м2", "<Ann> м3"], "Bob видит четыре");
    let ann = chat_of(&mut net, 0, seen[0]);
    assert_eq!(ann.iter().filter(|l| l.0 == CHAT_DENY && l.1.contains("Слишком часто")).count(), 2, "{ann:?}");
    net.ticks(45, |_, _| {});
    let seen = net.peer(1).chat_total();
    net.peer(0).chat_send("снова");
    net.ticks(6, |_, _| {});
    assert_eq!(chat_of(&mut net, 1, seen), vec![(CHAT_PLAYER, "<Ann> снова".to_string())]);
}

/// Ники сетевого сервера: 3–16, латиница и кириллица; короткий, с пробелом, иероглифами — отказ BadNick.
#[test]
fn g2_strict_nicks() {
    for bad in ["ab", "Игрок 7", "名前名前名", "abcdefghijklmnopq"] {
        let mut net = LagNet::new(&[bad], 0, 0);
        net.sv.set_strict_nicks(true);
        net.ticks(6, |_, _| {});
        assert_eq!(net.peer(0).client.sim.stats.reject, Some(crate::proto::Reject::BadNick), "{bad}");
        assert!(net.peer(0).rejected().unwrap().contains("3–16"), "{:?}", net.peer(0).rejected());
    }
    let mut net = LagNet::new(&["Вася_1"], 0, 0);
    net.sv.set_strict_nicks(true);
    net.until_ready();
    assert_eq!(net.peer(0).tab().len(), 1);
}

/// Выживание объявляет смерть через ServerSim::announce(CHAT_DEATH, …): строка доходит всем как есть.
#[test]
fn g2_announce_for_death_events() {
    let mut net = LagNet::new(&["Ann", "Bob"], 0, 0);
    net.until_ready();
    net.ticks(10, |_, _| {});
    let seen = [net.peer(0).chat_total(), net.peer(1).chat_total()];
    let id = net.peer(1).id();
    net.sv.sim.announce(CHAT_DEATH, id, "Bob разбился".into());
    net.ticks(4, |_, _| {});
    for k in 0..2 {
        assert_eq!(chat_of(&mut net, k, seen[k]), vec![(CHAT_DEATH, "Bob разбился".to_string())]);
    }
}

// ---------------------------------------------------------------- G7: вёдра по сети

#[test]
fn g7_bucket_over_lag() {
    // Ведро воды по сети с задержкой 75 ± 20 мс: клиент предсказал источник, сервер подтвердил без отказа и отката;
    // вода растекается, второй игрок видит те же клетки; ведро пустеет.
    let mut net = LagNet::new(&["A", "B"], 75, 20);
    net.until_ready();
    put_on(&mut net, 0, 0.5, 0.5);
    put_on(&mut net, 1, -5.5, 8.5);
    net.peer(0).give(crate::registry::items::I_WATER_BUCKET, 1);
    net.peer(0).set_look(0.0, 50.0);
    net.ticks(8, |_, _| {});
    assert_eq!(net.peer(0).target(), Some([0, 100, 1]));
    net.ticks(1, |n, _| n.peer(0).press(P_USE));
    net.ticks(60, |_, _| {});
    let w = net.sv.sim.sim.world.share();
    assert_eq!(w.get_block(0, 101, 1), WATER, "источник у сервера");
    assert_eq!((net.peer(0).block(0, 101, 1), net.peer(1).block(0, 101, 1)), (WATER, WATER), "оба видят источник");
    let flow = |net: &mut LagNet, k: usize| -> usize {
        let mut n = 0;
        for x in -8..=7 {
            for z in -7..=9 {
                if net.peer(k).block(x, 101, z) == WATER {
                    n += 1;
                }
            }
        }
        n
    };
    let (a, b) = (flow(&mut net, 0), flow(&mut net, 1));
    assert!(a > 30, "вода растеклась: {a}");
    assert_eq!(a, b, "оба игрока видят одну воду");
    assert_eq!(net.peer(0).count(crate::registry::items::I_WATER_BUCKET as u16), 0);
    assert_eq!(net.peer(0).count(crate::registry::items::I_BUCKET as u16), 1);
    assert_eq!((net.sv.status().rejected, net.peer(0).stats()[0]), (0, 0), "без отказа и отката");
}

// ---------------------------------------------------------------- мобы по сети (G6)

/// Два игрока по сети с задержкой 75 ± 20 мс видят ту же свинью (номер, вид) там же, где сервер, с отставанием ~2–4 тика;
/// удар одного — урон у сервера и красная вспышка у обоих; убитая — пропадает у обоих.
#[test]
fn mobs_seen_by_both_players() {
    use crate::game::mob::MobKind;
    let mut net = LagNet::new(&["Аня", "Боря"], 75, 20);
    net.sv.sim.sim.mobs.herds = false;
    net.sv.sim.sim.mobs.natural = false;
    net.until_ready();
    put_on(&mut net, 0, 0.5, 0.5);
    put_on(&mut net, 1, 4.5, 10.5);
    let id = net.sv.sim.sim.spawn_mob(MobKind::Pig, [2.5, 101.0, 4.5], false).unwrap();
    let mut worst = [0.0f64; 2];
    net.ticks(200, |n, k| {
        if k < 40 {
            return;
        }
        let sv = n.sv.sim.sim.mobs.get(id).unwrap().pos;
        for p in 0..2 {
            let c = &n.peers[p].0.client.sim.mobs;
            let m = c.list.get(&id).expect("свинью видят оба");
            assert_eq!(m.kind, MobKind::Pig);
            let s = m.at(c.t);
            let gap = ((s.pos[0] as f64 - sv[0]).powi(2) + (s.pos[2] as f64 - sv[2]).powi(2)).sqrt();
            worst[p] = worst[p].max(gap);
        }
    });
    assert!(worst.iter().all(|&g| g < 1.2), "отставание отрисовки от сервера: {worst:?}");
    // Удар A: у сервера −1, у обоих — вспышка (Anim 1).
    let i = net.sv.sim.idx(net.peers[0].0.id()).unwrap();
    net.sv.sim.players[i].atk_ticks = 100;
    let at = net.sv.sim.sim.mobs.get(id).unwrap().pos;
    net.sv.sim.players[i].player.pos = [at[0], at[1], at[2] - 1.5];
    net.sv.sim.attack_mob(i, id);
    assert_eq!(net.sv.sim.sim.mobs.get(id).unwrap().life.hp, 9.0);
    net.ticks(8, |_, _| {});
    for p in 0..2 {
        let d = net.peers[p].0.client.sim.mob_draws(0.0);
        let k = d.chunks(crate::game::mob::view::DRAW_STRIDE).position(|c| c[0] as u32 == id).unwrap();
        assert_eq!(d[k * crate::game::mob::view::DRAW_STRIDE + 10], 1.0, "красная вспышка у игрока {p}");
    }
    // Убита — через 20 тиков на боку пропадает у обоих.
    let k = net.sv.sim.sim.mobs.index(id).unwrap();
    let mut ev = crate::game::view::Events::default();
    net.sv.sim.sim.hurt_mob(k, crate::game::mob::Blow { kind: crate::game::health::DamageKind::Generic, amount: 50.0, by: None }, &mut ev);
    net.ticks(30, |_, _| {});
    assert!(net.peers.iter().all(|p| !p.0.client.sim.mobs.list.contains_key(&id)), "убранная — у обоих");
}

/// Номера мобов у сервера в радиусе r от тела игрока k (на сервере).
fn mobs_near_server(net: &LagNet, k: usize, r: f64) -> std::collections::BTreeSet<u32> {
    let id = net.peers[k].0.id();
    let Some(p) = net.sv.sim.player(id) else { return Default::default() };
    let me = p.player.pos;
    net.sv.sim.sim.mobs.list.iter().filter(|m| !m.removed && (0..3).map(|a| (m.pos[a] - me[a]).powi(2)).sum::<f64>() <= r * r).map(|m| m.id).collect()
}

/// «Невидимый зомби» (reports/agent-mobfix.md), сторона ядра: моб, который у сервера всё время в радиусе слежения
/// (110 из 128 блоков, дольше задержки сети), есть и в зеркале клиента — ночью со стадами и зомби, при ходьбе, уходе
/// (Away) и возврате, смерти и возрождении. Задержка 75 ± 20 мс.
#[test]
fn mobs_mirror_has_every_tracked_mob() {
    let mut net = LagNet::new(&["Аня"], 75, 20);
    net.until_ready();
    put_on(&mut net, 0, 0.5, 0.5);
    net.sv.sim.sim.day = 18000; // полночь: зомби появляются сами
    net.sv.sim.sim.day_cycle = false;
    let lag = 2 * 75 / 50 + 4;
    let mut hist: VecDeque<std::collections::BTreeSet<u32>> = VecDeque::new();
    let mut misses: Vec<String> = Vec::new();
    let mut seen = 0usize;
    let mut step = |net: &mut LagNet, what: &str| {
        let now = mobs_near_server(net, 0, 110.0);
        hist.push_back(now.clone());
        if hist.len() > lag {
            let old = hist.pop_front().unwrap();
            let c = &net.peers[0].0.client.sim.mobs;
            let both: Vec<u32> = old.intersection(&now).copied().collect();
            seen = seen.max(both.len());
            let miss: Vec<u32> = both.into_iter().filter(|id| !c.list.contains_key(id)).collect();
            if !miss.is_empty() && misses.len() < 5 {
                misses.push(format!("{what}: у клиента нет {miss:?}"));
            }
        }
        net.ticks(1, |_, _| {});
    };
    for t in 0..400 {
        net.peer(0).set_look((t * 7 % 360) as f32, 0.0);
        net.peer(0).set_keys(if t % 40 < 20 { K_FWD } else { 0 });
        step(&mut net, "ходьба");
    }
    net.peer(0).set_keys(0);
    net.peer(0).set_away(true);
    net.away[0] = true;
    for _ in 0..200 {
        step(&mut net, "ушёл");
    }
    net.peer(0).set_away(false);
    net.away[0] = false;
    for _ in 0..200 {
        step(&mut net, "вернулся");
    }
    let i = net.sv.sim.idx(net.peers[0].0.id()).unwrap();
    net.sv.sim.hurt(i, crate::game::health::DamageKind::Generic, 100.0);
    for t in 0..300 {
        if t == 40 {
            net.peer(0).respawn();
        }
        step(&mut net, "смерть и возрождение");
    }
    assert!(seen > 10, "мобы рядом были: {seen}");
    assert!(misses.is_empty(), "{misses:?}");
}
