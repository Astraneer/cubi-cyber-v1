//! Игровой сервер (mp/design.md §2): `voxel-server --listen 127.0.0.1:8121 --dev` — /ws (WebSocket, кадры postcard) и
//! /status (JSON); мир в памяти или в папке --data (запись при остановке; автосохранение — MP-2). Без --listen —
//! только симуляция --ticks тиков (проверка мира и записи).
//!
//!   cargo run -p voxel-server -- [--listen АДРЕС] [--dev] [--seed N] [--data ПАПКА] [--autosave СЕК] [--ticks N]
//!                                [--fast] [--origin ORIGIN]… [--idle СЕК]
//! --data — папка мира: при запуске мир и игроки грузятся оттуда, раз в --autosave секунд (30; 0 — выкл) изменённое
//! пишется отдельным потоком, при остановке (Ctrl+C, SIGTERM) — последняя запись.
//! --dev — отладка игрокам (полёт сквозь стены H, набор K, сдвиг J, время); --origin — ещё разрешённый Origin (кроме localhost);
//! --fast — тики без пауз (только без сети); --idle — через сколько секунд тишины соединение считается оборванным
//! (по умолчанию 15).
//! --set-owner НИК — записать владельца в roles.txt папки --data и выйти (G17; работающий сервер подхватит файл сам):
//!   docker exec cubi-server voxel-server --data /data --set-owner НИК

use voxel_core::api::{gen_probe, gen_version};
use voxel_server::{run_offline, start, Config};

fn args() -> Result<(Config, Option<String>), String> {
    let mut c = Config { ticks: 0, ..Config::default() };
    let mut owner = None;
    let mut ticks_set = false;
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{k}: нет значения"));
        match k.as_str() {
            "--seed" => c.seed = val()?.parse().map_err(|e| format!("--seed: {e}"))?,
            "--ticks" => {
                c.ticks = val()?.parse().map_err(|e| format!("--ticks: {e}"))?;
                ticks_set = true;
            }
            "--data" => c.data = Some(val()?),
            "--autosave" => c.autosave = val()?.parse().map_err(|e| format!("--autosave: {e}"))?,
            "--listen" => c.listen = Some(val()?),
            "--origin" => c.origins.push(val()?),
            "--dev" => c.dev = true,
            "--fast" => c.fast = true,
            "--idle" => c.idle = val()?.parse().map_err(|e| format!("--idle: {e}"))?,
            "--set-owner" => owner = Some(val()?),
            "-h" | "--help" => {
                return Err("voxel-server [--listen АДРЕС] [--dev] [--seed N] [--data ПАПКА] [--autosave СЕК] [--ticks N] [--fast] [--origin O]… [--idle СЕК]".into())
            }
            _ => return Err(format!("неизвестный ключ {k}")),
        }
    }
    if c.listen.is_none() && !ticks_set {
        c.ticks = 20;
    }
    if c.listen.is_some() {
        c.fast = false;
    }
    Ok((c, owner))
}

fn main() {
    let cfg = match args() {
        Ok((c, Some(nick))) => {
            let Some(dir) = c.data.as_deref() else {
                eprintln!("--set-owner: нужна --data ПАПКА");
                std::process::exit(2);
            };
            match voxel_core::net::set_owner(dir, &nick) {
                Ok(s) => {
                    println!("roles.txt: {s}");
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("--set-owner: {e}");
                    std::process::exit(1);
                }
            }
        }
        Ok((c, None)) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let probe: Vec<String> = gen_probe().iter().map(|h| format!("{h:016x}")).collect();
    eprintln!("voxel-server: сборка {}, генератор {}, проба [{}]", voxel_core::net::build(), gen_version(), probe.join(" "));
    let r = if cfg.listen.is_none() {
        run_offline(cfg)
    } else {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio");
        rt.block_on(async move {
            let ticks = cfg.ticks;
            let srv = start(cfg).await?;
            // Одно ожидание сигнала на весь цикл: между итерациями сигнал не теряется.
            let mut stop = Box::pin(voxel_server::shutdown_signal());
            loop {
                tokio::select! {
                    _ = &mut stop => break, // Ctrl+C или SIGTERM (docker stop): дальше srv.stop() пишет мир
                    _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {
                        if ticks > 0 && srv.sim_done() {
                            break;
                        }
                    }
                }
            }
            srv.stop().await
        })
    };
    match r {
        Ok(s) => eprintln!("voxel-server: {s}"),
        Err(e) => {
            eprintln!("voxel-server: {e}");
            std::process::exit(1);
        }
    }
}
