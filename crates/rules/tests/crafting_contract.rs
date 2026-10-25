use std::{alloc::System, hint::black_box};

use rules::{
    crafting::*,
    ids::*,
    items::*,
    stack::{ItemStack, StackError},
};
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator};

#[global_allocator]
static ALLOCATOR: CountingAllocator<System> = CountingAllocator::new(System);

fn ingredient(value: &Ing) -> String {
    match value {
        Ing::Item(id) => format!("i:{id}"),
        Ing::Set(ids) => format!(
            "s:{}",
            ids.iter().map(u16::to_string).collect::<Vec<_>>().join("|")
        ),
    }
}

fn stack(id: u16, count: u16) -> ItemStack {
    ItemStack::new(id, count).unwrap()
}

fn parse_stack(value: &str) -> ItemStack {
    let fields = value
        .split(',')
        .map(|value| value.parse::<u16>().unwrap())
        .collect::<Vec<_>>();
    ItemStack::from_parts(fields[0], fields[1], fields[2]).unwrap()
}

#[test]
fn every_recipe_definition_matches_the_independently_compiled_donor() {
    let book = RecipeBook::standard().unwrap();
    assert_eq!(path_recipes().len(), 96);
    assert_eq!(g7_recipes().len(), 11);
    assert_eq!(rules::machine_recipes::crafting().len(), 39);
    assert_eq!(book.len(), 97);
    let mut actual = String::new();
    for (id, recipe) in book.iter() {
        let shape = match recipe.kind() {
            RecipeKind::Shaped(pattern) => format!(
                "S:{}:{}:{}",
                pattern.width(),
                pattern.height(),
                pattern
                    .cells()
                    .iter()
                    .map(|value| value.as_ref().map_or("-".into(), ingredient))
                    .collect::<Vec<_>>()
                    .join(";")
            ),
            RecipeKind::Shapeless { ingredients, count } => format!(
                "L:{}",
                ingredients[..usize::from(*count)]
                    .iter()
                    .map(|value| ingredient(value.as_ref().unwrap()))
                    .collect::<Vec<_>>()
                    .join(";")
            ),
            RecipeKind::Repair => "R".into(),
        };
        actual.push_str(&format!(
            "{}\t{}\t{}\t{}\t{shape}\n",
            id.raw(),
            recipe.name(),
            recipe.output_item(),
            recipe.output_count()
        ));
    }
    assert_eq!(actual, include_str!("data/crafting.tsv"));
}

#[test]
fn every_recipe_canonical_grid_matches_the_independent_donor_result() {
    let book = RecipeBook::standard().unwrap();
    let mut checked = 0;
    for line in include_str!("data/crafting_cases.tsv").lines() {
        let columns: Vec<_> = line.split('\t').collect();
        let id = columns[0].parse::<u16>().unwrap();
        let mut grid = Grid::new(3, 3).unwrap();
        for (index, value) in columns[1].split(';').enumerate() {
            grid.set(index % 3, index / 3, parse_stack(value)).unwrap();
        }
        assert_eq!(book.find(&grid).map(|(id, _)| id.raw()), Some(id));
        assert_eq!(book.result(&grid), parse_stack(columns[2]), "recipe {id}");
        checked += 1;
    }
    assert_eq!(checked, 97);
}

