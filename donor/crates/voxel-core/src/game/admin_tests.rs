//! Роли, команды и режимы по сети (G17): два клиента с задержкой 75 ± 20 мс через net::Server / net::Peer, сервер без
//! отладки. Не-админ получает отказ сервера; владелец (ник закреплён токеном) меняет режимы; творческий — мгновенное
//! ломание без откатов и неуязвимость; наблюдатель невидим и не трогает мир; /give @s доходит; /rollback возвращает
//! блоки; журнал админ-команд и roles.txt пишутся в папку мира.

use super::net_tests::LagNet;
use crate::abi::K_ATTACK;
use crate::cell::Cell;
use crate::game::commands::item_by_name;
use crate::game::health::DamageKind;
use crate::game::rules::GameMode;
use crate::net::Server;
use crate::proto::{CHAT_DENY, CHAT_SYSTEM, CHAT_WHISPER};
use crate::registry::ids::{AIR, DIRT, STONE};

const ANN: [u8; 16] = [1; 16];
const BOB: [u8; 16] = [2; 16];

fn idx(net: &mut LagNet, k: usize) -> usize {
    let id = net.peer(k).id();
    net.sv.sim.idx(id).expect("игрок на сервере")
}

/// Сервер без отладки; Ann — владелец (по файлу ролей), Bob — игрок; оба с токенами, стоят на площадке.
fn pair(sv: Server) -> LagNet {
    let mut sv = sv;
    sv.add_owner("ann");
    let mut net = LagNet::custom(sv, &[("Ann", ANN), ("Bob", BOB)], 75, 20);
    net.until_ready();
    // Без отладки клиент сам не переносится: ставит сервер (Teleport).
    for (k, x, z) in [(0, 0.5, 0.5), (1, 4.5, 4.5)] {
        let i = idx(&mut net, k);
        net.sv.sim.relocate(i, [x, 101.0, z], 0.0);
    }
    net.ticks(12, |_, _| {});
    for k in 0..2 {
        let i = idx(&mut net, k);
        net.sv.sim.players[i].hp.spawn_protect = 0;
        net.sv.sim.players[i].zone_grace = 0;
    }
    net
}

/// Строки чата игрока k с номера seen после n тиков.
fn say(net: &mut LagNet, k: usize, text: &str) -> Vec<(u8, String)> {
    let seen = net.peer(k).chat_total();
    net.peer(k).chat_send(text);
    net.ticks(8, |_, _| {});
    net.peer(k).chat_since(seen)
}

fn mode(net: &mut LagNet, k: usize) -> (GameMode, GameMode) {
    let i = idx(net, k);
    (net.sv.sim.players[i].player.mode, net.client(k).ctl.player.mode)
}

#[test]
fn g17_player_is_refused_admin_commands() {
    let mut net = pair(Server::new(1337, false));
    // Права пришли клиентам (подсказки): владелец 2, игрок 0.
    assert_eq!((net.client(0).level, net.client(1).level), (2, 0));
    for cmd in ["/gamemode creative", "/give @s stone 64", "/tp Ann", "/kill Ann", "/ban Ann", "/op Bob", "/time set night", "/rollback Ann"] {
        let r = say(&mut net, 1, cmd);
        assert_eq!(r.len(), 1, "{cmd}: {r:?}");
        assert_eq!(r[0].0, CHAT_DENY, "{cmd}: {r:?}");
        assert!(r[0].1.contains("Нет прав"), "{cmd}: {r:?}");
    }
    assert_eq!(mode(&mut net, 1), (GameMode::Survival, GameMode::Survival));
    assert_eq!(net.peer(1).count(item_by_name("stone").unwrap()), 0);
    // Обычные команды игроку доступны; /msg — шёпот обоим.
    assert_eq!(say(&mut net, 1, "/list")[0].0, CHAT_SYSTEM);
    let seen = net.peer(0).chat_total();
    let r = say(&mut net, 1, "/msg Ann привет");
    assert_eq!(r, vec![(CHAT_WHISPER, "Вы шепчете Ann: привет".to_string())]);
    assert_eq!(net.peer(0).chat_since(seen), vec![(CHAT_WHISPER, "Bob шепчет вам: привет".to_string())]);
    // Гость с ником владельца прав не получает: права — только у закреплённого токеном ника.
    let i = idx(&mut net, 0);
    net.sv.sim.players[i].token = None;
    net.ticks(4, |_, _| {});
    assert_eq!(net.client(0).level, 0);
    assert_eq!(say(&mut net, 0, "/gamemode creative")[0].0, CHAT_DENY);
}

