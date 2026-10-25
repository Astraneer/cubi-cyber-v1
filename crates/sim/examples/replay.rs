//! Offline local reproduction, with no graphics device or network connection.
use sim::replay::{Archive, MAX_ARCHIVE_BYTES};
use std::{io::Read, path::PathBuf};
fn run() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(
        args.next()
            .ok_or("usage: cargo run -p sim --example replay -- <F8.json or CUBR.bin>")?,
    );
    if args.next().is_some() {
        return Err("expected one local replay file".into());
    }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let limit = MAX_ARCHIVE_BYTES * 2 + 16 * 1024 * 1024;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("replay file exceeds bound".into());
    }
    let archive = if bytes.starts_with(b"CUBR") {
        Archive::decode(&bytes)
    } else {
        Archive::from_report(std::str::from_utf8(&bytes).map_err(|_| "replay report is not UTF-8")?)
    }
    .map_err(str::to_owned)?;
    match archive.replay() {
        Ok(result) => println!(
            "MOVEMENT_REPLAY_OK steps={} checkpoints={} final_hash={:016x} complete_10s={}",
            result.steps, result.checkpoints, result.final_hash, result.complete_window
        ),
        Err(error) => {
            return Err(format!(
                "movement replay diverged at sequence {}: {}; expected={:016x} actual={:016x}",
                error.sequence, error.reason, error.expected_hash, error.actual_hash
            ));
        }
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("MOVEMENT_REPLAY_FAILED {error}");
        std::process::exit(1);
    }
}
