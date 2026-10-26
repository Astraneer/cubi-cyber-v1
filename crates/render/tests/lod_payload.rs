use render::{
    lod::{MaskedRayProxy, PublicPage, PublicVoxel, SurfaceKind, protection_groups},
    lod_gpu::{MAX_PUBLIC_BYTES, ProxyError, ProxyPayload},
};

fn source(page: &PublicPage) -> MaskedRayProxy {
    MaskedRayProxy::from_public_page(page, protection_groups(page, 4))
}
fn cutout(material: u16, tint: u32) -> PublicVoxel {
    PublicVoxel::Solid {
        material,
        tint,
        kind: SurfaceKind::Cutout,
    }
}
fn read(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn write(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn pxp2_retains_public_cells_and_contains_ordinary_cutout_descriptors() {
    let public = PublicPage::from_exposed(|[x, y, z]| match y {
        3 => PublicVoxel::solid(2, u32::MAX),
        4 if x < 8 && z < 8 => cutout(3, 0xff00ff00),
        _ => PublicVoxel::Air,
    });
    let payload = ProxyPayload::from_public(&source(&public), 4).unwrap();
    assert!(payload.cutout_quads() > 0);
    let bytes = payload.encode_public();
    assert_eq!(&bytes[..4], b"PXP2");
    assert_eq!(bytes.len(), 36 + read(&bytes, 4) as usize * 4);
    assert_eq!(read(&bytes, 32), 3);
    let decoded = ProxyPayload::decode_public(&bytes, 4).unwrap();
    assert_eq!(decoded.cutout_quads(), payload.cutout_quads());
    for y in 0..16 {
        for z in 0..16 {
            for x in 0..16 {
                assert_eq!(decoded.voxel([x, y, z]), public.get([x, y, z]));
            }
        }
    }
}

#[test]
fn wrong_version_headers_and_malformed_quad_words_are_rejected() {
    let public = PublicPage::from_exposed(|p| {
        if p == [8, 8, 8] {
            cutout(3, u32::MAX)
        } else {
            PublicVoxel::Air
        }
    });
    let payload = ProxyPayload::from_public(&source(&public), 4).unwrap();
    let good = payload.encode_public();
    assert_eq!(payload.cutout_quads(), 6);
    for length in 0..36 {
        assert!(ProxyPayload::decode_public(&good[..length], 4).is_err());
    }
    let mut legacy = good.clone();
    legacy[..4].copy_from_slice(b"PXP1");
    assert_eq!(
        ProxyPayload::decode_public(&legacy, 4).unwrap_err(),
        ProxyError::InvalidMaterial
    );
    for (offset, value) in [(4, u32::MAX), (24, u32::MAX), (28, 24577), (32, 7)] {
        let mut invalid = good.clone();
        write(&mut invalid, offset, value);
        assert!(
            ProxyPayload::decode_public(&invalid, 4).is_err(),
            "header offset{offset}"
        );
    }
    let quad = 36 + read(&good, 24) as usize * 4;
    for (offset, value) in [
        (quad, (read(&good, quad) & !31) | 31),
        (quad, (read(&good, quad) & !(7 << 15)) | (7 << 15)),
        (quad, read(&good, quad) | (1 << 31)),
        (quad + 4, (read(&good, quad + 4) & !65535) | 2),
        (quad + 4, read(&good, quad + 4) | (1 << 31)),
        (quad + 12, 99),
    ] {
        let mut invalid = good.clone();
        write(&mut invalid, offset, value);
        assert!(
            ProxyPayload::decode_public(&invalid, 4).is_err(),
            "quad offset{offset} value{value}"
        );
    }
}

#[test]
fn maximum_cutout_page_fits_the_existing_worker_packet_cap() {
    let public = PublicPage::from_exposed(|[x, y, z]| {
        let index = u32::from(x) + 16 * (u32::from(z) + 16 * u32::from(y));
        cutout((index % 186) as u16, 0xff000000 | index)
    });
    let payload = ProxyPayload::from_public(&source(&public), 186).unwrap();
    assert_eq!(payload.cutout_quads(), 24576);
    assert_eq!(payload.palette_len(), 4096);
    let bytes = payload.encode_public();
    assert_eq!(bytes.len(), MAX_PUBLIC_BYTES);
    assert!(bytes.len() + 50 < 450000);
    let decoded = ProxyPayload::decode_public(&bytes, 186).unwrap();
    assert_eq!(decoded.cutout_quads(), 24576);
    assert_eq!(decoded.voxel([15, 15, 15]), public.get([15, 15, 15]));
    println!(
        "PXP2 maxpage {}B,24576quads,4096palette,workerpacket{}B",
        bytes.len(),
        bytes.len() + 50
    );
}

#[test]
fn cutout_fully_occluded_by_opaque_cells_needs_no_quad_but_stays_public() {
    let public = PublicPage::from_exposed(|p| {
        if p == [8, 8, 8] {
            cutout(3, u32::MAX)
        } else {
            PublicVoxel::solid(2, u32::MAX)
        }
    });
    let payload = ProxyPayload::from_public(&source(&public), 4).unwrap();
    assert_eq!(payload.cutout_quads(), 0);
    let decoded = ProxyPayload::decode_public(&payload.encode_public(), 4).unwrap();
    assert_eq!(decoded.voxel([8, 8, 8]), cutout(3, u32::MAX));
}
