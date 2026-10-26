use core_types::{BlockPos, SpaceId};
use rules::{
    crafting::{Grid, RecipeBook},
    crossroads::*,
    stack::ItemStack,
};
fn center() -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: 100,
        y: 70,
        z: -200,
    }
}
#[test]
fn nine_planes_use_authoritative_floor_and_exact_wall_offsets() {
    let s = center();
    let p = boards(s).unwrap();
    for (i, dz) in [6, -6, -16].into_iter().enumerate() {
        let a = p[i * 2];
        let b = p[i * 2 + 1];
        assert_eq!(PAVILIONS[i].offset, [-19, dz]);
        assert_eq!(
            a.center_200,
            [
                20000 - 3992,
                14000 + 700,
                -40000 + i64::from(dz) * 200 + 100
            ]
        );
        assert_eq!(b.center_200[0], 20000 - 4208);
        assert_eq!(a.facing, Facing::East);
        assert_eq!(b.facing, Facing::West);
        assert_eq!(a.size_200, [1000, 600]);
        assert_eq!(a.backing_cells().count(), 15);
        assert!(
            a.backing_cells()
                .all(|c| c.x == 79 && (72..=74).contains(&c.y))
        );
        assert_eq!(a.light_sample().x, 80);
        assert_eq!(b.light_sample().x, 78);
    }
    assert_eq!(p[6].center_200, [17700, 14500, -39192]);
    assert_eq!(p[7].size_200, [175, 175]);
    assert_eq!(p[8].size_200, [600, 200]);
    assert_eq!(p[8].backing_cells().count(), 3);
    assert!(p[6..].iter().all(|p| p.facing == Facing::South));
}
#[test]
fn integer_rebase_and_unrepresentable_landmarks_are_explicit() {
    let s = BlockPos {
        x: i32::MAX - 100,
        y: -200,
        z: i32::MIN + 100,
        ..center()
    };
    let p = boards(s).unwrap();
    assert_eq!(
        p[0].relative_center([i64::from(s.x), i64::from(s.y), i64::from(s.z)]),
        [-19.96, 3.5, 6.5]
    );
    assert_eq!(
        boards(BlockPos {
            space: SpaceId(1),
            ..s
        }),
        Err(Error::UnsupportedSpace)
    );
    assert_eq!(
        boards(BlockPos { x: i32::MAX, ..s }),
        Err(Error::CoordinateOverflow)
    );
}
#[test]
fn every_pictured_grid_is_a_real_compiled_recipe() {
    let book = RecipeBook::standard().unwrap();
    let mut n = 0;
    for pavilion in PAVILIONS {
        for entry in pavilion.hall.craft() {
            let cells = recipe_cells(&book, entry.name).unwrap();
            let mut grid = Grid::new(3, 3).unwrap();
            for (i, item) in cells.grid.into_iter().enumerate() {
                if item != 0 {
                    grid.set(i % 3, i / 3, ItemStack::new(item, 1).unwrap())
                        .unwrap();
                }
            }
            let recipe = book.iter().find(|(_, r)| r.name() == entry.name).unwrap().1;
            assert!(recipe.matches(&grid), "{}", entry.name);
            assert_eq!(
                (cells.output, cells.count),
                (recipe.output_item(), recipe.output_count())
            );
            n += 1;
        }
    }
    assert_eq!(n, 12);
    assert_eq!(
        recipe_cells(&book, "not_a_recipe"),
        Err(Error::MissingRecipe)
    );
    assert_eq!(
        recipe_cells(&book, "repair_item"),
        Err(Error::UnsupportedRecipe)
    );
}
