use core_types::{SpaceId, Tick};
use net::entities::*;
fn pose(id: u64) -> EntityPose {
    EntityPose {
        id,
        space: SpaceId(0),
        position: [1_000_000_000.125, 1.0, 0.5],
        body_yaw: 0.0,
        head_yaw: 0.0,
        pitch: 0.0,
        limb_phase: 0.0,
        limb_amount: 0.0,
        death_roll: 0.0,
        scale: 1.0,
        extra: 0.0,
        kind: 0,
        flags: 64,
        color: 0,
        count: 0,
        damage: 0,
    }
}
fn frame() -> EntityFrame {
    EntityFrame {
        revision: 1,
        base: None,
        tick: Tick(4),
        upserts: vec![pose(1)],
        removes: Vec::new(),
    }
}
#[test]
fn public_pose_codec_preserves_integer_boundary_and_exact_bounded_layout() {
    let f = frame();
    let bytes = encode_frame(&f).unwrap();
    assert_eq!(bytes.len(), 36 + POSE_BYTES);
    assert_eq!(decode_frame(&bytes).unwrap(), f);
    let mut f = frame();
    f.upserts = (0..1000).map(pose).collect();
    let bytes = encode_frame(&f).unwrap();
    assert_eq!(bytes.len(), 80_036);
    assert_eq!(decode_frame(&bytes).unwrap().upserts.len(), 1000);
}
#[test]
fn request_codec_ack_and_full_are_strict_and_distinct() {
    for request in [
        EntityRequest::Ack { revision: 3 },
        EntityRequest::Full { have: None },
        EntityRequest::Full { have: Some(3) },
    ] {
        assert_eq!(
            decode_request(&encode_request(request).unwrap()).unwrap(),
            request
        );
    }
    assert!(encode_request(EntityRequest::Ack { revision: 0 }).is_err());
    let mut bytes = encode_request(EntityRequest::Ack { revision: 1 }).unwrap();
    bytes[4] = 2;
    assert!(decode_request(&bytes).is_err());
    assert!(decode_request(&bytes[..12]).is_err());
}
#[test]
fn malformed_count_reserved_nonfinite_duplicate_and_lifecycle_are_rejected() {
    let mut bytes = encode_frame(&frame()).unwrap();
    bytes[28..32].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decode_frame(&bytes).is_err());
    let mut bytes = encode_frame(&frame()).unwrap();
    bytes[36 + 71] = 1;
    assert!(decode_frame(&bytes).is_err());
    let mut f = frame();
    f.upserts[0].position[0] = f64::NAN;
    assert!(encode_frame(&f).is_err());
    let mut f = frame();
    f.upserts.push(pose(1));
    assert!(encode_frame(&f).is_err());
    let mut f = frame();
    f.base = Some(1);
    f.revision = 2;
    f.removes = vec![1];
    assert!(encode_frame(&f).is_err());
    f.upserts.clear();
    f.removes = vec![9];
    assert!(resulting_ids(&f, [1], 5).is_err());
    assert!(decode_frame(&encode_frame(&frame()).unwrap()[..100]).is_err());
}
#[test]
fn hidden_opaque_fields_have_no_public_codec_slot_and_kind_attributes_validate() {
    let mut f = frame();
    f.upserts[0].flags = 256;
    assert!(encode_frame(&f).is_err());
    let mut f = frame();
    f.upserts[0].kind = 6;
    f.upserts[0].flags = 32;
    f.upserts[0].color = 3;
    f.upserts[0].count = 64;
    assert!(encode_frame(&f).is_ok());
    f.upserts[0].count = 65;
    assert!(encode_frame(&f).is_err());
    f.upserts[0] = pose(1);
    f.upserts[0].kind = 3;
    f.upserts[0].color = f32::NAN.to_bits();
    assert!(encode_frame(&f).is_err());
    let mut bytes = encode_frame(&frame()).unwrap();
    bytes.push(0);
    assert!(decode_frame(&bytes).is_err());
}
