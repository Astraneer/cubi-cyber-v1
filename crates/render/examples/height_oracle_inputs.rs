//! Dump exact primary Height shader variants and cameras for the Dawn oracle.
use render::{
    height_gpu,
    terrain_gpu::Camera,
    terrain_mesh::{Face, QuadDescriptor},
};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/height-oracle");
    std::fs::create_dir_all(&directory)?;
    for samples in [1, 4] {
        std::fs::write(
            directory.join(format!("near-{samples}.wgsl")),
            height_gpu::near_shader_source(false, false, false, samples == 4),
        )?;
    }
    let terrain = include_str!("../src/terrain.wgsl");
    let (vertex, _) = terrain
        .split_once("\nfn owner_masked(")
        .ok_or("terrain vertex boundary changed")?;
    std::fs::write(
        directory.join("terrain.wgsl"),
        format!("{vertex}\n@fragment fn fragment()->@location(0)vec4<f32>{{return vec4(1.);}}\n"),
    )?;
    let quad = QuadDescriptor::new([0, 8, 16], Face::Up, [16, 16], 3, [3; 4], u32::MAX)
        .map_err(|_| "invalid independent near fixture descriptor")?;
    std::fs::write(directory.join("quad.json"), format!("{:?}\n", quad.words))?;
    let cameras = [
        Camera::perspective(
            [0., 150., 0.],
            [0., 8., 0.],
            [0., 0., -1.],
            2. * (32f32 / 142.).atan(),
            1.,
            0.1,
            300.,
        ),
        Camera::perspective(
            [0., 55., 100.],
            [0., 8., 0.],
            [0., 1., 0.],
            1.,
            1.,
            0.1,
            300.,
        ),
        Camera::perspective(
            [0., 175., 140.],
            [0., 24., 0.],
            [0., 1., 0.],
            1.,
            1.,
            0.1,
            300.,
        ),
    ];
    let matrices: Vec<_> = cameras.iter().map(|c| c.view_projection).collect();
    std::fs::write(directory.join("cameras.json"), format!("{matrices:?}\n"))?;
    println!("HEIGHT_ORACLE_INPUTS {}", directory.display());
    Ok(())
}
