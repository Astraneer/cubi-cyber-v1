//! Server-only private features composed with the public version-4 terrain.
//!
//! Production uses independent 256-bit private entropy. Exact donor entropy is
//! available only with the explicit `golden-tools` feature for parity tooling;
//! that tool feature must never be enabled by a production browser client.
#![forbid(unsafe_code)]

use core_types::{PageId, Version};
use terrain_gen::{GeneratedColumn, GeneratedPage, GenerationError, Generator, TerrainSeed};

mod secrets;

pub const AUTHORITY_VERSION: u32 = 1;
const CS: i32 = terrain_gen::COLUMN_EDGE;
const H: i32 = terrain_gen::PROFILE_HEIGHT;
const W: i32 = terrain_gen::SCRATCH_EDGE;
const PAD: i32 = terrain_gen::SCRATCH_PADDING;

/// A server secret. Obtain cryptographic entropy outside the deterministic
/// generator. Deliberately has no Debug, Display, Serialize or public key getter.
#[derive(Clone)]
pub struct PrivateSeed([u8; 32]);
impl PrivateSeed {
    pub const fn new(entropy: [u8; 32]) -> Self {
        Self(entropy)
    }
}

enum Entropy<'a> {
    Private(&'a [u8; 32]),
    #[cfg(feature = "golden-tools")]
    Legacy(u32),
}
impl Entropy<'_> {
    fn sample(&self, domain: u32, attempt: u32, x: i32, z: i32) -> u32 {
        match self {
            Self::Private(key) => {
                let mut input = [0u8; 16];
                for (out, field) in input
                    .chunks_exact_mut(4)
                    .zip([domain, attempt, x as u32, z as u32])
                {
                    out.copy_from_slice(&field.to_le_bytes());
                }
                let output = blake3::keyed_hash(key, &input);
                u32::from_le_bytes(
                    output.as_bytes()[..4]
                        .try_into()
                        .expect("four digest bytes"),
                )
            }
            #[cfg(feature = "golden-tools")]
            Self::Legacy(seed) => terrain_gen::compatibility::hash(*seed ^ domain, attempt, x, z),
        }
    }
}

struct Region<'a> {
    x0: i32,
    z0: i32,
    blocks: &'a mut terrain_gen::DraftBlocks,
}
impl Region<'_> {
    fn idx(x: i32, y: i32, z: i32) -> usize {
        ((z * W + x) * H + y) as usize
    }
}

pub struct ServerGenerator {
    terrain: Generator,
    private_seed: PrivateSeed,
}
impl ServerGenerator {
    pub fn new(terrain_seed: TerrainSeed, private_seed: PrivateSeed) -> Self {
        Self {
            terrain: Generator::new(terrain_seed),
            private_seed,
        }
    }
    pub fn terrain(&self) -> &Generator {
        &self.terrain
    }
    pub fn generate_column(&self, x: i32, z: i32) -> Result<GeneratedColumn, GenerationError> {
        compose(&self.terrain, &Entropy::Private(&self.private_seed.0), x, z)
    }
    pub fn generate_page(
        &self,
        page: PageId,
        expected: Option<Version>,
    ) -> Result<AuthorityPage, GenerationError> {
        Ok(AuthorityPage {
            page: self.generate_column(page.x, page.z)?.page(page, expected)?,
            authority_version: AUTHORITY_VERSION,
        })
    }
}

#[derive(Debug)]
pub struct AuthorityPage {
    pub page: GeneratedPage,
    pub authority_version: u32,
}

fn compose(
    terrain: &Generator,
    entropy: &Entropy<'_>,
    x: i32,
    z: i32,
) -> Result<GeneratedColumn, GenerationError> {
    let mut draft = terrain.draft_column(x, z)?;
    let tops = draft.height_field();
    let [x0, z0] = draft.origin();
    let mut region = Region {
        x0,
        z0,
        blocks: draft.blocks_mut(),
    };
    // Preserve donor order: lava, then ores, then trees/vegetation/spawn.
    secrets::place_lava_lake(&mut region, entropy, x, z, &tops);
    secrets::place_ores(&mut region, entropy, x, z, &tops);
    terrain.finish_column(draft)
}

