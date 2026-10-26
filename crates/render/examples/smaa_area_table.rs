//! Test fixture exporter for the exact Rust-generated table consumed by Dawn.
use std::io::Write;
fn main() {
    let table = render::smaa::area_table();
    std::io::stdout()
        .lock()
        .write_all(bytemuck::cast_slice(&table))
        .expect("area table stdout");
}