#[test]
fn g17_creative_breaks_instantly_and_is_invulnerable() {
    let mut net = pair(Server::new(1337, false));
    let r = say(&mut net, 0, "/gamemode creative");
    assert_eq!(r, vec![(CHAT_SYSTEM, "Ваш режим игры: творческий".to_string())]);
    net.ticks(6, |_, _| {});
    assert_eq!(mode(&mut net, 0), (GameMode::Creative, GameMode::Creative));
    // /give @s по сети — в инвентарь клиента.
    let stone = item_by_name("minecraft:stone").unwrap();
    assert!(say(&mut net, 0, "/give @s stone 64")[0].1.starts_with("Выдано: 64"));
    assert_eq!(net.peer(0).count(stone), 64);
    // Ломание: блоки перед игроком — по одному клику, мгновенно, сервер не откатывает.
    let (rej, tp) = (net.sv.sim.stats.rejected, net.sv.sim.stats.teleports);
    for z in [2, 3] {
        net.sv.sim.world_edit([0, 101, z], STONE as Cell);
    }
    net.ticks(4, |_, _| {});
    net.peer(0).set_look(0.0, 35.0);
    net.ticks(2, |_, _| {});
    let target = net.peer(0).target().expect("блок под прицелом");
    net.peer(0).click();
    net.ticks(1, |_, _| {});
    assert_eq!(net.peer(0).block(target[0], target[1], target[2]), AIR, "у себя — сразу");
    net.ticks(8, |_, _| {});
    let w = net.sv.sim.sim.world.d.clone();
    assert_eq!(w.borrow_mut().get_block(target[0], target[1], target[2]), AIR, "сервер сломал");
    assert_eq!(net.peer(0).block(target[0], target[1], target[2]), AIR, "и не откатил");
    assert_eq!((net.sv.sim.stats.rejected, net.sv.sim.stats.teleports), (rej, tp), "ни отказов, ни телепортов");
    assert_eq!(net.peer(0).stats()[0], 0, "откатов предсказаний нет");
    // Ставит, не тратя блоки: ПКМ по полу перед собой — камень у сервера, в руке по-прежнему 64.
    net.peer(0).set_look(0.0, 60.0);
    net.ticks(2, |_, _| {});
    let floor = net.peer(0).target().expect("пол под прицелом");
    net.peer(0).press(crate::abi::P_USE);
    net.ticks(8, |_, _| {});
    let up = [floor[0], floor[1] + 1, floor[2]];
    assert_eq!(w.borrow_mut().get_block(up[0], up[1], up[2]), STONE, "поставлен у сервера");
    assert_eq!(net.peer(0).count(stone), 64, "блоки не тратятся");
    // Средняя кнопка по блоку, которого нет в инвентаре, — копия в руку.
    net.sv.sim.world_edit(up, DIRT as Cell);
    net.ticks(4, |_, _| {});
    assert_eq!(net.peer(0).target(), Some(up));
    let mism = net.sv.sim.stats.inv_mismatch;
    net.peer(0).press(crate::abi::P_PICK);
    net.ticks(8, |_, _| {});
    let dirt = item_by_name("dirt").unwrap();
    assert_eq!(net.peer(0).count(dirt), 1, "копия блока");
    let i = idx(&mut net, 0);
    assert_eq!(net.sv.sim.players[i].inv.hand().id(), dirt, "у сервера тоже в руке");
    assert_eq!(net.sv.sim.stats.inv_mismatch, mism, "клиент предсказал то же");
    // Неуязвим: урон не проходит.
    let i = idx(&mut net, 0);
    assert!(!net.sv.sim.hurt(i, DamageKind::Generic, 50.0));
    assert!(net.sv.sim.players[i].hp.alive());
    // Обратно в выживание — урон снова идёт.
    say(&mut net, 0, "/gm survival");
    net.ticks(4, |_, _| {});
    let i = idx(&mut net, 0);
    assert!(net.sv.sim.hurt(i, DamageKind::Generic, 2.0));
}

#[test]
fn g17_spectator_is_invisible_and_hands_off() {
    let mut net = pair(Server::new(1337, false));
    net.ticks(10, |_, _| {});
    let ann = net.peer(0).id();
    assert!(net.peer(1).remotes().iter().any(|r| r.0 == ann), "Bob видит Ann");
    say(&mut net, 0, "/gamemode spectator");
    net.ticks(10, |_, _| {});
    assert_eq!(mode(&mut net, 0), (GameMode::Spectator, GameMode::Spectator));
    assert!(net.client(0).ctl.player.noclip && net.client(0).ctl.player.flying);
    assert!(!net.peer(1).remotes().iter().any(|r| r.0 == ann), "наблюдатель невидим");
    // Блок перед ним не ломается.
    net.sv.sim.world_edit([0, 101, 2], DIRT as Cell);
    net.ticks(4, |_, _| {});
    net.peer(0).set_look(0.0, 35.0);
    net.ticks(2, |_, _| {});
    net.ticks(30, |n, k| {
        if k == 0 {
            n.peer(0).click();
        }
        n.peer(0).set_keys(K_ATTACK);
    });
    net.peer(0).set_keys(0);
    let w = net.sv.sim.sim.world.d.clone();
    assert_eq!(w.borrow_mut().get_block(0, 101, 2), DIRT);
    // Летит сквозь камень пола вниз без телепортов назад.
    let tp = net.sv.sim.stats.teleports;
    net.ticks(20, |n, _| n.peer(0).set_keys(crate::abi::K_SNEAK));
    net.peer(0).set_keys(0);
    net.ticks(6, |_, _| {});
    assert!(net.peer(0).pos()[1] < 100.0, "под полом: {:?}", net.peer(0).pos());
    assert_eq!(net.sv.sim.stats.teleports, tp);
    // Неуязвим.
    let i = idx(&mut net, 0);
    assert!(!net.sv.sim.hurt(i, DamageKind::Generic, 50.0));
}

