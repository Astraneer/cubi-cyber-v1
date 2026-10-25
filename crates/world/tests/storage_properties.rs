#![cfg(not(target_arch = "wasm32"))]
use core_types::{Cell, LocalIdx, PAGE_VOLUME};
use proptest::prelude::*;
use world::{PageStorage, StorageMode};

#[test]
fn palette_packing_handles_every_bit_width_and_cross_word_boundary() {
    for unique in [2, 3, 5, 17, 33, 65, 129, 256] {
        let cells: Box<[Cell; PAGE_VOLUME]> = Box::new(std::array::from_fn(|i| {
            Cell::from_parts((i % unique) as u16, 0x8001)
        }));
        let storage = PageStorage::from_cells(cells.clone());
        assert_eq!(storage.mode(), StorageMode::Paletted);
        for (i, expected) in cells.iter().enumerate() {
            assert_eq!(
                storage.get(LocalIdx::new(i as u16).unwrap()),
                *expected,
                "unique={unique},index={i}"
            );
        }
    }
}

#[test]
fn dense_transition_has_hysteresis_and_uniform_collapse_is_lossless() {
    let mut storage = PageStorage::from_cells(Box::new([Cell::AIR; PAGE_VOLUME]));
    assert_eq!(storage.mode(), StorageMode::Uniform);
    assert_eq!(storage.payload_bytes(), 0);
    for index in 0..257_u16 {
        storage.set(
            LocalIdx::new(index).unwrap(),
            Cell::from_parts(index + 1, 0),
        );
    }
    assert_eq!(storage.mode(), StorageMode::Dense);
    for index in 128..257_u16 {
        storage.set(LocalIdx::new(index).unwrap(), Cell::AIR);
    }
    assert_eq!(storage.mode(), StorageMode::Dense); // 128 materials + air = 129.
    storage.set(LocalIdx::new(127).unwrap(), Cell::AIR);
    assert_eq!(storage.mode(), StorageMode::Paletted);
    for index in 0..127_u16 {
        storage.set(LocalIdx::new(index).unwrap(), Cell::AIR);
    }
    assert_eq!(storage.mode(), StorageMode::Uniform);
    assert!(storage.to_cells().iter().all(|cell| *cell == Cell::AIR));
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.local/proptest/world-storage.regressions")))), ..ProptestConfig::default() })]
    #[test]
    fn adaptive_storage_matches_plain_array(operations in prop::collection::vec((0_u16..4096, any::<u16>(), any::<u16>()), 1..1200)) {
        let mut expected = Box::new([Cell::AIR; PAGE_VOLUME]);
        let mut storage = PageStorage::from_cells(expected.clone());
        for (index, id, state) in operations {
            let at = LocalIdx::new(index).unwrap();
            let value = Cell::from_parts(id, state);
            prop_assert_eq!(storage.set(at, value), expected[usize::from(index)]);
            expected[usize::from(index)] = value;
            prop_assert_eq!(storage.get(at), value);
        }
        prop_assert_eq!(storage.to_cells(), expected);
    }
}
