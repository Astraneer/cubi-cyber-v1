use rules::machines::grid::*;
const BUDGET: TopologyBudget = TopologyBudget {
    nodes: 100_000,
    endpoints: 10_000,
    bytes: 8 * 1024 * 1024,
};
fn plan(grid: &Grid, edit: TopologyEdit) -> TopologyDelta {
    grid.plan_edit(edit, BUDGET, &mut GridWork::default())
        .unwrap()
}
#[test]
fn detached_add_remove_match_detailed_interpreter_and_exact_split_remainder() {
    let mut actual = Grid::new(128, 64);
    let mut reference = Grid::new(128, 64);
    for x in 0..10 {
        let before = actual.snapshot();
        let delta = plan(
            &actual,
            TopologyEdit::Add {
                position: [x, 0, 0],
                resource: Resource::Energy,
                links: 63,
            },
        );
        assert_eq!(
            actual.snapshot(),
            before,
            "planning must not change authority"
        );
        let records = delta.records().clone();
        let restored = actual
            .restore_topology_delta(records, BUDGET, &mut GridWork::default())
            .unwrap();
        actual.apply_edit(restored).unwrap();
        reference
            .add([x, 0, 0], Resource::Energy, 63, &mut GridWork::default())
            .unwrap();
        assert_eq!(actual.snapshot(), reference.snapshot());
    }
    actual.insert([0, 0, 0], 0, 40_000).unwrap();
    reference.insert([0, 0, 0], 0, 40_000).unwrap();
    let old = actual.network_at([0, 0, 0]).unwrap();
    let delta = plan(
        &actual,
        TopologyEdit::Remove {
            position: [4, 0, 0],
        },
    );
    assert_eq!(delta.overflow(), 0);
    actual.apply_edit(delta).unwrap();
    reference
        .remove([4, 0, 0], &mut GridWork::default())
        .unwrap();
    assert_eq!(actual.snapshot(), reference.snapshot());
    let left = actual.network_at([0, 0, 0]).unwrap();
    let right = actual.network_at([5, 0, 0]).unwrap();
    assert_eq!(right, old, "largest part keeps exact handle");
    assert_eq!(actual.network(left).unwrap().stored, 17_777);
    assert_eq!(actual.network(right).unwrap().stored, 22_223);
    let restored = Grid::restore(128, 64, &actual.snapshot()).unwrap();
    assert_eq!(restored.snapshot(), actual.snapshot());
}
#[test]
fn relinking_full_capacity_never_temporarily_deletes_the_node_or_loses_resource() {
    let mut grid = Grid::new(64, 16);
    for x in -2..=2 {
        grid.add([x, 0, 0], Resource::Energy, 63, &mut GridWork::default())
            .unwrap();
    }
    grid.insert([0, 0, 0], 0, 40_000).unwrap();
    let original = grid.network_at([-2, 0, 0]).unwrap();
    let split = plan(
        &grid,
        TopologyEdit::Relink {
            position: [0, 0, 0],
            links: 0,
        },
    );
    assert_eq!(split.overflow(), 0);
    grid.apply_edit(split).unwrap();
    assert_eq!(grid.network_at([-2, 0, 0]), Some(original));
    assert_eq!(
        grid.network_ids()
            .map(|id| grid.network(id).unwrap().stored)
            .sum::<u64>(),
        40_000
    );
    assert_eq!(grid.network_ids().count(), 3);
    let join = plan(
        &grid,
        TopologyEdit::Relink {
            position: [0, 0, 0],
            links: 63,
        },
    );
    grid.apply_edit(join).unwrap();
    assert_eq!(grid.network_ids().count(), 1);
    assert_eq!(
        grid.network(grid.network_at([0, 0, 0]).unwrap())
            .unwrap()
            .stored,
        40_000
    );
    let mut negative = Grid::restore(64, 16, &grid.snapshot()).unwrap();
    assert_eq!(
        negative
            .remove([0, 0, 0], &mut GridWork::default())
            .unwrap(),
        8_000,
        "remove/add would incorrectly spill full-capacity reservoir during a link-only edit"
    );
}
#[test]
fn budgets_reservoir_guards_and_tampered_records_reject_before_mutation() {
    let mut grid = Grid::new(64, 16);
    grid.add([0, 0, 0], Resource::Energy, 63, &mut GridWork::default())
        .unwrap();
    let before = grid.snapshot();
    let edit = TopologyEdit::Add {
        position: [1, 0, 0],
        resource: Resource::Energy,
        links: 63,
    };
    assert!(matches!(
        grid.plan_edit(
            edit,
            TopologyBudget { nodes: 0, ..BUDGET },
            &mut GridWork::default()
        ),
        Err(GridError::Capacity)
    ));
    assert_eq!(grid.snapshot(), before);
    let delta = plan(&grid, edit);
    let mut forged = delta.records().clone();
    forged.after_nodes[0].links = 0;
    assert!(matches!(
        grid.restore_topology_delta(forged, BUDGET, &mut GridWork::default()),
        Err(GridError::Stale)
    ));
    assert_eq!(grid.snapshot(), before);
    grid.insert([0, 0, 0], 0, 1).unwrap();
    let current = grid.snapshot();
    assert_eq!(grid.apply_edit(delta), Err(GridError::Stale));
    assert_eq!(grid.snapshot(), current);
}
#[test]
fn one_edit_visits_one_component_among_ten_thousand_unrelated_networks() {
    let mut grid = Grid::new(20_000, 16);
    for x in 0..10_000 {
        grid.add(
            [x * 3, 20, 0],
            Resource::Energy,
            63,
            &mut GridWork::default(),
        )
        .unwrap();
    }
    for x in 0..10 {
        grid.add([x, 0, 0], Resource::Energy, 63, &mut GridWork::default())
            .unwrap();
    }
    let mut work = GridWork::default();
    let delta = grid
        .plan_edit(
            TopologyEdit::Remove {
                position: [4, 0, 0],
            },
            TopologyBudget {
                nodes: 41,
                endpoints: 0,
                bytes: 64 * 1024,
            },
            &mut work,
        )
        .unwrap();
    assert_eq!(delta.records().before_nodes.len(), 10);
    assert_eq!(delta.affected_before().count(), 1);
    assert_eq!(work.visited_networks, 1);
    assert!(work.topology_nodes <= 41);
    assert!(delta.records().before_slots.len() <= 8);
    // An unrelated reservoir is deliberately changed after planning. Its value
    // is neither cloned into the delta nor overwritten by publication.
    grid.insert([0, 20, 0], 0, 1234).unwrap();
    grid.apply_edit(delta).unwrap();
    assert_eq!(
        grid.network(grid.network_at([0, 20, 0]).unwrap())
            .unwrap()
            .stored,
        1234
    );
}
#[test]
fn installing_one_cable_and_relinking_six_neighbors_is_one_guarded_publication() {
    let mut grid = Grid::new(64, 16);
    let neighbors = [
        [1, 0, 0],
        [-1, 0, 0],
        [0, 1, 0],
        [0, -1, 0],
        [0, 0, 1],
        [0, 0, -1],
    ];
    for p in neighbors {
        grid.add(p, Resource::Energy, 0, &mut GridWork::default())
            .unwrap();
        grid.insert(p, 0, 1000).unwrap();
    }
    assert_eq!(grid.network_ids().count(), 6);
    let mut edits = vec![TopologyEdit::Add {
        position: [0, 0, 0],
        resource: Resource::Energy,
        links: 63,
    }];
    edits.extend(neighbors.map(|position| TopologyEdit::Relink {
        position,
        links: 63,
    }));
    let before = grid.snapshot();
    let delta = grid
        .plan_edits(&edits, BUDGET, &mut GridWork::default())
        .unwrap();
    assert_eq!(grid.snapshot(), before);
    assert_eq!(delta.records().extra_edits.len(), 6);
    assert_eq!(delta.affected_before().count(), 6);
    assert_eq!(delta.affected_after().count(), 1);
    let restored = grid
        .restore_topology_delta(delta.records().clone(), BUDGET, &mut GridWork::default())
        .unwrap();
    grid.apply_edit(restored).unwrap();
    let network = grid.network(grid.network_at([0, 0, 0]).unwrap()).unwrap();
    assert_eq!(network.members().len(), 7);
    assert_eq!(network.stored, 6000);
    assert!(
        neighbors
            .iter()
            .all(|p| grid.network_at(*p) == grid.network_at([0, 0, 0]))
    );
}
#[test]
fn batch_preconditions_use_prior_edits_and_reject_a_bad_last_edit_atomically() {
    let mut grid = Grid::new(2, 16);
    let edits = [
        TopologyEdit::Add {
            position: [0, 0, 0],
            resource: Resource::Energy,
            links: 63,
        },
        TopologyEdit::Add {
            position: [1, 0, 0],
            resource: Resource::Energy,
            links: 63,
        },
        TopologyEdit::Relink {
            position: [0, 0, 0],
            links: 0,
        },
    ];
    let empty = grid.snapshot();
    let delta = grid
        .plan_edits(&edits, BUDGET, &mut GridWork::default())
        .unwrap();
    assert_eq!(grid.snapshot(), empty);
    grid.apply_edit(delta).unwrap();
    assert_eq!(grid.network_ids().count(), 2);
    let edits = [
        TopologyEdit::Remove {
            position: [1, 0, 0],
        },
        TopologyEdit::Add {
            position: [1, 0, 0],
            resource: Resource::Energy,
            links: 0,
        },
    ];
    let delta = grid
        .plan_edits(&edits, BUDGET, &mut GridWork::default())
        .unwrap();
    grid.apply_edit(delta).unwrap();
    assert_eq!(grid.node_count(), 2);
    let before = grid.snapshot();
    let invalid = [
        TopologyEdit::Remove {
            position: [1, 0, 0],
        },
        TopologyEdit::Relink {
            position: [123, 0, 0],
            links: 63,
        },
    ];
    assert!(matches!(
        grid.plan_edits(&invalid, BUDGET, &mut GridWork::default()),
        Err(GridError::Missing)
    ));
    assert_eq!(grid.snapshot(), before);
    assert!(matches!(
        grid.plan_edits(&[], BUDGET, &mut GridWork::default()),
        Err(GridError::Capacity)
    ));
}
