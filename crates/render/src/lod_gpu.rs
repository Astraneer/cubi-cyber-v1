//! Public far-world masked proxies. Six implicit box quads carry a bounded voxel
//! traversal; exact protected openings need no refined geometry. Unknown input
//! cannot replace an earlier valid representation.
use crate::{
    lod::{MaskedRayProxy, PublicVoxel, SurfaceKind},
    shadows::ShadowGpu,
    terrain_gpu::{Camera, TerrainGeometry, UploadBudget},
    terrain_mesh::{
        CellMaterial, Face, MAX_PAGE_QUADS, MeshClass, MeshScratch, PageHalo, PageMesh,
        QuadDescriptor, mesh_page,
    },
};
use bytemuck::{Pod, Zeroable};
use std::collections::BTreeMap;
const OCCUPANCY_WORDS: usize = 128;
const INDEX_OFFSET: usize = 161;
const OBJECT_BYTES: u64 = 256;
const PAYLOAD_HEADER_BYTES: usize = 36;
const MAX_PAYLOAD_WORDS: usize = INDEX_OFFSET + 1536 + 8192 + MAX_PAGE_QUADS * 4;
pub const MAX_PROXY_PAYLOAD_BYTES: usize = PAYLOAD_HEADER_BYTES + MAX_PAYLOAD_WORDS * 4;
pub const MAX_PUBLIC_BYTES: usize = MAX_PROXY_PAYLOAD_BYTES;
pub const MAX_RETAINED_BYTES: usize = std::mem::size_of::<ProxyPayload>() + MAX_PAYLOAD_WORDS * 4;
pub const MAX_DDA_STEPS: u32 = 46;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProxyError {
    UnknownData,
    UnsupportedWater,
    InvalidMaterial,
    InvalidConfig,
    InvalidTransform,
    InvalidCamera,
    Capacity,
    UploadBudget,
    StaleHandle,
    StaleVersion,
}
impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ProxyError {}
#[derive(Clone, Debug)]
pub struct ProxyPayload {
    words: Vec<u32>,
    palette_offset: u32,
    index_bits: u32,
    occupied: u32,
    palette_len: u32,
    quad_offset: u32,
    quad_count: u32,
    classes: u32,
}
impl ProxyPayload {
    /// Run in a background task. No registry, generator or authoritative cells
    /// enter this boundary; the caller already redacted its public samples.
    pub fn from_public(proxy: &MaskedRayProxy, atlas_layers: u32) -> Result<Self, ProxyError> {
        let mut occupied = [0u32; OCCUPANCY_WORDS];
        let mut palette = Vec::new();
        let mut lookup = BTreeMap::new();
        let mut indices = Vec::new();
        let mut halo = PageHalo::filled(0);
        let mut classes = 0u32;
        for y in 0..16u8 {
            for z in 0..16u8 {
                for x in 0..16u8 {
                    let index = usize::from(x) + 16 * (usize::from(z) + 16 * usize::from(y));
                    let voxel = proxy.voxel([x, y, z]);
                    match voxel {
                        PublicVoxel::Unknown => return Err(ProxyError::UnknownData),
                        PublicVoxel::Air => {}
                        PublicVoxel::Solid { material, kind, .. } => {
                            if u32::from(material) >= atlas_layers {
                                return Err(ProxyError::InvalidMaterial);
                            }
                            if kind == SurfaceKind::Water {
                                return Err(ProxyError::UnsupportedWater);
                            }
                            occupied[index / 32] |= 1 << (index % 32);
                            let palette_index = *lookup.entry(voxel).or_insert_with(|| {
                                let index = palette.len() as u32;
                                palette.push(voxel);
                                index
                            });
                            indices.push(palette_index);
                            halo.set(x as i8, y as i8, z as i8, palette_index + 1);
                            classes |= 1 << kind as u32;
                        }
                    }
                }
            }
        }
        let bits = if palette.len() <= 1 {
            0
        } else {
            32 - (palette.len() as u32 - 1).leading_zeros()
        };
        let index_words = (indices.len() * bits as usize).div_ceil(32);
        let palette_offset = INDEX_OFFSET + index_words;
        let quad_offset = palette_offset + palette.len() * 2;
        let mut mesh = PageMesh::default();
        if classes & 2 != 0 {
            mesh_page(
                &halo,
                |raw| {
                    if raw == 0 {
                        return CellMaterial::EMPTY;
                    }
                    let PublicVoxel::Solid {
                        material,
                        tint,
                        kind,
                    } = palette[raw as usize - 1]
                    else {
                        unreachable!("occupied palette")
                    };
                    let mut result = CellMaterial::opaque(material);
                    result.tint = tint;
                    if kind == SurfaceKind::Cutout {
                        result.class = MeshClass::Cutout;
                        result.occludes = false;
                        result.casts_ao = false;
                    }
                    result
                },
                &mut MeshScratch::new(),
                &mut mesh,
            );
        }
        let quad_count = mesh.cutout.len();
        let mut words = vec![0; quad_offset + quad_count * 4];
        words[..128].copy_from_slice(&occupied);
        let mut rank = 0u32;
        for word in 0..65 {
            words[128 + word / 2] |= rank << ((word % 2) * 16);
            if word < 64 {
                rank += occupied[word * 2].count_ones() + occupied[word * 2 + 1].count_ones();
            }
        }
        if bits > 0 {
            for (rank, index) in indices.iter().copied().enumerate() {
                let bit = rank * bits as usize;
                words[INDEX_OFFSET + bit / 32] |= index << (bit % 32);
                if bit % 32 + bits as usize > 32 {
                    words[INDEX_OFFSET + bit / 32 + 1] |= index >> (32 - bit % 32);
                }
            }
        }
        for (i, voxel) in palette.iter().copied().enumerate() {
            if let PublicVoxel::Solid {
                material,
                tint,
                kind,
            } = voxel
            {
                words[palette_offset + i * 2] = u32::from(material) | ((kind as u32) << 16);
                words[palette_offset + i * 2 + 1] = tint;
            }
        }
        for (index, quad) in mesh.cutout.iter().enumerate() {
            words[quad_offset + index * 4..quad_offset + index * 4 + 4]
                .copy_from_slice(&quad.words);
        }
        Ok(Self {
            words,
            palette_offset: palette_offset as u32,
            index_bits: bits,
            occupied: indices.len() as u32,
            palette_len: palette.len() as u32,
            quad_offset: quad_offset as u32,
            quad_count: quad_count as u32,
            classes,
        })
    }
    /// Bounded browser-worker result. The decoder checks every rank/index before
    /// constructing storage accepted by the GPU; no meshing runs on the owner.
    pub fn encode_public(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(PAYLOAD_HEADER_BYTES + self.words.len() * 4);
        bytes.extend_from_slice(b"PXP2");
        for value in [
            self.words.len() as u32,
            self.palette_offset,
            self.index_bits,
            self.occupied,
            self.palette_len,
            self.quad_offset,
            self.quad_count,
            self.classes,
        ] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        for word in &self.words {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes
    }
    pub fn decode_public(bytes: &[u8], atlas_layers: u32) -> Result<Self, ProxyError> {
        if bytes.len() < PAYLOAD_HEADER_BYTES
            || bytes.len() > MAX_PROXY_PAYLOAD_BYTES
            || &bytes[..4] != b"PXP2"
            || atlas_layers == 0
            || atlas_layers > 256
        {
            return Err(ProxyError::InvalidMaterial);
        }
        let read =
            |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("bounded header"));
        let count = read(4) as usize;
        let palette_offset = read(8);
        let bits = read(12);
        let occupied = read(16);
        let palette_len = read(20);
        let quad_offset = read(24) as usize;
        let quad_count = read(28);
        let classes = read(32);
        if count > MAX_PAYLOAD_WORDS
            || occupied > 4096
            || palette_len > occupied
            || bits > 12
            || bits
                != if palette_len <= 1 {
                    0
                } else {
                    32 - (palette_len - 1).leading_zeros()
                }
            || quad_count as usize > MAX_PAGE_QUADS
            || classes > 3
            || bytes.len() != PAYLOAD_HEADER_BYTES + count * 4
        {
            return Err(ProxyError::InvalidMaterial);
        }
        let index_words = (occupied as usize * bits as usize).div_ceil(32);
        if palette_offset as usize != INDEX_OFFSET + index_words
            || quad_offset != INDEX_OFFSET + index_words + palette_len as usize * 2
            || count != quad_offset + quad_count as usize * 4
        {
            return Err(ProxyError::InvalidMaterial);
        }
        let word = |index: usize| read(PAYLOAD_HEADER_BYTES + index * 4);
        let mut rank = 0u32;
        for index in 0..=64 {
            if ((word(128 + index / 2) >> ((index % 2) * 16)) & 65535) != rank {
                return Err(ProxyError::InvalidMaterial);
            }
            if index < 64 {
                rank += word(index * 2).count_ones() + word(index * 2 + 1).count_ones();
            }
        }
        if rank != occupied || word(160) >> 16 != 0 || ((occupied == 0) != (palette_len == 0)) {
            return Err(ProxyError::InvalidMaterial);
        }
        let mut expected_classes = 0u32;
        let mut cutout_materials = [0u32; 8];
        for index in 0..palette_len as usize {
            let info = word(palette_offset as usize + index * 2);
            if info & 65535 >= atlas_layers || info >> 16 > 1 {
                return Err(ProxyError::InvalidMaterial);
            }
            expected_classes |= 1 << (info >> 16);
            if info >> 16 == 1 {
                let material = info & 65535;
                cutout_materials[material as usize / 32] |= 1 << (material % 32);
            }
        }
        if classes != expected_classes || (classes & 2 == 0 && quad_count != 0) {
            return Err(ProxyError::InvalidMaterial);
        }
        for index in 0..quad_count as usize {
            let base = quad_offset + index * 4;
            let bits = word(base);
            let detail = word(base + 1);
            let material = (detail & 65535) as u16;
            let Some(face) = Face::from_index(((bits >> 15) & 7) as u8) else {
                return Err(ProxyError::InvalidMaterial);
            };
            if bits >> 26 != 0
                || detail >> 26 != 0
                || word(base + 3) != 0
                || u32::from(material) >= atlas_layers
                || cutout_materials[usize::from(material) / 32] & (1 << (material % 32)) == 0
            {
                return Err(ProxyError::InvalidMaterial);
            }
            QuadDescriptor::new(
                [
                    (bits & 31) as u8,
                    ((bits >> 5) & 31) as u8,
                    ((bits >> 10) & 31) as u8,
                ],
                face,
                [
                    (((bits >> 18) & 15) + 1) as u8,
                    (((bits >> 22) & 15) + 1) as u8,
                ],
                material,
                std::array::from_fn(|i| ((detail >> (16 + i * 2)) & 3) as u8),
                word(base + 2),
            )
            .map_err(|_| ProxyError::InvalidMaterial)?;
        }
        if bits > 0 {
            let mask = (1u32 << bits) - 1;
            for index in 0..occupied as usize {
                let bit = index * bits as usize;
                let mut value = word(INDEX_OFFSET + bit / 32) >> (bit % 32);
                if bit % 32 + bits as usize > 32 {
                    value |= word(INDEX_OFFSET + bit / 32 + 1) << (32 - bit % 32);
                }
                if value & mask >= palette_len {
                    return Err(ProxyError::InvalidMaterial);
                }
            }
            let used = occupied as usize * bits as usize % 32;
            if used > 0 && word(palette_offset as usize - 1) >> used != 0 {
                return Err(ProxyError::InvalidMaterial);
            }
        }
        Ok(Self {
            words: (0..count).map(word).collect(),
            palette_offset,
            index_bits: bits,
            occupied,
            palette_len,
            quad_offset: quad_offset as u32,
            quad_count,
            classes,
        })
    }
    pub fn materials_within(&self, atlas_layers: u32) -> bool {
        (0..self.palette_len as usize).all(|index| {
            self.words[self.palette_offset as usize + index * 2] & 65535 < atlas_layers
        })
    }
    pub fn upload_bytes(&self) -> u64 {
        self.words.len() as u64 * 4 + OBJECT_BYTES
    }
    pub fn payload_bytes(&self) -> usize {
        self.words.len() * 4
    }
    pub fn occupied_cells(&self) -> u32 {
        self.occupied
    }
    pub fn palette_len(&self) -> u32 {
        self.palette_len
    }
    pub fn cutout_quads(&self) -> u32 {
        self.quad_count
    }
    pub fn voxel(&self, position: [u8; 3]) -> PublicVoxel {
        let index = usize::from(position[0])
            + 16 * (usize::from(position[2]) + 16 * usize::from(position[1]));
        let word = index / 32;
        let bit = 1 << (index % 32);
        if self.words[word] & bit == 0 {
            return PublicVoxel::Air;
        }
        let word64 = index / 64;
        let rank_base = (self.words[128 + word64 / 2] >> ((word64 % 2) * 16)) & 65535;
        let within = index % 64;
        let mut rank = rank_base;
        if within >= 32 {
            rank += self.words[word64 * 2].count_ones();
        }
        let sub = within % 32;
        if sub > 0 {
            rank += (self.words[word] & ((1 << sub) - 1)).count_ones();
        }
        let palette = if self.index_bits == 0 {
            0
        } else {
            let bit = rank * self.index_bits;
            let mut value = self.words[INDEX_OFFSET + bit as usize / 32] >> (bit % 32);
            if bit % 32 + self.index_bits > 32 {
                value |= self.words[INDEX_OFFSET + bit as usize / 32 + 1] << (32 - bit % 32);
            }
            value & ((1 << self.index_bits) - 1)
        };
        let offset = self.palette_offset as usize + palette as usize * 2;
        let info = self.words[offset];
        PublicVoxel::Solid {
            material: (info & 65535) as u16,
            tint: self.words[offset + 1],
            kind: if info >> 16 == 1 {
                SurfaceKind::Cutout
            } else {
                SurfaceKind::Opaque
            },
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct ProxyConfig {
    pub max_objects: u32,
    pub max_payload_words: u32,
    pub atlas_layers: u32,
    pub instrument: bool,
}
impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            max_objects: 1024,
            max_payload_words: 4 * 1024 * 1024,
            atlas_layers: 186,
            instrument: false,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProxyHandle {
    pub slot: u32,
    pub generation: u32,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProxyCamera {
    pub camera: Camera,
    pub eye: [f32; 3],
    pub viewport_height: u32,
    pub orthographic_direction: Option<[f32; 3]>,
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraGpu {
    view_projection: [[f32; 4]; 4],
    eye: [f32; 4],
    direction: [f32; 4],
    params: [u32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ObjectGpu {
    model: [[f32; 4]; 4],
    inverse: [[f32; 4]; 4],
    bounds_min: [f32; 4],
    bounds_max: [f32; 4],
    data: [u32; 4],
    light: [f32; 4],
    reserved: [[u32; 4]; 4],
}
const _: () = assert!(std::mem::size_of::<ObjectGpu>() == 256);
#[derive(Clone, Copy)]
struct Span {
    start: u32,
    len: u32,
}
struct Slot {
    generation: u32,
    version: u64,
    span: Option<Span>,
    object: ObjectGpu,
    occupied: bool,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct ProxyWork {
    pub upload_bytes: u64,
    pub payload_samples: u64,
    pub pipeline_compilations: u32,
    pub cull_dispatches: u32,
    pub objects_considered: u32,
    pub camera_upload_bytes: u64,
}
/// Public proxy resources for an independent directional-shadow camera. Active
/// object flags already reflect atomic front/near publication. Player-camera
/// visibility and indirect buffers are intentionally excluded.
pub struct ProxyShadowSource<'a> {
    pub objects: &'a wgpu::Buffer,
    pub payload: &'a wgpu::Buffer,
    pub colors: &'a wgpu::TextureView,
    pub sampler: &'a wgpu::Sampler,
    pub materials: &'a wgpu::Buffer,
    pub max_objects: u32,
    pub max_payload_words: u32,
    pub atlas_layers: u32,
    pub revision: u64,
}
pub struct LodProxyGpu {
    config: ProxyConfig,
    model_staging: Vec<(u32, ObjectGpu)>,
    objects: wgpu::Buffer,
    payload: wgpu::Buffer,
    shadow_colors: wgpu::TextureView,
    shadow_sampler: wgpu::Sampler,
    shadow_materials: wgpu::Buffer,
    visible: wgpu::Buffer,
    cutout_visible: wgpu::Buffer,
    indirect: wgpu::Buffer,
    camera: wgpu::Buffer,
    counters: Option<wgpu::Buffer>,
    render_bind: wgpu::BindGroup,
    cutout_bind: wgpu::BindGroup,
    cull_bind: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    cutout_pipeline: wgpu::RenderPipeline,
    ambient_mrt: bool,
    reset: wgpu::ComputePipeline,
    cull: wgpu::ComputePipeline,
    slots: Vec<Slot>,
    free_slots: Vec<u32>,
    free_spans: Vec<Span>,
    revision: u64,
    last_frame: Option<(ProxyCamera, u64, u32)>,
    work: ProxyWork,
}
impl LodProxyGpu {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        geometry: TerrainGeometry<'_>,
        config: ProxyConfig,
        shadows: Option<&ShadowGpu>,
    ) -> Result<Self, ProxyError> {
        Self::new_prepared(device, queue, geometry, config, shadows, None)
    }
    /// Prepare the same atmospheric energy as near terrain at loading.
    pub fn new_with_atmosphere(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        geometry: TerrainGeometry<'_>,
        config: ProxyConfig,
        shadows: Option<&ShadowGpu>,
        atmosphere: &crate::atmosphere::AtmosphereGpu,
    ) -> Result<Self, ProxyError> {
        Self::new_prepared(device, queue, geometry, config, shadows, Some(atmosphere))
    }
    fn new_prepared(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        geometry: TerrainGeometry<'_>,
        config: ProxyConfig,
        shadows: Option<&ShadowGpu>,
        atmosphere: Option<&crate::atmosphere::AtmosphereGpu>,
    ) -> Result<Self, ProxyError> {
        let sample_count = geometry.sample_count;
        let limits = device.limits();
        if config.max_objects == 0
            || config.max_payload_words < 4
            || config.atlas_layers == 0
            || u64::from(config.max_payload_words) * 4
                > u64::from(limits.max_storage_buffer_binding_size)
            || u64::from(config.max_objects) * OBJECT_BYTES
                > u64::from(limits.max_storage_buffer_binding_size)
        {
            return Err(ProxyError::InvalidConfig);
        }
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let objects = buffer(
            "public LOD proxy objects",
            u64::from(config.max_objects) * OBJECT_BYTES,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let payload = buffer(
            "public occupancy and rank palettes",
            u64::from(config.max_payload_words) * 4,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let visible = buffer(
            "LOD visible object compaction",
            u64::from(config.max_objects) * 4,
            wgpu::BufferUsages::STORAGE,
        );
        let cutout_visible = buffer(
            "GPU visible public Cutout quads",
            u64::from(config.max_payload_words / 4) * 8,
            wgpu::BufferUsages::STORAGE,
        );
        let indirect = buffer(
            "one indirect LOD proxy draw",
            32,
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::COPY_SRC,
        );
        let camera = buffer(
            "LOD frame camera",
            112,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let counters = config.instrument.then(|| {
            buffer(
                "LOD DDA work counters",
                16,
                wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
            )
        });
        let v = wgpu::ShaderStages::VERTEX;
        let f = wgpu::ShaderStages::FRAGMENT;
        let c = wgpu::ShaderStages::COMPUTE;
        let mut render_entries = vec![
            buffer_layout(0, v | f, true, true),
            buffer_layout(1, v | f, false, true),
            buffer_layout(2, v | f, false, true),
            buffer_layout(3, v, false, true),
            texture_layout(4, f, wgpu::TextureSampleType::Float { filterable: true }),
            texture_layout(5, f, wgpu::TextureSampleType::Float { filterable: true }),
            wgpu::BindGroupLayoutEntry {
                binding: 6,
                visibility: f,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            buffer_layout(7, f, false, true),
        ];
        if config.instrument {
            render_entries.push(buffer_layout(8, f, false, false));
        }
        if shadows.is_some() {
            render_entries.extend([
                buffer_layout(9, f, true, true),
                texture_layout(10, f, wgpu::TextureSampleType::Depth),
                wgpu::BindGroupLayoutEntry {
                    binding: 11,
                    visibility: f,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ]);
        }
        if atmosphere.is_some() {
            render_entries.push(buffer_layout(20, f, false, true));
        }
        let render_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("LOD masked proxy render layout"),
            entries: &render_entries,
        });
        let cull_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("LOD proxy independent frustum layout"),
            entries: &[
                buffer_layout(0, c, true, true),
                buffer_layout(1, c, false, true),
                buffer_layout(2, c, false, false),
                buffer_layout(3, c, false, false),
                buffer_layout(4, c, false, false),
            ],
        });
        let mut entries = vec![
            buffer_binding(0, &camera),
            buffer_binding(1, &objects),
            buffer_binding(2, &payload),
            buffer_binding(3, &visible),
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(geometry.colors),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(geometry.tint_masks),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::Sampler(geometry.sampler),
            },
            buffer_binding(7, geometry.materials),
        ];
        if let Some(counters) = &counters {
            entries.push(buffer_binding(8, counters));
        }
        if let Some(shadows) = shadows {
            entries.extend([
                buffer_binding(9, shadows.sampling_buffer()),
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wgpu::BindingResource::TextureView(shadows.depth_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::Sampler(shadows.sampler()),
                },
            ]);
        }
        if let Some(atmosphere) = atmosphere {
            entries.push(buffer_binding(20, atmosphere.lighting_buffer()));
        }
        let render_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("LOD public data and materials"),
            layout: &render_layout,
            entries: &entries,
        });
        let cutout_entries: Vec<_> = entries
            .iter()
            .map(|entry| wgpu::BindGroupEntry {
                binding: entry.binding,
                resource: if entry.binding == 3 {
                    cutout_visible.as_entire_binding()
                } else {
                    entry.resource.clone()
                },
            })
            .collect();
        let cutout_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("public ordinary Cutout quad sources"),
            layout: &render_layout,
            entries: &cutout_entries,
        });
        let cull_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("LOD independent visibility"),
            layout: &cull_layout,
            entries: &[
                buffer_binding(0, &camera),
                buffer_binding(1, &objects),
                buffer_binding(2, &visible),
                buffer_binding(3, &indirect),
                buffer_binding(4, &cutout_visible),
            ],
        });
        let source = shader_source(
            shadows.is_some(),
            atmosphere.is_some(),
            geometry.ambient_mrt,
            config.instrument,
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bounded masked LOD traversal"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("LOD proxy pipeline"),
            bind_group_layouts: &[&render_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("six-quad masked LOD HDR MSAA4"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                alpha_to_coverage_enabled: false,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &crate::ambient_mrt::targets(None, geometry.ambient_mrt),
            }),
            multiview: None,
            cache: None,
        });
        let cutout_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("public pixel-rate Cutout quads with hardware A2C"),
            source: wgpu::ShaderSource::Wgsl(
                cutout_shader_source(
                    shadows.is_some(),
                    atmosphere.is_some(),
                    geometry.ambient_mrt,
                )
                .into(),
            ),
        });
        let cutout_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("public ordinary Cutout quad HDR"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &cutout_shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                alpha_to_coverage_enabled: sample_count > 1,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &cutout_shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &crate::ambient_mrt::targets(None, geometry.ambient_mrt),
            }),
            multiview: None,
            cache: None,
        });
        let compute_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("LOD public object culling"),
            source: wgpu::ShaderSource::Wgsl(include_str!("lod_cull.wgsl").into()),
        });
        let compute_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("LOD cull pipelines"),
            bind_group_layouts: &[&cull_layout],
            push_constant_ranges: &[],
        });
        let compute = |entry| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&compute_layout),
                module: &compute_shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let reset = compute("reset");
        let cull = compute("cull");
        let slots = (0..config.max_objects)
            .map(|_| Slot {
                generation: 0,
                version: 0,
                span: None,
                object: ObjectGpu::zeroed(),
                occupied: false,
            })
            .collect();
        queue.write_buffer(
            &objects,
            0,
            bytemuck::cast_slice(&vec![ObjectGpu::zeroed(); config.max_objects as usize]),
        );
        Ok(Self {
            config,
            model_staging: Vec::with_capacity(config.max_objects as usize),
            objects,
            payload,
            shadow_colors: geometry.colors.clone(),
            shadow_sampler: geometry.sampler.clone(),
            shadow_materials: geometry.materials.clone(),
            visible,
            cutout_visible,
            indirect,
            camera,
            counters,
            render_bind,
            cutout_bind,
            cull_bind,
            pipeline,
            cutout_pipeline,
            ambient_mrt: geometry.ambient_mrt,
            reset,
            cull,
            slots,
            free_slots: (0..config.max_objects).rev().collect(),
            free_spans: vec![Span {
                start: 0,
                len: config.max_payload_words,
            }],
            revision: 0,
            last_frame: None,
            work: ProxyWork {
                pipeline_compilations: 4,
                ..ProxyWork::default()
            },
        })
    }
    #[allow(clippy::too_many_arguments)] // One atomic object/data publication shares the terrain upload contract.
    pub fn upload(
        &mut self,
        queue: &wgpu::Queue,
        handle: Option<ProxyHandle>,
        version: u64,
        model: [[f32; 4]; 4],
        light: [f32; 4],
        payload: &ProxyPayload,
        budget: &mut UploadBudget,
    ) -> Result<ProxyHandle, ProxyError> {
        let inverse = inverse(model)?;
        if light.iter().any(|v| !v.is_finite() || *v < 0.) {
            return Err(ProxyError::InvalidTransform);
        }
        let len = u32::try_from(payload.words.len()).map_err(|_| ProxyError::Capacity)?;
        let (slot, generation, old_span) = if let Some(handle) = handle {
            let slot = self
                .slots
                .get(handle.slot as usize)
                .filter(|s| s.generation == handle.generation && s.span.is_some())
                .ok_or(ProxyError::StaleHandle)?;
            if version < slot.version {
                return Err(ProxyError::StaleVersion);
            }
            (handle.slot, handle.generation, slot.span)
        } else {
            let slot = *self.free_slots.last().ok_or(ProxyError::Capacity)?;
            let generation = self.slots[slot as usize]
                .generation
                .checked_add(1)
                .ok_or(ProxyError::Capacity)?;
            (slot, generation, None)
        };
        let reused = old_span.is_some_and(|span| span.len >= len);
        let free = if reused {
            None
        } else {
            Some(
                self.free_spans
                    .iter()
                    .position(|s| s.len >= len)
                    .ok_or(ProxyError::Capacity)?,
            )
        };
        let cost = u64::from(len) * 4 + OBJECT_BYTES;
        if budget.remaining_bytes < cost {
            return Err(ProxyError::UploadBudget);
        }
        let span = if let Some(index) = free {
            let free = self.free_spans[index];
            if free.len == len {
                self.free_spans.swap_remove(index);
            } else {
                self.free_spans[index].start += len;
                self.free_spans[index].len -= len;
            }
            Span {
                start: free.start,
                len,
            }
        } else {
            old_span.unwrap()
        };
        let (min, max) = bounds(model);
        let object = ObjectGpu {
            model,
            inverse,
            bounds_min: [min[0], min[1], min[2], 0.],
            bounds_max: [max[0], max[1], max[2], 0.],
            data: [
                span.start,
                span.start + payload.palette_offset,
                payload.index_bits,
                u32::from(payload.occupied > 0),
            ],
            light,
            reserved: [
                [
                    payload.classes,
                    span.start + payload.quad_offset,
                    payload.quad_count,
                    0,
                ],
                [0; 4],
                [0; 4],
                [0; 4],
            ],
        };
        queue.write_buffer(
            &self.payload,
            u64::from(span.start) * 4,
            bytemuck::cast_slice(&payload.words),
        );
        queue.write_buffer(
            &self.objects,
            u64::from(slot) * OBJECT_BYTES,
            bytemuck::bytes_of(&object),
        );
        if handle.is_none() {
            self.free_slots.pop();
        }
        if !reused && let Some(old) = old_span {
            self.free_span(old);
        }
        self.slots[slot as usize] = Slot {
            generation,
            version,
            span: Some(span),
            object,
            occupied: payload.occupied > 0,
        };
        budget.remaining_bytes -= cost;
        self.revision = self.revision.wrapping_add(1);
        self.work.upload_bytes += cost;
        Ok(ProxyHandle { slot, generation })
    }
    /// Prepare a new covering representation without showing it. Call
    /// set_enabled_batch only once every node of the replacement front is ready.
    #[allow(clippy::too_many_arguments)] // Mirrors the atomic upload operation.
    pub fn upload_inactive(
        &mut self,
        queue: &wgpu::Queue,
        version: u64,
        model: [[f32; 4]; 4],
        light: [f32; 4],
        payload: &ProxyPayload,
        budget: &mut UploadBudget,
    ) -> Result<ProxyHandle, ProxyError> {
        if budget.remaining_bytes < payload.upload_bytes() + 4 {
            return Err(ProxyError::UploadBudget);
        }
        let handle = self.upload(queue, None, version, model, light, payload, budget)?;
        self.set_enabled_batch(queue, &[(handle, false)], budget)
            .expect("prepaid staging disable");
        Ok(handle)
    }
    /// Sorted unique slots and the complete budget are validated before the
    /// first GPU write: a partial coarse/fine front can never leak into a frame.
    pub fn set_enabled_batch(
        &mut self,
        queue: &wgpu::Queue,
        changes: &[(ProxyHandle, bool)],
        budget: &mut UploadBudget,
    ) -> Result<u64, ProxyError> {
        if changes
            .windows(2)
            .any(|pair| pair[0].0.slot >= pair[1].0.slot)
        {
            return Err(ProxyError::StaleHandle);
        }
        let mut count = 0;
        for (handle, enabled) in changes {
            let slot = self
                .slots
                .get(handle.slot as usize)
                .filter(|s| s.generation == handle.generation && s.span.is_some())
                .ok_or(ProxyError::StaleHandle)?;
            let flags = u32::from(*enabled && slot.occupied);
            count += u64::from(slot.object.data[3] != flags);
        }
        let bytes = count * 4;
        if bytes > budget.remaining_bytes {
            return Err(ProxyError::UploadBudget);
        }
        for (handle, enabled) in changes {
            let slot = &mut self.slots[handle.slot as usize];
            let flags = u32::from(*enabled && slot.occupied);
            if slot.object.data[3] != flags {
                slot.object.data[3] = flags;
                queue.write_buffer(
                    &self.objects,
                    u64::from(handle.slot) * OBJECT_BYTES + 172,
                    bytemuck::bytes_of(&flags),
                );
            }
        }
        budget.remaining_bytes -= bytes;
        self.work.upload_bytes += bytes;
        if bytes > 0 {
            self.revision = self.revision.wrapping_add(1);
        }
        Ok(bytes)
    }
    /// Atomically updates transforms without uploading any occupancy/palette
    /// payload. Sorted unique handles and the full byte budget precede writes.
    pub fn set_models_batch(
        &mut self,
        queue: &wgpu::Queue,
        changes: &[(ProxyHandle, [[f32; 4]; 4])],
        budget: &mut UploadBudget,
    ) -> Result<u64, ProxyError> {
        if changes.windows(2).any(|p| p[0].0.slot >= p[1].0.slot) {
            return Err(ProxyError::StaleHandle);
        }
        self.model_staging.clear();
        for &(handle, model) in changes {
            let slot = self
                .slots
                .get(handle.slot as usize)
                .filter(|s| s.generation == handle.generation && s.span.is_some())
                .ok_or(ProxyError::StaleHandle)?;
            let inv = inverse(model)?;
            if slot.object.model == model {
                continue;
            }
            let mut object = slot.object;
            object.model = model;
            object.inverse = inv;
            let (min, max) = bounds(model);
            object.bounds_min = [min[0], min[1], min[2], 0.];
            object.bounds_max = [max[0], max[1], max[2], 0.];
            self.model_staging.push((handle.slot, object));
        }
        let bytes = self.model_staging.len() as u64 * OBJECT_BYTES;
        if budget.remaining_bytes < bytes {
            return Err(ProxyError::UploadBudget);
        }
        for &(slot, object) in &self.model_staging {
            queue.write_buffer(
                &self.objects,
                u64::from(slot) * OBJECT_BYTES,
                bytemuck::bytes_of(&object),
            );
            self.slots[slot as usize].object = object;
        }
        budget.remaining_bytes -= bytes;
        self.work.upload_bytes += bytes;
        if bytes > 0 {
            self.revision = self.revision.wrapping_add(1);
        }
        Ok(bytes)
    }
    pub fn release(
        &mut self,
        queue: &wgpu::Queue,
        handle: ProxyHandle,
        budget: &mut UploadBudget,
    ) -> Result<(), ProxyError> {
        let slot = self
            .slots
            .get_mut(handle.slot as usize)
            .filter(|s| s.generation == handle.generation && s.span.is_some())
            .ok_or(ProxyError::StaleHandle)?;
        if budget.remaining_bytes < OBJECT_BYTES {
            return Err(ProxyError::UploadBudget);
        }
        let span = slot.span.take().unwrap();
        slot.object.data[3] = 0;
        queue.write_buffer(
            &self.objects,
            u64::from(handle.slot) * OBJECT_BYTES,
            bytemuck::bytes_of(&slot.object),
        );
        self.free_span(span);
        self.free_slots.push(handle.slot);
        budget.remaining_bytes -= OBJECT_BYTES;
        self.work.upload_bytes += OBJECT_BYTES;
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }
    fn free_span(&mut self, span: Span) {
        self.free_spans.push(span);
        self.free_spans.sort_unstable_by_key(|s| s.start);
        let mut index = 0;
        while index + 1 < self.free_spans.len() {
            if self.free_spans[index].start + self.free_spans[index].len
                == self.free_spans[index + 1].start
            {
                self.free_spans[index].len += self.free_spans[index + 1].len;
                self.free_spans.remove(index + 1);
            } else {
                index += 1;
            }
        }
    }
    pub fn prepare_frame(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        camera: ProxyCamera,
    ) -> Result<ProxyWork, ProxyError> {
        let focal = |row: usize| {
            (0..3)
                .map(|axis| f64::from(camera.camera.view_projection[axis][row]).powi(2))
                .sum::<f64>()
                .sqrt()
        };
        let width = (f64::from(camera.viewport_height) * focal(1) / focal(0)).round() as u32;
        self.prepare_frame_extent(queue, encoder, camera, width)
    }
    /// Explicit attachment width for asymmetric projections; the compatibility
    /// method above infers the aspect of standard symmetric camera matrices.
    pub fn prepare_frame_extent(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        camera: ProxyCamera,
        viewport_width: u32,
    ) -> Result<ProxyWork, ProxyError> {
        if viewport_width == 0
            || camera.viewport_height == 0
            || camera
                .eye
                .iter()
                .chain(camera.camera.view_projection.iter().flatten())
                .any(|v| !v.is_finite())
            || camera.orthographic_direction.is_some_and(|d| {
                d.iter().any(|v| !v.is_finite()) || d.iter().map(|v| v * v).sum::<f32>() < 0.000001
            })
        {
            return Err(ProxyError::InvalidCamera);
        }
        if let Some(counters) = &self.counters {
            encoder.clear_buffer(counters, 0, None);
        }
        if self.last_frame == Some((camera, self.revision, viewport_width)) {
            return Ok(ProxyWork::default());
        }
        let direction = camera.orthographic_direction.unwrap_or([0.; 3]);
        let uniform = CameraGpu {
            view_projection: camera.camera.view_projection,
            eye: [
                camera.eye[0],
                camera.eye[1],
                camera.eye[2],
                if camera.orthographic_direction.is_some() {
                    1.
                } else {
                    0.
                },
            ],
            direction: [direction[0], direction[1], direction[2], 0.],
            params: [
                self.config.max_objects,
                camera.viewport_height,
                viewport_width,
                0,
            ],
        };
        queue.write_buffer(&self.camera, 0, bytemuck::bytes_of(&uniform));
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("LOD object frustum + indirect compaction"),
                timestamp_writes: None,
            });
            pass.set_bind_group(0, &self.cull_bind, &[]);
            pass.set_pipeline(&self.reset);
            pass.dispatch_workgroups(1, 1, 1);
            pass.set_pipeline(&self.cull);
            pass.dispatch_workgroups(self.config.max_objects.div_ceil(64), 1, 1);
        }
        self.last_frame = Some((camera, self.revision, viewport_width));
        Ok(ProxyWork {
            cull_dispatches: 2,
            objects_considered: self.config.max_objects,
            camera_upload_bytes: 112,
            ..ProxyWork::default()
        })
    }
    pub fn ambient_mrt(&self) -> bool {
        self.ambient_mrt
    }
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) -> u32 {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.render_bind, &[]);
        pass.draw_indirect(&self.indirect, 0);
        pass.set_pipeline(&self.cutout_pipeline);
        pass.set_bind_group(0, &self.cutout_bind, &[]);
        pass.draw_indirect(&self.indirect, 16);
        2
    }
    pub fn counter_buffer(&self) -> Option<&wgpu::Buffer> {
        self.counters.as_ref()
    }
    pub fn indirect_buffer(&self) -> &wgpu::Buffer {
        &self.indirect
    }
    pub fn take_work(&mut self) -> ProxyWork {
        std::mem::take(&mut self.work)
    }
    pub fn visible_buffer(&self) -> &wgpu::Buffer {
        &self.visible
    }
    /// Primary-camera quad compaction, for diagnostics only. Shadow cameras must
    /// build their own list from the shared payload/object descriptors.
    pub fn cutout_visible_buffer(&self) -> &wgpu::Buffer {
        &self.cutout_visible
    }
    pub fn shadow_source(&self) -> ProxyShadowSource<'_> {
        ProxyShadowSource {
            objects: &self.objects,
            payload: &self.payload,
            colors: &self.shadow_colors,
            sampler: &self.shadow_sampler,
            materials: &self.shadow_materials,
            max_objects: self.config.max_objects,
            max_payload_words: self.config.max_payload_words,
            atlas_layers: self.config.atlas_layers,
            revision: self.revision,
        }
    }
}
fn buffer_layout(
    binding: u32,
    visibility: wgpu::ShaderStages,
    uniform: bool,
    read_only: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: if uniform {
                wgpu::BufferBindingType::Uniform
            } else {
                wgpu::BufferBindingType::Storage { read_only }
            },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}
