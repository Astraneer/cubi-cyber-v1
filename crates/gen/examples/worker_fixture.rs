//! Native bytes for the exact request executed by browser-worker smoke tests.
use core_types::{PageId, SpaceId};
use r#gen::{GenerationTask, TerrainSeed};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::path::PathBuf::from(".local/artifacts");
    std::fs::create_dir_all(&path)?;
    let task = GenerationTask {
        terrain_seed: TerrainSeed(1),
        page: PageId {
            space: SpaceId(0),
            x: 0,
            y: 3,
            z: 0,
        },
        expected: None,
    };
    let page = task.execute(&jobs::CancellationToken::new())?;
    std::fs::write(path.join("jobs-request.bin"), task.encode())?;
    std::fs::write(path.join("jobs-expected.bin"), page.encode())?;
    println!(
        "WORKER_FIXTURE hash={:016x} request_bytes={} result_bytes={}",
        page.canonical_hash(),
        r#gen::TASK_BYTES,
        r#gen::RESULT_BYTES
    );
    Ok(())
}
