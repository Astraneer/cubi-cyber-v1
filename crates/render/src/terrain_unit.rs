//! Bounded diagnostic reference: split greedy rectangles into identical unit faces.
//! Both initial and final publications must use this mode for a valid raster control.
use crate::terrain_mesh::{DescriptorError, MAX_PAGE_QUADS, PageMesh, QuadDescriptor};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitError {
    Descriptor(DescriptorError),
    MergedNonuniformAo,
    Capacity,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UnitWork {
    pub source_quads: u64,
    pub emitted_quads: u64,
    pub upload_bytes: u64,
}

/// Reuses destination capacity. Invalid input is rejected before changing output.
/// Unit quads retain exact AO, material, tint, UV rotation and object slot.
/// Greedy rectangles may split only when all four AO values are equal, as in
/// `mesh_page`; interpolating a nonuniform merged rectangle would change its AO.
pub fn expand(source: &PageMesh, destination: &mut PageMesh) -> Result<UnitWork, UnitError> {
    let mut counts = [0usize; 3];
    let mut work = UnitWork::default();
    for (bucket, quads) in [&source.opaque, &source.cutout, &source.water]
        .into_iter()
        .enumerate()
    {
        for &quad in quads {
            QuadDescriptor::from_le_bytes(quad.to_le_bytes()).map_err(UnitError::Descriptor)?;
            let [width, height] = quad.size();
            let area = usize::from(width) * usize::from(height);
            if area > 1 && quad.ao().iter().any(|&ao| ao != quad.ao()[0]) {
                return Err(UnitError::MergedNonuniformAo);
            }
            counts[bucket] = counts[bucket]
                .checked_add(area)
                .ok_or(UnitError::Capacity)?;
            work.source_quads += 1;
        }
    }
    let count = counts.iter().sum::<usize>();
    if count > MAX_PAGE_QUADS {
        return Err(UnitError::Capacity);
    }
    for ((source, destination), count) in [&source.opaque, &source.cutout, &source.water]
        .into_iter()
        .zip([
            &mut destination.opaque,
            &mut destination.cutout,
            &mut destination.water,
        ])
        .zip(counts)
    {
        destination.clear();
        destination.reserve(count);
        for &quad in source {
            let [width, height] = quad.size();
            let origin = quad.origin();
            let u = quad.face().tangent_u();
            let v = quad.face().tangent_v();
            for y in 0..height {
                for x in 0..width {
                    let corner = std::array::from_fn(|axis| {
                        (i16::from(origin[axis])
                            + i16::from(u[axis]) * i16::from(x)
                            + i16::from(v[axis]) * i16::from(y)) as u8
                    });
                    let unit = QuadDescriptor::new(
                        corner,
                        quad.face(),
                        [1, 1],
                        quad.material(),
                        quad.ao(),
                        quad.tint(),
                    )
                    .expect("validated greedy subrectangle")
                    .with_uv_rotation(quad.uv_rotation())
                    .expect("two-bit rotation")
                    .with_object(quad.object());
                    destination.push(unit);
                }
            }
        }
    }
    work.emitted_quads = count as u64;
    work.upload_bytes = work.emitted_quads * 16;
    Ok(work)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain_mesh::{CellMaterial, MeshScratch, PageHalo, mesh_page};
    #[test]
    fn cube_control_has_the_same_six_surfaces_and_descriptor_attributes() {
        let mut source = PageMesh::default();
        mesh_page(
            &PageHalo::from_page(&[1; 4096], 0),
            |raw| {
                if raw == 0 {
                    CellMaterial::EMPTY
                } else {
                    CellMaterial {
                        face_uv_rotations: [0, 1, 2, 3, 1, 2],
                        tint: 0xabcdefff,
                        ..CellMaterial::opaque(42)
                    }
                }
            },
            &mut MeshScratch::new(),
            &mut source,
        );
        let mut destination = PageMesh::default();
        let work = expand(&source, &mut destination).unwrap();
        assert_eq!(
            (work.source_quads, work.emitted_quads, work.upload_bytes),
            (6, 1536, 24576)
        );
        for face in crate::terrain_mesh::Face::ALL {
            let units: Vec<_> = destination
                .opaque
                .iter()
                .copied()
                .filter(|q| q.face() == face)
                .collect();
            assert_eq!(units.len(), 256);
            let mut points = std::collections::BTreeSet::new();
            for unit in units {
                assert_eq!(unit.size(), [1, 1]);
                assert_eq!(unit.material(), 42);
                assert_eq!(unit.tint(), 0xabcdefff);
                assert_eq!(unit.uv_rotation(), [0, 1, 2, 3, 1, 2][face as usize]);
                assert!(points.insert(unit.origin()));
                for vertex in 0..4 {
                    assert!(
                        unit.vertex(vertex)
                            .into_iter()
                            .all(|c| (0..=16).contains(&c))
                    );
                }
            }
        }
    }
    #[test]
    fn unsupported_merged_ao_rejects_before_changing_destination() {
        let quad = QuadDescriptor::new(
            [0, 0, 0],
            crate::terrain_mesh::Face::Down,
            [2, 2],
            0,
            [0, 1, 2, 3],
            u32::MAX,
        )
        .unwrap();
        let source = PageMesh {
            opaque: vec![quad],
            ..PageMesh::default()
        };
        let mut destination = PageMesh {
            water: vec![quad],
            ..PageMesh::default()
        };
        assert_eq!(
            expand(&source, &mut destination),
            Err(UnitError::MergedNonuniformAo)
        );
        assert_eq!(destination.water, vec![quad]);
        assert!(destination.opaque.is_empty());
        let unit = QuadDescriptor::new(
            [0, 0, 0],
            crate::terrain_mesh::Face::Down,
            [1, 1],
            0,
            [0, 1, 2, 3],
            u32::MAX,
        )
        .unwrap();
        let source = PageMesh {
            opaque: vec![unit.with_object(7)],
            ..PageMesh::default()
        };
        expand(&source, &mut destination).unwrap();
        assert_eq!(destination.opaque, source.opaque);
    }
}
