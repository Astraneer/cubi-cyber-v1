#![cfg(not(target_arch = "wasm32"))]
use render::{
    shadow_alpha::{FAIL_ALL, FLAGS, PASS_ALL, classify_mip0, merge},
    terrain_gpu::{AtlasImage, AtlasMip},
};
#[test]
fn binary_mip0_classification_preserves_tint_emission_and_exact_threshold() {
    let atlas = AtlasImage {
        tile_size: 2,
        layers: 5,
        mips: vec![AtlasMip {
            size: 2,
            rgba: [0u8, 127, 128, 255, 0]
                .into_iter()
                .flat_map(|a| [37, 129, 211, a].repeat(4))
                .collect(),
            tint_mask: vec![255; 20],
        }],
    };
    let mut atlas = atlas;
    atlas.mips[0].rgba[4 * 16 + 7] = 255;
    let flags = classify_mip0(&atlas).unwrap();
    assert_eq!(&*flags, &[FAIL_ALL, FAIL_ALL, PASS_ALL, PASS_ALL, 0]);
    for (layer, &flag) in flags.iter().enumerate() {
        let original = [layer as u32, 0x8000_0001 | FLAGS, 3, 1.0f32.to_bits()];
        assert_eq!(
            merge(original, &flags, true),
            [layer as u32, 0x8000_0001 | flag, 3, 1.0f32.to_bits()]
        );
        assert_eq!(
            merge(original, &flags, false),
            [layer as u32, 0x8000_0001, 3, 1.0f32.to_bits()]
        );
    }
    assert_eq!(merge([500, FLAGS | 1, 7, 9], &flags, true), [500, 1, 7, 9]);
    for mutate in 0..4 {
        let mut invalid = AtlasImage {
            tile_size: 2,
            layers: 1,
            mips: vec![AtlasMip {
                size: 2,
                rgba: vec![255; 16],
                tint_mask: vec![255; 4],
            }],
        };
        match mutate {
            0 => invalid.layers = 0,
            1 => invalid.tile_size = 0,
            2 => invalid.mips[0].size = 1,
            _ => {
                invalid.mips[0].rgba.pop();
            }
        }
        assert!(classify_mip0(&invalid).is_err());
    }
}
#[test]
fn exact_shipping_cutout_source_validates_with_bounds_before_cached_alpha() {
    let source = render::distant_shadows::shader_sources(64, 128)
        .into_iter()
        .last()
        .unwrap();
    let module = wgpu::naga::front::wgsl::parse_str(&source)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
    wgpu::naga::valid::Validator::new(
        wgpu::naga::valid::ValidationFlags::all(),
        wgpu::naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap();
    assert!(
        source.find("material_id>=arrayLength").unwrap() < source.find("material.y&2u").unwrap()
    );
    assert!(
        source.find("material.x>=textureNumLayers").unwrap()
            < source.find("material.y&2u").unwrap()
    );
}