#[test]
fn all_forward_and_reverse_nei_indexes_match_the_independent_donor() {
    let book = RecipeBook::standard().unwrap();
    let mut checked = 0;
    for line in include_str!("data/crafting_nei.tsv").lines() {
        let fields: Vec<_> = line.split('\t').collect();
        let item = fields[0].parse().unwrap();
        let parse_ids = |text: &str| {
            text.split(',')
                .filter(|value| !value.is_empty())
                .map(|value| value.parse::<u16>().unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            book.recipes_producing(item)
                .map(|(id, _)| id.raw())
                .collect::<Vec<_>>(),
            parse_ids(fields[1]),
            "output {item}"
        );
        assert_eq!(
            book.recipes_using(item)
                .map(|(id, _)| id.raw())
                .collect::<Vec<_>>(),
            parse_ids(fields[2]),
            "uses {item}"
        );
        checked += 1;
    }
    assert_eq!(checked, 180);
    assert_eq!(book.recipes_producing(0).count(), 0);
    assert_eq!(book.recipes_using(u16::MAX).count(), 0);
}

#[test]
fn matcher_supports_trim_mirror_tags_and_shapeless_augmenting_paths() {
    let book = RecipeBook::standard().unwrap();
    // Mirrored wooden axe, shifted to the right; mixed plank species are allowed.
    let mut grid = Grid::new(3, 3).unwrap();
    for (x, y, item) in [
        (1, 0, PLANKS),
        (2, 0, BIRCH_PLANKS),
        (1, 1, I_STICK),
        (2, 1, SPRUCE_PLANKS),
        (1, 2, I_STICK),
    ] {
        grid.set(x, y, stack(item, 1)).unwrap();
    }
    assert_eq!(book.result(&grid).item(), I_WOODEN_PICKAXE + 1);
    grid.set(0, 2, stack(DIRT, 1)).unwrap();
    assert!(book.result(&grid).is_empty());
    let flexible = RecipeBook::build([RecipeDef {
        name: "augmenting_path",
        shape: RShape::Shapeless(vec![Ing::Set(&[I_STICK, I_COAL]), Ing::Item(I_STICK)]),
        out: DIRT,
        count: 1,
    }])
    .unwrap();
    let mut grid = Grid::new(2, 2).unwrap();
    grid.set(0, 0, stack(I_STICK, 1)).unwrap();
    grid.set(1, 1, stack(I_COAL, 1)).unwrap();
    assert_eq!(flexible.result(&grid).item(), DIRT);
}

#[test]
fn stack_and_pattern_boundaries_reject_invalid_input() {
    assert_eq!(
        ItemStack::from_parts(0, 1, 0),
        Err(StackError::InvalidEmpty)
    );
    assert!(matches!(
        ItemStack::new(u16::MAX, 1),
        Err(StackError::UnknownItem(_))
    ));
    assert!(matches!(
        ItemStack::new(I_WATER_BUCKET, 2),
        Err(StackError::CountExceedsMaximum { .. })
    ));
    assert!(matches!(
        ItemStack::from_parts(PLANKS, 1, 1),
        Err(StackError::DamageExceedsMaximum { .. })
    ));
    assert!(ItemStack::from_parts(I_WOODEN_PICKAXE, 1, 60).is_err());
    assert!(ItemStack::from_parts(I_WOODEN_PICKAXE, 1, 59).is_ok());
    assert_eq!(Grid::new(0, 3), Err(CraftError::InvalidGridSize));
    assert_eq!(Grid::new(4, 3), Err(CraftError::InvalidGridSize));
    let mut grid = Grid::new(2, 2).unwrap();
    assert_eq!(grid.set(2, 0, stack(DIRT, 1)), Err(CraftError::OutsideGrid));
    assert_eq!(
        compile_pattern(&["XX"], &[('X', Ing::Item(DIRT)), ('X', Ing::Item(STONE))]),
        Err(CraftError::InvalidPattern)
    );
    assert_eq!(
        compile_pattern(&["X"], &[('X', Ing::Set(&[]))]),
        Err(CraftError::InvalidIngredient)
    );
    assert!(
        RecipeBook::build([RecipeDef {
            name: "invalid",
            shape: RShape::Shapeless(vec![Ing::Item(DIRT)]),
            out: I_WOODEN_PICKAXE,
            count: 2
        }])
        .is_err()
    );
}

fn stick_grid(count: u16) -> Grid {
    let mut grid = Grid::new(2, 2).unwrap();
    grid.set(0, 0, stack(PLANKS, count)).unwrap();
    grid.set(0, 1, stack(BIRCH_PLANKS, count)).unwrap();
    grid
}

#[test]
fn craft_plan_merges_bounded_output_and_publishes_all_values_atomically() {
    let book = RecipeBook::standard().unwrap();
    let mut grid = stick_grid(2);
    let mut inventory = [stack(I_STICK, 60), ItemStack::EMPTY];
    let plan =
        CraftPlan::prepare_exact(&book, &grid, &inventory, 2, RemainderPolicy::RejectOverflow)
            .unwrap();
    assert_eq!(plan.crafts(), 2);
    assert_eq!(plan.changes().len(), 2);
    assert!(plan.drops().is_empty());
    assert_eq!(inventory, [stack(I_STICK, 60), ItemStack::EMPTY]);
    assert_eq!(grid, stick_grid(2));
    assert!(plan.apply(&mut grid, &mut inventory).unwrap().is_empty());
    assert!(grid.cells().iter().all(|value| value.is_empty()));
    assert_eq!(inventory, [stack(I_STICK, 64), stack(I_STICK, 4)]);
}

#[test]
fn capacity_insufficient_ingredients_and_stale_plans_never_partially_apply() {
    let book = RecipeBook::standard().unwrap();
    let mut grid = stick_grid(2);
    let full = [stack(I_STICK, 64)];
    assert!(matches!(
        CraftPlan::prepare_exact(&book, &grid, &full, 1, RemainderPolicy::RejectOverflow),
        Err(CraftError::NoCapacity)
    ));
    assert_eq!(grid, stick_grid(2));
    assert_eq!(full, [stack(I_STICK, 64)]);
    let empty = [ItemStack::EMPTY; 2];
    assert!(matches!(
        CraftPlan::prepare_exact(&book, &grid, &empty, 3, RemainderPolicy::RejectOverflow),
        Err(CraftError::NotEnoughIngredients)
    ));
    assert_eq!(grid, stick_grid(2));
    assert_eq!(empty, [ItemStack::EMPTY; 2]);
    let plan =
        CraftPlan::prepare_exact(&book, &grid, &empty, 1, RemainderPolicy::RejectOverflow).unwrap();
    let mut changed_inventory = [stack(DIRT, 1), ItemStack::EMPTY];
    assert_eq!(
        plan.apply(&mut grid, &mut changed_inventory),
        Err(CraftError::StalePlan)
    );
    assert_eq!(grid, stick_grid(2));
    assert_eq!(changed_inventory, [stack(DIRT, 1), ItemStack::EMPTY]);
    let plan =
        CraftPlan::prepare_exact(&book, &grid, &empty, 1, RemainderPolicy::RejectOverflow).unwrap();
    grid.set(1, 1, stack(DIRT, 1)).unwrap();
    let expected_grid = grid;
    let mut inventory = empty;
    assert_eq!(
        plan.apply(&mut grid, &mut inventory),
        Err(CraftError::StalePlan)
    );
    assert_eq!(grid, expected_grid);
    assert_eq!(inventory, empty);
}

#[test]
fn crusher_recipe_returns_both_buckets_without_duplication() {
    let book = RecipeBook::standard().unwrap();
    let (_, recipe) = book.recipes_producing(CRUSHER).next().unwrap();
    let RecipeKind::Shaped(pattern) = recipe.kind() else {
        panic!("shaped crusher")
    };
    let mut grid = Grid::new(3, 3).unwrap();
    for (index, ingredient) in pattern.cells().iter().enumerate() {
        if let Some(ingredient) = ingredient {
            grid.set(index % 3, index / 3, stack(ingredient.items()[0], 1))
                .unwrap();
        }
    }
    let mut inventory = [ItemStack::EMPTY];
    let plan = CraftPlan::prepare_exact(&book, &grid, &inventory, 1, RemainderPolicy::CollectDrops)
        .unwrap();
    assert!(plan.drops().is_empty());
    assert!(plan.apply(&mut grid, &mut inventory).unwrap().is_empty());
    assert_eq!(inventory, [stack(CRUSHER, 1)]);
    assert_eq!(grid.get(0, 1), Some(stack(I_BUCKET, 1)));
    assert_eq!(grid.get(2, 1), Some(stack(I_BUCKET, 1)));
    assert_eq!(
        grid.cells()
            .iter()
            .filter(|value| !value.is_empty())
            .count(),
        2
    );
    assert!(matches!(
        CraftPlan::prepare_exact(&book, &grid, &inventory, 1, RemainderPolicy::CollectDrops),
        Err(CraftError::NoRecipe)
    ));
}

#[test]
fn repair_uses_integer_bonus_rounding_and_keeps_item_durability() {
    let book = RecipeBook::standard().unwrap();
    for (item, damage, expected) in [
        (I_WOODEN_PICKAXE, 35, 9),
        (I_WOODEN_PICKAXE, 59, 57),
        (I_WOODEN_PICKAXE + 10, 249, 236),
    ] {
        let mut grid = Grid::new(2, 2).unwrap();
        let value = ItemStack::from_parts(item, 1, damage).unwrap();
        grid.set(0, 0, value).unwrap();
        grid.set(1, 1, value).unwrap();
        let result = book.result(&grid);
        assert_eq!(
            (result.item(), result.count(), result.damage()),
            (item, 1, expected)
        );
        let mut inventory = [ItemStack::EMPTY];
        let plan =
            CraftPlan::prepare_exact(&book, &grid, &inventory, 1, RemainderPolicy::RejectOverflow)
                .unwrap();
        plan.apply(&mut grid, &mut inventory).unwrap();
        assert_eq!(inventory, [result]);
        assert!(grid.cells().iter().all(|value| value.is_empty()));
    }
}

#[test]
fn prepared_recipe_matching_and_nei_queries_allocate_nothing() {
    let book = RecipeBook::standard().unwrap();
    let grid = stick_grid(1);
    let positive = AllocationScope::begin().unwrap();
    let control = Vec::<u8>::with_capacity(black_box(31));
    black_box(&control);
    assert!(positive.finish().allocating_calls() > 0);
    drop(control);
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        black_box(book.result(black_box(&grid)));
        black_box(book.recipes_producing(black_box(I_STICK)).count());
        black_box(book.recipes_using(black_box(PLANKS)).count());
        black_box(rules::machine_recipes::recipes_using(black_box(I_RAW_IRON)).count());
        black_box(
            rules::machine_recipes::recipes_producing(rules::machine_recipes::ProductKey::Item(
                black_box(I_DUST_IRON),
            ))
            .count(),
        );
    }
    let counts = scope.finish();
    assert_eq!(counts, AllocationCounts::ZERO);
}

#[test]
fn crafting_reach_preserves_strict_squared_distance_boundary() {
    assert!(crafting_table_in_reach([0.5, 0.5, 0.5], [0; 3], true));
    assert!(crafting_table_in_reach([9.49, 0.5, 0.5], [0; 3], true));
    assert!(!crafting_table_in_reach([9.5, 0.5, 0.5], [0; 3], true));
    assert!(!crafting_table_in_reach([0.5; 3], [0; 3], false));
}
