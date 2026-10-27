//! Bounded browser-worker protocol: public height-plan hints and public voxels.
//! Decoder loops are bounded validation, never generator/mesher execution.
use super::*;
use render::lod_gpu::ProxyPayload;
pub const MAX_BACKGROUND_BYTES: usize = 450_000;
// CHR4 Proxy envelope: magic/type/status6 +stamp32 +revision8 +length4.
const _: () = assert!(render::lod_gpu::MAX_PUBLIC_BYTES + 50 <= MAX_BACKGROUND_BYTES);
const MAX_BYTES: usize = MAX_BACKGROUND_BYTES;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    Length,
    Magic,
    Invalid,
    Cancelled,
}
impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for WireError {}
pub fn encode_background_task(task: &BackgroundTask) -> Result<Vec<u8>, WireError> {
    let mut out = Vec::with_capacity(30_000);
    out.extend_from_slice(b"CHJ5");
    match task {
        BackgroundTask::Plan(task) => {
            if task.refine.len() > net::height::MAX_TILES
                || task.certificates.len() > net::height::MAX_SOURCES
                || task.previous.len() > net::height::MAX_TILES
            {
                return Err(WireError::Length);
            }
            out.push(1);
            w64(&mut out, task.id);
            w64(&mut out, task.generation);
            for n in [
                task.config.max_tiles,
                task.config.max_sources,
                task.config.max_proxy_pages,
            ] {
                w32(&mut out, n as u32);
            }
            w32(&mut out, task.config.atlas_layers);
            out.push(task.config.horizon_level);
            w64(&mut out, task.config.screen_error_pixels.to_bits());
            w64(&mut out, task.config.hysteresis_pixels.to_bits());
            out.push(u8::from(task.config.near_volume.is_some()));
            if let Some(volume) = task.config.near_volume {
                for p in volume {
                    for c in p {
                        w32(&mut out, c as u32);
                    }
                }
            }
            w32(&mut out, task.space);
            for n in task.camera {
                w64(&mut out, n as u64);
            }
            w32(&mut out, task.near_radius as u32);
            w16(&mut out, task.refine.len() as u16);
            for key in &task.refine {
                key_write(&mut out, *key);
            }
            out.push(u8::from(task.projection.is_some()));
            if let Some(p) = task.projection {
                for n in p
                    .eye_min
                    .into_iter()
                    .chain(p.eye_max)
                    .chain([
                        p.focal_pixels,
                        p.diagonal_tangent,
                        p.target_pixels,
                        p.hysteresis_pixels,
                    ])
                    .chain(p.view_direction)
                    .chain([p.view_cosine, p.max_abs_forward_y])
                {
                    w64(&mut out, n.to_bits());
                }
            }
            w16(&mut out, task.certificates.len() as u16);
            for c in &task.certificates {
                key_write(&mut out, c.key);
                w64(&mut out, c.bound.max_error_q1024);
                w32(&mut out, c.bound.min_height as u32);
                w32(&mut out, c.bound.max_height as u32);
                w64(&mut out, c.seam_error_q1024);
                out.push(u8::from(c.exact_replacement));
            }
            w16(&mut out, task.previous.len() as u16);
            for key in &task.previous {
                key_write(&mut out, wire_key(*key));
            }
        }
        BackgroundTask::Proxy(task) => {
            if task.input.palette.len() > 4096 || task.input.indices.len() > 6144 {
                return Err(WireError::Length);
            }
            out.push(2);
            stamp_write(&mut out, task.stamp);
            w64(&mut out, task.revision);
            w32(&mut out, task.atlas_layers);
            out.push(u8::from(task.separate_water));
            out.extend_from_slice(&task.input.known);
            out.extend_from_slice(&task.input.occupied);
            out.push(task.input.index_bits);
            w16(&mut out, task.input.palette.len() as u16);
            w16(&mut out, task.input.indices.len() as u16);
            for surface in &task.input.palette {
                w16(&mut out, surface.atlas_layer);
                w32(&mut out, surface.tint);
                out.push(match surface.kind {
                    net::PublicSurfaceKind::Opaque => 0,
                    net::PublicSurfaceKind::Cutout => 1,
                    net::PublicSurfaceKind::Water => 2,
                });
            }
            out.extend_from_slice(&task.input.indices);
        }
        BackgroundTask::Water { context, task } => {
            out.push(3);
            water_context_write(&mut out, *context);
            out.extend_from_slice(&task.encode().map_err(|_| WireError::Invalid)?);
        }
    }
    if out.len() > MAX_BYTES {
        return Err(WireError::Length);
    }
    Ok(out)
}
fn decode_task(bytes: &[u8]) -> Result<BackgroundTask, WireError> {
    if bytes.len() > MAX_BYTES {
        return Err(WireError::Length);
    }
    let mut r = Reader { bytes, at: 0 };
    let version = match r.take::<4>()? {
        magic if magic == *b"CHJ4" => 4,
        magic if magic == *b"CHJ5" => 5,
        _ => return Err(WireError::Magic),
    };
    let task = match r.u8()? {
        1 => {
            let id = r.u64()?;
            let generation = r.u64()?;
            let max_tiles = r.u32()? as usize;
            let max_sources = r.u32()? as usize;
            let max_proxy_pages = r.u32()? as usize;
            let atlas_layers = r.u32()?;
            let horizon_level = r.u8()?;
            let screen_error_pixels = f64::from_bits(r.u64()?);
            let hysteresis_pixels = f64::from_bits(r.u64()?);
            let near_volume = match r.u8()? {
                0 => None,
                1 => {
                    let mut volume = [[0; 3]; 2];
                    for p in &mut volume {
                        for c in p {
                            *c = r.u32()? as i32;
                        }
                    }
                    Some(volume)
                }
                _ => return Err(WireError::Invalid),
            };
            if !valid_near_volume(near_volume) {
                return Err(WireError::Invalid);
            }
            let space = r.u32()?;
            let camera = [r.u64()? as i64, r.u64()? as i64];
            let near_radius = i64::from(r.u32()?);
            let n = r.u16()? as usize;
            if id == 0
                || !(16..=net::height::MAX_TILES).contains(&max_tiles)
                || max_sources < max_tiles
                || max_sources > net::height::MAX_SOURCES
                || max_proxy_pages == 0
                || max_proxy_pages > 512
                || atlas_layers == 0
                || atlas_layers > 256
                || horizon_level > 10
                || !screen_error_pixels.is_finite()
                || screen_error_pixels <= 0.
                || !hysteresis_pixels.is_finite()
                || hysteresis_pixels < 0.
                || hysteresis_pixels >= screen_error_pixels
                || near_radius > 128
                || n > net::height::MAX_TILES
                || camera
                    .iter()
                    .any(|p| *p < i64::from(i32::MIN) / 16 || *p > i64::from(i32::MAX) / 16)
            {
                return Err(WireError::Invalid);
            }
            if r.bytes.len().saturating_sub(r.at) < n * 13 + 5 {
                return Err(WireError::Length);
            }
            let mut refine = Vec::with_capacity(n);
            for _ in 0..n {
                refine.push(key_read(&mut r)?);
            }
            let projection = match r.u8()? {
                0 => None,
                1 => {
                    let mut values = [0.; 14];
                    for value in &mut values {
                        *value = f64::from_bits(r.u64()?);
                    }
                    let p = Projection {
                        eye_min: values[..3].try_into().unwrap(),
                        eye_max: values[3..6].try_into().unwrap(),
                        focal_pixels: values[6],
                        diagonal_tangent: values[7],
                        target_pixels: values[8],
                        hysteresis_pixels: values[9],
                        view_direction: values[10..13].try_into().unwrap(),
                        view_cosine: values[13],
                        max_abs_forward_y: if version == 4 {
                            1.
                        } else {
                            f64::from_bits(r.u64()?)
                        },
                    };
                    p.validate().map_err(|_| WireError::Invalid)?;
                    Some(p)
                }
                _ => return Err(WireError::Invalid),
            };
            let count = r.u16()? as usize;
            if count > net::height::MAX_SOURCES
                || r.bytes.len().saturating_sub(r.at) < count * 38 + 2
            {
                return Err(WireError::Length);
            }
            let mut certificates = Vec::with_capacity(count);
            for _ in 0..count {
                let key = key_read(&mut r)?;
                let bound = net::height::HeightErrorBound {
                    max_error_q1024: r.u64()?,
                    min_height: r.u32()? as i32,
                    max_height: r.u32()? as i32,
                };
                bound.validate().map_err(|_| WireError::Invalid)?;
                let seam_error_q1024 = r.u64()?;
                let exact_replacement = match r.u8()? {
                    0 => false,
                    1 if key.level == 0 => true,
                    _ => return Err(WireError::Invalid),
                };
                if seam_error_q1024 > (u64::from(u32::MAX) + 1) * 1024 {
                    return Err(WireError::Invalid);
                }
                certificates.push(Certificate {
                    key,
                    bound,
                    seam_error_q1024,
                    exact_replacement,
                });
            }
            if certificates
                .windows(2)
                .any(|pair| pair[0].key >= pair[1].key)
            {
                return Err(WireError::Invalid);
            }
            let count = r.u16()? as usize;
            if count > net::height::MAX_TILES || r.bytes.len().saturating_sub(r.at) != count * 13 {
                return Err(WireError::Length);
            }
            let mut previous = Vec::with_capacity(count);
            for _ in 0..count {
                previous.push(render_key(key_read(&mut r)?));
            }
            BackgroundTask::Plan(Box::new(PlanTask {
                id,
                generation,
                config: Config {
                    max_tiles,
                    max_sources,
                    max_proxy_pages,
                    atlas_layers,
                    horizon_level,
                    screen_error_pixels,
                    hysteresis_pixels,
                    near_volume,
                },
                space,
                camera,
                refine,
                near_radius,
                projection,
                certificates,
                previous,
            }))
        }
        2 => {
            let stamp = stamp_read(&mut r)?;
            let revision = r.u64()?;
            let atlas_layers = r.u32()?;
            let separate_water = match r.u8()? {
                0 => false,
                1 => true,
                _ => return Err(WireError::Invalid),
            };
            let known = r.take::<512>()?;
            let occupied = r.take::<512>()?;
            let index_bits = r.u8()?;
            let n = r.u16()? as usize;
            let len = r.u16()? as usize;
            if revision == 0
                || atlas_layers == 0
                || atlas_layers > 256
                || n > 4096
                || len > 6144
                || r.bytes.len().saturating_sub(r.at) != n * 7 + len
            {
                return Err(WireError::Invalid);
            }
            let mut palette = Vec::with_capacity(n);
            for _ in 0..n {
                let atlas_layer = r.u16()?;
                let tint = r.u32()?;
                let kind = match r.u8()? {
                    0 => net::PublicSurfaceKind::Opaque,
                    1 => net::PublicSurfaceKind::Cutout,
                    2 => net::PublicSurfaceKind::Water,
                    _ => return Err(WireError::Invalid),
                };
                if u32::from(atlas_layer) >= atlas_layers {
                    return Err(WireError::Invalid);
                }
                palette.push(net::PublicSurface {
                    atlas_layer,
                    tint,
                    kind,
                });
            }
            let indices = r.bytes[r.at..].to_vec();
            r.at = r.bytes.len();
            let input = Arc::new(net::PublicGeometry {
                page: stamp.page,
                source_version: stamp.source,
                view_version: stamp.view,
                known,
                occupied,
                palette,
                index_bits,
                indices,
            });
            BackgroundTask::Proxy(ProxyTask {
                stamp,
                revision,
                atlas_layers,
                input,
                separate_water,
            })
        }
        3 => {
            let context = water_context_read(&mut r)?;
            let task = FarWaterTask::decode(&r.bytes[r.at..]).map_err(|_| WireError::Invalid)?;
            r.at = r.bytes.len();
            if task.stamp() != context.stamp {
                return Err(WireError::Invalid);
            }
            BackgroundTask::Water { context, task }
        }
        _ => return Err(WireError::Invalid),
    };
    if r.at != bytes.len() {
        return Err(WireError::Length);
    }
    Ok(task)
}
pub fn execute_background_request(
    bytes: &[u8],
    cancel: impl Fn() -> bool,
) -> Result<Vec<u8>, WireError> {
    if cancel() {
        return Err(WireError::Cancelled);
    }
    let task = decode_task(bytes)?;
    let result = match task {
        BackgroundTask::Plan(task) => BackgroundResult::Plan(task.execute()),
        BackgroundTask::Proxy(task) => BackgroundResult::Proxy(task.execute()),
        BackgroundTask::Water { context, task } => BackgroundResult::Water {
            context,
            result: task.execute(&cancel),
        },
    };
    if cancel() {
        return Err(WireError::Cancelled);
    }
    encode_result(&result)
}
fn encode_result(result: &BackgroundResult) -> Result<Vec<u8>, WireError> {
    let mut out = Vec::with_capacity(10_000);
    out.extend_from_slice(b"CHR4");
    match result {
        BackgroundResult::Plan(Err(_)) => out.extend_from_slice(&[1, 1]),
        BackgroundResult::Proxy(Err(_)) => out.extend_from_slice(&[2, 1]),
        BackgroundResult::Plan(Ok(result)) => {
            if result.plan.tiles.len() > net::height::MAX_TILES {
                return Err(WireError::Length);
            }
            out.extend_from_slice(&[1, 0]);
            w64(&mut out, result.id);
            w64(&mut out, result.generation);
            for n in [
                result.work.base_nodes,
                result.work.refinements,
                result.work.near_tiles_removed,
                result.work.adjacency_tests,
                result.work.seam_tests,
            ] {
                w64(&mut out, n);
            }
            w64(&mut out, result.quality.missing_certificates);
            out.push(u8::from(result.quality.capacity_limited));
            w64(&mut out, result.quality.leaf_limited);
            w64(&mut out, result.quality.maximum_error_pixels.to_bits());
            w64(&mut out, result.quality.sse_refinements);
            w16(&mut out, result.exact_leaves.len() as u16);
            for key in &result.exact_leaves {
                key_write(&mut out, *key);
            }
            w32(&mut out, result.plan.visited_nodes);
            for n in result.plan.outer_bounds_pages {
                w64(&mut out, n as u64);
            }
            w16(&mut out, result.plan.tiles.len() as u16);
            for tile in &result.plan.tiles {
                key_write(&mut out, wire_key(tile.key));
                w32(&mut out, tile.seams);
            }
        }
        BackgroundResult::Proxy(Ok(result)) => {
            out.extend_from_slice(&[2, 0]);
            stamp_write(&mut out, result.stamp);
            w64(&mut out, result.built.revision);
            let payload = result.built.payload.encode_public();
            w32(&mut out, payload.len() as u32);
            out.extend_from_slice(&payload);
        }
        BackgroundResult::Water { context, result } => {
            out.extend_from_slice(&[3, u8::from(result.is_err())]);
            water_context_write(&mut out, *context);
            if let Ok(built) = result {
                out.extend_from_slice(&built.encode().map_err(|_| WireError::Invalid)?);
            }
        }
    }
    if out.len() > MAX_BYTES {
        return Err(WireError::Length);
    }
    Ok(out)
}
pub fn decode_background_result(bytes: &[u8]) -> Result<BackgroundResult, WireError> {
    if bytes.len() > MAX_BYTES {
        return Err(WireError::Length);
    }
    let mut r = Reader { bytes, at: 0 };
    if r.take::<4>()? != *b"CHR4" {
        return Err(WireError::Magic);
    }
    let kind = r.u8()?;
    let status = r.u8()?;
    let result = match (kind, status) {
        (1, 1) => BackgroundResult::Plan(Err(Error::Plan)),
        (2, 1) => BackgroundResult::Proxy(Err(ProxyError::InvalidMaterial)),
        (3, 0) | (3, 1) => {
            let context = water_context_read(&mut r)?;
            let result = if status == 0 {
                let built =
                    BuiltFarWater::decode(&r.bytes[r.at..]).map_err(|_| WireError::Invalid)?;
                r.at = r.bytes.len();
                if built.stamp != context.stamp {
                    return Err(WireError::Invalid);
                }
                Ok(built)
            } else {
                Err(far_water_worker::Error::Mesh)
            };
            BackgroundResult::Water { context, result }
        }
        (1, 0) => {
            let id = r.u64()?;
            let generation = r.u64()?;
            let work = PlanWork {
                base_nodes: r.u64()?,
                refinements: r.u64()?,
                near_tiles_removed: r.u64()?,
                adjacency_tests: r.u64()?,
                seam_tests: r.u64()?,
            };
            if work.base_nodes > 8192
                || work.refinements > 8192
                || work.near_tiles_removed > 32768
                || work.adjacency_tests.saturating_add(work.seam_tests) > MAX_PLAN_PAIRS
            {
                return Err(WireError::Invalid);
            }
            let missing_certificates = r.u64()?;
            let capacity_limited = match r.u8()? {
                0 => false,
                1 => true,
                _ => return Err(WireError::Invalid),
            };
            let leaf_limited = r.u64()?;
            let maximum_error_pixels = f64::from_bits(r.u64()?);
            let sse_refinements = r.u64()?;
            if missing_certificates > net::height::MAX_TILES as u64
                || leaf_limited > net::height::MAX_TILES as u64
                || maximum_error_pixels.is_nan()
                || maximum_error_pixels < 0.
                || sse_refinements > 8192
            {
                return Err(WireError::Invalid);
            }
            let quality = SelectionQuality {
                missing_certificates,
                capacity_limited,
                leaf_limited,
                maximum_error_pixels,
                sse_refinements,
            };
            let exact_count = r.u16()? as usize;
            if exact_count > net::height::MAX_TILES
                || r.bytes.len().saturating_sub(r.at) < exact_count * 13
            {
                return Err(WireError::Length);
            }
            let mut exact_leaves = Vec::with_capacity(exact_count);
            for _ in 0..exact_count {
                let key = key_read(&mut r)?;
                if key.level != 0 {
                    return Err(WireError::Invalid);
                }
                exact_leaves.push(key);
            }
            if exact_leaves.windows(2).any(|p| p[0] >= p[1]) {
                return Err(WireError::Invalid);
            }
            let visited_nodes = r.u32()?;
            let bounds = [
                r.u64()? as i64,
                r.u64()? as i64,
                r.u64()? as i64,
                r.u64()? as i64,
            ];
            let n = r.u16()? as usize;
            if id == 0
                || n > net::height::MAX_TILES
                || bounds[0] >= bounds[2]
                || bounds[1] >= bounds[3]
                || r.bytes.len().saturating_sub(r.at) != n * 17
            {
                return Err(WireError::Invalid);
            }
            let mut tiles = Vec::with_capacity(n);
            for _ in 0..n {
                let key = key_read(&mut r)?;
                let seams = r.u32()?;
                if seams & !15 != 0 {
                    return Err(WireError::Invalid);
                }
                tiles.push(PlannedHeightTile {
                    key: render_key(key),
                    seams,
                });
            }
            if tiles.windows(2).any(|p| p[0].key >= p[1].key) {
                return Err(WireError::Invalid);
            }
            if exact_leaves
                .iter()
                .any(|key| !tiles.iter().any(|tile| wire_key(tile.key) == *key))
            {
                return Err(WireError::Invalid);
            }
            BackgroundResult::Plan(Ok(Planned {
                id,
                generation,
                plan: HeightPlan {
                    tiles,
                    visited_nodes,
                    outer_bounds_pages: bounds,
                },
                work,
                quality,
                exact_leaves,
            }))
        }
        (2, 0) => {
            let stamp = stamp_read(&mut r)?;
            let revision = r.u64()?;
            let n = r.u32()? as usize;
            if revision == 0 || n != r.bytes.len().saturating_sub(r.at) {
                return Err(WireError::Invalid);
            }
            let payload = ProxyPayload::decode_public(&r.bytes[r.at..], 256)
                .map_err(|_| WireError::Invalid)?;
            r.at = r.bytes.len();
            BackgroundResult::Proxy(Ok(ProxyBuilt {
                stamp,
                built: BuiltProxy {
                    key: lod_key(stamp.page),
                    revision,
                    payload: Arc::new(payload),
                },
            }))
        }
        _ => return Err(WireError::Invalid),
    };
    if r.at != bytes.len() {
        return Err(WireError::Length);
    }
    Ok(result)
}
fn key_write(out: &mut Vec<u8>, key: HeightKey) {
    w32(out, key.space);
    out.push(key.level);
    w32(out, key.x as u32);
    w32(out, key.z as u32);
}
fn water_context_write(out: &mut Vec<u8>, context: far_water_source::Context) {
    w64(out, context.epoch);
    stamp_write(
        out,
        GeometryStamp {
            page: context.stamp.page,
            source: context.stamp.geometry_source,
            view: context.stamp.geometry_view,
        },
    );
    w64(out, context.stamp.column_source);
    w64(out, context.stamp.water_view);
}
fn water_context_read(r: &mut Reader<'_>) -> Result<far_water_source::Context, WireError> {
    let epoch = r.u64()?;
    let geometry = stamp_read(r)?;
    let column_source = r.u64()?;
    let water_view = r.u64()?;
    if epoch == 0 || column_source == 0 || water_view == 0 {
        return Err(WireError::Invalid);
    }
    Ok(far_water_source::Context {
        epoch,
        stamp: far_water_worker::Stamp {
            page: geometry.page,
            geometry_source: geometry.source,
            geometry_view: geometry.view,
            column_source,
            water_view,
        },
    })
}
fn key_read(r: &mut Reader<'_>) -> Result<HeightKey, WireError> {
    let key = HeightKey {
        space: r.u32()?,
        level: r.u8()?,
        x: r.u32()? as i32,
        z: r.u32()? as i32,
    };
    key.validate().map_err(|_| WireError::Invalid)?;
    Ok(key)
}
fn stamp_write(out: &mut Vec<u8>, stamp: GeometryStamp) {
    w32(out, stamp.page.space.0);
    w32(out, stamp.page.x as u32);
    w32(out, stamp.page.y as u32);
    w32(out, stamp.page.z as u32);
    w64(out, stamp.source.0);
    w64(out, stamp.view);
}
fn stamp_read(r: &mut Reader<'_>) -> Result<GeometryStamp, WireError> {
    let page = PageId {
        space: SpaceId(r.u32()?),
        x: r.u32()? as i32,
        y: r.u32()? as i32,
        z: r.u32()? as i32,
    };
    let source = core_types::Version(r.u64()?);
    let view = r.u64()?;
    if source.0 == 0 || view == 0 {
        return Err(WireError::Invalid);
    }
    Ok(GeometryStamp { page, source, view })
}
fn w16(out: &mut Vec<u8>, n: u16) {
    out.extend_from_slice(&n.to_le_bytes());
}
fn w32(out: &mut Vec<u8>, n: u32) {
    out.extend_from_slice(&n.to_le_bytes());
}
fn w64(out: &mut Vec<u8>, n: u64) {
    out.extend_from_slice(&n.to_le_bytes());
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        let end = self.at.checked_add(N).ok_or(WireError::Length)?;
        let out = self
            .bytes
            .get(self.at..end)
            .ok_or(WireError::Length)?
            .try_into()
            .map_err(|_| WireError::Length)?;
        self.at = end;
        Ok(out)
    }
    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16, WireError> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_le_bytes(self.take()?))
    }
}

