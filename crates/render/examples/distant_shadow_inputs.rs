//! CPU-only exact shader/camera/public payload inputs for Dawn shadow oracles.
use render::{
    lod::{MaskedRayProxy, PublicPage, PublicVoxel, SurfaceKind},
    lod_gpu::ProxyPayload,
    shadows::{ShadowCache, ShadowConfig},
};
fn main() {
    let path = std::path::Path::new(".local/distant-shadow-oracle");
    std::fs::create_dir_all(path).unwrap();
    for (name, source) in ["cull", "height", "opaque", "cutout"]
        .into_iter()
        .zip(render::distant_shadows::shader_sources(64, 128))
    {
        std::fs::write(path.join(format!("{name}.wgsl")), source).unwrap();
    }
    let config = ShadowConfig {
        resolution: 128,
        half_widths: [64., 128., 256.],
        depth_half_extent: 256.,
        sun_step_radians: 1e-6,
        ..ShadowConfig::default()
    };
    let sampling = ShadowCache::new(config, [16., 0., 16.], [1., 1., 0.])
        .unwrap()
        .sampling();
    let matrix = sampling.matrices[0];
    let mut camera = [0u32; 36];
    for (i, v) in matrix.iter().flatten().enumerate() {
        camera[i] = v.to_bits();
    }
    camera[19] = 1f32.to_bits();
    for a in 0..3 {
        camera[20 + a] = (-matrix[a][2]).to_bits();
    }
    camera[24..28].copy_from_slice(&[1, 1, 128, 0]);
    camera[28] = 1;
    std::fs::write(path.join("camera.bin"), bytemuck::cast_slice(&camera)).unwrap();
    for (name, kind) in [
        ("opaque", SurfaceKind::Opaque),
        ("cutout", SurfaceKind::Cutout),
    ] {
        let page = PublicPage::from_exposed(|[x, _, z]| {
            if (4..12).contains(&x) && (4..12).contains(&z) {
                PublicVoxel::Solid {
                    material: 0,
                    tint: u32::MAX,
                    kind,
                }
            } else {
                PublicVoxel::Air
            }
        });
        let payload =
            ProxyPayload::from_public(&MaskedRayProxy::from_protected(&page, 4), 186).unwrap();
        std::fs::write(path.join(format!("{name}.pxp")), payload.encode_public()).unwrap();
    }
    // The browser consumes flags computed by the exact loading classifier, not
    // a second JavaScript approximation of the threshold.
    let mut atlas = render::terrain_gpu::AtlasImage {
        tile_size: 16,
        layers: 5,
        mips: vec![render::terrain_gpu::AtlasMip {
            size: 16,
            rgba: vec![255; 16 * 16 * 5 * 4],
            tint_mask: vec![255; 16 * 16 * 5],
        }],
    };
    for (layer, alpha) in [0, 127, 128, 255, 0].into_iter().enumerate() {
        for (index, pixel) in atlas.mips[0].rgba[layer * 1024..(layer + 1) * 1024]
            .chunks_exact_mut(4)
            .enumerate()
        {
            pixel[3] = if layer == 4 && index % 2 == 0 {
                255
            } else {
                alpha
            };
        }
    }
    std::fs::write(
        path.join("alpha-flags.bin"),
        bytemuck::cast_slice(&render::shadow_alpha::classify_mip0(&atlas).unwrap()),
    )
    .unwrap();
    std::fs::write(path.join("alpha-rgba.bin"), &atlas.mips[0].rgba).unwrap();
    println!("distant_shadow_inputs fourExactShaders camera144B publicPXP2 alpha5 ready");
}
