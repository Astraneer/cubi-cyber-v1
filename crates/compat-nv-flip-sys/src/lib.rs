//! Native NVIDIA FLIP bindings; the numerical BSD-3-Clause kernel is retained.
//! Optional filename/image codecs and their dependencies are excluded.
include!("bindings.rs");

pub const DEFAULT_PIXELS_PER_DEGREE: f32 = 67.0;

pub fn pixels_per_degree(distance: f32, resolution_x: f32, monitor_width: f32) -> f32 {
    distance * (resolution_x / monitor_width) * (std::f32::consts::PI / 180.0)
}
