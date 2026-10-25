//! Page-local binary greedy terrain meshing.
//!
//! Input is one fully sampled 16³ page plus its one-cell rim. Sampling the world,
//! deciding what to do with Unknown neighbours, and validating input versions are
//! the caller's responsibility. No generation or global world access occurs here.
//! The raw input is classified once; visibility thereafter uses bit rows and AO
//! reads cached classifications. All counters state which work they measure.

use telemetry::WorkCounts;

pub const PAGE_EDGE: usize = 16;
pub const PAGE_VOLUME: usize = PAGE_EDGE * PAGE_EDGE * PAGE_EDGE;
pub const HALO_EDGE: usize = PAGE_EDGE + 2;
pub const HALO_VOLUME: usize = HALO_EDGE * HALO_EDGE * HALO_EDGE;
pub const HALO_RIM_CELLS: usize = HALO_VOLUME - PAGE_VOLUME;
pub const MAX_PAGE_QUADS: usize = PAGE_VOLUME * 6;
/// An edit can affect face visibility and AO of all 27 cells in its 3³ vicinity.
pub const MAX_CELL_PATCH_CELLS: usize = 27;
pub const MAX_CELL_PATCH_QUADS: usize = MAX_CELL_PATCH_CELLS * 6;
const ROWS: usize = HALO_EDGE * HALO_EDGE;

/// Minecraft direction order; matches the shared block registry convention.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Face {
    Down = 0,
    Up = 1,
    North = 2,
    South = 3,
    West = 4,
    East = 5,
}

impl Face {
    pub const ALL: [Self; 6] = [
        Self::Down,
        Self::Up,
        Self::North,
        Self::South,
        Self::West,
        Self::East,
    ];

    pub const fn from_index(index: u8) -> Option<Self> {
        match index {
            0 => Some(Self::Down),
            1 => Some(Self::Up),
            2 => Some(Self::North),
            3 => Some(Self::South),
            4 => Some(Self::West),
            5 => Some(Self::East),
            _ => None,
        }
    }

    pub const fn normal(self) -> [i8; 3] {
        match self {
            Self::Down => [0, -1, 0],
            Self::Up => [0, 1, 0],
            Self::North => [0, 0, -1],
            Self::South => [0, 0, 1],
            Self::West => [-1, 0, 0],
            Self::East => [1, 0, 0],
        }
    }

    /// U×V is the outward normal. Vertices 0,1,2,3 form a triangle strip.
    pub const fn tangent_u(self) -> [i8; 3] {
        match self {
            Self::Down | Self::Up | Self::South => [1, 0, 0],
            Self::North => [-1, 0, 0],
            Self::West => [0, 0, 1],
            Self::East => [0, 0, -1],
        }
    }

    pub const fn tangent_v(self) -> [i8; 3] {
        match self {
            Self::Down => [0, 0, 1],
            Self::Up => [0, 0, -1],
            _ => [0, 1, 0],
        }
    }

    fn cell(self, depth: usize, u: usize, v: usize) -> [i8; 3] {
        let (d, u, v) = (depth as i8, u as i8, v as i8);
        match self {
            Self::Down => [u, d, v],
            Self::Up => [u, d, 15 - v],
            Self::North => [15 - u, v, d],
            Self::South => [u, v, d],
            Self::West => [d, v, u],
            Self::East => [d, v, 15 - u],
        }
    }