#[cfg(test)]
mod vertical_wire_tests {
    use super::*;
    #[test]
    fn old_projection_defaults_s1_new_wire_roundtrips_and_invalid_s_rejects() {
        let fixture = BackgroundTask::Plan(Box::new(PlanTask {
            id: 1,
            generation: 1,
            config: Config::default(),
            space: 0,
            camera: [0, 0],
            refine: vec![],
            near_radius: 2,
            projection: Some(Projection::default()),
            certificates: vec![],
            previous: vec![],
        }));
        let mut bytes = encode_background_task(&fixture).unwrap();
        let offset = bytes.len() - 12; // final S then two empty counts
        bytes.drain(offset..offset + 8);
        bytes[3] = b'4';
        assert_eq!(&bytes[..4], b"CHJ4");
        let mut task = decode_task(&bytes).unwrap();
        let BackgroundTask::Plan(p) = &mut task else {
            panic!("plan")
        };
        assert_eq!(p.projection.unwrap().max_abs_forward_y, 1.);
        p.projection.as_mut().unwrap().max_abs_forward_y = libm::sin(10f64.to_radians());
        let encoded = encode_background_task(&task).unwrap();
        assert_eq!(&encoded[..4], b"CHJ5");
        assert_eq!(encoded.len(), bytes.len() + 8);
        let BackgroundTask::Plan(decoded) = decode_task(&encoded).unwrap() else {
            panic!("plan")
        };
        assert_eq!(
            decoded.projection.unwrap().max_abs_forward_y,
            libm::sin(10f64.to_radians())
        );
        let mut bad = encoded.clone();
        bad[offset..offset + 8].copy_from_slice(&1.0001f64.to_bits().to_le_bytes());
        assert!(decode_task(&bad).is_err());
        let mut bad = encoded;
        bad[offset..offset + 8].copy_from_slice(&f64::NAN.to_bits().to_le_bytes());
        assert!(decode_task(&bad).is_err());
    }
}

