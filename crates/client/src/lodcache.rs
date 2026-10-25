//! Background-owned public LOD cache and bounded immutable proxy jobs.
//! Coarse horizon samples and selected protected geometry remain distinct. A
//! newer coarse announcement cannot erase a previously confirmed opening while
//! the replacement protected geometry is still in flight.
use core_types::{Cell, PageId, Version};
use render::{
    lod::{
        LodForest, LodKey, ObjectSelection, PublicPage, PublicVoxel, SurfaceKind, UpdateError,
        UpdateWork,
    },
    lod_gpu::{ProxyError, ProxyPayload},
};
use std::{collections::BTreeMap, sync::Arc};
#[derive(Clone, Copy, Debug)]
pub struct LodCacheConfig {
    pub max_sources: usize,
    pub max_protected_pages: usize,
    pub max_pending: usize,
    pub max_inflight: usize,
    pub max_ready: usize,
    pub max_forest_nodes: usize,
    pub max_level: u8,
    pub atlas_layers: u32,
}
impl Default for LodCacheConfig {
    fn default() -> Self {
        Self {
            max_sources: 4096,
            max_protected_pages: 64,
            max_pending: 64,
            max_inflight: 4,
            max_ready: 2048,
            max_forest_nodes: 16384,
            max_level: 10,
            atlas_layers: 186,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LodCacheError {
    InvalidConfig,
    Capacity,
    MalformedGeometry,
    InvalidPalette,
    ConflictingVersion,
    VersionExhausted,
    Forest(UpdateError),
}
#[derive(Clone, Copy, Debug, Default)]
pub struct CacheUpdate {
    pub accepted: bool,
    pub held_previous: bool,
    pub work: UpdateWork,
}
#[derive(Clone, Debug)]
enum SourceData {
    Coarse(Arc<[PublicVoxel; 64]>),
    Protected(Arc<PublicPage>),
}
impl SourceData {
    fn page(&self) -> PublicPage {
        match self {
            Self::Coarse(grid) => PublicPage::from_exposed(|[x, y, z]| {
                grid[usize::from(x / 4) + 4 * (usize::from(z / 4) + 4 * usize::from(y / 4))]
            }),
            Self::Protected(page) => (**page).clone(),
        }
    }
}
struct Source {
    announced: Version,
    published: Version,
    view: u64,
    coarse: Arc<[PublicVoxel; 64]>,
    has_summary: bool,
    data: SourceData,
}
pub struct GeometryInput<'a> {
    pub page: PageId,
    pub source: Version,
    pub view: u64,
    pub known: &'a [u8; 512],
    pub occupied: &'a [u8; 512],
    pub palette: &'a [PublicVoxel],
    pub index_bits: u8,
    pub indices: &'a [u8],
}
#[derive(Clone, Debug)]
pub struct LodBuildTask {
    pub key: LodKey,
    pub revision: u64,
    source: SourceData,
    atlas_layers: u32,
}
#[derive(Debug)]
pub struct BuiltProxy {
    pub key: LodKey,
    pub revision: u64,
    pub payload: Arc<ProxyPayload>,
}
impl LodBuildTask {
    pub fn execute(self) -> Result<BuiltProxy, ProxyError> {
        let page = self.source.page();
        let proxy = render::lod::MaskedRayProxy::from_public_page(
            &page,
            render::lod::protection_groups(&page, 4),
        );
        let payload = ProxyPayload::from_public(&proxy, self.atlas_layers)?;
        Ok(BuiltProxy {
            key: self.key,
            revision: self.revision,
            payload: Arc::new(payload),
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishDisposition {
    Published,
    Stale,
}
pub struct PublicLodCache {
    config: LodCacheConfig,
    forest: LodForest,
    sources: BTreeMap<PageId, Source>,
    publication: u64,
    pending: BTreeMap<LodKey, LodBuildTask>,
    inflight: BTreeMap<LodKey, u64>,
    ready: BTreeMap<LodKey, (u64, Arc<ProxyPayload>)>,
    protected: usize,
    stale_results: u64,
}
impl PublicLodCache {
    pub fn new(config: LodCacheConfig) -> Result<Self, LodCacheError> {
        if config.max_sources == 0
            || config.max_protected_pages > config.max_sources
            || config.max_pending == 0
            || config.max_inflight == 0
            || config.max_ready == 0
            || config.atlas_layers == 0
        {
            return Err(LodCacheError::InvalidConfig);
        }
        let forest = LodForest::with_limits(config.max_level, config.max_forest_nodes)
            .map_err(LodCacheError::Forest)?;
        Ok(Self {
            config,
            forest,
            sources: BTreeMap::new(),
            publication: 0,
            pending: BTreeMap::new(),
            inflight: BTreeMap::new(),
            ready: BTreeMap::new(),
            protected: 0,
            stale_results: 0,
        })
    }
    pub fn forest(&self) -> &LodForest {
        &self.forest
    }
    pub fn pending_jobs(&self) -> usize {
        self.pending.len()
    }
    pub fn inflight_jobs(&self) -> usize {
        self.inflight.len()
    }
    pub fn stale_results(&self) -> u64 {
        self.stale_results
    }
    pub fn protected_pages(&self) -> usize {
        self.protected
    }
    /// Execute on the same bounded background owner as selection/summary updates.
    pub fn ingest_summary(
        &mut self,
        input: &net::FarSummary,
    ) -> Result<CacheUpdate, LodCacheError> {
        let coarse = std::array::from_fn(|index| {
            if input.occupied & (1 << index) == 0 {
                PublicVoxel::Air
            } else {
                public_material(input.materials[index])
            }
        });
        if let Some(old) = self.sources.get(&input.page) {
            if input.source_version < old.announced {
                return Ok(CacheUpdate::default());
            }
            if input.source_version == old.announced {
                if old.has_summary && *old.coarse != coarse {
                    return Err(LodCacheError::ConflictingVersion);
                }
                if !old.has_summary {
                    let source = self.sources.get_mut(&input.page).unwrap();
                    source.coarse = Arc::new(coarse);
                    source.has_summary = true;
                    return Ok(CacheUpdate {
                        accepted: true,
                        held_previous: false,
                        ..CacheUpdate::default()
                    });
                }
                return Ok(CacheUpdate::default());
            }
            if matches!(old.data, SourceData::Protected(_)) {
                let source = self.sources.get_mut(&input.page).unwrap();
                source.announced = input.source_version;
                source.coarse = Arc::new(coarse);
                source.has_summary = true;
                return Ok(CacheUpdate {
                    accepted: true,
                    held_previous: true,
                    ..CacheUpdate::default()
                });
            }
        }
        if !self.sources.contains_key(&input.page) && self.sources.len() >= self.config.max_sources
        {
            return Err(LodCacheError::Capacity);
        }
        let coarse = Arc::new(coarse);
        let data = SourceData::Coarse(coarse.clone());
        let work = self.publish_page(input.page, &data.page())?;
        self.sources.insert(
            input.page,
            Source {
                announced: input.source_version,
                published: input.source_version,
                view: 0,
                coarse,
                has_summary: true,
                data,
            },
        );
        Ok(CacheUpdate {
            accepted: true,
            held_previous: false,
            work,
        })
    }
    /// Protocol palettes contain atlas materials, never authority Cell/state.
    /// Run on the LOD background owner after Connection's bounded frame queue.
    pub fn ingest_wire_geometry(
        &mut self,
        input: &net::PublicGeometry,
    ) -> Result<CacheUpdate, LodCacheError> {
        input
            .validate()
            .map_err(|_| LodCacheError::MalformedGeometry)?;
        let palette = input
            .palette
            .iter()
            .map(|surface| PublicVoxel::Solid {
                material: surface.atlas_layer,
                tint: surface.tint,
                kind: match surface.kind {
                    net::PublicSurfaceKind::Opaque => SurfaceKind::Opaque,
                    net::PublicSurfaceKind::Cutout => SurfaceKind::Cutout,
                    net::PublicSurfaceKind::Water => SurfaceKind::Water,
                },
            })
            .collect::<Vec<_>>();
        self.ingest_geometry(GeometryInput {
            page: input.page,
            source: input.source_version,
            view: input.view_version,
            known: &input.known,
            occupied: &input.occupied,
            palette: &palette,
            index_bits: input.index_bits,
            indices: &input.indices,
        })
    }
    pub fn ingest_geometry(
        &mut self,
        input: GeometryInput<'_>,
    ) -> Result<CacheUpdate, LodCacheError> {
        if let Some(old) = self.sources.get(&input.page)
            && (input.source < old.announced
                || (input.source == old.published && input.view < old.view))
        {
            return Ok(CacheUpdate::default());
        }
        let page = decode_geometry(&input, self.config.atlas_layers)?;
        if let Some(old) = self.sources.get(&input.page)
            && input.source == old.published
            && input.view == old.view
            && let SourceData::Protected(known) = &old.data
        {
            if known.canonical_hash() != page.canonical_hash() {
                return Err(LodCacheError::ConflictingVersion);
            }
            return Ok(CacheUpdate::default());
        }
        if !self.sources.contains_key(&input.page) && self.sources.len() >= self.config.max_sources
        {
            return Err(LodCacheError::Capacity);
        }
        let already = self
            .sources
            .get(&input.page)
            .is_some_and(|old| matches!(old.data, SourceData::Protected(_)));
        if !already && self.protected >= self.config.max_protected_pages {
            return Err(LodCacheError::Capacity);
        }
        let work = self.publish_page(input.page, &page)?;
        let has_summary = self
            .sources
            .get(&input.page)
            .is_some_and(|old| old.announced == input.source && old.has_summary);
        let coarse = if has_summary {
            self.sources.get(&input.page).unwrap().coarse.clone()
        } else {
            let node = self
                .forest
                .get(key(input.page))
                .expect("published page summary");
            Arc::new(std::array::from_fn(|i| {
                node.grid
                    .get([(i % 4) as u8, (i / 16) as u8, ((i / 4) % 4) as u8])
            }))
        };
        self.sources.insert(
            input.page,
            Source {
                announced: input.source,
                published: input.source,
                view: input.view,
                coarse,
                has_summary,
                data: SourceData::Protected(Arc::new(page)),
            },
        );
        if !already {
            self.protected += 1;
        }
        Ok(CacheUpdate {
            accepted: true,
            held_previous: false,
            work,
        })
    }
    fn publish_page(
        &mut self,
        page: PageId,
        data: &PublicPage,
    ) -> Result<UpdateWork, LodCacheError> {
        let publication = self
            .publication
            .checked_add(1)
            .ok_or(LodCacheError::VersionExhausted)?;
        let work = self
            .forest
            .upsert_page(key(page), publication, data)
            .map_err(LodCacheError::Forest)?;
        self.publication = publication;
        Ok(work)
    }
    /// Only selected level-zero objects request protected detail. Coarse horizon
    /// parents never expand into a complete set of exact block pages.
    pub fn geometry_requests(&self, selection: &ObjectSelection, limit: usize) -> Vec<PageId> {
        selection
            .objects
            .iter()
            .filter(|object| object.key.level == 0)
            .filter_map(|object| page(object.key))
            .filter(|page| {
                self.sources.get(page).is_some_and(|source| {
                    !matches!(source.data, SourceData::Protected(_))
                        || source.announced > source.published
                })
            })
            .take(limit)
            .collect()
    }
    pub fn queue_selection(&mut self, selection: &ObjectSelection) -> Result<usize, LodCacheError> {
        let wanted = selection
            .objects
            .iter()
            .filter(|object| {
                self.forest
                    .get(object.key)
                    .is_some_and(|node| node.revision == object.revision)
                    && self
                        .ready
                        .get(&object.key)
                        .is_none_or(|(revision, _)| *revision != object.revision)
                    && self
                        .pending
                        .get(&object.key)
                        .is_none_or(|task| task.revision != object.revision)
                    && self.inflight.get(&object.key) != Some(&object.revision)
            })
            .collect::<Vec<_>>();
        let extra = wanted
            .iter()
            .filter(|object| !self.pending.contains_key(&object.key))
            .count();
        if self.pending.len() + extra > self.config.max_pending {
            return Err(LodCacheError::Capacity);
        }
        let mut count = 0;
        for object in wanted {
            let data = if object.key.level == 0 {
                page(object.key)
                    .and_then(|page| self.sources.get(&page))
                    .map(|source| source.data.clone())
            } else {
                None
            }
            .unwrap_or_else(|| {
                let node = self.forest.get(object.key).expect("selected current node");
                SourceData::Coarse(Arc::new(std::array::from_fn(|i| {
                    node.grid
                        .get([(i % 4) as u8, (i / 16) as u8, ((i / 4) % 4) as u8])
                })))
            });
            self.pending.insert(
                object.key,
                LodBuildTask {
                    key: object.key,
                    revision: object.revision,
                    source: data,
                    atlas_layers: self.config.atlas_layers,
                },
            );
            count += 1;
        }
        Ok(count)
    }
    pub fn take_task(&mut self) -> Option<LodBuildTask> {
        if self.inflight.len() >= self.config.max_inflight {
            return None;
        }
        let key = *self
            .pending
            .keys()
            .find(|key| !self.inflight.contains_key(key))?;
        let task = self.pending.remove(&key).expect("retained pending task");
        self.inflight.insert(task.key, task.revision);
        Some(task)
    }
    /// Backpressure returns ownership of an unconsumed result for retry. A stale
    /// completion never overwrites the prior valid representation.
    pub fn complete(&mut self, built: BuiltProxy) -> Result<PublishDisposition, Box<BuiltProxy>> {
        let current = self.inflight.get(&built.key) == Some(&built.revision)
            && self
                .forest
                .get(built.key)
                .is_some_and(|node| node.revision == built.revision);
        if !current {
            if self.inflight.get(&built.key) == Some(&built.revision) {
                self.inflight.remove(&built.key);
            }
            self.stale_results += 1;
            return Ok(PublishDisposition::Stale);
        }
        if !self.ready.contains_key(&built.key) && self.ready.len() >= self.config.max_ready {
            return Err(Box::new(built));
        }
        self.inflight.remove(&built.key);
        self.ready
            .insert(built.key, (built.revision, built.payload));
        Ok(PublishDisposition::Published)
    }
    pub fn fail_task(&mut self, key: LodKey, revision: u64) -> bool {
        if self.inflight.get(&key) == Some(&revision) {
            self.inflight.remove(&key);
            true
        } else {
            false
        }
    }
    pub fn ready(&self, key: LodKey, revision: u64) -> Option<&Arc<ProxyPayload>> {
        self.ready
            .get(&key)
            .filter(|(stored, _)| *stored == revision)
            .map(|(_, payload)| payload)
    }
    pub fn retain_ready(&mut self, selection: &ObjectSelection) {
        self.ready
            .retain(|key, _| selection.objects.iter().any(|object| object.key == *key));
    }
    pub fn evict_geometry(&mut self, id: PageId) -> Result<bool, LodCacheError> {
        let Some(source) = self.sources.get(&id) else {
            return Ok(false);
        };
        if !matches!(source.data, SourceData::Protected(_)) {
            return Ok(false);
        }
        let coarse = source.coarse.clone();
        let version = source.announced;
        self.publish_page(id, &SourceData::Coarse(coarse.clone()).page())?;
        let source = self.sources.get_mut(&id).unwrap();
        source.data = SourceData::Coarse(coarse);
        source.published = version;
        self.protected -= 1;
        Ok(true)
    }
}
fn key(page: PageId) -> LodKey {
    LodKey::page(
        page.space.0,
        i64::from(page.x),
        i64::from(page.y),
        i64::from(page.z),
    )
}
fn page(key: LodKey) -> Option<PageId> {
    Some(PageId {
        space: core_types::SpaceId(key.space),
        x: i32::try_from(key.x).ok()?,
        y: i32::try_from(key.y).ok()?,
        z: i32::try_from(key.z).ok()?,
    })
}
fn public_material(id: u16) -> PublicVoxel {
    let id = if matches!(
        id,
        rules::ids::COAL_ORE
            | rules::ids::IRON_ORE
            | rules::ids::OSMIUM_ORE
            | rules::ids::TIN_ORE
            | rules::ids::COPPER_ORE
            | rules::ids::GOLD_ORE
            | rules::ids::REDSTONE_ORE
    ) {
        rules::ids::STONE
    } else {
        id
    };
    let Some(definition) = rules::block(id) else {
        return PublicVoxel::Unknown;
    };
    let kind = match definition.render {
        rules::blocks::Render::None
        | rules::blocks::Render::Cross
        | rules::blocks::Render::Model(_) => return PublicVoxel::Air,
        rules::blocks::Render::Cube => SurfaceKind::Opaque,
        rules::blocks::Render::Leaves | rules::blocks::Render::Glass => SurfaceKind::Cutout,
        rules::blocks::Render::Water => SurfaceKind::Water,
    };
    PublicVoxel::Solid {
        material: rules::blocks::layer_cell(
            id,
            Cell::from_parts(id, 0).state(),
            rules::blocks::mesh_face(1),
        )
        .0 as u16,
        tint: u32::MAX,
        kind,
    }
}
pub fn decode_geometry(
    input: &GeometryInput<'_>,
    atlas_layers: u32,
) -> Result<PublicPage, LodCacheError> {
    let bits = if input.palette.len() <= 1 {
        0
    } else {
        32 - (input.palette.len() as u32 - 1).leading_zeros()
    };
    if input.palette.len()>4096||u32::from(input.index_bits)!=bits||input.indices.len()!=(4096*bits as usize).div_ceil(8)||input.occupied.iter().zip(input.known).any(|(occupied,known)|occupied&!known!=0)||input.palette.iter().any(|voxel|!matches!(voxel,PublicVoxel::Solid{material,..}if u32::from(*material)<atlas_layers)){return Err(LodCacheError::MalformedGeometry);}
    let palette_index = |index: usize| {
        if bits == 0 {
            return 0usize;
        }
        let bit = index * bits as usize;
        let byte = bit / 8;
        let mut value = u32::from(input.indices[byte]);
        if byte + 1 < input.indices.len() {
            value |= u32::from(input.indices[byte + 1]) << 8;
        }
        if byte + 2 < input.indices.len() {
            value |= u32::from(input.indices[byte + 2]) << 16;
        }
        ((value >> (bit % 8)) & ((1 << bits) - 1)) as usize
    };
    for index in 0..4096 {
        let occupied = input.occupied[index / 8] & (1 << (index % 8)) != 0;
        let palette = palette_index(index);
        if (occupied && palette >= input.palette.len()) || (!occupied && palette != 0) {
            return Err(LodCacheError::InvalidPalette);
        }
    }
    Ok(PublicPage::from_exposed(|[x, y, z]| {
        let index = usize::from(x) + 16 * (usize::from(z) + 16 * usize::from(y));
        let bit = 1 << (index % 8);
        if input.known[index / 8] & bit == 0 {
            PublicVoxel::Unknown
        } else if input.occupied[index / 8] & bit == 0 {
            PublicVoxel::Air
        } else {
            input.palette[palette_index(index)]
        }
    }))
}
