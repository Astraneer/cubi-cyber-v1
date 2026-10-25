//! Startup-only cuboid catalog. Texture dimensions and UV packing come from the
//! allowed mob_art.py asset table. Model arrangement and the GPU pose path are
//! original code; no donor JS/WebGL renderer is read or linked.
use bytemuck::{Pod, Zeroable};
pub const MODEL_KINDS: usize = 8;
pub const WOOL: u32 = 1;
pub const SKIN: u32 = 2;
pub const BLOCK_ATLAS: u32 = 4;
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct ModelVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub pivot: [f32; 3],
    pub kind: u32,
    pub bone: u32,
    pub flags: u32,
    pub base_pitch: f32,
    pub layer: u32,
}
const _: () = assert!(std::mem::size_of::<ModelVertex>() == 64);
#[derive(Clone, Copy, Debug, Default)]
pub struct ModelRange {
    pub first_vertex: u32,
    pub vertex_count: u32,
    pub parts: u32,
}
struct BoxUv {
    name: &'static str,
    u: u16,
    v: u16,
    w: u16,
    h: u16,
    d: u16,
}
include!("entity_uv.rs");
struct Part {
    kind: u32,
    bone: u32,
    flags: u32,
    pivot: [f32; 3],
    origin: [f32; 3],
    pitch: f32,
    inflation: f32,
    texture: &'static str,
}
pub struct ModelCatalog {
    pub vertices: Vec<ModelVertex>,
    pub ranges: [ModelRange; MODEL_KINDS],
}
impl Default for ModelCatalog {
    fn default() -> Self {
        Self::new()
    }
}
impl ModelCatalog {
    pub fn new() -> Self {
        let mut out = Self {
            vertices: Vec::with_capacity(2048),
            ranges: [ModelRange::default(); MODEL_KINDS],
        };
        for kind in 0..6u32 {
            let first = out.vertices.len() as u32;
            let parts = parts(kind);
            for part in &parts {
                out.cuboid(part);
            }
            out.ranges[kind as usize] = ModelRange {
                first_vertex: first,
                vertex_count: out.vertices.len() as u32 - first,
                parts: parts.len() as u32,
            };
        }
        let first = out.vertices.len() as u32;
        out.drop_cube();
        out.ranges[6] = ModelRange {
            first_vertex: first,
            vertex_count: 36,
            parts: 1,
        };
        let first = out.vertices.len() as u32;
        out.drop_icon();
        out.ranges[7] = ModelRange {
            first_vertex: first,
            vertex_count: 12,
            parts: 1,
        };
        out
    }
    fn cuboid(&mut self, part: &Part) {
        let box_index = BOXES
            .iter()
            .position(|b| b.name == part.texture)
            .expect("static mob asset box");
        let b = &BOXES[box_index];
        let w = f32::from(b.w);
        let h = f32::from(b.h);
        let d = f32::from(b.d);
        let normals = [
            [0., 1., 0.],
            [0., -1., 0.],
            [-1., 0., 0.],
            [0., 0., -1.],
            [1., 0., 0.],
            [0., 0., 1.],
        ];
        for (face, normal) in normals.into_iter().enumerate() {
            for corner in [0, 1, 2, 2, 1, 3] {
                let s = (corner & 1) as f32;
                let t = (corner >> 1) as f32;
                let p = match face {
                    0 => [w - s * w, 0., t * d],
                    1 => [s * w, h, t * d],
                    2 => [0., t * h, d - s * d],
                    3 => [s * w, t * h, 0.],
                    4 => [w, t * h, s * d],
                    _ => [w - s * w, t * h, d],
                };
                let dimensions = [w, h, d];
                let p = std::array::from_fn::<_, 3, _>(|axis| {
                    part.origin[axis]
                        + (p[axis] - dimensions[axis] * 0.5)
                            * (1. + part.inflation * 2. / dimensions[axis])
                        + dimensions[axis] * 0.5
                });
                let position = [p[0] / 16., -p[1] / 16., p[2] / 16.];

                self.vertices.push(ModelVertex {
                    position,
                    normal,
                    uv: [s, t],
                    pivot: part.pivot,
                    kind: part.kind,
                    bone: part.bone,
                    flags: part.flags,
                    base_pitch: part.pitch,
                    layer: (box_index * 6 + face) as u32,
                });
            }
        }
    }
    fn drop_cube(&mut self) {
        let origins = [
            [0., 0., 0.],
            [0., 1., 1.],
            [1., 0., 0.],
            [0., 0., 1.],
            [0., 0., 0.],
            [1., 0., 1.],
        ];
        let us = [
            [1., 0., 0.],
            [1., 0., 0.],
            [-1., 0., 0.],
            [1., 0., 0.],
            [0., 0., 1.],
            [0., 0., -1.],
        ];
        let vs = [
            [0., 0., 1.],
            [0., 0., -1.],
            [0., 1., 0.],
            [0., 1., 0.],
            [0., 1., 0.],
            [0., 1., 0.],
        ];
        let ns = [
            [0., -1., 0.],
            [0., 1., 0.],
            [0., 0., -1.],
            [0., 0., 1.],
            [-1., 0., 0.],
            [1., 0., 0.],
        ];
        for face in 0..6 {
            for corner in [0, 1, 2, 2, 1, 3] {
                let u = (corner & 1) as f32;
                let v = (corner >> 1) as f32;
                self.vertices.push(ModelVertex {
                    position: std::array::from_fn(|a| {
                        origins[face][a] + us[face][a] * u + vs[face][a] * v - 0.5
                    }),
                    normal: ns[face],
                    uv: [u, 1. - v],
                    pivot: [0.; 3],
                    kind: 6,
                    bone: 0,
                    flags: BLOCK_ATLAS,
                    base_pitch: 0.,
                    layer: 0,
                });
            }
        }
    }
    fn drop_icon(&mut self) {
        for side in [-1., 1.] {
            for corner in [0, 1, 2, 2, 1, 3] {
                let u = (corner & 1) as f32;
                let v = (corner >> 1) as f32;
                self.vertices.push(ModelVertex {
                    position: [(u - 0.5) * side, v - 0.5, 0.],
                    normal: [0., 0., side],
                    uv: [u, 1. - v],
                    pivot: [0.; 3],
                    kind: 7,
                    bone: 0,
                    flags: BLOCK_ATLAS,
                    base_pitch: 0.,
                    layer: 0,
                });
            }
        }
    }
}
fn parts(kind: u32) -> Vec<Part> {
    let mut out = Vec::new();
    let mut add = |texture, bone, pivot, origin, pitch, flags, inflation| {
        out.push(Part {
            kind,
            bone,
            flags,
            pivot,
            origin,
            pitch,
            inflation,
            texture,
        })
    };
    let half_pi = std::f32::consts::FRAC_PI_2;
    match kind {
        0 => {
            add(
                "pig_body",
                0,
                [0., 0.625, 0.],
                [-5., -8., -4.],
                half_pi,
                0,
                0.,
            );
            add(
                "pig_head",
                1,
                [0., 0.75, -0.375],
                [-4., -4., -8.],
                0.,
                0,
                0.,
            );
            add(
                "pig_snout",
                1,
                [0., 0.75, -0.375],
                [-2., 0., -9.],
                0.,
                0,
                0.,
            );
            for (i, (x, z)) in [(-3., 5.), (3., 5.), (-3., -5.), (3., -5.)]
                .into_iter()
                .enumerate()
            {
                add(
                    "pig_leg",
                    2 + ((i ^ (i >> 1)) & 1) as u32,
                    [x / 16., 0.375, z / 16.],
                    [-2., 0., -2.],
                    0.,
                    0,
                    0.,
                );
            }
        }
        1 => {
            add(
                "cow_body",
                0,
                [0., 1.0625, 0.],
                [-6., -9., -5.],
                half_pi,
                0,
                0.,
            );
            add("cow_head", 1, [0., 1.25, -0.5], [-4., -4., -6.], 0., 0, 0.);
            for x in [-4., 3.] {
                add("cow_horn", 1, [0., 1.25, -0.5], [x, -7., -4.], 0., 0, 0.);
            }
            add(
                "cow_udder",
                0,
                [0., 0.71875, 0.],
                [-2., -3., -0.5],
                half_pi,
                0,
                0.,
            );
            for (i, (x, z)) in [(-4., 6.), (4., 6.), (-4., -6.), (4., -6.)]
                .into_iter()
                .enumerate()
            {
                add(
                    "cow_leg",
                    2 + ((i ^ (i >> 1)) & 1) as u32,
                    [x / 16., 0.75, z / 16.],
                    [-2., 0., -2.],
                    0.,
                    0,
                    0.,
                );
            }
        }
        2 => {
            add(
                "sheep_body",
                0,
                [0., 1., 0.],
                [-4., -8., -3.],
                half_pi,
                0,
                0.,
            );
            add(
                "wool_body",
                0,
                [0., 1., 0.],
                [-4., -8., -3.],
                half_pi,
                WOOL,
                1.75,
            );
            add(
                "sheep_head",
                1,
                [0., 1.125, -0.5],
                [-3., -3., -8.],
                0.,
                0,
                0.,
            );
            add(
                "wool_head",
                1,
                [0., 1.125, -0.5],
                [-3., -3., -6.],
                0.,
                WOOL,
                0.6,
            );
            for (i, (x, z)) in [(-3., 5.), (3., 5.), (-3., -5.), (3., -5.)]
                .into_iter()
                .enumerate()
            {
                let bone = 2 + ((i ^ (i >> 1)) & 1) as u32;
                add(
                    "sheep_leg",
                    bone,
                    [x / 16., 0.75, z / 16.],
                    [-2., 0., -2.],
                    0.,
                    0,
                    0.,
                );
                add(
                    "wool_leg",
                    bone,
                    [x / 16., 0.75, z / 16.],
                    [-2., 0., -2.],
                    0.,
                    WOOL,
                    0.5,
                );
            }
        }
        3 => {
            add(
                "chick_body",
                0,
                [0., 0.5, 0.],
                [-3., -4., -3.],
                half_pi,
                0,
                0.,
            );
            add(
                "chick_head",
                1,
                [0., 0.5625, -0.25],
                [-2., -6., -2.],
                0.,
                0,
                0.,
            );
            add(
                "chick_beak",
                1,
                [0., 0.5625, -0.25],
                [-2., -4., -4.],
                0.,
                0,
                0.,
            );
            add(
                "chick_wattle",
                1,
                [0., 0.5625, -0.25],
                [-1., -2., -3.],
                0.,
                0,
                0.,
            );
            for (i, x) in [-2., 2.].into_iter().enumerate() {
                add(
                    "chick_leg",
                    2 + i as u32,
                    [x / 16., 0.3125, 0.],
                    [-1.5, 0., -1.5],
                    0.,
                    0,
                    0.,
                );
            }
            add(
                "chick_wing",
                4,
                [-0.21875, 0.625, 0.],
                [-0.5, 0., -3.],
                0.,
                0,
                0.,
            );
            add(
                "chick_wing",
                5,
                [0.21875, 0.625, 0.],
                [-0.5, 0., -3.],
                0.,
                0,
                0.,
            );
        }
        _ => {
            add("zomb_head", 1, [0., 1.5, 0.], [-4., -8., -4.], 0., SKIN, 0.);
            add("zomb_body", 0, [0., 1.5, 0.], [-4., 0., -2.], 0., 0, 0.);
            for (i, x) in [-5., 5.].into_iter().enumerate() {
                add(
                    "zomb_arm",
                    6 + i as u32,
                    [x / 16., 1.375, 0.],
                    [-2., -2., -2.],
                    if kind == 4 { half_pi } else { 0. },
                    SKIN,
                    0.,
                );
            }
            for (i, x) in [-2., 2.].into_iter().enumerate() {
                add(
                    "zomb_leg",
                    2 + i as u32,
                    [x / 16., 0.75, 0.],
                    [-2., 0., -2.],
                    0.,
                    0,
                    0.,
                );
            }
        }
    }
    out
}
/// Split the permitted 128² sheet into isolated face layers before mipmaking.
/// This prevents unrelated neighboring boxes bleeding into distant mob texels.
pub fn model_atlas() -> Result<crate::terrain_gpu::AtlasImage, String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(include_bytes!(
        "../../../donor/web/mobs.png"
    )));
    decoder.set_limits(png::Limits {
        bytes: 128 * 128 * 4,
    });
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    if reader.info().width != 128 || reader.info().height != 128 {
        return Err("mob sheet must be128x128".into());
    }
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels).map_err(|e| e.to_string())?;
    if info.color_type != png::ColorType::Rgba {
        return Err("mob sheet must beRGBA".into());
    }
    let layers = BOXES.len() * 6;
    let mut rgba = vec![0u8; 32 * 32 * 4 * layers];
    for (box_index, b) in BOXES.iter().enumerate() {
        let rects = [
            (b.u + b.d, b.v, b.w, b.d),
            (b.u + b.d + b.w, b.v, b.w, b.d),
            (b.u, b.v + b.d, b.d, b.h),
            (b.u + b.d, b.v + b.d, b.w, b.h),
            (b.u + b.d + b.w, b.v + b.d, b.d, b.h),
            (b.u + b.d + b.w + b.d, b.v + b.d, b.w, b.h),
        ];
        for (face, (u, v, w, h)) in rects.into_iter().enumerate() {
            if u + w > 128 || v + h > 128 {
                return Err("mob UV outside sheet".into());
            }
            for y in 0..32usize {
                for x in 0..32usize {
                    let sx = usize::from(u) + x * usize::from(w) / 32;
                    let sy = usize::from(v) + y * usize::from(h) / 32;
                    let source = (sx + 128 * sy) * 4;
                    let target = ((box_index * 6 + face) * 32 * 32 + x + y * 32) * 4;
                    rgba[target..target + 4].copy_from_slice(&pixels[source..source + 4]);
                }
            }
        }
    }
    let mut png_data = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_data, 32, layers as u32 * 32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .map_err(|e| e.to_string())?
            .write_image_data(&rgba)
            .map_err(|e| e.to_string())?;
    }
    crate::terrain_gpu::AtlasImage::from_png(&png_data)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_models_are_built_once_with_bounded_valid_uvs() {
        let catalog = ModelCatalog::new();
        assert_eq!(
            catalog.ranges.map(|range| range.parts),
            [7, 9, 12, 8, 6, 6, 1, 1]
        );
        assert_eq!(
            catalog.ranges.map(|range| range.vertex_count),
            [252, 324, 432, 288, 216, 216, 36, 12]
        );
        assert_eq!(catalog.vertices.len(), 1776);
        assert!(catalog.vertices.iter().all(|v| {
            v.uv.iter().all(|x| *x >= 0. && *x <= 1.)
                && v.position
                    .iter()
                    .chain(v.normal.iter())
                    .all(|x| x.is_finite())
        }));
    }
    #[test]
    fn face_array_has_isolated_linear_mips_and_no_unused_sheet_pixels() {
        let atlas = model_atlas().unwrap();
        assert_eq!(atlas.tile_size, 32);
        assert_eq!(atlas.layers, BOXES.len() as u32 * 6);
        assert_eq!(atlas.mips.len(), 6);
        for mip in &atlas.mips {
            assert_eq!(
                mip.rgba.len(),
                mip.size as usize * mip.size as usize * atlas.layers as usize * 4
            );
            assert!(mip.rgba.chunks_exact(4).all(|pixel| pixel[3] == 255));
        }
        assert!(
            atlas.mips[0]
                .rgba
                .chunks_exact(4)
                .any(|pixel| pixel[0] > pixel[1] && pixel[0] > pixel[2])
        );
        assert!(
            atlas.mips[0]
                .rgba
                .chunks_exact(4)
                .any(|pixel| pixel[1] > pixel[0] && pixel[1] > pixel[2])
        );
    }
}