/// Bounded authoritative job payload. The private seed stays in this server-only
/// package and is absent from client tasks and generated output.
pub struct AuthorityTask {
    pub terrain_seed: TerrainSeed,
    pub private_seed: PrivateSeed,
    pub page: PageId,
    pub expected: Option<Version>,
}
impl AuthorityTask {
    pub fn execute(self, cancel: &jobs::CancellationToken) -> Result<AuthorityPage, String> {
        if cancel.is_cancelled() {
            return Err(GenerationError::Cancelled.to_string());
        }
        let generator = ServerGenerator::new(self.terrain_seed, self.private_seed);
        let page = generator
            .generate_page(self.page, self.expected)
            .map_err(|e| e.to_string())?;
        if cancel.is_cancelled() {
            return Err(GenerationError::Cancelled.to_string());
        }
        Ok(page)
    }
}

#[cfg(feature = "golden-tools")]
pub const LEGACY_PROBE_REFERENCE: [u64; 4] = [
    0x433b_5b0e_f285_6c41,
    0x0e00_9b2a_a4b1_3193,
    0xf9b2_1370_b7f5_0024,
    0x45da_32de_09dd_965a,
];

/// Evaluated original donor probe, not a return of the expected constants.
#[cfg(feature = "golden-tools")]
pub fn legacy_probe_reference() -> [u64; 4] {
    let terrain = Generator::new(TerrainSeed(terrain_gen::PROBE_SEED));
    terrain_gen::PROBE_COLUMNS.map(|(x, z)| {
        compose(&terrain, &Entropy::Legacy(terrain_gen::PROBE_SEED), x, z)
            .expect("fixed probe valid")
            .legacy_hash()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rules::ids::*;
    fn is_ore(id: u16) -> bool {
        matches!(
            id,
            COAL_ORE | IRON_ORE | GOLD_ORE | REDSTONE_ORE | COPPER_ORE | OSMIUM_ORE | TIN_ORE
        )
    }
    #[test]
    fn private_features_deterministic_separate_and_not_public() {
        let a = ServerGenerator::new(TerrainSeed(1337), PrivateSeed::new([7; 32]));
        let b = ServerGenerator::new(TerrainSeed(1337), PrivateSeed::new([9; 32]));
        let mut ore_count = 0;
        let mut changed = 0;
        for (x, z) in [(0, 0), (7, -3), (-21, 12)] {
            let first = a.generate_column(x, z).unwrap();
            let second = a.generate_column(x, z).unwrap();
            let other = b.generate_column(x, z).unwrap();
            let public = a.terrain().generate_column(x, z).unwrap();
            assert_eq!(first.raw_blocks(), second.raw_blocks());
            assert!(
                public
                    .raw_blocks()
                    .iter()
                    .all(|&id| !is_ore(u16::from(id)) && u16::from(id) != LAVA)
            );
            for (&id, &different) in first.raw_blocks().iter().zip(other.raw_blocks()) {
                ore_count += usize::from(is_ore(u16::from(id)));
                changed += usize::from(id != different);
            }
            assert_eq!(first.tints(), other.tints());
        }
        assert!(ore_count > 0);
        assert!(changed > 0);
    }
    #[test]
    fn keyed_domains_and_attempts_are_distinct() {
        let key = [13; 32];
        let a = Entropy::Private(&key);
        let values = [
            a.sample(1, 1, 3, -2),
            a.sample(2, 1, 3, -2),
            a.sample(1, 2, 3, -2),
            a.sample(1, 1, 4, -2),
        ];
        for (i, x) in values.iter().enumerate() {
            assert!(values[i + 1..].iter().all(|y| x != y));
        }
    }
    #[cfg(feature = "golden-tools")]
    #[test]
    fn exact_donor_v4_probe() {
        assert_eq!(legacy_probe_reference(), LEGACY_PROBE_REFERENCE);
    }
}

#[cfg(test)]
mod seam_tests {
    use super::*;
    #[test]
    fn private_veins_cross_column_boundaries_consistently() {
        let generator = ServerGenerator::new(TerrainSeed(1337), PrivateSeed::new([5; 32]));
        let mut crossing = 0;
        for (x, z) in [(3, -2), (-7, 5), (20, 11), (-30, 40)] {
            let left = generator.generate_column(x, z).unwrap();
            let right = generator.generate_column(x + 1, z).unwrap();
            for lz in 1..17 {
                for y in 0..H {
                    let a = left.block_padded(16, y, lz);
                    let b = right.block_padded(1, y, lz);
                    crossing += usize::from(a == b && matches!(a, 45 | 46 | 57..=61));
                }
            }
        }
        assert!(
            crossing > 0,
            "private keyed veins retain cross-column continuity"
        );
    }
}
