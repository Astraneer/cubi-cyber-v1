use core_types::{PageId, SpaceId, Version};
use net::water::{self, Error, HALO_CELLS, Request, WaterCell, WaterHalo};
fn halo() -> WaterHalo {
    WaterHalo {
        page: PageId {
            space: SpaceId(0),
            x: -3,
            y: 4,
            z: 7,
        },
        geometry_source: Version(19),
        geometry_view: 23,
        water_view: 31,
        column_source: 37,
        cells: Box::new(std::array::from_fn(|i| (i % 19) as u8)),
        tints: std::array::from_fn(|i| (i * 257) as u16),
    }
}
#[test]
fn classified_halo_roundtrips_every_legal_state_without_raw_world_payload() {
    let halo = halo();
    let bytes = water::encode(&halo).unwrap();
    assert_eq!(bytes.len(), 6396);
    assert!(bytes.len() < 7 * 1024);
    assert_eq!(*water::decode(&bytes).unwrap(), halo);
    for byte in 0..=18 {
        let cell = WaterCell::decode(byte).unwrap();
        assert_eq!(cell.encode().unwrap(), byte);
    }
    for byte in 19..=255 {
        assert_eq!(WaterCell::decode(byte), Err(Error::Cell));
    }
    assert_eq!(
        WaterCell::Water {
            amount: 0,
            falling: false
        }
        .encode(),
        Err(Error::Cell)
    );
    assert_eq!(
        WaterCell::Water {
            amount: 9,
            falling: true
        }
        .encode(),
        Err(Error::Cell)
    );
    assert_eq!(
        WaterCell::decode(10).unwrap(),
        WaterCell::Water {
            amount: 8,
            falling: false
        }
    );
    assert_eq!(
        WaterCell::decode(18).unwrap(),
        WaterCell::Water {
            amount: 8,
            falling: true
        }
    );
    assert_eq!(HALO_CELLS, 5832);
}
#[test]
fn malformed_packet_is_rejected_before_unbounded_allocation_or_ambiguous_binding() {
    let bytes = water::encode(&halo()).unwrap();
    for len in [0, 4, 20, 52, 52 + HALO_CELLS, bytes.len() - 1] {
        assert_eq!(water::decode(&bytes[..len]).unwrap_err(), Error::Length);
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(water::decode(&trailing).unwrap_err(), Error::Length);
    let mut bad = bytes.clone();
    bad[52] = 19;
    assert_eq!(water::decode(&bad).unwrap_err(), Error::Cell);
    for at in [20, 28, 36, 44] {
        let mut bad = bytes.clone();
        bad[at..at + 8].fill(0);
        assert_eq!(water::decode(&bad).unwrap_err(), Error::Version);
    }
    let mut bad = bytes;
    bad[4..8].fill(255);
    assert_eq!(water::decode(&bad).unwrap().page.space, SpaceId(u32::MAX));
    bad[8..12].copy_from_slice(&i32::MAX.to_le_bytes());
    assert_eq!(water::decode(&bad).unwrap_err(), Error::Bounds);
}
#[test]
fn requests_preserve_explicit_full_repair_and_exact_geometry_column_binding() {
    for acknowledged in [None, Some(43)] {
        let h = halo();
        let request = Request {
            page: h.page,
            geometry_source: h.geometry_source,
            geometry_view: h.geometry_view,
            column_source: h.column_source,
            acknowledged,
        };
        let bytes = water::encode_request(request).unwrap();
        assert_eq!(water::decode_request(&bytes).unwrap(), request);
        for end in [0, 4, bytes.len() - 1] {
            assert!(water::decode_request(&bytes[..end]).is_err());
        }
    }
    let h = halo();
    let bad = Request {
        page: h.page,
        geometry_source: h.geometry_source,
        geometry_view: h.geometry_view,
        column_source: h.column_source,
        acknowledged: Some(0),
    };
    assert_eq!(water::encode_request(bad), Err(Error::Version));
}
