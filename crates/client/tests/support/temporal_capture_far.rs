//! Authored public Far inputs for the complete capture graph. No generator or
//! production gate override: Height, Geometry and water use normal admission.
use client::view::ClientView;
use core_types::{Cell, PageId, SpaceId, Version};
use net::height::{GeometryStamp, HeightKey, HeightTile, Replacement};
use std::{collections::BTreeSet, sync::Arc};

const KEY: HeightKey = HeightKey {
    space: 0,
    level: 0,
    x: 0,
    z: -1,
};
const SOURCE: u64 = 2;
const GEOMETRY_SOURCE: Version = Version(42);

pub fn cell(x: i32, y: i32, z: i32) -> Cell {
    if y == 0 || (4..12).contains(&x) && (1..9).contains(&y) && (-12..-4).contains(&z) {
        Cell::from_parts(rules::ids::STONE, 0)
    } else if y == 1
        && ((4..12).contains(&x) && (4..12).contains(&z)
            || (1..4).contains(&x) && (-10..-6).contains(&z))
    {
        Cell::from_parts(rules::ids::WATER, 0)
    } else if [x, y, z] == [10, 1, 3] {
        Cell::from_parts(rules::ids::CHEST, 0)
    } else {
        Cell::AIR
    }
}

fn page(y: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x: 0,
        y,
        z: -1,
    }
}

fn tile(key: HeightKey) -> Box<HeightTile> {
    let mut known = [255; 37];
    known[36] = 1;
    let mut tile = HeightTile {
        key,
        source: if key == KEY { SOURCE } else { 1 },
        error: Some(net::height::HeightErrorBound {
            max_error_q1024: if key == KEY { 8 * 1024 } else { 0 },
            min_height: 1,
            max_height: if key == KEY { 9 } else { 1 },
        }),
        heights: [1; 289],
        known,
        atlas: [rules::ids::L_STONE as u16; 256],
        tint: [u32::MAX; 256],
        protected: [0; 32],
        replacement: Replacement::None,
    };
    if key == KEY {
        tile.protected.fill(255);
        tile.replacement = Replacement::Column {
            min_y: 0,
            max_y: 7,
            geometry: (0..8)
                .map(|y| GeometryStamp {
                    page: page(y),
                    source: GEOMETRY_SOURCE,
                    view: SOURCE,
                })
                .collect(),
        };
    }
    let bytes = net::height::encode(&tile).unwrap();
    Box::new(net::height::decode(&bytes).unwrap())
}

fn geometry(y: i32) -> net::PublicGeometry {
    let page = page(y);
    let mut occupied = [0; 512];
    let mut water = [false; 4096];
    for (index, is_water) in water.iter_mut().enumerate() {
        let value = cell(
            page.x * 16 + (index % 16) as i32,
            page.y * 16 + (index / 256) as i32,
            page.z * 16 + ((index / 16) % 16) as i32,
        );
        if value != Cell::AIR {
            occupied[index / 8] |= 1 << (index % 8);
            *is_water = value.id() == rules::ids::WATER;
        }
    }
    let has_water = water.iter().any(|&value| value);
    let mut palette = Vec::new();
    let mut indices = Vec::new();
    if occupied.iter().any(|&value| value != 0) {
        palette.push(net::PublicSurface {
            atlas_layer: rules::ids::L_STONE as u16,
            tint: u32::MAX,
            kind: net::PublicSurfaceKind::Opaque,
        });
        if has_water {
            palette.push(net::PublicSurface {
                atlas_layer: rules::ids::L_WATER as u16,
                tint: u32::MAX,
                kind: net::PublicSurfaceKind::Water,
            });
            // Public material indices are dense over all4096 local cells.
            indices.resize(512, 0);
            for (index, &value) in water.iter().enumerate() {
                if value {
                    indices[index / 8] |= 1 << (index % 8);
                }
            }
        }
    }
    let geometry = net::PublicGeometry {
        page,
        source_version: GEOMETRY_SOURCE,
        view_version: SOURCE,
        known: [255; 512],
        occupied,
        palette,
        index_bits: u8::from(has_water),
        indices,
    };
    geometry.validate().unwrap();
    geometry
}

fn halo(request: net::water::Request) -> Arc<net::water::WaterHalo> {
    request.validate().unwrap();
    let cells = std::array::from_fn(|index| {
        let value = cell(
            request.page.x * 16 + (index % 18) as i32 - 1,
            request.page.y * 16 + (index / 324) as i32 - 1,
            request.page.z * 16 + ((index / 18) % 18) as i32 - 1,
        );
        if value.id() == rules::ids::WATER {
            net::water::WaterCell::Water {
                amount: 8,
                falling: false,
            }
        } else if value == Cell::AIR {
            net::water::WaterCell::Air
        } else {
            net::water::WaterCell::Solid
        }
        .encode()
        .unwrap()
    });
    let image = net::water::WaterHalo {
        page: request.page,
        geometry_source: request.geometry_source,
        geometry_view: request.geometry_view,
        water_view: 1,
        column_source: request.column_source,
        cells: Box::new(cells),
        tints: [0xffff; 256],
    };
    let bytes = net::water::encode(&image).unwrap();
    Arc::from(net::water::decode(&bytes).unwrap())
}

#[derive(Debug)]
pub struct Fixture {
    heights: BTreeSet<HeightKey>,
    water: BTreeSet<(PageId, u64, u64, u64)>,
}
impl Fixture {
    pub fn install(view: &mut ClientView) -> Self {
        view.ingest_remote_height(tile(KEY)).unwrap();
        for y in 0..8 {
            view.ingest_remote_geometry(&geometry(y)).unwrap();
        }
        Self {
            heights: BTreeSet::new(),
            water: BTreeSet::new(),
        }
    }
    pub fn admit_wanted(&mut self, view: &mut ClientView) {
        for key in view.horizon_wanted_height().collect::<Vec<_>>() {
            if self.heights.insert(key) {
                assert!(self.heights.len() <= 64, "bounded authored Far selection");
                view.ingest_remote_height(tile(key)).unwrap();
            }
        }
        for request in view.horizon_wanted_water().collect::<Vec<_>>() {
            if self.water.insert((
                request.page,
                request.geometry_source.0,
                request.geometry_view,
                request.column_source,
            )) {
                assert!(self.water.len() <= 16, "bounded authored Far water column");
                view.ingest_remote_water(halo(request)).unwrap();
            }
        }
    }
    pub fn assert_committed(&self, view: &mut ClientView) {
        assert!(view.has_committed_far_front());
        let stats = view.horizon_stats().unwrap();
        assert!(stats.height_front > 0 && stats.proxy_front == 8 && stats.near_guard_ready);
        assert!(stats.projection_present && stats.committed_plan_id != 0);
        assert!(view.lod_error_setting_applied(32.));
        assert!(view.far_water_guard_ready());
        let water = view.horizon_water_stats().unwrap();
        assert!(water.front == 8 && water.front_quads > 0);
        assert_eq!(view.horizon_mut().unwrap().front_source(KEY), Some(SOURCE));
    }
}
