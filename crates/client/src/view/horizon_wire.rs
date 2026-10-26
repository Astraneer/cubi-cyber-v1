//! Bounded browser-worker protocol: public height-plan hints and public voxels.
//! Decoder loops are bounded validation, never generator/mesher execution.
use super::*;
use render::lod_gpu::ProxyPayload;
const MAX_BYTES: usize = 50_000;
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
    out.extend_from_slice(b"CHJ1");
    match task {
        BackgroundTask::Plan(task) => {
            if task.refine.len() > 512 {
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
            w32(&mut out, task.space);
            for n in task.camera {
                w64(&mut out, n as u64);
            }
            w32(&mut out, task.near_radius as u32);
            w16(&mut out, task.refine.len() as u16);
            for key in &task.refine {
                key_write(&mut out, *key);
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
    }
    Ok(out)
}
fn decode_task(bytes: &[u8]) -> Result<BackgroundTask, WireError> {
    if bytes.len() > MAX_BYTES {
        return Err(WireError::Length);
    }
    let mut r = Reader { bytes, at: 0 };
    if r.take::<4>()? != *b"CHJ1" {
        return Err(WireError::Magic);
    }
    let task = match r.u8()? {
        1 => {
            let id = r.u64()?;
            let generation = r.u64()?;
            let max_tiles = r.u32()? as usize;
            let max_sources = r.u32()? as usize;
            let max_proxy_pages = r.u32()? as usize;
            let atlas_layers = r.u32()?;
            let horizon_level = r.u8()?;
            let space = r.u32()?;
            let camera = [r.u64()? as i64, r.u64()? as i64];
            let near_radius = i64::from(r.u32()?);
            let n = r.u16()? as usize;
            if id == 0
                || !(16..=512).contains(&max_tiles)
                || max_sources < max_tiles
                || max_sources > 4096
                || max_proxy_pages == 0
                || max_proxy_pages > 512
                || atlas_layers == 0
                || atlas_layers > 256
                || horizon_level > 10
                || near_radius > 128
                || n > 512
                || camera
                    .iter()
                    .any(|p| *p < i64::from(i32::MIN) / 16 || *p > i64::from(i32::MAX) / 16)
            {
                return Err(WireError::Invalid);
            }
            if r.bytes.len().saturating_sub(r.at) != n * 13 {
                return Err(WireError::Length);
            }
            let mut refine = Vec::with_capacity(n);
            for _ in 0..n {
                refine.push(key_read(&mut r)?);
            }
            BackgroundTask::Plan(PlanTask {
                id,
                generation,
                config: Config {
                    max_tiles,
                    max_sources,
                    max_proxy_pages,
                    atlas_layers,
                    horizon_level,
                },
                space,
                camera,
                refine,
                near_radius,
            })
        }
        2 => {
            let stamp = stamp_read(&mut r)?;
            let revision = r.u64()?;
            let atlas_layers = r.u32()?;
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
            })
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
    };
    if cancel() {
        return Err(WireError::Cancelled);
    }
    encode_result(&result)
}
fn encode_result(result: &BackgroundResult) -> Result<Vec<u8>, WireError> {
    let mut out = Vec::with_capacity(10_000);
    out.extend_from_slice(b"CHR1");
    match result {
        BackgroundResult::Plan(Err(_)) => out.extend_from_slice(&[1, 1]),
        BackgroundResult::Proxy(Err(_)) => out.extend_from_slice(&[2, 1]),
        BackgroundResult::Plan(Ok(result)) => {
            if result.plan.tiles.len() > 512 {
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
    if r.take::<4>()? != *b"CHR1" {
        return Err(WireError::Magic);
    }
    let kind = r.u8()?;
    let status = r.u8()?;
    let result = match (kind, status) {
        (1, 1) => BackgroundResult::Plan(Err(Error::Plan)),
        (2, 1) => BackgroundResult::Proxy(Err(ProxyError::InvalidMaterial)),
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
            let visited_nodes = r.u32()?;
            let bounds = [
                r.u64()? as i64,
                r.u64()? as i64,
                r.u64()? as i64,
                r.u64()? as i64,
            ];
            let n = r.u16()? as usize;
            if id == 0
                || n > 512
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
            BackgroundResult::Plan(Ok(Planned {
                id,
                generation,
                plan: HeightPlan {
                    tiles,
                    visited_nodes,
                    outer_bounds_pages: bounds,
                },
                work,
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
