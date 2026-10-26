//! CPU-only, exact production inputs for the independent Dawn proxy oracle.
use render::{
    lod::{MaskedRayProxy, PublicPage, PublicVoxel, SurfaceKind},
    lod_gpu::ProxyPayload,
    terrain_gpu::Camera,
};
use std::{fs, path::Path};

fn camera(path: &Path, name: &str, camera: Camera, eye: [f32; 3], direction: Option<[f32; 3]>) {
    let mut words = [0u32; 28];
    for (index, value) in camera.view_projection.iter().flatten().enumerate() {
        words[index] = value.to_bits();
    }
    for axis in 0..3 {
        words[16 + axis] = eye[axis].to_bits();
        words[20 + axis] = direction.unwrap_or([0.; 3])[axis].to_bits();
    }
    words[19] = f32::from(direction.is_some()).to_bits();
    words[24..].copy_from_slice(&[1, 96, 96, 0]);
    fs::write(path.join(name), bytemuck::cast_slice(&words)).unwrap();
}
fn payload(path: &Path, name: &str, samples: impl Fn([u8; 3]) -> PublicVoxel) {
    let page = PublicPage::from_exposed(samples);
    let mask = render::lod::coarsen_page(&page, 4, true).protection;
    let source = MaskedRayProxy::from_public_page(&page, mask);
    let payload = ProxyPayload::from_public(&source, 186).unwrap();
    fs::write(path.join(name), payload.encode_public()).unwrap();
}
fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".local/lod-oracle".into());
    let path = Path::new(&path);
    fs::create_dir_all(path).unwrap();
    for ambient in [false, true] {
        fs::write(
            path.join(if ambient {
                "proxy-mrt.wgsl"
            } else {
                "proxy.wgsl"
            }),
            render::lod_gpu::shader_source(false, false, ambient, true),
        )
        .unwrap();
    }
    payload(path, "opaque.pxp", |[_, y, _]| {
        if y == 7 {
            PublicVoxel::solid(3, u32::MAX)
        } else {
            PublicVoxel::Air
        }
    });
    payload(path, "cutout.pxp", |[_, y, _]| {
        if y == 7 {
            PublicVoxel::Solid {
                material: 3,
                tint: u32::MAX,
                kind: SurfaceKind::Cutout,
            }
        } else {
            PublicVoxel::Air
        }
    });
    payload(path, "mixed.pxp", |[_, y, _]| match y {
        7 => PublicVoxel::solid(3, u32::MAX),
        8 => PublicVoxel::Solid {
            material: 4,
            tint: u32::MAX,
            kind: SurfaceKind::Cutout,
        },
        _ => PublicVoxel::Air,
    });
    payload(path, "far.pxp", |[_, _, z]| {
        if z == 15 {
            PublicVoxel::solid(3, u32::MAX)
        } else {
            PublicVoxel::Air
        }
    });
    let eye = [0., 150., 0.];
    camera(
        path,
        "perspective.camera",
        Camera::perspective(
            eye,
            [0., 8., 0.],
            [0., 0., -1.],
            2. * (32f32 / 142.).atan(),
            1.,
            0.1,
            300.,
        ),
        eye,
        None,
    );
    let eye = [8., 8., 40.];
    camera(
        path,
        "far.camera",
        Camera::orthographic(eye, [8., 8., 8.], [0., 1., 0.], 8., 1., 0.1, 100.),
        eye,
        Some([0., 0., -1.]),
    );
    println!(
        "Exact Proxy shader/payload/camera inputs: {}",
        path.display()
    );
}
