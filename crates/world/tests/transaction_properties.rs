#![cfg(not(target_arch = "wasm32"))]
use core_types::{BlockPos, Cell, CellProperties, PAGE_VOLUME, PageId, SpaceId, Tick};
use proptest::prelude::*;
use world::{CellRead, CommitError, Transaction, World};

fn properties(cell: Cell) -> CellProperties {
    if cell.id() == 0 {
        CellProperties::AIR
    } else {
        CellProperties {
            shape: cell.raw(),
            ..CellProperties::SOLID
        }
    }
}
proptest! {
    #![proptest_config(ProptestConfig { cases: 48, failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.local/proptest/world-transactions.regressions")))), ..ProptestConfig::default() })]
    #[test]
    fn arbitrary_multi_page_transaction_matches_model_or_rolls_back(
        operations in prop::collection::vec((-16_i32..16, 0_i32..16, 0_i32..16, any::<u16>(), any::<u16>()), 0..250),
        reject in any::<bool>(),
    ) {
        let mut world = World::new(properties, 8);
        let mut initialize = Transaction::new(Tick(0), SpaceId(0));
        for x in [-1, 0] { initialize.create_page(PageId { space: SpaceId(0), x, y: 0, z: 0 }, vec![Cell::AIR; PAGE_VOLUME]); }
        world.commit(initialize).unwrap();
        let before = world.canonical_hash();
        let sequence = world.journal_next();
        let mut expected = vec![Cell::AIR; PAGE_VOLUME * 2];
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        for (x, y, z, id, state) in operations {
            let value = Cell::from_parts(id, state);
            tx.set_cell(BlockPos { space: SpaceId(0), x, y, z }, value);
            expected[((x + 16) + z * 32 + y * 32 * 16) as usize] = value;
        }
        if reject { tx.set_cell(BlockPos { space: SpaceId(0), x: 32, y: 0, z: 0 }, Cell::from_parts(1, 0)); }
        let result = world.commit(tx);
        if reject {
            prop_assert!(matches!(result, Err(CommitError::UnknownPage(_))));
            prop_assert_eq!(world.canonical_hash(), before);
            prop_assert_eq!(world.journal_next(), sequence);
        } else {
            let change = result.unwrap();
            prop_assert!(change.pages.len() <= 2);
            for y in 0..16 {
                for z in 0..16 {
                    for x in -16..16 {
                        prop_assert_eq!(world.read(BlockPos { space: SpaceId(0), x, y, z }), CellRead::Known(expected[((x + 16) + z * 32 + y * 32 * 16) as usize]));
                    }
                }
            }
        }
    }
}
