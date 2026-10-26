use core_types::{PageId, SpaceId, Version};
use net::light::*;
fn page() -> PageId {
    PageId {
        space: SpaceId(0),
        x: -1,
        y: 8,
        z: 1024,
    }
}
fn header(cell_view: u64, version: u64, source: u64) -> Header {
    Header {
        page: page(),
        cell_view,
        version,
        source: Version(source),
    }
}
fn snapshot(cell_view: u64, version: u64, source: u64) -> Update {
    Update::Snapshot(Snapshot {
        header: header(cell_view, version, source),
        values: Box::new([pack(15, 7, [255, 128, 17]).unwrap(); 4096]),
    })
}
#[test]
fn actual_light_words_preserve_full_rgb_and_all_wire_prefixes_are_bounded() {
    for byte in 0..=255 {
        assert_eq!(
            unpack(pack(15, 13, [byte, 255 - byte, byte.rotate_left(3)]).unwrap()),
            (15, 13, [byte, 255 - byte, byte.rotate_left(3)])
        );
    }
    assert!(pack(16, 0, [0; 3]).is_none());
    assert!(pack(0, 16, [0; 3]).is_none());
    let full = snapshot(9, 10, 0);
    let bytes = encode(&full).unwrap();
    assert_eq!(bytes.len(), 49);
    assert_eq!(decode(&bytes).unwrap(), full);
    for end in 0..bytes.len() {
        assert!(decode(&bytes[..end]).is_err(), "prefix{end}");
    }
    let mut extra = bytes.clone();
    extra.push(0);
    assert_eq!(decode(&extra), Err(Error::Trailing));
    let delta = Update::Delta(Delta {
        header: header(10, 11, 5),
        base: 10,
        changes: vec![(0, pack(0, 0, [0; 3]).unwrap()), (4095, u32::MAX)],
    });
    let bytes = encode(&delta).unwrap();
    assert_eq!(bytes.len(), 55 + 2 * 6);
    assert_eq!(decode(&bytes).unwrap(), delta);
    let request = Request {
        page: page(),
        cell_view: 10,
        acknowledged: Some(11),
    };
    let bytes = encode_request(request).unwrap();
    assert_eq!(bytes.len(), MAX_REQUEST_BYTES);
    assert_eq!(decode_request(&bytes).unwrap(), request);
    for end in 0..bytes.len() {
        assert!(decode_request(&bytes[..end]).is_err());
    }
}