#[cfg(test)]
mod vertical_public_fixture_tests {
    use super::*;
    #[test]
    fn actual_public_plan_and_height_converge_without_new_exact_columns() {
        let bytes = include_bytes!("../../tests/fixtures/horizon_vertical/public-plan.chj4");
        let height_bytes =
            include_bytes!("../../tests/fixtures/horizon_vertical/public-height-L2-1--2.hti2");
        assert_eq!(bytes.len(), 15347);
        assert_eq!(height_bytes.len(), 2804);
        let BackgroundTask::Plan(old) = decode_task(bytes).unwrap() else {
            panic!("plan")
        };
        assert_eq!(old.projection.unwrap().max_abs_forward_y, 1.);
        assert_eq!((old.id, old.generation), (313, 348));
        assert_eq!(
            (
                old.config.max_tiles,
                old.config.max_sources,
                old.config.max_proxy_pages
            ),
            (4096, 8192, 512)
        );
        assert_eq!(
            (old.config.screen_error_pixels, old.config.hysteresis_pixels),
            (32., 8.)
        );
        let original_exact = old
            .certificates
            .iter()
            .filter(|c| c.exact_replacement)
            .map(|c| c.key)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(original_exact.len(), 64);
        let legacy = old.execute().unwrap();
        assert_eq!(legacy.quality.leaf_limited, 22);
        assert_eq!(legacy.quality.missing_certificates, 0);
        assert!((legacy.quality.maximum_error_pixels - 50.28962549113061).abs() < 1e-10);
        assert_eq!(legacy.exact_leaves.len(), 22);
        assert_eq!(legacy.plan.tiles.len(), 265);
        let original_selected = legacy
            .plan
            .tiles
            .iter()
            .map(|t| wire_key(t.key))
            .filter(|key| original_exact.contains(key))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            original_selected, original_exact,
            "all64 original exact manifests belonged to the observed512-page interest"
        );
        let tile = net::height::decode(height_bytes).unwrap();
        assert_eq!(
            tile.key,
            net::height::HeightKey {
                space: 0,
                level: 2,
                x: 1,
                z: -2
            }
        );
        assert_eq!(tile.source, 1);
        assert!(matches!(tile.replacement, net::height::Replacement::None));
        assert_eq!(tile.protected, [0; 32]);
        let added = Certificate {
            key: tile.key,
            bound: tile.error.unwrap(),
            seam_error_q1024: seam_error_q1024(&tile.heights),
            exact_replacement: false,
        };
        assert_eq!(added.seam_error_q1024, 512);
        assert_eq!(
            (
                added.bound.max_error_q1024,
                added.bound.min_height,
                added.bound.max_height
            ),
            (1024, 70, 80)
        );
        for yaw_step in [45., 15.] {
            let BackgroundTask::Plan(mut plan) = decode_task(bytes).unwrap() else {
                panic!("plan")
            };
            let prior = plan.certificates.clone();
            assert!(!prior.iter().any(|c| c.key == added.key));
            plan.certificates.push(added);
            plan.certificates.sort_unstable_by_key(|c| c.key);
            for old in &prior {
                let current = plan.certificates.iter().find(|c| c.key == old.key).unwrap();
                assert_eq!(current.bound, old.bound);
                assert_eq!(current.seam_error_q1024, old.seam_error_q1024);
                assert_eq!(current.exact_replacement, old.exact_replacement);
            }
            let (axis, margin, s) = orientation_certificate(0., 0., yaw_step, 10.).unwrap();
            let p = plan.projection.as_mut().unwrap();
            assert_eq!(p.eye_min, [0., 80., 0.]);
            assert_eq!(p.eye_max, [16., 96., 16.]);
            p.view_direction = axis;
            p.view_cosine =
                libm::cos((libm::atan(p.diagonal_tangent) + margin).min(std::f64::consts::PI));
            p.max_abs_forward_y = s;
            let result = plan.execute().unwrap();
            assert_eq!((result.id, result.generation), (313, 348));
            assert_eq!(result.quality.missing_certificates, 0);
            assert_eq!(result.quality.leaf_limited, 0);
            assert!(!result.quality.capacity_limited);
            assert!(result.quality.maximum_error_pixels <= 32.);
            assert!((result.quality.maximum_error_pixels - 31.78570467936542).abs() < 1e-8);
            assert!(result.exact_leaves.is_empty());
            let selected = result
                .plan
                .tiles
                .iter()
                .map(|t| wire_key(t.key))
                .filter(|k| original_exact.contains(k))
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(selected.len(), 62);
            assert!(selected.is_subset(&original_exact));
            // These unchanged manifests are a subset of the original64 whose
            // observed public interest was already512 pages (provenance.json).
            // No new exact keys/Geometry pages are requested. This does not
            // synthesize manifests or certify physical GPU publication.
            assert!(selected.len() <= 64);
        }
    }
}
