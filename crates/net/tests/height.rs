use net::height::*;
fn tile() -> HeightTile {
    HeightTile {
        key: HeightKey {
            space: 0,
            level: 10,
            x: 0,
            z: 0,
        },
        source: 1,
        error: None,
        heights: [0; 289],
        known: [0; 37],
        atlas: [0; 256],
        tint: [0xffffffff; 256],
        protected: [0; 32],
        replacement: Replacement::None,
    }
}
#[test]
fn public_height_unknown_is_canonical_and_decoder_is_bounded() {
    let t = tile();
    let bytes = encode(&t).unwrap();
    assert_eq!(decode(&bytes).unwrap(), t);
    for n in 0..bytes.len() {
        assert!(decode(&bytes[..n]).is_err());
    }
    let mut bad = bytes.clone();
    bad.push(0);
    assert_eq!(decode(&bad), Err(HeightError::Trailing));
    let mut bad = t.clone();
    bad.heights[0] = 1;
    assert!(encode(&bad).is_err());
    let mut bad = t.clone();
    bad.known[36] = 2;
    assert!(encode(&bad).is_err());
    let mut bad = t;
    bad.atlas[1] = 186;
    assert!(encode(&bad).is_err());
}
#[test]
fn protected_holes_require_refinement_or_complete_source_and_view_manifest() {
    let mut t = tile();
    t.protected[0] = 1;
    assert!(encode(&t).is_err());
    t.replacement = Replacement::Refine;
    assert_eq!(decode(&encode(&t).unwrap()).unwrap(), t);
    assert_eq!(
        t.key.children().unwrap().map(|k| (k.level, k.x, k.z)),
        [(9, 0, 0), (9, 1, 0), (9, 0, 1), (9, 1, 1)]
    );
    t.key.level = 0;
    t.protected = [255; 32];
    t.replacement = Replacement::Column {
        min_y: 0,
        max_y: 7,
        geometry: Vec::new(),
    };
    assert_eq!(decode(&encode(&t).unwrap()).unwrap(), t);
    let Replacement::Column { geometry, .. } = &mut t.replacement else {
        unreachable!()
    };
    for y in 0..8 {
        geometry.push(GeometryStamp {
            page: core_types::PageId {
                space: core_types::SpaceId(0),
                x: 0,
                y,
                z: 0,
            },
            source: core_types::Version(1),
            view: 2,
        });
    }
    assert_eq!(decode(&encode(&t).unwrap()).unwrap(), t);
    let Replacement::Column { geometry, .. } = &mut t.replacement else {
        unreachable!()
    };
    geometry[0].view = 0;
    assert!(encode(&t).is_err());
}
#[test]
fn outer_height_messages_keep_tags_bounded_and_roundtrip_replacement_evidence() {
    let t = tile();
    for message in [
        net::ClientMessage::RequestHeight {
            key: t.key,
            known: None,
        },
        net::ClientMessage::RequestHeight {
            key: t.key,
            known: Some(7),
        },
        net::ClientMessage::ForgetHeight(t.key),
    ] {
        let bytes = net::encode_client(&message).unwrap();
        assert!(bytes.len() < 64);
        assert_eq!(net::decode_client(&bytes).unwrap(), message);
    }
    for message in [
        net::ServerMessage::Height(Box::new(t.clone())),
        net::ServerMessage::HeightUnavailable(t.key),
    ] {
        let bytes = net::encode_server(&message).unwrap();
        assert!(bytes.len() < MAX_HEIGHT_BYTES + 32);
        assert_eq!(net::decode_server(&bytes).unwrap(), message);
    }
    assert!(
        net::encode_client(&net::ClientMessage::RequestHeight {
            key: t.key,
            known: Some(0)
        })
        .is_err()
    );
}

#[test]
fn height_error_is_source_bound_quantized_metadata_and_unknown_is_distinct() {
    let mut t = tile();
    let without = encode(&t).unwrap();
    assert_eq!(&without[..4], b"HTI2");
    t.error = Some(HeightErrorBound {
        max_error_q1024: 1025,
        min_height: -32,
        max_height: 128,
    });
    let bytes = encode(&t).unwrap();
    assert_eq!(bytes.len(), without.len() + 16);
    assert_eq!(decode(&bytes).unwrap(), t);
    assert_eq!(
        decode(&bytes).unwrap().error.unwrap().blocks(),
        1.0009765625
    );
    t.error.as_mut().unwrap().min_height = 129;
    assert_eq!(encode(&t), Err(HeightError::Bounds));
    t.error = Some(HeightErrorBound {
        max_error_q1024: 0,
        min_height: 1,
        max_height: 2,
    });
    t.known[0] = 1;
    assert_eq!(
        encode(&t),
        Err(HeightError::Bounds),
        "known corner outside claimed enclosure"
    );
    t.error = None;
    assert!(
        encode(&t).is_ok(),
        "Unknown error is allowed but never inferred as zero"
    );
}

#[test]
fn exact_leaf_requests_have_an_additive_bounded_tag_and_preserve_normal_bytes() {
    let key = HeightKey {
        space: 0,
        level: 0,
        x: -2,
        z: 5,
    };
    let ordinary = net::ClientMessage::RequestHeight {
        key,
        known: Some(7),
    };
    let exact = net::ClientMessage::RequestExactHeight {
        key,
        known: Some(7),
    };
    let a = net::encode_client(&ordinary).unwrap();
    let b = net::encode_client(&exact).unwrap();
    assert_eq!(a.len(), b.len());
    assert_ne!(a, b);
    assert_eq!(net::decode_client(&a).unwrap(), ordinary);
    assert_eq!(net::decode_client(&b).unwrap(), exact);
    for n in 0..b.len() {
        assert!(net::decode_client(&b[..n]).is_err());
    }
    assert!(
        net::encode_client(&net::ClientMessage::RequestExactHeight {
            key: HeightKey { level: 1, ..key },
            known: None
        })
        .is_err()
    );
    let mut changed = b;
    // Versioned outer prefix is four-byte magic plus the request tag.
    let tag = a.iter().zip(&changed).position(|(a, b)| a != b).unwrap();
    changed[tag] = 255;
    assert!(net::decode_client(&changed).is_err());
}
