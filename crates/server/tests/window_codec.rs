use core_types::{BlockPos, Cell, ComponentId, SpaceId};
use server::{
    ui,
    windows::{Binding, BoundWindow, Error},
};
fn bound() -> BoundWindow {
    let position = BlockPos {
        space: SpaceId(4),
        x: -8,
        y: 64,
        z: 17,
    };
    let cell = Cell::from_parts(rules::ids::CHEST, 0);
    let mut slots = [rules::stack::ItemStack::EMPTY; 27];
    slots[0] =
        rules::stack::ItemStack::from_parts(rules::items::block_item(rules::ids::STONE), 3, 0)
            .unwrap();
    BoundWindow {
        state: ui::WindowState::open(7, ui::WindowKind::Chest(27), &slots).unwrap(),
        binding: Some(Binding {
            target: position,
            expected_target: cell,
            anchor: position,
            expected_anchor: cell,
            component: Some(ComponentId(17)),
        }),
    }
}
#[test]
fn binding_tail_preserves_the_exact_wnd1_prefix_and_legacy_owner_state() {
    let image = bound();
    let bytes = image.encode();
    assert_eq!(&bytes[..407], image.state.encode());
    assert_eq!(bytes.len(), 461);
    assert_eq!(BoundWindow::decode(&bytes).unwrap(), image);
    let legacy = BoundWindow::decode(&bytes[..407]).unwrap();
    assert_eq!(legacy.state, image.state);
    assert_eq!(legacy.binding, None);
    let empty = BoundWindow::decode(&[]).unwrap();
    assert_eq!(empty.state.id(), 1);
    assert_eq!(empty.state.kind(), ui::WindowKind::Inventory);
    let id_zero = ui::WindowState::inventory().encode();
    assert_eq!(BoundWindow::decode(&id_zero).unwrap().state.id(), 0);
    let plain = BoundWindow::inventory(1).unwrap();
    assert_eq!(plain.encode().len(), 412);
    assert_eq!(BoundWindow::decode(&plain.encode()).unwrap(), plain);
}
#[test]
fn binding_codec_rejects_truncation_tails_bad_flags_coordinates_and_cross_space() {
    let original = bound().encode();
    for end in 1..original.len() {
        if end != 407 {
            assert!(
                BoundWindow::decode(&original[..end]).is_err(),
                "truncation {end}"
            );
        }
    }
    for (offset, value) in [(407, b'X'), (411, 2), (452, 2)] {
        let mut bad = original.clone();
        bad[offset] = value;
        assert_eq!(BoundWindow::decode(&bad), Err(Error::Encoding));
    }
    let mut bad = original.clone();
    bad[416..420].copy_from_slice(&i32::MAX.to_le_bytes());
    assert!(BoundWindow::decode(&bad).is_err());
    let mut bad = original.clone();
    bad[432..436].copy_from_slice(&5u32.to_le_bytes());
    assert!(BoundWindow::decode(&bad).is_err());
    let mut bad = original;
    bad.push(0);
    assert!(BoundWindow::decode(&bad).is_err());
    assert!(BoundWindow::decode(&vec![0; 4097]).is_err());
}
