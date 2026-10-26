#[path = "../src/props.rs"]
#[allow(dead_code)]
mod props;
use core_types::{LocalIdx, PageId, SpaceId, Tick, Version};
use props::*;
fn index(n: u16) -> LocalIdx {
    LocalIdx::new(n).unwrap()
}
fn frame() -> Frame {
    Frame {
        page: PageId {
            space: SpaceId(0),
            x: -1024,
            y: 2,
            z: 7,
        },
        cell_view: 5,
        view: 7,
        source: Version(11),
        tick: Tick(100),
        hints: vec![
            Hint::from_visual(index(3), Kind::Chest, None, false, Some(0.75), None).unwrap(),
            Hint::from_visual(
                index(7),
                Kind::Wind,
                Some(0o543210),
                true,
                None,
                Some((1.25, 0.08)),
            )
            .unwrap(),
        ],
    }
}
#[test]
fn fixed_public_visual_packet_roundtrips_without_private_fields() {
    let frame = frame();
    let bytes = frame.encode().unwrap();
    assert_eq!(bytes.len(), HEADER_BYTES + 2 * HINT_BYTES);
    assert_eq!(Frame::decode(&bytes).unwrap(), frame);
    assert_eq!(
        Request::decode(
            &Request {
                page: frame.page,
                known: Some(7)
            }
            .encode()
        )
        .unwrap(),
        Request {
            page: frame.page,
            known: Some(7)
        }
    );
    for n in 0..bytes.len() {
        assert!(Frame::decode(&bytes[..n]).is_err());
    }
    let mut extra = bytes.clone();
    extra.push(0);
    assert!(Frame::decode(&extra).is_err());
}
#[test]
fn stale_order_reserved_fields_and_nonfinite_pose_are_rejected() {
    let mut sample = frame();
    sample.hints.swap(0, 1);
    assert!(sample.encode().is_err());
    let mut hint = frame().hints[0];
    hint.ports = 1;
    assert!(hint.validate().is_err());
    assert!(
        Hint::from_visual(
            index(0),
            Kind::Wind,
            None,
            true,
            None,
            Some((f32::NAN, 0.1))
        )
        .is_err()
    );
    assert!(Hint::from_visual(index(0), Kind::Machine, None, true, Some(0.5), None).is_err());
}
#[test]
fn rotor_epoch_interpolates_without_repeated_server_packets() {
    let hint =
        Hint::from_visual(index(0), Kind::Wind, None, true, None, Some((1.25, 0.08))).unwrap();
    let actual = hint.rotor_at(Tick(100), Tick(120), 0.5);
    let expected = (1.25 + 20.5 * 0.08) % std::f32::consts::TAU;
    assert!((actual - expected).abs() < 0.002);
    let mut all = frame();
    all.hints = (0..4096)
        .map(|n| Hint::from_visual(index(n), Kind::Chest, None, false, Some(0.), None).unwrap())
        .collect();
    let bytes = all.encode().unwrap();
    assert_eq!(bytes.len(), MAX_BYTES);
    assert_eq!(Frame::decode(&bytes).unwrap(), all);
}

#[test]
fn connector_colors_use_spare_bits_and_preserve_zero_color_bytes() {
    for color in 0..=18u32 {
        let packed = 0xA55 | (color << 12);
        let hint =
            Hint::from_visual(index(0), Kind::Connector, Some(packed), false, None, None).unwrap();
        let mut sample = frame();
        sample.hints = vec![hint];
        let bytes = sample.encode().unwrap();
        assert_eq!(
            &bytes[HEADER_BYTES + 4..HEADER_BYTES + 8],
            &[
                0x55,
                0x0A | ((color as u8 & 15) << 4),
                (color >> 4) as u8,
                0
            ]
        );
        assert_eq!(Frame::decode(&bytes).unwrap(), sample);
    }
    for color in 19..32u32 {
        assert!(
            Hint::from_visual(
                index(0),
                Kind::Connector,
                Some(color << 12),
                false,
                None,
                None
            )
            .is_err()
        );
    }
    for bit in 17..32 {
        assert!(
            Hint::from_visual(index(0), Kind::Connector, Some(1 << bit), false, None, None)
                .is_err()
        );
    }
}