    fn corner(self, cell: [i8; 3]) -> [u8; 3] {
        let [x, y, z] = cell;
        match self {
            Self::Down => [x as u8, y as u8, z as u8],
            Self::Up => [x as u8, (y + 1) as u8, (z + 1) as u8],
            Self::North => [(x + 1) as u8, y as u8, z as u8],
            Self::South => [x as u8, y as u8, (z + 1) as u8],
            Self::West => [x as u8, y as u8, z as u8],
            Self::East => [(x + 1) as u8, y as u8, (z + 1) as u8],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MeshClass {
    Empty,
    Opaque,
    Cutout,
    Water,
}

impl MeshClass {
    fn bucket(self) -> Option<usize> {
        match self {
            Self::Empty => None,
            Self::Opaque => Some(0),
            Self::Cutout => Some(1),
            Self::Water => Some(2),
        }
    }
}

/// Fixed storage for a single voxel's six faces. Each face keeps its stable
/// slot; Empty disables a prior face without uploading or rebuilding neighbours.
/// This is CPU transport data, not a new GPU quad format (quads remain 16 B).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnerPatch {
    pub owner: u16,
    pub quads: [QuadDescriptor; 6],
    pub classes: [MeshClass; 6],
}
impl OwnerPatch {
    pub fn from_mesh(owner: u16, mesh: &PageMesh) -> Option<Self> {
        if usize::from(owner) >= PAGE_VOLUME {
            return None;
        }
        let mut patch = Self {
            owner,
            quads: [QuadDescriptor { words: [0; 4] }; 6],
            classes: [MeshClass::Empty; 6],
        };
        for (class, quads) in [
            (MeshClass::Opaque, &mesh.opaque),
            (MeshClass::Cutout, &mesh.cutout),
            (MeshClass::Water, &mesh.water),
        ] {
            for quad in quads {
                let face = quad.face() as usize;
                if quad.size() != [1, 1] || patch.classes[face] != MeshClass::Empty {
                    return None;
                }
                patch.quads[face] = *quad;
                patch.classes[face] = class;
            }
        }
        Some(patch)
    }
}

/// Registry classification of one voxel, independent of page/world storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellMaterial {
    pub face_materials: [u16; 6],
    /// Clockwise UV quarter turns per Face; the low two bits are used.
    pub face_uv_rotations: [u8; 6],
    /// Packed RGBA8 tint, R in the low byte (WGSL pack4x8unorm convention).
    pub tint: u32,
    /// One bit per Face; untinted faces encode white instead of the biome tint.
    pub tint_faces: u8,
    pub class: MeshClass,
    pub occludes: bool,
    pub casts_ao: bool,
    /// Nonzero equal transparent groups hide their mutual internal faces.
    pub cull_group: u16,
}

impl CellMaterial {
    pub const EMPTY: Self = Self {
        face_materials: [0; 6],
        face_uv_rotations: [0; 6],
        tint: u32::MAX,
        tint_faces: 0x3f,
        class: MeshClass::Empty,
        occludes: false,
        casts_ao: false,
        cull_group: 0,
    };

    pub const fn opaque(material: u16) -> Self {
        Self {
            face_materials: [material; 6],
            face_uv_rotations: [0; 6],
            tint: u32::MAX,
            tint_faces: 0x3f,
            class: MeshClass::Opaque,
            occludes: true,
            casts_ao: true,
            cull_group: 0,
        }
    }
}

/// A complete immutable input after snapshot assembly, including edge/corner rim.
#[derive(Clone)]
pub struct PageHalo {
    cells: [u32; HALO_VOLUME],
}

impl PageHalo {
    pub const fn filled(cell: u32) -> Self {
        Self {
            cells: [cell; HALO_VOLUME],
        }
    }

    pub fn from_page(page: &[u32; PAGE_VOLUME], border: u32) -> Self {
        let mut halo = Self::filled(border);
        for y in 0..PAGE_EDGE {
            for z in 0..PAGE_EDGE {
                for x in 0..PAGE_EDGE {
                    halo.set(
                        x as i8,
                        y as i8,
                        z as i8,
                        page[x + PAGE_EDGE * (z + PAGE_EDGE * y)],
                    );
                }
            }
        }
        halo
    }