fn texture_layout(
    binding: u32,
    visibility: wgpu::ShaderStages,
    sample_type: wgpu::TextureSampleType,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        count: None,
    }
}
pub fn shader_source(
    shadows: bool,
    atmosphere: bool,
    ambient_mrt: bool,
    instrument: bool,
) -> String {
    let source = include_str!("lod_proxy.wgsl")
        .replace(
            "// INSTRUMENTATION",
            if instrument { COUNTERS } else { NO_COUNTERS },
        )
        .replace("// LIGHTING", "// SHADOWS");
    let source = if atmosphere {
        source.replace("// ILLUMINATION", "let light=object.light;// ILLUMINATION")
    } else {
        source
    };
    let source = crate::atmosphere::surface_shader(
        &source,
        shadows,
        atmosphere,
        true,
        if shadows {
            "return object.light.rgb+vec3(object.light.a*(0.15+0.85*cascade_shadow(world,normal)*max(dot(normal,normalize(shadow_sampling.sun.xyz)),0.0)));"
        } else {
            "return vec3(0.65+0.35*max(dot(normal,normalize(vec3(0.35,0.8,0.2))),0.0));"
        },
    );

    if !ambient_mrt {
        return source;
    }
    let output = "struct FragmentOut{@location(0)color:vec4<f32>,@builtin(frag_depth)depth:f32};";
    let color = "out.depth=clip.z/clip.w;";
    assert!(source.contains(output) && source.contains(color));
    let source=source.replace(output,"struct FragmentOut{@location(0)color:vec4<f32>,@builtin(frag_depth)depth:f32,@location(1)ambient:vec4<f32>};")
        .replace(color,"out.ambient=vec4(select(texel.rgb*tinted*proxy_ambient(object,world.xyz,world_normal),vec3(0.),kind==2u),out.color.a);out.depth=clip.z/clip.w;")
        .replace("return FragmentOut(vec4(0.,0.,0.,1.),-1.);","return FragmentOut(vec4(0.,0.,0.,1.),-1.,vec4(0.0));");
    let ambient = if atmosphere {
        "object.light.rgb+object.light.a*atmosphere_diffuse(normal)"
    } else if shadows {
        "object.light.rgb+vec3(object.light.a*0.15)"
    } else {
        "vec3(0.65)"
    };
    format!(
        "{source}\nfn proxy_ambient(object:Object,world:vec3<f32>,normal:vec3<f32>)->vec3<f32>{{return {ambient};}}"
    )
}

