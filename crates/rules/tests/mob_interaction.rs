use core_types::{SpaceId, Tick};
use rules::{
    entities::{
        EntityError, EntityId, MobKind,
        combat::{
            AttackPose, CombatStatus, Combatant,
            mob::{AttackContext, MobAttackPlan},
        },
        mob::{AiBudget, Environment, Mob, PlayerIndex, PlayerProbe},
    },
    items::*,
    stack::ItemStack,
    survival::{Inventory, health::Health},
};
fn actor() -> Combatant {
    let mut health = Health::new();
    health.set_spawn_protection(0);
    Combatant {
        id: EntityId(99),
        position: [0.5, 1.0, 0.5],
        velocity: [0.0; 3],
        crouching: false,
        inventory: Inventory::empty(),
        health,
        status: CombatStatus {
            attack_ticks: 100,
            ..Default::default()
        },
        pose: AttackPose {
            on_ground: true,
            ..Default::default()
        },
    }
}
fn context(actor: Combatant) -> AttackContext {
    AttackContext {
        space: SpaceId(0),
        eye: [0.5, 2.62, 0.5],
        look_horizontal: [0.0, 1.0],
        expected_held: actor.inventory.hand(),
        now: Tick(10),
    }
}
#[test]
fn donor_fist_cow_window_knockback_panic_and_stale_guard() {
    let mut actor = actor();
    let mut cow = Mob::new(EntityId(1), MobKind::Cow, [0.5, 1.0, 2.5]).unwrap();
    let plan = MobAttackPlan::prepare(&actor, &cow, context(actor), |_, _| Ok(true)).unwrap();
    let stale = plan;
    plan.apply(&mut actor, &mut cow).unwrap();
    assert_eq!(cow.life.hp(), 9.0);
    assert_eq!(cow.ai.panic_until, Tick(50));
    assert!((cow.body.velocity[2] - 0.4).abs() < 1e-12);
    assert_eq!(cow.last_player_hit, Some((actor.id, Tick(10))));
    assert_eq!(
        stale.apply(&mut actor, &mut cow),
        Err(EntityError::StalePlan)
    );
    actor.status.attack_ticks = 100;
    let next = MobAttackPlan::prepare(&actor, &cow, context(actor), |_, _| Ok(true)).unwrap();
    assert!(next.hit.is_none());
    assert_eq!(next.target.life.hp(), 9.0);
    assert_eq!(next.attacker.status.attack_ticks, 0);
}
#[test]
fn owned_sword_wear_zombie_armor_and_revenge_prefer_attacker() {
    let mut actor = actor();
    actor
        .inventory
        .set_trusted(0, ItemStack::new(I_WOODEN_PICKAXE + 14, 1).unwrap())
        .unwrap();
    let zombie = Mob::new(EntityId(1), MobKind::Zombie, [0.5, 1.0, 2.5]).unwrap();
    let plan = MobAttackPlan::prepare(&actor, &zombie, context(actor), |_, _| Ok(true)).unwrap();
    assert!((plan.target.life.hp() - 14.096).abs() < 1e-5);
    assert_eq!(plan.attacker.inventory.hand().damage(), 1);
    assert_eq!(
        Mob::decode(&plan.target.encode().unwrap()).unwrap(),
        plan.target
    );
    let mut players = PlayerIndex::new(2).unwrap();
    players
        .rebuild(&[
            PlayerProbe {
                id: actor.id,
                space: SpaceId(0),
                position: [0.5, 1.0, 8.5],
                eye: [0.5, 2.62, 8.5],
                hands: [0; 2],
                alive: true,
                creative: false,
                spectator: false,
                protected: false,
            },
            PlayerProbe {
                id: EntityId(100),
                space: SpaceId(0),
                position: [0.5, 1.0, 3.5],
                eye: [0.5, 2.62, 3.5],
                hands: [0; 2],
                alive: true,
                creative: false,
                spectator: false,
                protected: false,
            },
        ])
        .unwrap();
    let (mob, _) = plan
        .target
        .plan_ai(
            Tick(11),
            1,
            &players,
            Environment::default(),
            &mut AiBudget::default(),
        )
        .unwrap();
    assert_eq!(mob.ai.target, Some(actor.id));
    let (mob, _) = mob
        .plan_ai(
            Tick(312),
            1,
            &players,
            Environment::default(),
            &mut AiBudget::default(),
        )
        .unwrap();
    assert_eq!(mob.ai.target, Some(EntityId(100)));
    let mut wrong = context(actor);
    wrong.expected_held = ItemStack::EMPTY;
    assert_eq!(
        MobAttackPlan::prepare(&actor, &zombie, wrong, |_, _| Ok(true)),
        Err(EntityError::NoEntitlement)
    );
}
#[test]
fn real_chicken_box_los_unknown_space_and_reach_guard_before_cooldown() {
    let actor = actor();
    let chicken = Mob::new(EntityId(1), MobKind::Chicken, [0.5, 1.0, 2.5]).unwrap();
    let mut heights = Vec::new();
    assert!(
        MobAttackPlan::prepare(&actor, &chicken, context(actor), |_, b| {
            heights.push(b[1]);
            Ok(false)
        })
        .is_err()
    );
    assert!(
        heights
            .iter()
            .zip([1.1, 1.35, 1.6])
            .all(|(a, b)| (a - b).abs() < 1e-12)
    );
    assert_eq!(
        MobAttackPlan::prepare(&actor, &chicken, context(actor), |_, _| Err(
            EntityError::UnknownInput
        )),
        Err(EntityError::UnknownInput)
    );
    let mut far = chicken;
    far.body.position[2] = 20.0;
    assert_eq!(
        MobAttackPlan::prepare(&actor, &far, context(actor), |_, _| panic!(
            "reach precedes ray"
        )),
        Err(EntityError::NoEntitlement)
    );
    let mut other = context(actor);
    other.space = SpaceId(1);
    assert_eq!(
        MobAttackPlan::prepare(&actor, &chicken, other, |_, _| Ok(true)),
        Err(EntityError::NoEntitlement)
    );
    assert_eq!(actor.status.attack_ticks, 100);
}