    pub fn set(&mut self, x: i8, y: i8, z: i8, cell: u32) {
        self.cells[halo_index(x, y, z)] = cell;
    }
    pub fn get(&self, x: i8, y: i8, z: i8) -> u32 {
        self.cells[halo_index(x, y, z)]
    }
    pub fn cells(&self) -> &[u32; HALO_VOLUME] {
        &self.cells
    }
}

fn halo_index(x: i8, y: i8, z: i8) -> usize {
    assert!(
        (-1..=16).contains(&x) && (-1..=16).contains(&y) && (-1..=16).contains(&z),
        "mesh halo coordinate outside -1..=16"
    );
    (x + 1) as usize + HALO_EDGE * ((z + 1) as usize + HALO_EDGE * (y + 1) as usize)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescriptorError {
    Origin,
    Size,
    AmbientOcclusion,
    Bounds,
    Face,
    Reserved,
    UvRotation,
}

/// GPU wire layout: geometry, material/AO, tint, object-table slot. Exactly 16 B.
///
/// word0: x/y/z 5 bits each, face 3, width-1 4, height-1 4 (6 reserved).
/// word1: material 16 bits, AO vertices 0..3 2 bits each, UV quarter turns 2 bits
/// at 24..25 (6 reserved).
/// word2: RGBA8 tint. word3: caller-assigned object-table slot, initially zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct QuadDescriptor {
    pub words: [u32; 4],
}

const _: () = assert!(std::mem::size_of::<QuadDescriptor>() == 16);

impl QuadDescriptor {
    pub fn new(
        origin: [u8; 3],
        face: Face,
        size: [u8; 2],
        material: u16,
        ao: [u8; 4],
        tint: u32,
    ) -> Result<Self, DescriptorError> {
        if origin.into_iter().any(|c| c > 16) {
            return Err(DescriptorError::Origin);
        }
        if size.into_iter().any(|s| !(1..=16).contains(&s)) {
            return Err(DescriptorError::Size);
        }
        if ao.into_iter().any(|a| a > 3) {
            return Err(DescriptorError::AmbientOcclusion);
        }
        let u = face.tangent_u();
        let v = face.tangent_v();
        for axis in 0..3 {
            let far = i16::from(origin[axis])
                + i16::from(u[axis]) * i16::from(size[0])
                + i16::from(v[axis]) * i16::from(size[1]);
            if !(0..=16).contains(&far) {
                return Err(DescriptorError::Bounds);
            }
        }
        let packed_ao = ao
            .into_iter()
            .enumerate()
            .fold(0, |bits, (i, a)| bits | (u32::from(a) << (i * 2)));
        Ok(Self {
            words: [
                u32::from(origin[0])
                    | (u32::from(origin[1]) << 5)
                    | (u32::from(origin[2]) << 10)
                    | ((face as u32) << 15)
                    | (u32::from(size[0] - 1) << 18)
                    | (u32::from(size[1] - 1) << 22),
                u32::from(material) | (packed_ao << 16),
                tint,
                0,
            ],
        })
    }

    pub const fn origin(self) -> [u8; 3] {
        [
            (self.words[0] & 31) as u8,
            ((self.words[0] >> 5) & 31) as u8,
            ((self.words[0] >> 10) & 31) as u8,
        ]
    }
    pub fn face(self) -> Face {
        Face::from_index(((self.words[0] >> 15) & 7) as u8).expect("validated quad face")
    }
    pub const fn size(self) -> [u8; 2] {
        [
            ((self.words[0] >> 18) & 15) as u8 + 1,
            ((self.words[0] >> 22) & 15) as u8 + 1,
        ]
    }
    pub const fn material(self) -> u16 {
        self.words[1] as u16
    }
    pub const fn tint(self) -> u32 {
        self.words[2]
    }
    pub const fn object(self) -> u32 {
        self.words[3]
    }
    pub const fn with_object(mut self, slot: u32) -> Self {
        self.words[3] = slot;
        self
    }
    pub const fn uv_rotation(self) -> u8 {
        ((self.words[1] >> 24) & 3) as u8
    }
    pub const fn with_uv_rotation(mut self, rotation: u8) -> Result<Self, DescriptorError> {
        if rotation > 3 {
            return Err(DescriptorError::UvRotation);
        }
        self.words[1] = (self.words[1] & !0x0300_0000) | ((rotation as u32) << 24);
        Ok(self)
    }
    pub const fn ao(self) -> [u8; 4] {
        [
            ((self.words[1] >> 16) & 3) as u8,
            ((self.words[1] >> 18) & 3) as u8,
            ((self.words[1] >> 20) & 3) as u8,
            ((self.words[1] >> 22) & 3) as u8,
        ]
    }

