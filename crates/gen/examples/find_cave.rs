//! Server-side public-terrain geometry probe; never labels generated light known.
use r#gen::{Generator, TerrainSeed};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let seed = std::env::args()
        .nth(1)
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(1);
    let generator = Generator::new(TerrainSeed(seed));
    let mut found = Vec::<[i32; 3]>::new();
    for ring in 0i32..=8 {
        for cz in -ring..=ring {
            for cx in -ring..=ring {
                if cx.abs().max(cz.abs()) != ring {
                    continue;
                }
                let column = generator.generate_column(cx, cz)?;
                for y in (20..=60).rev() {
                    for z in 2..14 {
                        for x in 2..14 {
                            let air = (-1..=1).all(|dz| {
                                (-1..=1).all(|dx| {
                                    (0..3).all(|dy| {
                                        column.block_padded(x + 1 + dx, y + dy, z + 1 + dz)
                                            == rules::ids::AIR
                                    })
                                })
                            });
                            if !air || column.block_padded(x + 1, y - 1, z + 1) != rules::ids::STONE
                            {
                                continue;
                            }
                            let roof = (3..=12).find(|dy| {
                                (-1..=1).all(|dz| {
                                    (-1..=1).all(|dx| {
                                        column.block_padded(x + 1 + dx, y + dy, z + 1 + dz)
                                            == rules::ids::STONE
                                    })
                                })
                            });
                            let Some(roof) = roof else {
                                continue;
                            };
                            let point = [cx * 16 + x, y, cz * 16 + z];
                            if found
                                .iter()
                                .any(|p| (p[0] - point[0]).abs() + (p[2] - point[2]).abs() < 24)
                            {
                                continue;
                            }
                            found.push(point);
                            println!(
                                "CAVE_GEOMETRY candidateFeet=[{}.5,{},{}.5] AIR3x3x3=true floorSTONE=true roof3x3STONE@y{} publicseed{} SKY_NOT_YET_CERTIFIED",
                                point[0],
                                point[1],
                                point[2],
                                y + roof,
                                seed
                            );
                            if found.len() >= 6 {
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }
    }
    if found.is_empty() {
        return Err("no cave candidate in bounded8-column search".into());
    }
    Ok(())
}