pub fn cutout_shader_source(shadows: bool, atmosphere: bool, ambient_mrt: bool) -> String {
    let source = include_str!("lod_cutout.wgsl");
    let source = if atmosphere {
        source.replace("// ILLUMINATION", "let light=object.light;// ILLUMINATION")
    } else {
        source.into()
    };
    let source = crate::atmosphere::surface_shader(
        &source,
        shadows,
        atmosphere,
        true,
        if shadows {
            "return object.light.rgb+vec3(object.light.a*(0.15+0.85*cascade_shadow(world,normal)*max(dot(normal,normalize(shadow_sampling.sun.xyz)),0.0)));"
        } else {
            "return vec3(0.65+0.35*max(dot(normal,normalize(vec3(0.35,0.8,0.2))),0.0));"
        },
    );
    let ambient = if atmosphere {
        "object.light.rgb+object.light.a*atmosphere_diffuse(normal)"
    } else if shadows {
        "object.light.rgb+vec3(object.light.a*0.15)"
    } else {
        "vec3(0.65)"
    };
    source
        .replace("// AMBIENT", &format!("return {ambient};"))
        .replace(
            "// OUTPUT",
            if ambient_mrt {
                "struct CutoutOutput{@location(0)color:vec4<f32>,@location(1)ambient:vec4<f32>};"
            } else {
                "struct CutoutOutput{@location(0)color:vec4<f32>};"
            },
        )
        .replace(
            "// RETURN",
            if ambient_mrt {
                "return CutoutOutput(vec4(color,texel.a),vec4(albedo*indirect,texel.a));"
            } else {
                "return CutoutOutput(vec4(color,texel.a));"
            },
        )
}

