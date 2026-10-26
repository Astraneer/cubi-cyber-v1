//! Full canonical owner workload; run explicitly in the root release graph.
#[path = "scale/bots.rs"]
mod bots;
#[path = "scale/fixture.rs"]
mod fixture;
#[path = "scale/runner.rs"]
mod runner;
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator =
    telemetry::CountingAllocator::new(std::alloc::System);
#[test]
#[ignore = "actual 32 UDP peers, 5000 actors, 10000 machines and 100000 due fluid events; use --release"]
fn actual_combined_owner_preserves_results_and_recovers_durable_state() {
    let path = runner::run();
    println!("actual_server_workload={}", path.display());
}
