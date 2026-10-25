use rules::{ids::*, items::*, machine_recipes::*};

fn canonical(recipe: MachineRecipe) -> String {
    let product = match recipe.product {
        Product::Item { item, count } => format!("item:{item}:{count}"),
        Product::Substance { substance, amount } => format!("sub:{substance}:{amount}"),
        Product::Energy { joules } => format!("energy:{joules}"),
    };
    let reagent = recipe.reagent.map_or("-".into(), |value| {
        format!(
            "{}:{}:{}",
            value.substance,
            value.amount,
            match value.usage {
                ReagentUsage::PerOperation => "operation",
                ReagentUsage::PerTick => "tick",
            }
        )
    });
    format!(
        "{:?}\t{}\t{}\t{}\t{product}\t{reagent}\n",
        recipe.kind, recipe.index, recipe.input, recipe.input_count
    )
}

#[test]
fn all_machine_recipe_and_conversion_data_match_independent_donor_references() {
    assert_eq!(
        (INFUSE.len(), ENRICH.len(), CRUSH.len(), PURIFY.len()),
        (6, 17, 15, 6)
    );
    assert_eq!(all().count(), 73);
    assert_eq!(
        all().map(canonical).collect::<String>(),
        include_str!("data/machines.tsv")
    );
}

#[test]
fn every_machine_forward_and_reverse_query_matches_the_reference_records() {
    let records: Vec<_> = include_str!("data/machines.tsv").lines().collect();
    for (item, _) in rules::all_items() {
        let expected = records
            .iter()
            .filter(|line| line.split('\t').nth(2).unwrap().parse::<u16>().unwrap() == item)
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        assert_eq!(
            recipes_using(item).map(canonical).collect::<String>(),
            expected,
            "input {item}"
        );
        let prefix = format!("item:{item}:");
        let expected = records
            .iter()
            .filter(|line| line.split('\t').nth(4).unwrap().starts_with(&prefix))
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        assert_eq!(
            recipes_producing(ProductKey::Item(item))
                .map(canonical)
                .collect::<String>(),
            expected,
            "output {item}"
        );
    }
    for substance in 0..=SUB_WATER {
        let prefix = format!("sub:{substance}:");
        let expected = records
            .iter()
            .filter(|line| line.split('\t').nth(4).unwrap().starts_with(&prefix))
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        assert_eq!(
            recipes_producing(ProductKey::Substance(substance))
                .map(canonical)
                .collect::<String>(),
            expected
        );
    }
    assert_eq!(recipes_using(u16::MAX).count(), 0);
    assert_eq!(recipes_producing(ProductKey::Energy).count(), 2);
}

#[test]
fn reagent_boundaries_and_conversion_amounts_are_exact() {
    assert_eq!(find_infuse(I_IRON_INGOT, SUB_CARBON, 9), None);
    assert_eq!(find_infuse(I_IRON_INGOT, SUB_CARBON, 10), Some(0));
    assert_eq!(find_infuse(I_IRON_INGOT, SUB_REDSTONE, 100), None);
    assert_eq!(chem_of(I_COAL), Some((SUB_CARBON, 10)));
    assert_eq!(chem_of(I_CHARCOAL), Some((SUB_CARBON, 20)));
    assert_eq!(chem_of(COAL_BLOCK), Some((SUB_CARBON, 90)));
    assert_eq!(chem_of(I_ENRICHED_CARBON), Some((SUB_CARBON, 80)));
    assert_eq!(energy_of(I_REDSTONE), 10_000);
    assert_eq!(energy_of(REDSTONE_BLOCK), 90_000);
    assert_eq!(oxygen_of(I_FLINT), 10);
    assert_eq!(oxygen_of(I_COAL), 0);
    let purify = all()
        .find(|value| value.kind == MachineKind::Purify && value.input == RAW_IRON_BLOCK)
        .unwrap();
    assert_eq!(
        purify.product,
        Product::Item {
            item: I_CLUMP_IRON,
            count: 18
        }
    );
    assert_eq!(purify.reagent.unwrap().usage, ReagentUsage::PerTick);
    assert_eq!(purify.reagent.unwrap().amount, 2);
    assert_eq!(
        all()
            .find(|value| value.kind == MachineKind::Infuse)
            .unwrap()
            .reagent
            .unwrap()
            .usage,
        ReagentUsage::PerOperation
    );
}

#[test]
fn complete_five_metal_doubling_tables_preserve_output_count() {
    for (index, &(raw, dust, ingot)) in METALS.iter().enumerate() {
        let purify = PURIFY[usize::from(find_purify(raw).unwrap())];
        assert_eq!((purify.out, purify.out_n), (I_CLUMP_IRON + index as u16, 2));
        let crush = CRUSH[usize::from(find_crush(purify.out).unwrap())];
        assert_eq!((crush.in_n, crush.out_n), (1, 1));
        let enrich = ENRICH[usize::from(find_enrich(crush.out).unwrap())];
        assert_eq!((enrich.in_n, enrich.out, enrich.out_n), (1, dust, 1));
        assert_eq!(smelt(enrich.out), Some(ingot));
        assert_eq!(10 * purify.out_n * crush.out_n * enrich.out_n, 20);
    }
}