fn buffer_binding(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}
fn inverse(matrix: [[f32; 4]; 4]) -> Result<[[f32; 4]; 4], ProxyError> {
    if matrix.iter().flatten().any(|v| !v.is_finite())
        || matrix[0][3] != 0.
        || matrix[1][3] != 0.
        || matrix[2][3] != 0.
        || matrix[3][3] != 1.
    {
        return Err(ProxyError::InvalidTransform);
    }
    let mut rows = [[0f64; 8]; 4];
    for r in 0..4 {
        for (c, column) in matrix.iter().enumerate() {
            rows[r][c] = f64::from(column[r]);
        }
        rows[r][4 + r] = 1.;
    }
    for c in 0..4 {
        let pivot = (c..4)
            .max_by(|a, b| rows[*a][c].abs().total_cmp(&rows[*b][c].abs()))
            .unwrap();
        if rows[pivot][c].abs() < 1e-15 {
            return Err(ProxyError::InvalidTransform);
        }
        rows.swap(c, pivot);
        let scale = rows[c][c];
        for value in &mut rows[c] {
            *value /= scale;
        }
        for r in 0..4 {
            if r != c {
                let scale = rows[r][c];
                for k in 0..8 {
                    rows[r][k] -= scale * rows[c][k];
                }
            }
        }
    }
    let output = std::array::from_fn(|c| std::array::from_fn(|r| rows[r][4 + c] as f32));
    if output.iter().flatten().any(|v| !v.is_finite()) {
        return Err(ProxyError::InvalidTransform);
    }
    Ok(output)
}
fn bounds(model: [[f32; 4]; 4]) -> ([f32; 3], [f32; 3]) {
    let mut low = [f32::INFINITY; 3];
    let mut high = [f32::NEG_INFINITY; 3];
    for i in 0..8 {
        let p = [
            if i & 1 == 0 { 0. } else { 16. },
            if i & 2 == 0 { 0. } else { 16. },
            if i & 4 == 0 { 0. } else { 16. },
        ];
        for r in 0..3 {
            let value = model[3][r] + (0..3).map(|c| model[c][r] * p[c]).sum::<f32>();
            low[r] = low[r].min(value);
            high[r] = high[r].max(value);
        }
    }
    (low, high)
}
const COUNTERS: &str = r#"
@group(0) @binding(8) var<storage,read_write> stats:array<atomic<u32>,4>;
fn record_trace(steps:u32,hit:bool,budget:bool){atomicAdd(&stats[0],1u);atomicAdd(&stats[1],steps);atomicAdd(&stats[2],select(0u,1u,hit));atomicAdd(&stats[3],select(0u,1u,budget));}
"#;
const NO_COUNTERS: &str = "fn record_trace(steps:u32,hit:bool,budget:bool){}";
