//! Loopback development authority. Production requires encrypted establishment.
use server::ServerConfig;
use server::transport::RunningServer;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = ServerConfig::new(".local/server");
    let mut ws: SocketAddr = "127.0.0.1:25565".parse()?;
    let mut udp: SocketAddr = "127.0.0.1:25566".parse()?;
    let mut ready_file: Option<PathBuf> = None;
    let mut run_millis = None;
    let mut args = std::env::args().skip(1);
    while let Some(option) = args.next() {
        match option.as_str() {
            "--data" => config.data_dir = args.next().ok_or("--data needs path")?.into(),
            "--ws" => ws = args.next().ok_or("--ws needs address")?.parse()?,
            "--udp" => udp = args.next().ok_or("--udp needs address")?.parse()?,
            "--seed" => config.public_seed = args.next().ok_or("--seed needs u32")?.parse()?,
            "--debug" => config.server_debug = true,
            "--no-generation" => config.generation_enabled = false,
            "--spawn" => {
                let value = args.next().ok_or("--spawn needs x,y,z")?;
                let values = value
                    .split(',')
                    .map(str::parse)
                    .collect::<Result<Vec<f64>, _>>()?;
                config.spawn_override = Some(values.try_into().map_err(|_| "--spawn needs x,y,z")?);
            }
            "--ready-file" => {
                ready_file = Some(args.next().ok_or("--ready-file needs path")?.into())
            }
            "--run-millis" => {
                run_millis = Some(
                    args.next()
                        .ok_or("--run-millis needs count")?
                        .parse::<u64>()?,
                )
            }
            "--help" => {
                println!(
                    "server [--data PATH] [--ws 127.0.0.1:PORT] [--udp 127.0.0.1:PORT] [--seed U32] [--debug] [--no-generation] [--spawn X,Y,Z] [--run-millis N] [--ready-file PATH]"
                );
                return Ok(());
            }
            _ => return Err(format!("unknown argument {option}").into()),
        }
    }
    let runtime = RunningServer::spawn(config, ws, udp)?;
    let public_addresses = format!(
        "ws={}\nudp={}\n",
        runtime.ws_address(),
        runtime.udp_address()
    );
    if let Some(path) = ready_file {
        std::fs::write(path, &public_addresses)?;
    }
    print!("{public_addresses}");
    if let Some(millis) = run_millis {
        std::thread::sleep(Duration::from_millis(millis));
        let counters = runtime.stop()?;
        println!(
            "received_datagrams={} sent_datagrams={} queue_overflows={}",
            counters.received_datagrams, counters.sent_datagrams, counters.queue_overflows
        );
    } else {
        loop {
            std::thread::park_timeout(Duration::from_secs(30));
        }
    }
    Ok(())
}