#[test]
fn uniform_full_images_keep_binding_and_accept_every_packed_bit_without_a_base() {
    for value in [0, u32::MAX, pack(2, 11, [8, 255, 193]).unwrap()] {
        let image = Update::Snapshot(Snapshot {
            header: header(17, 28, 31),
            values: Box::new([value; 4096]),
        });
        let bytes = encode(&image).unwrap();
        assert_eq!(&bytes[..5], b"LGT1\x02");
        assert_eq!(bytes.len(), 49);
        assert_eq!(&bytes[45..], &value.to_le_bytes());
        assert_eq!(decode(&bytes).unwrap(), image);
        let mut replica = Replica::new(1).unwrap();
        assert_eq!(
            replica.apply(decode(&bytes).unwrap(), Some(17)),
            Ok(Apply::Applied)
        );
        assert_eq!(
            replica.page(page()).unwrap().values.as_ref(),
            &[value; 4096]
        );
        for n in 0..bytes.len() {
            assert!(decode(&bytes[..n]).is_err());
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(decode(&trailing), Err(Error::Trailing));
        let mut bad = bytes.clone();
        bad[4] = 3;
        assert_eq!(decode(&bad), Err(Error::Malformed));
        let mut bad = bytes.clone();
        bad[21..29].fill(0);
        assert_eq!(decode(&bad), Err(Error::InvalidVersion));
    }
}

#[test]
fn original_dense_snapshot_bytes_stay_identical_and_legacy_uniform_is_readable() {
    let value = pack(15, 7, [255, 128, 17]).unwrap();
    // Original LGT1 kind0 header for page(0,-1,8,1024), view9/revision10/source0.
    let prefix = [
        76, 71, 84, 49, 0, 0, 0, 0, 0, 255, 255, 255, 255, 8, 0, 0, 0, 0, 4, 0, 0, 9, 0, 0, 0, 0,
        0, 0, 0, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    let mut legacy = prefix.to_vec();
    for _ in 0..4096 {
        legacy.extend_from_slice(&value.to_le_bytes());
    }
    assert_eq!(legacy.len(), 45 + 4096 * 4);
    assert_eq!(decode(&legacy).unwrap(), snapshot(9, 10, 0));
    let replacement = pack(0, 1, [18, 3, 4]).unwrap();
    legacy[45 + 4095 * 4..].copy_from_slice(&replacement.to_le_bytes());
    let dense = decode(&legacy).unwrap();
    assert_eq!(encode(&dense).unwrap(), legacy);
    for n in 0..legacy.len() {
        assert!(decode(&legacy[..n]).is_err(), "legacy dense prefix{n}");
    }
}
#[test]
fn missing_cell_and_light_bases_and_invalid_deltas_preserve_previous_replica() {
    let mut replica = Replica::new(1).unwrap();
    let initial = snapshot(5, 10, 7);
    assert!(matches!(
        replica.apply(initial.clone(), None),
        Err(Error::MissingCells { .. })
    ));
    assert!(replica.is_empty());
    assert!(matches!(
        replica.apply(initial.clone(), Some(4)),
        Err(Error::MissingCells { .. })
    ));
    assert!(replica.is_empty());
    assert_eq!(replica.apply(initial, Some(5)).unwrap(), Apply::Applied);
    let before = replica.page(page()).unwrap().clone();
    let update = |base, changes| {
        Update::Delta(Delta {
            header: header(6, 12, 9),
            base,
            changes,
        })
    };
    assert!(matches!(
        replica.apply(update(11, vec![(3, 0)]), Some(6)),
        Err(Error::MissingBase { .. })
    ));
    assert_eq!(replica.page(page()), Some(&before));
    for changes in [vec![(3, 0), (3, 1)], vec![(4096, 0)], vec![(4, 0), (3, 1)]] {
        assert_eq!(
            replica.apply(update(10, changes), Some(6)),
            Err(Error::InvalidDelta)
        );
        assert_eq!(replica.page(page()), Some(&before));
    }
    assert_eq!(
        replica.apply(update(10, vec![(3, 0)]), Some(6)).unwrap(),
        Apply::Applied
    );
    assert_eq!(replica.page(page()).unwrap().header.cell_view, 6);
    assert_eq!(replica.page(page()).unwrap().values[3], 0);
    assert_eq!(
        replica.apply(snapshot(5, 11, 8), Some(6)).unwrap(),
        Apply::Stale
    );
    assert_eq!(
        replica.apply(update(10, vec![(3, 0)]), Some(6)).unwrap(),
        Apply::Stale
    );
    assert_eq!(
        replica.apply(update(10, vec![(3, 1)]), Some(6)),
        Err(Error::ConflictingVersion)
    );
    assert!(replica.remove(page()));
    assert!(replica.is_empty());
    assert_eq!(
        replica.apply(update(10, vec![(3, 0)]), Some(7)).unwrap(),
        Apply::Stale,
        "old ABA epoch"
    );
    let delta = Update::Delta(Delta {
        header: header(7, 13, 10),
        base: 12,
        changes: vec![],
    });
    assert!(matches!(
        replica.apply(delta, Some(7)),
        Err(Error::MissingBase { .. })
    ));
    replica.apply(snapshot(7, 14, 10), Some(7)).unwrap();
    let mut other = match snapshot(7, 15, 10) {
        Update::Snapshot(s) => s,
        _ => unreachable!(),
    };
    other.header.page.x = 10;
    assert_eq!(
        replica.apply(Update::Snapshot(other), Some(7)),
        Err(Error::Capacity)
    );
    assert_eq!(replica.len(), 1);
}
#[test]
fn source_regression_and_conflicting_snapshot_are_rejected_without_partial_mutation() {
    let mut replica = Replica::new(1).unwrap();
    replica.apply(snapshot(2, 10, 9), Some(2)).unwrap();
    let before = replica.page(page()).unwrap().clone();
    assert_eq!(
        replica.apply(snapshot(2, 11, 8), Some(2)),
        Err(Error::InvalidVersion)
    );
    let mut forged = before.clone();
    forged.values[4095] = 0;
    assert_eq!(
        replica.apply(Update::Snapshot(forged), Some(2)),
        Err(Error::ConflictingVersion)
    );
    assert_eq!(replica.page(page()), Some(&before));
}
