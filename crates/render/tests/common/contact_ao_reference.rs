//! Independent f64 plane intersections. No production depth/normal shader is
//! ported: receiver normals come from the exact authored floor/wall geometry.
use super::*;
type V = [f64; 3];
fn sub(a: V, b: V) -> V {
    std::array::from_fn(|i| a[i] - b[i])
}
fn add(a: V, b: V) -> V {
    std::array::from_fn(|i| a[i] + b[i])
}
fn mul(a: V, k: f64) -> V {
    a.map(|x| x * k)
}
fn dot(a: V, b: V) -> f64 {
    (0..3).map(|i| a[i] * b[i]).sum()
}
fn norm(a: V) -> V {
    mul(a, 1. / dot(a, a).sqrt())
}
fn transform(m: [[f32; 4]; 4], v: [f64; 4]) -> [f64; 4] {
    std::array::from_fn(|r| (0..4).map(|c| f64::from(m[c][r]) * v[c]).sum())
}
fn unproject(camera: Camera, uv: [f64; 2], z: f64) -> V {
    let p = transform(
        camera.inverse_view_projection().unwrap(),
        [uv[0] * 2. - 1., 1. - uv[1] * 2., z, 1.],
    );
    [p[0] / p[3], p[1] / p[3], p[2] / p[3]]
}
fn hit(camera: Camera, uv: [f64; 2]) -> Option<(V, u8)> {
    let origin = unproject(camera, uv, 1.);
    let ray = norm(sub(unproject(camera, uv, 0.), origin));
    let mut best = f64::INFINITY;
    let mut kind = 0;
    let t = -origin[1] / ray[1];
    let p = add(origin, mul(ray, t));
    if t > 0. && p[0].abs() < 2. && p[2].abs() < 2. {
        best = t;
        kind = 1;
    }
    let t = -origin[0] / ray[0];
    let p = add(origin, mul(ray, t));
    if t > 0. && p[1] >= 0. && p[1] < 2. && p[2].abs() < 2. && t < best {
        best = t;
        kind = 2;
    }
    (kind != 0).then(|| (add(origin, mul(ray, best)), kind))
}
pub(super) fn sample(
    camera: Camera,
    pixel: [i32; 2],
    location: [f64; 2],
    quality: u8,
) -> Option<f64> {
    let uv = |p: [i32; 2]| {
        [
            (f64::from(p[0]) + location[0]) / f64::from(N),
            (f64::from(p[1]) + location[1]) / f64::from(N),
        ]
    };
    let (point, kind) = hit(camera, uv(pixel))?;
    for offset in [[-1, 0], [1, 0], [0, -1], [0, 1]] {
        let p = [pixel[0] + offset[0], pixel[1] + offset[1]];
        if p.iter().any(|v| *v < 0 || *v >= N as i32)
            || hit(camera, uv(p)).is_none_or(|(_, other)| other != kind)
        {
            return None;
        }
    }
    let (normal, tangent, bitangent) = if kind == 1 {
        ([0., 1., 0.], [0., 0., 1.], [1., 0., 0.])
    } else {
        ([1., 0., 0.], [0., 0., -1.], [0., 1., 0.])
    };
    let view = norm(sub(unproject(camera, uv(pixel), 1.), point));
    let count = if quality == 1 { 16 } else { 32 };
    let mut blocked = 0.;
    for index in 0..count {
        let j = index * (32 / count);
        let z = (j as f64 + 0.5) / 32.;
        let angle = j as f64 * std::f64::consts::PI * (3. - 5f64.sqrt());
        let radial = (1. - z * z).sqrt();
        let scale = 0.1 + 0.9 * z * z;
        let k = [
            radial * angle.cos() * scale,
            radial * angle.sin() * scale,
            z * scale,
        ];
        let offset = add(
            add(mul(tangent, k[0]), mul(bitangent, k[1])),
            mul(normal, k[2]),
        );
        let probe = add(point, mul(offset, 1.25));
        let p = transform(camera.view_projection, [probe[0], probe[1], probe[2], 1.]);
        if p[3] <= 0. {
            continue;
        }
        let clip = [p[0] / p[3], p[1] / p[3], p[2] / p[3]];
        let puv = [clip[0] * 0.5 + 0.5, 0.5 - clip[1] * 0.5];
        if puv.iter().any(|v| *v < 0. || *v >= 1.) || clip[2] < 0. || clip[2] > 1. {
            continue;
        }
        let screen = puv.map(|v| v * f64::from(N));
        if screen.iter().any(|v| (v - v.round()).abs() < 0.0001) {
            return None;
        }
        let other_pixel = screen.map(|v| v.floor() as i32);
        let Some((actual, _)) = hit(camera, uv(other_pixel)) else {
            continue;
        };
        let delta = sub(actual, point);
        let squared = dot(delta, delta);
        let normal_distance = dot(delta, normal);
        let toward_camera = dot(sub(actual, probe), view);
        if (squared - 1.25f64.powi(2)).abs() < 0.0001
            || (normal_distance - 0.025).abs() < 0.0001
            || (toward_camera - 0.025).abs() < 0.0001
        {
            return None;
        }
        if squared < 1.25f64.powi(2) && normal_distance > 0.025 && toward_camera > 0.025 {
            blocked += 1. - squared / 1.25f64.powi(2);
        }
    }
    Some(1. - blocked / count as f64)
}
