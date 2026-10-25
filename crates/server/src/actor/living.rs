//! LIV1 is an optional, exact continuation after the unchanged PLYR v1 prefix.
//! Legacy public HP migrates without granting another spawn protection window.
use super::*;
use rules::survival::health::{DamageKind, HEALTH_BYTES};
const MAGIC: &[u8; 4] = b"LIV1";
const VERSION: u16 = 1;
const BYTES: usize = 4 + 2 + HEALTH_BYTES + 6;

pub(crate) fn capture_living(
    identity: PlayerId,
    space: SpaceId,
    sequence: u64,
    player: &Player,
    health: &Health,
    combat: &CombatStatus,
) -> Result<SavedPlayer, &'static str> {
    let mut saved = capture(identity, space, sequence, player);
    validate_public(&saved.state)?;
    validate(health, combat, player.health)?;
    let encoded = health.encode();
    Health::decode(&encoded).map_err(|_| "invalid living health continuation")?;
    saved.simulation.reserve(BYTES);
    saved.simulation.extend_from_slice(MAGIC);
    saved.simulation.extend_from_slice(&VERSION.to_le_bytes());
    saved.simulation.extend_from_slice(&encoded);
    for value in [combat.zone_grace, combat.combat_tag, combat.attack_ticks] {
        saved.simulation.extend_from_slice(&value.to_le_bytes());
    }
    if saved.simulation.len() > MAX_BYTES {
        return Err("living continuation exceeds bound");
    }
    Ok(saved)
}
fn validate(health: &Health, combat: &CombatStatus, public_hp: f32) -> Result<(), &'static str> {
    if health.hp() != public_hp
        || health.maximum() != 20.0
        || combat.zone_grace > 200
        || combat.combat_tag > 200
    {
        return Err("living continuation disagrees with public health or player bounds");
    }
    Ok(())
}
pub(super) fn read(
    input: &mut Reader<'_>,
    public_hp: f32,
) -> Result<Option<(Health, CombatStatus)>, &'static str> {
    if input.offset == input.bytes.len() {
        return Ok(None);
    }
    if input.bytes.len() - input.offset != BYTES
        || input.array::<4>()? != *MAGIC
        || u16::from_le_bytes(input.array()?) != VERSION
    {
        return Err("invalid living continuation envelope");
    }
    let health = Health::decode(&input.array::<HEALTH_BYTES>()?)
        .map_err(|_| "invalid living health continuation")?;
    let combat = CombatStatus {
        zone_grace: u16::from_le_bytes(input.array()?),
        combat_tag: u16::from_le_bytes(input.array()?),
        attack_ticks: u16::from_le_bytes(input.array()?),
    };
    validate(&health, &combat, public_hp)?;
    Ok(Some((health, combat)))
}
pub(super) fn legacy(public_hp: f32, ticks: u64) -> Result<(Health, CombatStatus), &'static str> {
    let mut bytes = Health::new().encode();
    bytes[4..8].copy_from_slice(&public_hp.to_le_bytes());
    bytes[8..16].copy_from_slice(&ticks.to_le_bytes());
    bytes[31] = 0;
    if public_hp == 0.0 {
        bytes[39] = DamageKind::Generic as u8;
    }
    let health = Health::decode(&bytes).map_err(|_| "invalid legacy public health")?;
    Ok((
        health,
        CombatStatus {
            attack_ticks: u16::MAX,
            ..CombatStatus::default()
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn saved(player: &Player, health: &Health, combat: &CombatStatus) -> SavedPlayer {
        super::super::capture_living(PlayerId([9; 16]), SpaceId(0), 47, player, health, combat)
            .unwrap()
    }
    #[test]
    fn exact_health_and_combat_resume_the_same_next_ticks_and_legacy_prefix_is_unchanged() {
        let mut health = Health::new();
        health.set_spawn_protection(0);
        health.ignite(130);
        for _ in 0..31 {
            health.begin_tick();
            health.tick_air(true);
            health.tick_fire(false, false);
            health.base_tick_end();
        }
        health.damage(DamageKind::PlayerAttack, 6.0, (0, 0.0), false);
        let mut player = Player::new([0.5, 6.0, 0.5]);
        player.health = health.hp();
        let mut combat = CombatStatus {
            zone_grace: 71,
            combat_tag: 133,
            attack_ticks: 9,
        };
        let legacy = capture(PlayerId([9; 16]), SpaceId(0), 47, &player);
        let encoded = saved(&player, &health, &combat);
        assert_eq!(
            &encoded.simulation[..legacy.simulation.len()],
            legacy.simulation
        );
        let (restored, mut other_health, mut other_combat) =
            restore_living(&encoded, false).unwrap();
        assert_eq!(restored.state_hash(), player.state_hash());
        assert_eq!(health.encode(), other_health.encode());
        assert_eq!(combat, other_combat);
        for tick in 0..100 {
            for h in [&mut health, &mut other_health] {
                h.begin_tick();
                h.tick_air(tick < 70);
                h.tick_fire(tick > 80, false);
                if tick % 17 == 0 {
                    h.damage(DamageKind::MobAttack, 2.0, (0, 0.0), false);
                }
                h.base_tick_end();
            }
            combat.tick(tick < 2);
            other_combat.tick(tick < 2);
            assert_eq!(health.encode(), other_health.encode());
            assert_eq!(combat, other_combat);
        }
    }
    #[test]
    fn legacy_migration_keeps_hp_and_does_not_grant_a_spawn_shield() {
        for hp in [20.0, 7.0, 0.0] {
            let mut player = Player::new([0.5, 6.0, 0.5]);
            player.health = hp;
            player.ticks = 1234;
            let old = capture(PlayerId([9; 16]), SpaceId(0), 47, &player);
            let (_, mut health, combat) = restore_living(&old, false).unwrap();
            assert_eq!(health.hp(), hp);
            assert_eq!(health.now(), 1234);
            assert_eq!(health.encode()[31], 0);
            assert_eq!(combat.zone_grace, 0);
            if hp > 0.0 {
                assert!(
                    health
                        .damage(DamageKind::PlayerAttack, 1.0, (0, 0.0), false)
                        .unwrap()
                        .taken
                        > 0.0
                );
            }
        }
    }
    #[test]
    fn partial_invalid_nonfinite_and_public_mismatch_extensions_fail_closed() {
        let player = Player::new([0.5, 6.0, 0.5]);
        let health = Health::new();
        let combat = CombatStatus::default();
        let encoded = saved(&player, &health, &combat);
        let base = encoded.simulation.len() - BYTES;
        for n in 1..BYTES {
            let mut short = encoded.clone();
            short.simulation.truncate(base + n);
            assert!(restore_living(&short, false).is_err());
        }
        for offset in [base, base + 4, base + 6, base + 6 + 44] {
            let mut invalid = encoded.clone();
            invalid.simulation[offset] ^= 255;
            assert!(restore_living(&invalid, false).is_err());
        }
        let mut invalid = encoded.clone();
        invalid.state.health = 19.0;
        assert!(restore_living(&invalid, false).is_err());
        let mut invalid = encoded.clone();
        invalid.simulation[base + 6 + 4..base + 6 + 8].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(restore_living(&invalid, false).is_err());
        let mut invalid = encoded.clone();
        invalid.simulation[base + 6 + HEALTH_BYTES..base + 8 + HEALTH_BYTES]
            .copy_from_slice(&201_u16.to_le_bytes());
        assert!(restore_living(&invalid, false).is_err());
        let mut invalid = encoded;
        invalid.simulation.push(0);
        assert!(restore_living(&invalid, false).is_err());
    }
}
