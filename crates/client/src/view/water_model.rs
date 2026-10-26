//! One affine transform for cached page-local fluid quads. World and render
//! origins are applied in f64 before conversion to the GPU matrix.
use core_types::PageId;

pub fn page_model(world: &world::World, page: PageId, origin: [f64; 3]) -> Option<[[f32; 4]; 4]> {
    let transform = world.space(page.space)?.transform;
    if !transform.is_valid() || !origin.into_iter().all(f64::is_finite) {
        return None;
    }
    let [x, y, z, w] = transform.rotation;
    let rotation = [
        [
            1. - 2. * (y * y + z * z),
            2. * (x * y + z * w),
            2. * (x * z - y * w),
        ],
        [
            2. * (x * y - z * w),
            1. - 2. * (x * x + z * z),
            2. * (y * z + x * w),
        ],
        [
            2. * (x * z + y * w),
            2. * (y * z - x * w),
            1. - 2. * (x * x + y * y),
        ],
    ];
    let local = [page.x, page.y, page.z].map(|value| f64::from(value) * 16.);
    let translated: [f64; 3] = std::array::from_fn(|axis| {
        transform.translation[axis] - origin[axis]
            + rotation[0][axis] * local[0]
            + rotation[1][axis] * local[1]
            + rotation[2][axis] * local[2]
    });
    let matrix = [
        [
            rotation[0][0] as f32,
            rotation[0][1] as f32,
            rotation[0][2] as f32,
            0.,
        ],
        [
            rotation[1][0] as f32,
            rotation[1][1] as f32,
            rotation[1][2] as f32,
            0.,
        ],
        [
            rotation[2][0] as f32,
            rotation[2][1] as f32,
            rotation[2][2] as f32,
            0.,
        ],
        [
            translated[0] as f32,
            translated[1] as f32,
            translated[2] as f32,
            1.,
        ],
    ];
    matrix
        .iter()
        .flatten()
        .all(|value| value.is_finite())
        .then_some(matrix)
}
