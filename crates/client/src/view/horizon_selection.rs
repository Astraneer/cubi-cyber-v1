//! Worker-only SSE selection over public height certificates. Unknown error is
//! kept explicit. Capacity can limit requested quality, never protected geometry.
use super::*;

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
        radius += libm::sqrt(radius_squared);
        let distance = libm::sqrt(delta.iter().map(|v| v * v).sum::<f64>());
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
        actual + 1e-12 >= allowed
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
        let k = libm::sqrt(1. + self.diagonal_tangent * self.diagonal_tangent);
        let denominator = libm::sqrt(distance_squared) / k - error;
        if denominator <= 0. {
            f64::INFINITY
        } else {
            self.focal_pixels * error * k / denominator
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
pub(super) fn seam_error_q1024(heights: &[i32; 289]) -> u64 {
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
    while let Some(key) = pending.pop() {
        if !tiles.iter().any(|tile| tile.key == key) {
            continue;
        }
        for index in 0..tiles.len() {
            if work.adjacency_tests + work.seam_tests >= MAX_PLAN_PAIRS {
                return Err(Error::Capacity);
            }
            work.adjacency_tests += 1;
            let coarse = tiles[index].key;
            if coarse.level > key.level + 1 && adjacent(key, coarse) {
                if tiles.len() + 3 > maximum {
                    return Err(Error::Capacity);
                }
                split(tiles, index);
                work.refinements += 1;
                work.base_nodes += 4;
                pending.push(key);
                pending.extend(children(coarse));
                break;
            }
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
    for z in center[1] - 2..center[1] + 2 {
        for x in center[0] - 2..center[0] + 2 {
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
    let near = [
        (task.camera[0] - task.near_radius) * 16,
        (task.camera[1] - task.near_radius) * 16,
        (task.camera[0] + task.near_radius + 1) * 16,
        (task.camera[1] + task.near_radius + 1) * 16,
    ];
    let mut work = PlanWork {
        base_nodes: 16,
        ..PlanWork::default()
    };
    // Protection and the whole near cutout are mandatory, regardless of SSE.
    loop {
        let mut action = None;
        for (i, tile) in tiles.iter().enumerate() {
            let b = tile.key.bounds().ok_or(Error::Plan)?;
            if contains(near, b) {
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
            if !projection.could_be_visible(*certificate) {
                continue;
            }
            let b = tile.key.bounds().ok_or(Error::Plan)?;
            let previously_split = task.previous.iter().any(|key| {
                key.space == tile.key.space
                    && key.level < tile.key.level
                    && key.bounds().is_some_and(|p| contains(b, p))
            });
            let threshold = projection.target_pixels
                + if previously_split {
                    -projection.hysteresis_pixels
                } else {
                    projection.hysteresis_pixels
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
            if !projection.could_be_visible(*certificate) {
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
                (center[0] + 2) * span,
                (center[1] + 2) * span,
            ],
        },
        work,
        quality,
        exact_leaves,
    })
}
fn set_seams(tiles: &mut [PlannedHeightTile], work: &mut PlanWork) -> Result<(), Error> {
    for i in 0..tiles.len() {
        for j in i + 1..tiles.len() {
            if work.adjacency_tests + work.seam_tests >= MAX_PLAN_PAIRS {
                return Err(Error::Capacity);
            }
            work.seam_tests += 1;
            let a = tiles[i].key;
            let b = tiles[j].key;
            if !adjacent(a, b) || a.level == b.level {
                continue;
            }
            if a.level.abs_diff(b.level) != 1 {
                return Err(Error::Plan);
            }
            let (fine, coarse) = if a.level < b.level { (i, b) } else { (j, a) };
            let f = tiles[fine].key.bounds().ok_or(Error::Plan)?;
            let c = coarse.bounds().ok_or(Error::Plan)?;
            tiles[fine].seams |= if f[0] == c[2] {
                1
            } else if f[2] == c[0] {
                2
            } else if f[1] == c[3] {
                4
            } else {
                8
            };
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn projection() -> Projection {
        Projection {
            eye_min: [0., 10000., 0.],
            eye_max: [0., 10000., 0.],
            focal_pixels: 100.,
            diagonal_tangent: 1.,
            target_pixels: 10.,
            hysteresis_pixels: 2.,
            view_direction: [0., -1., 0.],
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
        t.certificates.push(certificate(10.));
        assert!(
            has_root(&t.execute().unwrap()),
            "a coarse node remains coarse below12px"
        );
        let mut t = task();
        t.certificates.push(certificate(10.));
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
        t.certificates.push(certificate(14.));
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
        t.config.max_tiles = 32;
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
        assert!(result.plan.tiles.len() <= 32);
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
            256 * 256 - 16 * 16,
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
    fn offscreen_bounds_do_not_consume_refinement_but_viewport_margin_keeps_intersections() {
        let p = Projection {
            eye_min: [0.; 3],
            eye_max: [0.; 3],
            focal_pixels: 100.,
            diagonal_tangent: 0.5,
            target_pixels: 10.,
            hysteresis_pixels: 2.,
            view_direction: [0., 0., 1.],
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
            t.config = Config::default();
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
}
