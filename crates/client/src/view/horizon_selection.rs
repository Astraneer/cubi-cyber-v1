//! Worker-only SSE selection over public height certificates. Unknown error is
//! kept explicit. Capacity can limit requested quality, never protected geometry.
use super::*;
use crate::height_neighbors::ancestor_neighbour;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Projection {
    /// Every actual eye in this box uses the same immutable plan snapshot.
    pub eye_min: [f64; 3],
    pub eye_max: [f64; 3],
    pub focal_pixels: f64,
    pub diagonal_tangent: f64,
    pub target_pixels: f64,
    pub hysteresis_pixels: f64,
    /// Quantized view axis; cosine includes the viewport diagonal AND the full
    /// angular quantization margin, so a current visible tile cannot be culled.
    pub view_direction: [f64; 3],
    pub view_cosine: f64,
    /// Vertical upper-envelope displacement only; this does not certify arbitrary
    /// voxel walls, underwater surfaces or horizontal correspondence.
    /// Supremum of |actual camera forward dot world Y| for this snapshot.
    /// Generic/legacy projections must use1; only a certified pitch bin tightens it.
    pub max_abs_forward_y: f64,
}
impl Default for Projection {
    fn default() -> Self {
        Self {
            eye_min: [0.; 3],
            eye_max: [0.; 3],
            focal_pixels: 1.,
            diagonal_tangent: 1.,
            target_pixels: 32.,
            hysteresis_pixels: 8.,
            view_direction: [0., 0., 1.],
            view_cosine: -1.,
            max_abs_forward_y: 1.,
        }
    }
}
/// The whole floor-aligned pitch bin bounds vertical displacement, including
/// clipped polar bins. Input is the actual camera axis after bob/third-person.
pub fn orientation_certificate(
    yaw: f64,
    pitch: f64,
    yaw_step: f64,
    pitch_step: f64,
) -> Result<([f64; 3], f64, f64), Error> {
    if !yaw.is_finite()
        || !pitch.is_finite()
        || !(-90. ..=90.).contains(&pitch)
        || ![yaw_step, pitch_step]
            .into_iter()
            .all(|v| v.is_finite() && v > 0. && v <= 90.)
    {
        return Err(Error::InvalidConfig);
    }
    let yaw_bucket = (yaw / yaw_step).floor();
    let pitch_bucket = (pitch / pitch_step).floor();
    if !yaw_bucket.is_finite() || !pitch_bucket.is_finite() {
        return Err(Error::InvalidConfig);
    }
    let centre_yaw = yaw_bucket * yaw_step + yaw_step * 0.5;
    let centre_pitch = pitch_bucket * pitch_step + pitch_step * 0.5;
    let y = centre_yaw.to_radians();
    let p = centre_pitch.to_radians();
    if ![centre_yaw, centre_pitch, y, p]
        .into_iter()
        .all(f64::is_finite)
    {
        return Err(Error::InvalidConfig);
    }
    let axis = [
        libm::sin(y) * libm::cos(p),
        libm::sin(p),
        libm::cos(y) * libm::cos(p),
    ];
    let cosine =
        libm::cos((yaw_step * 0.5).to_radians()) * libm::cos((pitch_step * 0.5).to_radians());
    let margin = libm::acos(cosine.clamp(-1., 1.)) + 1e-10;
    let limit = (centre_pitch.abs() + pitch_step * 0.5)
        .min(90.)
        .to_radians();
    let s = (libm::sin(limit) + 1e-10).min(1.);
    if axis.iter().any(|v| !v.is_finite())
        || !margin.is_finite()
        || !s.is_finite()
        || !(0. ..=1.).contains(&s)
    {
        return Err(Error::InvalidConfig);
    }
    Ok((axis, margin, s))
}
impl Projection {
    pub fn validate(self) -> Result<(), Error> {
        if self
            .eye_min
            .into_iter()
            .chain(self.eye_max)
            .any(|v| !v.is_finite())
            || (0..3).any(|i| self.eye_min[i] > self.eye_max[i])
            || !self.focal_pixels.is_finite()
            || self.focal_pixels <= 0.
            || !self.diagonal_tangent.is_finite()
            || self.diagonal_tangent <= 0.
            || !self.target_pixels.is_finite()
            || self.target_pixels <= 0.
            || !self.hysteresis_pixels.is_finite()
            || self.hysteresis_pixels < 0.
            || self.hysteresis_pixels >= self.target_pixels
            || self.view_direction.iter().any(|v| !v.is_finite())
            || (self.view_direction.iter().map(|v| v * v).sum::<f64>() - 1.).abs() > 1e-6
            || !self.max_abs_forward_y.is_finite()
            || !(0. ..=1.).contains(&self.max_abs_forward_y)
            || !self.view_cosine.is_finite()
            || !(-1.0..=1.0).contains(&self.view_cosine)
        {
            return Err(Error::InvalidConfig);
        }
        Ok(())
    }
    pub fn could_be_visible(self, certificate: Certificate) -> bool {
        let Some(b) = render_key(certificate.key).bounds() else {
            return true;
        };
        let lo = [
            b[0] as f64,
            f64::from(certificate.bound.min_height),
            b[1] as f64,
        ];
        let hi = [
            b[2] as f64,
            f64::from(certificate.bound.max_height),
            b[3] as f64,
        ];
        let mut delta = [0.; 3];
        let mut radius = certificate.bound.blocks();
        let mut radius_squared = 0.;
        for axis in 0..3 {
            delta[axis] = (lo[axis] + hi[axis] - self.eye_min[axis] - self.eye_max[axis]) * 0.5;
            let half = (hi[axis] - lo[axis] + self.eye_max[axis] - self.eye_min[axis]) * 0.5;
            radius_squared += half * half;
        }
        if !radius_squared.is_finite() || delta.iter().any(|v| !v.is_finite()) {
            return true;
        }
        radius += libm::sqrt(radius_squared);
        let distance = libm::sqrt(delta.iter().map(|v| v * v).sum::<f64>());
        if !radius.is_finite() || !distance.is_finite() {
            return true;
        }
        if distance <= radius {
            return true;
        }
        let sine = radius / distance;
        let cosine = libm::sqrt((1. - sine * sine).max(0.));
        if self.view_cosine < -cosine {
            return true;
        }
        let cone_sine = libm::sqrt((1. - self.view_cosine * self.view_cosine).max(0.));
        let allowed = self.view_cosine * cosine - cone_sine * sine;
        let actual = delta
            .into_iter()
            .zip(self.view_direction)
            .map(|(a, b)| a * b)
            .sum::<f64>()
            / distance;
        !actual.is_finite() || !allowed.is_finite() || actual + 1e-12 >= allowed
    }
    /// Conservative displacement within the viewport cone. A visible endpoint
    /// has z>=distance/sqrt(1+tan(diagonal)^2). Bounding the perspective quotient
    /// also accounts for the displaced endpoint's denominator and off-axis x/y.
    pub fn error_pixels(self, certificate: Certificate) -> f64 {
        if certificate.exact_replacement {
            return 0.;
        }
        let Some(b) = render_key(certificate.key).bounds() else {
            return f64::INFINITY;
        };
        let low = [
            b[0] as f64,
            f64::from(certificate.bound.min_height),
            b[1] as f64,
        ];
        let high = [
            b[2] as f64,
            f64::from(certificate.bound.max_height),
            b[3] as f64,
        ];
        let mut distance_squared = 0.;
        for axis in 0..3 {
            let gap = (low[axis] - self.eye_max[axis])
                .max(self.eye_min[axis] - high[axis])
                .max(0.);
            distance_squared += gap * gap;
        }
        let error = certificate.bound.blocks() + certificate.seam_error_q1024 as f64 / 1024.;
        if error == 0. {
            return 0.;
        }
        self.displacement_error_pixels(libm::sqrt(distance_squared), error)
    }
    fn displacement_error_pixels(self, distance: f64, error: f64) -> f64 {
        if !distance.is_finite()
            || !error.is_finite()
            || !self.focal_pixels.is_finite()
            || !self.diagonal_tangent.is_finite()
            || !self.max_abs_forward_y.is_finite()
            || distance < 0.
            || error < 0.
        {
            return f64::INFINITY;
        }
        let k = libm::sqrt(1. + self.diagonal_tangent * self.diagonal_tangent);
        if !k.is_finite() || k <= 0. {
            return f64::INFINITY;
        }
        // Preserve exact legacy/general arithmetic, not only algebraic equality.
        if self.max_abs_forward_y == 1. {
            let denominator = distance / k - error;
            if !denominator.is_finite() || denominator <= 0. {
                return f64::INFINITY;
            }
            let result = self.focal_pixels * error * k / denominator;
            return if result.is_finite() && result >= 0. {
                result
            } else {
                f64::INFINITY
            };
        }
        let s = self.max_abs_forward_y.min(self.diagonal_tangent / k);
        let numerator = libm::sqrt((1. - s * s).max(0.)) + self.diagonal_tangent * s;
        // Only the proxy/LOD endpoint needs to be inside the certified AABB.
        // For a visible true endpoint q=(z*t,z), p=q+e*n satisfies
        // ||p|| <= k*p_z + E*N. A positive denominator also proves p_z>0.
        let denominator = distance / k - error * numerator / k;
        if !numerator.is_finite() || !denominator.is_finite() || denominator <= 0. {
            return f64::INFINITY;
        }
        let result = self.focal_pixels * error * numerator / denominator;
        if result.is_finite() && result >= 0. {
            result.next_up()
        } else {
            f64::INFINITY
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Certificate {
    pub key: HeightKey,
    pub bound: net::height::HeightErrorBound,
    /// Exact maximum public odd-edge adjustment if this tile gains a seam.
    pub seam_error_q1024: u64,
    /// All height triangles are replaced by a validated public column manifest.
    /// The owner still holds the old cover until every proxy is GPU resident.
    pub exact_replacement: bool,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct SelectionQuality {
    pub missing_certificates: u64,
    pub capacity_limited: bool,
    pub leaf_limited: u64,
    pub maximum_error_pixels: f64,
    pub sse_refinements: u64,
}
fn fully_near(certificate: Certificate, volume: Option<[[i32; 3]; 2]>) -> bool {
    let Some([min, max]) = volume else {
        return false;
    };
    let Some(b) = render_key(certificate.key).bounds() else {
        return false;
    };
    b[0] >= i64::from(min[0]) * 16
        && b[1] >= i64::from(min[2]) * 16
        && b[2] <= i64::from(max[0]) * 16
        && b[3] <= i64::from(max[2]) * 16
        && i64::from(certificate.bound.min_height) > i64::from(min[1]) * 16
        && i64::from(certificate.bound.max_height) <= i64::from(max[1]) * 16
}
pub fn seam_error_q1024(heights: &[i32; 289]) -> u64 {
    let mut maximum = 0;
    for i in (1..16).step_by(2) {
        for (a, b, c) in [
            (i - 1, i, i + 1),
            (272 + i - 1, 272 + i, 272 + i + 1),
            (17 * (i - 1), 17 * i, 17 * (i + 1)),
            (16 + 17 * (i - 1), 16 + 17 * i, 16 + 17 * (i + 1)),
        ] {
            let twice = (i64::from(heights[a]) + i64::from(heights[c]) - 2 * i64::from(heights[b]))
                .unsigned_abs();
            maximum = maximum.max(twice * 512);
        }
    }
    maximum
}

fn children(key: HeightTileKey) -> [HeightTileKey; 4] {
    std::array::from_fn(|i| HeightTileKey {
        level: key.level - 1,
        x: key.x * 2 + (i as i32 & 1),
        z: key.z * 2 + (i as i32 >> 1),
        ..key
    })
}
fn split(tiles: &mut Vec<PlannedHeightTile>, index: usize) {
    let parent = tiles.swap_remove(index).key;
    tiles.extend(children(parent).map(|key| PlannedHeightTile { key, seams: 0 }));
}
fn balance(
    tiles: &mut Vec<PlannedHeightTile>,
    maximum: usize,
    work: &mut PlanWork,
) -> Result<(), Error> {
    let mut pending = tiles.iter().map(|tile| tile.key).collect::<Vec<_>>();
    balance_from(tiles, maximum, work, &mut pending)
}
fn balance_from(
    tiles: &mut Vec<PlannedHeightTile>,
    maximum: usize,
    work: &mut PlanWork,
    pending: &mut Vec<HeightTileKey>,
) -> Result<(), Error> {
    let mut indices = tiles
        .iter()
        .enumerate()
        .map(|(i, tile)| (tile.key, i))
        .collect::<BTreeMap<_, _>>();
    while let Some(key) = pending.pop() {
        if !indices.contains_key(&key) {
            continue;
        }
        let mut neighbour = None;
        'search: for level in key.level + 2..=render::height_tile::MAX_LEVEL {
            for edge in 0..4 {
                if work.adjacency_tests + work.seam_tests >= MAX_PLAN_PAIRS {
                    return Err(Error::Capacity);
                }
                work.adjacency_tests += 1;
                let Some(coarse) = ancestor_neighbour(key, edge, level) else {
                    continue;
                };
                if let Some(&index) = indices.get(&coarse) {
                    neighbour = Some((coarse, index));
                    break 'search;
                }
            }
        }
        if let Some((coarse, index)) = neighbour {
            if tiles.len() + 3 > maximum {
                return Err(Error::Capacity);
            }
            indices.remove(&coarse);
            tiles.swap_remove(index);
            if let Some(moved) = tiles.get(index) {
                indices.insert(moved.key, index);
            }
            for child in children(coarse) {
                indices.insert(child, tiles.len());
                tiles.push(PlannedHeightTile {
                    key: child,
                    seams: 0,
                });
                pending.push(child);
            }
            work.refinements += 1;
            work.base_nodes += 4;
            pending.push(key);
        }
    }
    Ok(())
}
pub(super) fn execute_sse(task: &PlanTask, projection: Projection) -> Result<Planned, Error> {
    projection.validate()?;
    let span = 1_i64 << task.config.horizon_level;
    let center = [
        task.camera[0].div_euclid(span),
        task.camera[1].div_euclid(span),
    ];
    let mut tiles = Vec::with_capacity(task.config.max_tiles);
    // Five roots per axis retain two full root spans in both directions at
    // every camera position inside its root bin, including its last page.
    for z in center[1] - 2..center[1] + 3 {
        for x in center[0] - 2..center[0] + 3 {
            let key = HeightTileKey {
                space: task.space,
                level: task.config.horizon_level,
                x: i32::try_from(x).map_err(|_| Error::Plan)?,
                z: i32::try_from(z).map_err(|_| Error::Plan)?,
            };
            key.bounds().ok_or(Error::Plan)?;
            tiles.push(PlannedHeightTile { key, seams: 0 });
        }
    }
    let near = if let Some([min, max]) = task.config.near_volume {
        [
            i64::from(min[0]) * 16,
            i64::from(min[2]) * 16,
            i64::from(max[0]) * 16,
            i64::from(max[2]) * 16,
        ]
    } else {
        [
            (task.camera[0] - task.near_radius) * 16,
            (task.camera[1] - task.near_radius) * 16,
            (task.camera[0] + task.near_radius + 1) * 16,
            (task.camera[1] + task.near_radius + 1) * 16,
        ]
    };
    let mut work = PlanWork {
        base_nodes: 25,
        ..PlanWork::default()
    };
    // Protection and the whole near cutout are mandatory, regardless of SSE.
    loop {
        let mut action = None;
        for (i, tile) in tiles.iter().enumerate() {
            let b = tile.key.bounds().ok_or(Error::Plan)?;
            if task.config.near_volume.is_none() && contains(near, b) {
                action = Some((i, false));
                break;
            }
            let protected = task.refine.iter().any(|hint| {
                hint.space == task.space
                    && hint.level <= tile.key.level
                    && render_key(*hint).bounds().is_some_and(|h| contains(b, h))
            });
            if tile.key.level > 0 && (protected || intersects(near, b)) {
                action = Some((i, true));
                break;
            }
        }
        let Some((index, refine)) = action else { break };
        if refine {
            if tiles.len() + 3 > task.config.max_tiles {
                return Err(Error::Capacity);
            }
            split(&mut tiles, index);
            work.refinements += 1;
            work.base_nodes += 4;
        } else {
            tiles.swap_remove(index);
            work.near_tiles_removed += 1;
        }
    }
    balance(&mut tiles, task.config.max_tiles, &mut work)?;
    let certificates = task
        .certificates
        .iter()
        .map(|c| (c.key, *c))
        .collect::<BTreeMap<_, _>>();
    let mut previously_split = BTreeSet::new();
    for key in &task.previous {
        let mut key = *key;
        while key.level < task.config.horizon_level {
            key = HeightTileKey {
                level: key.level + 1,
                x: key.x.div_euclid(2),
                z: key.z.div_euclid(2),
                ..key
            };
            previously_split.insert(key);
        }
    }
    let mut quality = SelectionQuality::default();
    let mut blocked = BTreeSet::new();
    loop {
        let mut candidate = None;
        let mut worst = 0.;
        for (i, tile) in tiles.iter().enumerate() {
            if tile.key.level == 0 || blocked.contains(&tile.key) {
                continue;
            }
            let Some(certificate) = certificates.get(&wire_key(tile.key)) else {
                continue;
            };
            if fully_near(*certificate, task.config.near_volume)
                || !projection.could_be_visible(*certificate)
            {
                continue;
            }
            let threshold = projection.target_pixels
                + if previously_split.contains(&tile.key) {
                    -projection.hysteresis_pixels
                } else {
                    0.
                };
            let pixels = projection.error_pixels(*certificate);
            let score = pixels / threshold;
            if score > 1. && score > worst {
                candidate = Some(i);
                worst = score;
            }
        }
        let Some(index) = candidate else { break };
        let key = tiles[index].key;
        if tiles.len() + 3 > task.config.max_tiles {
            blocked.insert(key);
            quality.capacity_limited = true;
            continue;
        }
        let old = tiles.clone();
        split(&mut tiles, index);
        let mut pending = children(key).to_vec();
        if balance_from(&mut tiles, task.config.max_tiles, &mut work, &mut pending).is_err() {
            tiles = old;
            blocked.insert(key);
            quality.capacity_limited = true;
            if work.adjacency_tests + work.seam_tests >= MAX_PLAN_PAIRS {
                return Err(Error::Capacity);
            }
            continue;
        }
        work.refinements += 1;
        work.base_nodes += 4;
        quality.sse_refinements += 1;
    }
    let mut exact_leaves = Vec::new();
    for tile in &tiles {
        if let Some(certificate) = certificates.get(&wire_key(tile.key)) {
            if fully_near(*certificate, task.config.near_volume)
                || !projection.could_be_visible(*certificate)
            {
                continue;
            }
            let pixels = projection.error_pixels(*certificate);
            quality.maximum_error_pixels = quality.maximum_error_pixels.max(pixels);
            if tile.key.level == 0 && pixels > projection.target_pixels {
                quality.leaf_limited += 1;
                exact_leaves.push(wire_key(tile.key));
            }
        } else {
            quality.missing_certificates += 1;
        }
    }
    exact_leaves.sort_unstable();
    set_seams(&mut tiles, &mut work)?;
    tiles.sort_unstable_by_key(|t| t.key);
    Ok(Planned {
        id: task.id,
        generation: task.generation,
        plan: HeightPlan {
            tiles,
            visited_nodes: work.base_nodes as u32,
            outer_bounds_pages: [
                (center[0] - 2) * span,
                (center[1] - 2) * span,
                (center[0] + 3) * span,
                (center[1] + 3) * span,
            ],
        },
        work,
        quality,
        exact_leaves,
    })
}
fn set_seams(tiles: &mut [PlannedHeightTile], work: &mut PlanWork) -> Result<(), Error> {
    let keys = tiles.iter().map(|tile| tile.key).collect::<BTreeSet<_>>();
    for tile in tiles {
        tile.seams = 0;
        for level in tile.key.level + 1..=render::height_tile::MAX_LEVEL {
            for edge in 0..4 {
                if work.adjacency_tests + work.seam_tests >= MAX_PLAN_PAIRS {
                    return Err(Error::Capacity);
                }
                work.seam_tests += 1;
                if ancestor_neighbour(tile.key, edge, level).is_some_and(|key| keys.contains(&key))
                {
                    if level != tile.key.level + 1 {
                        return Err(Error::Plan);
                    }
                    tile.seams |= 1 << edge;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dyadic_balance_and_seams_match_independent_all_pairs_for_negative_and_positive_cells() {
        for seed in 1..=16u64 {
            let mut random = seed;
            let mut tiles = (-2..2)
                .flat_map(|z| {
                    (-2..2).map(move |x| PlannedHeightTile {
                        key: HeightTileKey {
                            space: 7,
                            level: 5,
                            x,
                            z,
                        },
                        seams: 0,
                    })
                })
                .collect::<Vec<_>>();
            for _ in 0..64 {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                let index = (random as usize) % tiles.len();
                if tiles[index].key.level > 0 {
                    split(&mut tiles, index);
                }
            }
            let mut work = PlanWork::default();
            balance(&mut tiles, 4096, &mut work).unwrap();
            set_seams(&mut tiles, &mut work).unwrap();
            let mut area = 0i64;
            for (i, tile) in tiles.iter().enumerate() {
                let b = tile.key.bounds().unwrap();
                area += (b[2] - b[0]) * (b[3] - b[1]);
                let mut expected = 0;
                for (j, other) in tiles.iter().enumerate() {
                    if i == j {
                        continue;
                    }
                    let o = other.key.bounds().unwrap();
                    assert!(!intersects(b, o));
                    if adjacent(tile.key, other.key) {
                        assert!(tile.key.level.abs_diff(other.key.level) <= 1);
                        if other.key.level > tile.key.level {
                            expected |= if b[0] == o[2] {
                                1
                            } else if b[2] == o[0] {
                                2
                            } else if b[1] == o[3] {
                                4
                            } else {
                                8
                            };
                        }
                    }
                }
                assert_eq!(tile.seams, expected, "seed{seed}tile{:?}", tile.key);
            }
            assert_eq!(area, 2048 * 2048);
            assert!(work.adjacency_tests + work.seam_tests < 100_000);
        }
    }
    #[test]
    fn doubling_unknown_horizon_adds_a_bounded_ring_of_planning_work() {
        let mut counts = Vec::new();
        for level in [8, 9, 10] {
            let mut t = task();
            t.config.horizon_level = level;
            t.near_radius = 2;
            let result = t.execute().unwrap();
            counts.push((
                result.plan.tiles.len(),
                result.work.adjacency_tests + result.work.seam_tests,
            ));
        }
        for pair in counts.windows(2) {
            assert!(pair[1].0 <= pair[0].0 + 64, "{counts:?}");
            assert!(pair[1].1 < pair[0].1 * 3 / 2, "{counts:?}");
        }
    }
    #[test]
    fn horizon_radius_is_retained_at_both_ends_of_every_camera_bin() {
        for level in [8u8, 9, 10] {
            let span = 1i64 << level;
            for camera in [[0, 0], [span - 1, span - 1], [-1, -1], [-span, -span]] {
                let mut t = task();
                t.config.horizon_level = level;
                t.camera = camera;
                t.near_radius = 2;
                let plan = t.execute().unwrap();
                let b = plan.plan.outer_bounds_pages;
                assert!(camera[0] - b[0] >= 2 * span && b[2] - camera[0] >= 2 * span);
                assert!(camera[1] - b[1] >= 2 * span && b[3] - camera[1] >= 2 * span);
                let area = plan
                    .plan
                    .tiles
                    .iter()
                    .map(|tile| {
                        let b = tile.key.bounds().unwrap();
                        (b[2] - b[0]) * (b[3] - b[1])
                    })
                    .sum::<i64>();
                assert_eq!(
                    area,
                    ((5 * span) * (5 * span) - 25) * 256,
                    "only the true5x5 near cutout may be missing"
                );
            }
        }
    }
    fn projection() -> Projection {
        Projection {
            eye_min: [0., 10000., 0.],
            eye_max: [0., 10000., 0.],
            focal_pixels: 100.,
            diagonal_tangent: 1.,
            target_pixels: 10.,
            hysteresis_pixels: 2.,
            view_direction: [0., -1., 0.],
            max_abs_forward_y: 1.,
            view_cosine: 0.,
        }
    }
    fn task() -> PlanTask {
        PlanTask {
            id: 1,
            generation: 0,
            config: Config {
                horizon_level: 2,
                ..Config::default()
            },
            space: 0,
            camera: [0, 0],
            refine: vec![],
            near_radius: 0,
            projection: Some(projection()),
            certificates: vec![],
            previous: vec![],
        }
    }
    fn root() -> HeightKey {
        HeightKey {
            space: 0,
            level: 2,
            x: 1,
            z: 1,
        }
    }
    fn certificate(pixels: f64) -> Certificate {
        // Independent inversion of p=f*e*k/(distance/k-e), using the nearest
        // point [64,0,64] on this fixture's flat root bounds.
        let distance = libm::sqrt(10000. * 10000. + 64. * 64. + 64. * 64.);
        let k = libm::sqrt(2.);
        let error = pixels * (distance / k) / (100. * k + pixels);
        Certificate {
            key: root(),
            bound: net::height::HeightErrorBound {
                max_error_q1024: libm::ceil(error * 1024.) as u64,
                min_height: 0,
                max_height: 0,
            },
            seam_error_q1024: 0,
            exact_replacement: false,
        }
    }
    fn has_root(plan: &Planned) -> bool {
        plan.plan
            .tiles
            .iter()
            .any(|tile| tile.key == render_key(root()))
    }
    #[test]
    fn hysteresis_uses_the_previous_branch_and_does_not_toggle_inside_the_band() {
        let mut t = task();
        t.certificates.push(certificate(9.));
        assert!(
            has_root(&t.execute().unwrap()),
            "a coarse node remains coarse below the10px maximum"
        );
        let mut t = task();
        t.certificates.push(certificate(9.));
        t.previous.push(children(render_key(root()))[0]);
        assert!(
            !has_root(&t.execute().unwrap()),
            "a split node remains split above8px"
        );
        let mut t = task();
        t.certificates.push(certificate(6.));
        t.previous.push(children(render_key(root()))[0]);
        assert!(
            has_root(&t.execute().unwrap()),
            "coarsen only after crossing the lower threshold"
        );
        let mut t = task();
        t.certificates.push(certificate(11.));
        assert!(
            !has_root(&t.execute().unwrap()),
            "refine only after crossing the upper threshold"
        );
    }
    #[test]
    fn unknown_error_never_claims_quality_and_capacity_preserves_complete_balanced_cover() {
        let unknown = task().execute().unwrap();
        assert_eq!(
            unknown.quality.missing_certificates,
            unknown.plan.tiles.len() as u64
        );
        let mut t = task();
        t.config.max_tiles = 64;
        for z in -2..2 {
            for x in -2..2 {
                t.certificates.push(Certificate {
                    key: HeightKey { x, z, ..root() },
                    bound: net::height::HeightErrorBound {
                        max_error_q1024: 1_000_000_000,
                        min_height: 0,
                        max_height: 0,
                    },
                    seam_error_q1024: 0,
                    exact_replacement: false,
                });
            }
        }
        let result = t.execute().unwrap();
        assert!(result.quality.capacity_limited);
        assert!(result.plan.tiles.len() <= 64);
        let mut area = 0i64;
        for (i, a) in result.plan.tiles.iter().enumerate() {
            let ab = a.key.bounds().unwrap();
            assert!(!intersects([0, 0, 16, 16], ab));
            area += (ab[2] - ab[0]) * (ab[3] - ab[1]);
            for b in result.plan.tiles.iter().skip(i + 1) {
                assert!(!intersects(ab, b.key.bounds().unwrap()));
                if adjacent(a.key, b.key) {
                    assert!(a.key.level.abs_diff(b.key.level) <= 1);
                }
            }
        }
        assert_eq!(
            area,
            320 * 320 - 16 * 16,
            "only the exact near page is cut out"
        );
    }
    #[test]
    fn projected_bound_covers_off_axis_perspective_and_accounts_for_a_whole_eye_box() {
        let p = Projection {
            eye_min: [0.; 3],
            eye_max: [0.; 3],
            focal_pixels: 100.,
            diagonal_tangent: 1.,
            target_pixels: 10.,
            hysteresis_pixels: 2.,
            view_direction: [0., -1., 0.],
            max_abs_forward_y: 1.,
            view_cosine: 0.,
        };
        let c = Certificate {
            key: HeightKey {
                space: 0,
                level: 0,
                x: 0,
                z: 6,
            },
            bound: net::height::HeightErrorBound {
                max_error_q1024: 1024,
                min_height: 0,
                max_height: 0,
            },
            seam_error_q1024: 0,
            exact_replacement: false,
        };
        let bound = p.error_pixels(c);
        assert!(bound > 2.1 && bound < 2.2);
        for z in [96f64, 104., 112.] {
            for x in [0., 8., 16.] {
                let before = [100. * x / z, 0.];
                let after = [100. * x / (z - 0.6), 100. * 0.8 / (z - 0.6)];
                let actual =
                    libm::sqrt((after[0] - before[0]).powi(2) + (after[1] - before[1]).powi(2));
                assert!(actual <= bound);
            }
        }
        let expanded = Projection {
            eye_max: [0., 0., 16.],
            ..p
        };
        assert!(expanded.error_pixels(c) > bound);
        let inside = Projection {
            eye_min: [8., 0., 100.],
            eye_max: [8., 0., 100.],
            ..p
        };
        assert!(
            inside.error_pixels(c).is_infinite(),
            "a near-plane crossing cannot claim finite quality"
        );
    }
    #[test]
    fn browser_selection_packet_preserves_projection_hysteresis_and_certificates() {
        let mut t = task();
        t.certificates.push(certificate(14.));
        t.previous.push(children(render_key(root()))[0]);
        let bytes = encode_background_task(&BackgroundTask::Plan(Box::new(t))).unwrap();
        let BackgroundResult::Plan(Ok(result)) =
            decode_background_result(&execute_background_request(&bytes, || false).unwrap())
                .unwrap()
        else {
            panic!("valid public worker selection")
        };
        assert!(!has_root(&result));
        assert!(result.quality.sse_refinements > 0);
        assert!(execute_background_request(&bytes, || true).is_err());
        for end in [0, 4, 17, bytes.len() - 1] {
            assert!(execute_background_request(&bytes[..end], || false).is_err());
        }
        let mut malformed = task();
        let mut invalid = certificate(14.);
        invalid.bound.min_height = 1;
        malformed.certificates.push(invalid);
        let bytes = encode_background_task(&BackgroundTask::Plan(Box::new(malformed))).unwrap();
        assert!(execute_background_request(&bytes, || false).is_err());
    }
    #[test]
    fn maximum_plan_metadata_packet_is_bounded_and_worker_decodes_the_larger_profile() {
        let mut t = task();
        for x in 0..net::height::MAX_SOURCES {
            let key = HeightKey {
                space: 0,
                level: 0,
                x: x as i32,
                z: 0,
            };
            t.certificates.push(Certificate {
                key,
                bound: net::height::HeightErrorBound {
                    max_error_q1024: 0,
                    min_height: 0,
                    max_height: 0,
                },
                seam_error_q1024: 0,
                exact_replacement: false,
            });
            if x < net::height::MAX_TILES {
                t.previous.push(render_key(key));
                t.refine.push(key);
            }
        }
        let bytes = encode_background_task(&BackgroundTask::Plan(Box::new(t))).unwrap();
        assert!(bytes.len() > 180_000 && bytes.len() <= MAX_BACKGROUND_BYTES);
        let result = execute_background_request(&bytes, || false).unwrap();
        let BackgroundResult::Plan(Ok(plan)) = decode_background_result(&result).unwrap() else {
            panic!("maximum metadata plan must execute");
        };
        assert!(plan.plan.tiles.len() <= net::height::MAX_TILES);
        let mut oversized = bytes;
        oversized.resize(MAX_BACKGROUND_BYTES + 1, 0);
        assert_eq!(
            execute_background_request(&oversized, || false),
            Err(WireError::Length)
        );
    }
    #[test]
    fn offscreen_bounds_do_not_consume_refinement_but_viewport_margin_keeps_intersections() {
        let p = Projection {
            eye_min: [0.; 3],
            eye_max: [0.; 3],
            focal_pixels: 100.,
            diagonal_tangent: 0.5,
            target_pixels: 10.,
            hysteresis_pixels: 2.,
            view_direction: [0., 0., 1.],
            max_abs_forward_y: 1.,
            view_cosine: 0.9,
        };
        let c = Certificate {
            key: HeightKey {
                space: 0,
                level: 0,
                x: 0,
                z: 20,
            },
            bound: net::height::HeightErrorBound {
                max_error_q1024: 1024,
                min_height: 0,
                max_height: 16,
            },
            seam_error_q1024: 0,
            exact_replacement: false,
        };
        assert!(p.could_be_visible(c));
        assert!(!p.could_be_visible(Certificate {
            key: HeightKey { z: -20, ..c.key },
            ..c
        }));
        let p = Projection {
            eye_max: [0., 0., 350.],
            ..p
        };
        assert!(
            p.could_be_visible(c),
            "an intersecting eye box must not reject the tile"
        );
    }
    #[test]
    fn irreducible_leaf_requests_exact_public_column_and_preserves_original_error_certificate() {
        let mut t = task();
        t.config.horizon_level = 0;
        let c = Certificate {
            key: HeightKey {
                space: 0,
                level: 0,
                x: 1,
                z: 1,
            },
            bound: net::height::HeightErrorBound {
                max_error_q1024: 1_000_000_000,
                min_height: 0,
                max_height: 0,
            },
            seam_error_q1024: 0,
            exact_replacement: false,
        };
        t.certificates.push(c);
        let result = t.execute().unwrap();
        assert_eq!(result.exact_leaves, vec![c.key]);
        assert_eq!(result.quality.leaf_limited, 1);
        let mut t = task();
        t.config.horizon_level = 0;
        t.certificates.push(Certificate {
            exact_replacement: true,
            ..c
        });
        let result = t.execute().unwrap();
        assert!(result.exact_leaves.is_empty());
        assert_eq!(result.quality.leaf_limited, 0);
        assert_eq!(result.quality.maximum_error_pixels, 0.);
        assert!(
            c.bound.max_error_q1024 > 0,
            "exact representation does not rewrite the source bound"
        );
    }
    #[test]
    fn canonical_1024_horizon_converges_using_only_public_certificates_under_fixed_capacity() {
        for seed in [1, 1337] {
            let generator = terrain_gen::Generator::new(terrain_gen::TerrainSeed(seed));
            let mut t = task();
            t.config = Config {
                max_tiles: 512,
                max_sources: 2048,
                ..Config::default()
            };
            t.near_radius = 2;
            let tan = libm::tan(70f64.to_radians() * 0.5);
            let projection = Projection {
                eye_min: [0., 80., 0.],
                eye_max: [16., 96., 16.],
                focal_pixels: 437. / (2. * tan),
                diagonal_tangent: libm::tan(80f64.to_radians() * 0.5)
                    * libm::sqrt(1. + (780f64 / 437.).powi(2)),
                target_pixels: 32.,
                hysteresis_pixels: 8.,
                view_direction: [0., 0., 1.],
                max_abs_forward_y: 1.,
                view_cosine: libm::cos(
                    libm::atan(
                        libm::tan(80f64.to_radians() * 0.5)
                            * libm::sqrt(1. + (780f64 / 437.).powi(2)),
                    ) + 37.5f64.to_radians(),
                ),
            };
            let mut certificates = BTreeMap::new();
            let mut previous = Vec::new();
            let mut final_quality = None;
            for round in 0..16 {
                t.id = round + 1;
                t.projection = Some(projection);
                t.certificates = certificates.values().copied().collect();
                t.previous = previous;
                let result = selection::execute_sse(&t, projection).unwrap();
                assert!(result.plan.tiles.len() <= 512);
                previous = result.plan.tiles.iter().map(|tile| tile.key).collect();
                let mut added = 0;
                for tile in &result.plan.tiles {
                    let key = wire_key(tile.key);
                    if certificates.contains_key(&key) {
                        continue;
                    }
                    let b = tile.key.bounds().unwrap();
                    let bound = generator
                        .public_height_error([b[0] as i32, b[1] as i32], key.level)
                        .unwrap();
                    let step = 1i64 << key.level;
                    let heights = std::array::from_fn(|i| {
                        generator
                            .column_info(
                                (b[0] + (i % 17) as i64 * step) as i32,
                                (b[1] + (i / 17) as i64 * step) as i32,
                            )
                            .height
                            .max(terrain_gen::SEA_LEVEL)
                            + 1
                    });
                    certificates.insert(
                        key,
                        Certificate {
                            key,
                            bound: net::height::HeightErrorBound {
                                max_error_q1024: libm::ceil(bound.max_error_blocks * 1024.) as u64,
                                min_height: bound.min_height,
                                max_height: bound.max_height,
                            },
                            seam_error_q1024: seam_error_q1024(&heights),
                            exact_replacement: false,
                        },
                    );
                    added += 1;
                }
                // The independently tested server promotion+GPU manifest path
                // supplies exact public leaves in this pure selection fixture.
                for key in &result.exact_leaves {
                    let certificate = certificates
                        .get_mut(key)
                        .expect("selected exact leaf certificate");
                    if !certificate.exact_replacement {
                        certificate.exact_replacement = true;
                        added += 1;
                    }
                }
                if added == 0 {
                    let strict = Projection {
                        max_abs_forward_y: 1.,
                        view_cosine: libm::cos(libm::atan(projection.diagonal_tangent)),
                        ..projection
                    };
                    let mut worst = result
                        .plan
                        .tiles
                        .iter()
                        .filter(|tile| tile.key.level == 0)
                        .filter_map(|tile| certificates.get(&wire_key(tile.key)).copied())
                        .filter(|certificate| strict.could_be_visible(*certificate))
                        .map(|certificate| (projection.error_pixels(certificate), certificate))
                        .collect::<Vec<_>>();
                    worst.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
                    for (pixels, c) in worst.iter().take(4) {
                        println!(
                            "M5 leaf audit seed{seed} key{:?} cornerRange{} seamError{} px{} height{}..{}",
                            c.key,
                            c.bound.blocks(),
                            c.seam_error_q1024 as f64 / 1024.,
                            pixels,
                            c.bound.min_height,
                            c.bound.max_height
                        );
                    }
                    let mut coarse = result
                        .plan
                        .tiles
                        .iter()
                        .filter(|tile| tile.key.level > 0)
                        .filter_map(|tile| certificates.get(&wire_key(tile.key)).copied())
                        .filter(|c| strict.could_be_visible(*c))
                        .map(|c| (projection.error_pixels(c), c))
                        .collect::<Vec<_>>();
                    coarse.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
                    for (pixels, c) in coarse.iter().take(3) {
                        println!(
                            "M5 coarse audit seed{seed} key{:?} bound{} seam{} px{}",
                            c.key,
                            c.bound.blocks(),
                            c.seam_error_q1024 as f64 / 1024.,
                            pixels
                        );
                    }
                    final_quality = Some((result.plan.tiles.len(), result.quality, round + 1));
                    break;
                }
            }
            let (tiles, quality, rounds) =
                final_quality.expect("a bounded public tree must settle");
            assert_eq!(quality.missing_certificates, 0);
            assert_eq!(quality.leaf_limited, 0);
            assert!(certificates.len() <= 2048);
            println!(
                "M5 SSE seed{seed} rounds{rounds} tiles{tiles} certificates{} maxPixels{} capacityLimited{} leafLimited{}",
                certificates.len(),
                quality.maximum_error_pixels,
                quality.capacity_limited,
                quality.leaf_limited
            );
        }
    }
    #[test]
    fn three_dimensional_near_cover_retains_full_height_area_and_worker_snapshot() {
        for vertical in [[-4, -1], [0, 3], [4, 7], [20, 23]] {
            let mut direct = task();
            let volume = [[1, vertical[0], 1], [4, vertical[1], 4]];
            direct.config.near_volume = Some(volume);
            let plan = direct.execute().unwrap();
            let area = plan
                .plan
                .tiles
                .iter()
                .map(|tile| {
                    let b = tile.key.bounds().unwrap();
                    (b[2] - b[0]) * (b[3] - b[1])
                })
                .sum::<i64>();
            assert_eq!(
                area,
                25 * 64 * 64,
                "3D masking must retain far surfaces outside the near Y volume"
            );
            let mut worker = task();
            worker.config.near_volume = Some(volume);
            let request = encode_background_task(&BackgroundTask::Plan(Box::new(worker))).unwrap();
            let reply = execute_background_request(&request, || false).unwrap();
            let BackgroundResult::Plan(Ok(roundtrip)) = decode_background_result(&reply).unwrap()
            else {
                panic!("plan result")
            };
            assert_eq!(roundtrip.plan.tiles, plan.plan.tiles);
            assert_eq!(roundtrip.work.near_tiles_removed, 0);
        }
        let c = Certificate {
            key: HeightKey {
                space: 0,
                level: 0,
                x: 1,
                z: 1,
            },
            bound: net::height::HeightErrorBound {
                max_error_q1024: 1024,
                min_height: 1,
                max_height: 10,
            },
            seam_error_q1024: 0,
            exact_replacement: false,
        };
        assert!(fully_near(c, Some([[1, 0, 1], [2, 1, 2]])));
        assert!(
            !fully_near(c, Some([[1, 4, 1], [2, 7, 2]])),
            "flight must not exclude lower terrain from quality"
        );
        assert!(
            !fully_near(
                Certificate {
                    bound: net::height::HeightErrorBound {
                        min_height: 0,
                        ..c.bound
                    },
                    ..c
                },
                Some([[1, 0, 1], [2, 1, 2]])
            ),
            "top at lower boundary belongs to the page below"
        );
    }
}

#[cfg(test)]
mod vertical_bound_tests {
    use super::*;
    #[test]
    fn general_s1_is_exact_legacy_and_invalid_inputs_fail_closed() {
        let p = Projection::default();
        assert_eq!(p.max_abs_forward_y, 1.);
        for invalid in [-0.001, 1.001, f64::NAN, f64::INFINITY] {
            assert!(
                Projection {
                    max_abs_forward_y: invalid,
                    ..p
                }
                .validate()
                .is_err()
            );
        }
        for d in [0., 1., 4., 10., 64., 10000.] {
            for e in [0.001, 0.5, 1., 10.] {
                let k = libm::sqrt(1. + p.diagonal_tangent * p.diagonal_tangent);
                let old = if d / k - e <= 0. {
                    f64::INFINITY
                } else {
                    p.focal_pixels * e * k / (d / k - e)
                };
                assert_eq!(p.displacement_error_pixels(d, e).to_bits(), old.to_bits());
            }
        }
    }
    #[test]
    fn finite_extremes_cannot_be_culled_or_hide_nan_as_zero_quality() {
        let ordinary = Projection {
            max_abs_forward_y: 0.174,
            focal_pixels: 385.,
            diagonal_tangent: 1.7115357351239278,
            ..Projection::default()
        };
        let huge_diagonal = Projection {
            diagonal_tangent: f64::MAX,
            ..ordinary
        };
        let huge_focal = Projection {
            focal_pixels: f64::MAX,
            ..ordinary
        };
        let huge_eye = Projection {
            eye_min: [f64::MAX; 3],
            eye_max: [f64::MAX; 3],
            ..ordinary
        };
        for projection in [huge_diagonal, huge_focal, huge_eye] {
            projection.validate().unwrap();
            let certificates = (-2..=2)
                .flat_map(|z| {
                    (-2..=2).map(move |x| Certificate {
                        key: HeightKey {
                            space: 0,
                            level: 0,
                            x,
                            z,
                        },
                        bound: net::height::HeightErrorBound {
                            max_error_q1024: 1024,
                            min_height: 70,
                            max_height: 80,
                        },
                        seam_error_q1024: 512,
                        exact_replacement: false,
                    })
                })
                .collect::<Vec<_>>();
            assert!(projection.could_be_visible(certificates[0]));
            assert_eq!(projection.error_pixels(certificates[0]), f64::INFINITY);
            let result = PublicSelection {
                config: Config {
                    horizon_level: 0,
                    near_volume: None,
                    ..Config::default()
                },
                space: 0,
                camera: [0, 0],
                near_radius: 0,
                projection,
                certificates,
                previous: vec![],
                refine: vec![],
            }
            .execute()
            .unwrap();
            assert_eq!(
                result.quality.maximum_error_pixels,
                f64::INFINITY,
                "quality must hold, never hide NaN with f64::max"
            );
            assert!(result.quality.leaf_limited > 0);
        }
    }
    #[test]
    fn finite_inputs_cannot_escape_as_nonfinite_orientation_certificates() {
        for (yaw, pitch, yaw_step, pitch_step) in [
            (f64::MAX, 0., f64::MIN_POSITIVE, 10.),
            (0., 90., 45., f64::from_bits(1)),
            (f64::MAX, 0., 45., 10.),
        ] {
            match orientation_certificate(yaw, pitch, yaw_step, pitch_step) {
                Ok((axis, margin, s)) => assert!(
                    axis.into_iter().all(f64::is_finite)
                        && margin.is_finite()
                        && s.is_finite()
                        && (0. ..=1.).contains(&s)
                ),
                Err(Error::InvalidConfig) => (),
                Err(other) => panic!("unexpected error: {other}"),
            }
        }
        assert!(orientation_certificate(f64::MAX, 0., f64::MIN_POSITIVE, 10.).is_err());
        assert!(orientation_certificate(0., 90., 45., f64::from_bits(1)).is_err());
    }
    #[test]
    fn certified_pitch_bin_includes_bob_axis_and_both_poles() {
        for p in [-90., -89., -10., -0.00001, 0., 5., 9.99999, 10., 89., 90.] {
            let (axis, margin, s) = orientation_certificate(0., p, 45., 10.).unwrap();
            assert!(s <= 1. && s + 1e-15 >= libm::sin(p.to_radians()).abs());
            let actual = [0., libm::sin(p.to_radians()), libm::cos(p.to_radians())];
            let dot = axis
                .into_iter()
                .zip(actual)
                .map(|(a, b)| a * b)
                .sum::<f64>();
            assert!(dot + 1e-14 >= libm::cos(margin));
            if p.abs() == 90. {
                assert_eq!(s, 1.);
            }
        }
        let (_, _, s) = orientation_certificate(0., 0., 45., 10.).unwrap();
        assert!(s >= libm::sin(10f64.to_radians()) && s < 0.174);
    }
    #[test]
    fn arbitrary_roll_pinhole_displacements_are_bounded_and_false_horizontal_certificate_fails() {
        let mut state = 3056019349u64;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (state >> 11) as f64 / (1u64 << 53) as f64
        };
        let mut cases = 0;
        for limit in [0f64, 5., 10., 20., 30., 60., 90.] {
            let smax = libm::sin(limit.to_radians());
            let p = Projection {
                focal_pixels: 385.59996182037094,
                diagonal_tangent: 1.7115357351239278,
                max_abs_forward_y: smax,
                ..Projection::default()
            };
            let general = Projection {
                max_abs_forward_y: 1.,
                ..p
            };
            for _ in 0..5000 {
                let s = libm::sin(((next() * 2. - 1.) * limit).to_radians());
                let rotation = (next() * 2. - 1.) * std::f64::consts::PI;
                let n = [
                    libm::cos(rotation) * libm::sqrt(1. - s * s),
                    libm::sin(rotation) * libm::sqrt(1. - s * s),
                    s,
                ];
                let theta = (next() * 2. - 1.) * std::f64::consts::PI;
                let r = p.diagonal_tangent * libm::sqrt(next());
                let radius = 10. + 1990. * next();
                let distance_fraction = 0.2 + 0.8 * next();
                let z = radius / libm::sqrt(1. + r * r);
                let point = [r * libm::cos(theta) * z, r * libm::sin(theta) * z, z];
                let error = radius * (0.0001 + 1.9999 * next());
                let e = (2. * next() - 1.) * error;
                let displaced: [f64; 3] = std::array::from_fn(|i| point[i] + e * n[i]);
                let proxy_norm = libm::sqrt(displaced.into_iter().map(|v| v * v).sum::<f64>());
                let d = proxy_norm * distance_fraction;
                let bound = p.displacement_error_pixels(d, error);
                if !bound.is_finite() {
                    cases += 1;
                    continue;
                }
                assert!(
                    displaced[2] > 0.,
                    "positive bound proves proxy positive depth"
                );
                let du = displaced[0] / displaced[2] - point[0] / point[2];
                let dv = displaced[1] / displaced[2] - point[1] / point[2];
                let actual = p.focal_pixels * libm::sqrt(du * du + dv * dv);
                assert!(
                    actual <= bound + bound.max(1.) * 2e-13,
                    "{limit}/{actual}/{bound}"
                );
                assert!(
                    bound <= general.displacement_error_pixels(d, error) + bound.max(1.) * 2e-13
                );
                cases += 1;
            }
        }
        assert_eq!(cases, 35000);
        let d_tangent = 1.7115357351239278f64;
        let s = libm::sin(10f64.to_radians());
        let e = 0.1;
        let q = [-d_tangent, 0., 1.];
        let proxy = [q[0] - e * libm::sqrt(1. - s * s), 0., 1. - e * s];
        let proxy_distance = libm::sqrt(proxy.into_iter().map(|v| v * v).sum::<f64>());
        let k = libm::sqrt(1. + d_tangent * d_tangent);
        let numerator = libm::sqrt(1. - s * s) + d_tangent * s;
        let actual = 385.59996182037094 * (proxy[0] / proxy[2] - q[0]).abs();
        let stronger = 385.59996182037094 * e * numerator / (proxy_distance / k - e * s);
        assert!(
            actual > stronger,
            "the -ES bound is unsafe with only a proxy endpoint AABB"
        );
        let robust = Projection {
            focal_pixels: 385.59996182037094,
            diagonal_tangent: d_tangent,
            max_abs_forward_y: s,
            ..Projection::default()
        }
        .displacement_error_pixels(proxy_distance, e);
        assert!(actual <= robust);

        let p = Projection {
            focal_pixels: 385.59996182037094,
            diagonal_tangent: 1.7115357351239278,
            max_abs_forward_y: 0.,
            ..Projection::default()
        };
        let k = libm::sqrt(1. + p.diagonal_tangent * p.diagonal_tangent);
        let e = 0.1;
        let s = 0.5;
        let x = -p.diagonal_tangent;
        let moved_x = x - e * libm::sqrt(1. - s * s);
        let moved_z = 1. - e * s;
        let actual = p.focal_pixels * (moved_x / moved_z - x).abs();
        assert!(actual > p.displacement_error_pixels(k, e));
    }
}