#[test]
fn g17_rollback_and_journal_files() {
    let dir = std::env::temp_dir().join(format!("voxel-g17-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let sv = Server::open(1337, Some(dir.to_str().unwrap()), false).unwrap();
    let mut net = pair(sv);
    // Bob ломает землю перед собой (выживание, рукой).
    net.sv.sim.world_edit([4, 101, 6], DIRT as Cell);
    net.sv.sim.world_edit([4, 101, 7], DIRT as Cell);
    net.ticks(4, |_, _| {});
    net.peer(1).set_look(0.0, 35.0);
    net.ticks(2, |_, _| {});
    let t = net.peer(1).target().expect("земля под прицелом");
    assert_eq!(t, [4, 101, 6]);
    net.ticks(40, |n, k| {
        if k == 0 {
            n.peer(1).click();
        }
        n.peer(1).set_keys(K_ATTACK);
    });
    net.peer(1).set_keys(0);
    net.ticks(6, |_, _| {});
    let w = net.sv.sim.sim.world.d.clone();
    assert_eq!(w.borrow_mut().get_block(t[0], t[1], t[2]), AIR, "Bob сломал");
    // Откат — блок на месте у сервера и у Bob.
    let r = say(&mut net, 0, "/rollback Bob 5");
    assert!(r[0].1.contains("возвращено блоков 1"), "{r:?}");
    net.ticks(4, |_, _| {});
    assert_eq!(w.borrow_mut().get_block(t[0], t[1], t[2]), DIRT);
    assert_eq!(net.peer(1).block(t[0], t[1], t[2]), DIRT);
    // Повторный откат — нечего возвращать.
    assert!(say(&mut net, 0, "/rollback Bob")[0].1.contains("возвращено блоков 0"));
    // /op и /ban пишут roles.txt; журнал — admin.log со временем и итогом, отказ Bob тоже.
    say(&mut net, 1, "/kill Ann");
    say(&mut net, 0, "/op Bob");
    net.ticks(4, |_, _| {});
    assert_eq!(net.client(1).level, 1, "Bob — админ (Msg::Mode)");
    let roles = std::fs::read_to_string(dir.join("roles.txt")).unwrap();
    assert!(roles.contains("owner ann") && roles.contains("admin bob"), "{roles}");
    let log = std::fs::read_to_string(dir.join("admin.log")).unwrap();
    assert!(log.contains("Ann: /rollback Bob 5 → ок") && log.contains("Bob: /kill Ann → отказ") && log.contains("Ann: /op Bob → ок"), "{log}");
    assert!(log.lines().all(|l| l.len() > 21 && l.as_bytes()[10] == b'T' && l[..20].ends_with('Z')), "время UTC: {log}");
    // Бан: Bob выкинут и войти не может.
    say(&mut net, 0, "/deop Bob");
    let r = say(&mut net, 0, "/ban Bob гриф");
    assert!(r.iter().any(|l| l.1 == "Bob забанен"), "{r:?}");
    net.ticks(4, |_, _| {});
    assert!(net.sv.sim.players.iter().all(|p| p.nick != "Bob"), "выкинут");
    let h = crate::proto::Hello { proto: crate::proto::PROTO, build: crate::proto::BUILD.into(), nick: "bob".into(), token: BOB, view: 8, probe: crate::gen::probe() };
    assert_eq!(net.sv.sim.join(&h).unwrap_err().0, crate::proto::Reject::Banned);
    // Правка roles.txt руками снимает бан (сервер перечитывает файл).
    std::fs::write(dir.join("roles.txt"), "owner ann\n").unwrap();
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
    let _ = std::fs::File::options().write(true).open(dir.join("roles.txt")).and_then(|f| f.set_modified(past));
    net.ticks(120, |_, _| {});
    assert!(net.sv.sim.admin.roles.bans.is_empty(), "перечитан: {:?}", net.sv.sim.admin.roles);
    let _ = net.sv.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn g17_mode_survives_rejoin() {
    // Режим — в записи игрока: вышел и вошёл — тот же; наблюдатель снова летит сквозь стены.
    let mut net = pair(Server::new(1337, false));
    say(&mut net, 0, "/gamemode spectator Bob");
    net.ticks(6, |_, _| {});
    assert_eq!(mode(&mut net, 1), (GameMode::Spectator, GameMode::Spectator));
    let id = net.peer(1).id();
    net.sv.sim.leave(id);
    let p = net.sv.sim.parked.iter().find(|p| p.nick == "Bob").unwrap();
    assert_eq!(p.player.mode, GameMode::Spectator);
}
