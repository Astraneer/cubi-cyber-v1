use render::lod::{PublicPage, PublicVoxel};
fn stone() -> PublicVoxel {
    PublicVoxel::solid(3, u32::MAX)
}

pub struct Fixture {
    pub name: String,
    pub page: PublicPage,
    pub air: Vec<[u8; 3]>,
    pub solid: Vec<[u8; 3]>,
}

fn fill(page: &mut PublicPage, lo: [u8; 3], hi: [u8; 3]) {
    for y in lo[1]..=hi[1] {
        for z in lo[2]..=hi[2] {
            for x in lo[0]..=hi[0] {
                page.set([x, y, z], stone());
            }
        }
    }
}

pub fn fixture(index: usize) -> Fixture {
    let mut page = PublicPage::filled(PublicVoxel::Air);
    let mut air = Vec::new();
    let mut solid = Vec::new();
    match index % 5 {
        0 => {
            let height = 5 + (index % 4) as u8;
            fill(&mut page, [1, 1, 2], [14, 1, 13]);
            fill(&mut page, [2, height, 7], [13, height, 8]);
            fill(&mut page, [2, 2, 7], [2, height, 8]);
            fill(&mut page, [13, 2, 7], [13, height, 8]);
            for z in 2..14 {
                air.push([8, height - 1, z]);
            }
            solid.push([8, height, 7]);
        }
        1 => {
            let height = 7 + (index % 3) as u8;
            fill(&mut page, [1, 1, 3], [14, 1, 12]);
            fill(&mut page, [4, 2, 7], [4, height, 8]);
            fill(&mut page, [9, 2, 7], [9, height, 8]);
            fill(&mut page, [4, height, 7], [9, height, 8]);
            for y in 2..height {
                air.push([6, y, 7]);
            }
            solid.push([6, height, 7]);
        }
        2 => {
            let x = 3 + (index % 7) as u8;
            let height = 8 + (index % 5) as u8;
            fill(&mut page, [1, 1, 2], [14, 1, 13]);
            fill(&mut page, [x, 2, 8], [x, height, 8]);
            fill(&mut page, [x + 2, 2, 8], [x + 2, height, 8]);
            for y in 2..=height {
                solid.push([x, y, 8]);
                air.push([x + 1, y, 8]);
            }
        }
        3 => {
            fill(&mut page, [2, 2, 8], [13, 12, 8]);
            let width = 1 + (index % 2) as u8;
            for y in 5..=7 {
                for x in 7..7 + width {
                    page.set([x, y, 8], PublicVoxel::Air);
                    air.push([x, y, 8]);
                }
            }
            solid.push([2, 2, 8]);
        }
        _ => {
            fill(&mut page, [1, 1, 1], [14, 1, 14]);
            fill(&mut page, [2, 7, 7], [13, 7, 7]);
            fill(&mut page, [7, 7, 2], [7, 7, 13]);
            for [x, z] in [[2, 7], [13, 7], [7, 2], [7, 13]] {
                fill(&mut page, [x, 2, z], [x, 7, z]);
            }
            solid.extend([[7, 7, 7], [4, 7, 7], [7, 7, 11]]);
            air.extend([[7, 6, 7], [4, 6, 7], [7, 6, 11]]);
        }
    }
    for at in &air {
        assert_eq!(page.get(*at), PublicVoxel::Air);
    }
    for at in &solid {
        assert!(page.get(*at).occupied());
    }
    Fixture {
        name: format!("building-{index:02}"),
        page,
        air,
        solid,
    }
}