    /// Vertex-pulling reference. The GPU must use this same 0,1,2,3 strip order.
    pub fn vertex(self, index: u8) -> [i16; 3] {
        assert!(index < 4, "quad has four triangle-strip vertices");
        let origin = self.origin();
        let [width, height] = self.size();
        let u = self.face().tangent_u();
        let v = self.face().tangent_v();
        std::array::from_fn(|axis| {
            i16::from(origin[axis])
                + i16::from(u[axis]) * i16::from(width) * i16::from(index & 1)
                + i16::from(v[axis]) * i16::from(height) * i16::from(index >> 1)
        })
    }

    /// Texture coordinates before repeat sampling, matching the GPU rotation.
    /// Integer translation preserves the phase of repeated greedy rectangles.
    pub fn vertex_uv(self, index: u8) -> [f32; 2] {
        assert!(index < 4, "quad has four triangle-strip vertices");
        let [width, height] = self.size();
        let u = f32::from(width) * f32::from(index & 1);
        let v = f32::from(height) * f32::from(index >> 1);
        match self.uv_rotation() {
            0 => [u, v],
            1 => [v, 1.0 - u],
            2 => [1.0 - u, 1.0 - v],
            _ => [1.0 - v, u],
        }
    }

    pub fn to_le_bytes(self) -> [u8; 16] {
        let mut bytes = [0; 16];
        for (word, out) in self.words.into_iter().zip(bytes.chunks_exact_mut(4)) {
            out.copy_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    pub fn from_le_bytes(bytes: [u8; 16]) -> Result<Self, DescriptorError> {
        let mut words = [0; 4];
        for (word, source) in words.iter_mut().zip(bytes.chunks_exact(4)) {
            *word = u32::from_le_bytes(source.try_into().expect("four bytes"));
        }
        if words[0] & 0xfc00_0000 != 0 || words[1] & 0xfc00_0000 != 0 {
            return Err(DescriptorError::Reserved);
        }
        let descriptor = Self { words };
        let face = Face::from_index(((words[0] >> 15) & 7) as u8).ok_or(DescriptorError::Face)?;
        let mut validated = Self::new(
            descriptor.origin(),
            face,
            descriptor.size(),
            descriptor.material(),
            descriptor.ao(),
            descriptor.tint(),
        )?;
        validated = validated.with_uv_rotation(descriptor.uv_rotation())?;
        validated.words[3] = descriptor.words[3];
        Ok(validated)
    }
}

#[derive(Debug, Default)]
pub struct PageMesh {
    pub opaque: Vec<QuadDescriptor>,
    pub cutout: Vec<QuadDescriptor>,
    pub water: Vec<QuadDescriptor>,
}

impl PageMesh {
    pub fn with_capacity(capacity: [usize; 3]) -> Self {
        Self {
            opaque: Vec::with_capacity(capacity[0]),
            cutout: Vec::with_capacity(capacity[1]),
            water: Vec::with_capacity(capacity[2]),
        }
    }
    pub fn clear(&mut self) {
        self.opaque.clear();
        self.cutout.clear();
        self.water.clear();
    }
    pub fn quad_count(&self) -> usize {
        self.opaque.len() + self.cutout.len() + self.water.len()
    }
    pub fn upload_bytes(&self) -> usize {
        self.quad_count() * std::mem::size_of::<QuadDescriptor>()
    }
    pub fn iter(&self) -> impl Iterator<Item = &QuadDescriptor> {
        self.opaque.iter().chain(&self.cutout).chain(&self.water)
    }
    fn push(&mut self, bucket: usize, quad: QuadDescriptor) {
        match bucket {
            0 => self.opaque.push(quad),
            1 => self.cutout.push(quad),
            2 => self.water.push(quad),
            _ => unreachable!(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MeshWork {
    pub work: WorkCounts,
    /// CPU descriptor payload size, not a claim that a GPU upload occurred.
    pub generated_bytes: u64,
    pub rim_cells_scanned: u32,
    pub visibility_words: u32,
    pub cached_material_lookups: u64,
    pub ao_samples: u64,
}

/// Reusable worker-local memory; not retained for every world page.
pub struct MeshScratch {
    materials: Vec<CellMaterial>,
    occupied: [u32; ROWS],
    occluding: [u32; ROWS],
    visibility: [[[u16; PAGE_EDGE]; PAGE_EDGE]; 6],
    keys: [FaceKey; PAGE_EDGE * PAGE_EDGE],
}

impl Default for MeshScratch {
    fn default() -> Self {
        Self::new()
    }
}
impl MeshScratch {
    pub fn new() -> Self {
        Self {
            materials: vec![CellMaterial::EMPTY; HALO_VOLUME],
            occupied: [0; ROWS],
            occluding: [0; ROWS],
            visibility: [[[0; PAGE_EDGE]; PAGE_EDGE]; 6],
            keys: [FaceKey::EMPTY; PAGE_EDGE * PAGE_EDGE],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FaceKey {
    material: u16,
    uv_rotation: u8,
    tint: u32,
    ao: u8,
    bucket: u8,
}

impl FaceKey {
    const EMPTY: Self = Self {
        material: 0,
        uv_rotation: 0,
        tint: 0,
        ao: 0,
        bucket: 0,
    };
    fn ambient(self) -> [u8; 4] {
        [
            self.ao & 3,
            (self.ao >> 2) & 3,
            (self.ao >> 4) & 3,
            (self.ao >> 6) & 3,
        ]
    }
    fn uniform_ao(self) -> bool {
        let a = self.ao & 3;
        self.ao == a * 0b0101_0101
    }
}

fn cached(scratch: &MeshScratch, position: [i8; 3], work: &mut MeshWork) -> CellMaterial {
    work.cached_material_lookups += 1;
    scratch.materials[halo_index(position[0], position[1], position[2])]
}

fn offset(position: [i8; 3], direction: [i8; 3]) -> [i8; 3] {
    std::array::from_fn(|axis| position[axis] + direction[axis])
}

fn face_visible(material: CellMaterial, neighbour: CellMaterial) -> bool {
    material.class != MeshClass::Empty
        && !neighbour.occludes
        && !(material.cull_group != 0
            && material.class == neighbour.class
            && material.cull_group == neighbour.cull_group)
}

/// Shared by the full-page greedy worker and the synchronous edit patch.
fn face_key_from_samples(
    material: CellMaterial,
    face: Face,
    cell: [i8; 3],
    mut sample: impl FnMut([i8; 3]) -> CellMaterial,
) -> FaceKey {
    let exterior = offset(cell, face.normal());
    let u = face.tangent_u();
    let v = face.tangent_v();
    let mut ambient = 0;
    for vertex in 0..4 {
        let su = if vertex & 1 == 0 { -1 } else { 1 };
        let sv = if vertex & 2 == 0 { -1 } else { 1 };
        let du = u.map(|value| value * su);
        let dv = v.map(|value| value * sv);
        let side_u = sample(offset(exterior, du)).casts_ao;
        let side_v = sample(offset(exterior, dv)).casts_ao;
        let corner = sample(offset(offset(exterior, du), dv)).casts_ao;
        let value = if side_u && side_v {
            0
        } else {
            3 - u8::from(side_u) - u8::from(side_v) - u8::from(corner)
        };
        ambient |= value << (vertex * 2);
    }
    FaceKey {
        material: material.face_materials[face as usize],
        uv_rotation: material.face_uv_rotations[face as usize] & 3,
        tint: if material.tint_faces & (1 << face as u8) != 0 {
            material.tint
        } else {
            u32::MAX
        },
        ao: ambient,
        bucket: material.class.bucket().expect("visible cell is not empty") as u8,
    }
}

fn face_key(scratch: &MeshScratch, face: Face, cell: [i8; 3], work: &mut MeshWork) -> FaceKey {
    let material = cached(scratch, cell, work);
    work.ao_samples += 12;
    face_key_from_samples(material, face, cell, |position| {
        cached(scratch, position, work)
    })
}

fn face_quad(face: Face, cell: [i8; 3], size: [u8; 2], key: FaceKey) -> QuadDescriptor {
    QuadDescriptor::new(
        face.corner(cell),
        face,
        size,
        key.material,
        key.ambient(),
        key.tint,
    )
    .expect("face rectangle lies inside one page")
    .with_uv_rotation(key.uv_rotation)
    .expect("two-bit UV rotation")
}

fn classify(
    input: &PageHalo,
    classifier: impl Fn(u32, [i8; 3]) -> CellMaterial,
    scratch: &mut MeshScratch,
    work: &mut MeshWork,
) {
    scratch.occupied.fill(0);
    scratch.occluding.fill(0);
    scratch.visibility.fill([[0; PAGE_EDGE]; PAGE_EDGE]);
    for (index, raw) in input.cells.iter().copied().enumerate() {
        let coordinates = [
            (index % HALO_EDGE) as i8 - 1,
            (index / (HALO_EDGE * HALO_EDGE)) as i8 - 1,
            ((index / HALO_EDGE) % HALO_EDGE) as i8 - 1,
        ];
        let material = classifier(raw, coordinates);
        scratch.materials[index] = material;
        let row = index / HALO_EDGE;
        let bit = 1 << (index % HALO_EDGE);
        if material.class != MeshClass::Empty {
            scratch.occupied[row] |= bit;
        }
        if material.occludes {
            scratch.occluding[row] |= bit;
        }
        work.work.cells_scanned += 1;
    }
    work.rim_cells_scanned = HALO_RIM_CELLS as u32;
}

/// Build all six directional face sets using 16-bit row Boolean operations.
/// X-directed planes are transposed into the same U/V layout as the other faces.
fn visibility(scratch: &mut MeshScratch, work: &mut MeshWork) {
    for y in 0..PAGE_EDGE {
        for z in 0..PAGE_EDGE {
            let row = z + 1 + HALO_EDGE * (y + 1);
            let current = (scratch.occupied[row] >> 1) & 0xffff;
            scratch.visibility[Face::Down as usize][y][z] =
                (current & !(scratch.occluding[row - HALO_EDGE] >> 1)) as u16;
            scratch.visibility[Face::Up as usize][y][15 - z] =
                (current & !(scratch.occluding[row + HALO_EDGE] >> 1)) as u16;
            scratch.visibility[Face::North as usize][z][y] =
                ((current & !(scratch.occluding[row - 1] >> 1)) as u16).reverse_bits();
            scratch.visibility[Face::South as usize][z][y] =
                (current & !(scratch.occluding[row + 1] >> 1)) as u16;
            let mut west = (current & !scratch.occluding[row]) as u16;
            while west != 0 {
                let x = west.trailing_zeros() as usize;
                scratch.visibility[Face::West as usize][x][y] |= 1 << z;
                west &= west - 1;
            }
            let mut east = (current & !(scratch.occluding[row] >> 2)) as u16;
            while east != 0 {
                let x = east.trailing_zeros() as usize;
                scratch.visibility[Face::East as usize][x][y] |= 1 << (15 - z);
                east &= east - 1;
            }
            work.visibility_words += 6;
        }
    }
    // Transparent cull groups are an additional condition beyond opaque masks.
    for face in Face::ALL {
        for depth in 0..PAGE_EDGE {
            for v in 0..PAGE_EDGE {
                let mut bits = scratch.visibility[face as usize][depth][v];
                while bits != 0 {
                    let u = bits.trailing_zeros() as usize;
                    let cell = face.cell(depth, u, v);
                    let material = cached(scratch, cell, work);
                    let neighbour = cached(scratch, offset(cell, face.normal()), work);
                    if !face_visible(material, neighbour) {
                        scratch.visibility[face as usize][depth][v] &= !(1 << u);
                    }
                    bits &= bits - 1;
                }
            }
        }
    }
}

/// Mesh exactly one page. Existing vectors are cleared without dropping capacity.
///
/// `cells_scanned` counts actual raw input reads/classifications: exactly 18³,
/// including the 1736-cell rim. Cached visibility/AO work is reported separately;
/// output bytes are not recorded as GPU uploads because this function uploads none.
pub fn mesh_page(
    input: &PageHalo,
    classifier: impl Fn(u32) -> CellMaterial,
    scratch: &mut MeshScratch,
    output: &mut PageMesh,
) -> MeshWork {
    mesh_page_indexed(input, |raw, _| classifier(raw), scratch, output)
}

/// Position-aware classification for immutable biome tint/material snapshots.
/// Coordinates are local to the centre page, including the -1..=16 rim. The
/// callback is still invoked exactly once for every input sample.
pub fn mesh_page_indexed(
    input: &PageHalo,
    classifier: impl Fn(u32, [i8; 3]) -> CellMaterial,
    scratch: &mut MeshScratch,
    output: &mut PageMesh,
) -> MeshWork {
    output.clear();
    let mut work = MeshWork::default();
    classify(input, classifier, scratch, &mut work);
    visibility(scratch, &mut work);
    for face in Face::ALL {
        for depth in 0..PAGE_EDGE {
            for v in 0..PAGE_EDGE {
                let mut bits = scratch.visibility[face as usize][depth][v];
                while bits != 0 {
                    let u = bits.trailing_zeros() as usize;
                    scratch.keys[v * PAGE_EDGE + u] =
                        face_key(scratch, face, face.cell(depth, u, v), &mut work);
                    bits &= bits - 1;
                }
            }
            for v in 0..PAGE_EDGE {
                while scratch.visibility[face as usize][depth][v] != 0 {
                    let u = scratch.visibility[face as usize][depth][v].trailing_zeros() as usize;
                    let key = scratch.keys[v * PAGE_EDGE + u];
                    let mut width = 1;
                    if key.uniform_ao() {
                        while u + width < PAGE_EDGE
                            && scratch.visibility[face as usize][depth][v] & (1 << (u + width)) != 0
                            && scratch.keys[v * PAGE_EDGE + u + width] == key
                        {
                            width += 1;
                        }
                    }
                    let mask = (((1_u32 << width) - 1) << u) as u16;
                    let mut height = 1;
                    if key.uniform_ao() {
                        'rows: while v + height < PAGE_EDGE {
                            if scratch.visibility[face as usize][depth][v + height] & mask != mask {
                                break;
                            }
                            for x in u..u + width {
                                if scratch.keys[(v + height) * PAGE_EDGE + x] != key {
                                    break 'rows;
                                }
                            }
                            height += 1;
                        }
                    }
                    let quad = face_quad(
                        face,
                        face.cell(depth, u, v),
                        [width as u8, height as u8],
                        key,
                    );
                    output.push(key.bucket as usize, quad);
                    for row in v..v + height {
                        scratch.visibility[face as usize][depth][row] &= !mask;
                    }
                }
            }
        }
    }
    work.work.quads_emitted = output.quad_count() as u64;
    work.generated_bytes = output.upload_bytes() as u64;
    work
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellPatchError {
    TooManyCells,
    Coordinate,
    DuplicateCell,
    /// Capacity required for the opaque, cutout and water vectors respectively.
    Capacity {
        required: [usize; 3],
    },
}

/// Rebuild faces owned by at most 27 unique cells, without allocation or a halo.
///
/// Owner coordinates are page-local 0..15. The callback samples immutable current
/// classifications in -1..=16, including tint and per-face metadata. Exactly 27
/// callback reads occur per owner, even for empty owners; every face uses the same
/// visibility and AO helpers as `mesh_page_indexed`. Output is unit quads, at most
/// six per owner, with the same four-corner AO and triangle-strip diagonal.
///
/// Output is replaced only on success. Invalid owners and insufficient existing
/// vector capacities leave it untouched. No reserve/reallocation occurs here.
/// The sampler itself must not allocate for a fully allocation-free caller.
/// For one edit the caller must mask/rebuild its whole Chebyshev-radius-one owner
/// set (up to 27 cells, split over pages), because diagonal owners can change AO.
/// Larger coalesced owner sets may be partitioned into batches of 27.
pub fn mesh_cell_patch(
    cells: &[[u8; 3]],
    mut sample: impl FnMut([i8; 3]) -> CellMaterial,
    output: &mut PageMesh,
) -> Result<MeshWork, CellPatchError> {
    if cells.len() > MAX_CELL_PATCH_CELLS {
        return Err(CellPatchError::TooManyCells);
    }
    for (index, cell) in cells.iter().enumerate() {
        if cell.iter().any(|axis| *axis >= PAGE_EDGE as u8) {
            return Err(CellPatchError::Coordinate);
        }
        if cells[..index].contains(cell) {
            return Err(CellPatchError::DuplicateCell);
        }
    }
    let mut quads = [QuadDescriptor::default(); MAX_CELL_PATCH_QUADS];
    let mut buckets = [0_u8; MAX_CELL_PATCH_QUADS];
    let mut required = [0; 3];
    let mut count = 0;
    let mut work = MeshWork::default();
    for owner in cells {
        let cell = owner.map(|axis| axis as i8);
        let mut neighbourhood = [CellMaterial::EMPTY; 27];
        for dy in -1..=1_i8 {
            for dz in -1..=1_i8 {
                for dx in -1..=1_i8 {
                    let position = [cell[0] + dx, cell[1] + dy, cell[2] + dz];
                    let index = (dx + 1) as usize + 3 * ((dz + 1) as usize + 3 * (dy + 1) as usize);
                    neighbourhood[index] = sample(position);
                    work.work.cells_scanned += 1;
                    if position.iter().any(|axis| !(0..16).contains(axis)) {
                        work.rim_cells_scanned += 1;
                    }
                }
            }
        }
        let mut local = |position: [i8; 3]| {
            work.cached_material_lookups += 1;
            let delta = std::array::from_fn::<_, 3, _>(|axis| position[axis] - cell[axis] + 1);
            neighbourhood[delta[0] as usize + 3 * (delta[2] as usize + 3 * delta[1] as usize)]
        };
        let material = local(cell);
        if material.class == MeshClass::Empty {
            continue;
        }
        for face in Face::ALL {
            if !face_visible(material, local(offset(cell, face.normal()))) {
                continue;
            }
            let key = face_key_from_samples(material, face, cell, &mut local);
            quads[count] = face_quad(face, cell, [1, 1], key);
            buckets[count] = key.bucket;
            required[key.bucket as usize] += 1;
            count += 1;
        }
    }
    let capacities = [
        output.opaque.capacity(),
        output.cutout.capacity(),
        output.water.capacity(),
    ];
    if (0..3).any(|bucket| capacities[bucket] < required[bucket]) {
        return Err(CellPatchError::Capacity { required });
    }
    output.clear();
    for index in 0..count {
        output.push(buckets[index] as usize, quads[index]);
    }
    work.ao_samples = count as u64 * 12;
    work.work.quads_emitted = count as u64;
    work.generated_bytes = output.upload_bytes() as u64;
    Ok(work)
}
