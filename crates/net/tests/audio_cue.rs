use core_types::{SpaceId, Tick};
use net::audio_cue::*;
#[test]
fn causal_cue_has_fixed_canonical_bytes_and_rejects_every_bad_prefix() {
    let cue = Cue {
        kind: Kind::ChickenEgg,
        actor: 7,
        sequence: 9,
        tick: Tick(9),
        space: SpaceId(2),
        position: [1.25, 2., -3.],
    };
    let b = cue.encode().unwrap();
    assert_eq!(&b[..8], b"ACU1\0\0\0\0");
    assert_eq!(&b[8..16], &7u64.to_le_bytes());
    assert_eq!(&b[16..24], &9u64.to_le_bytes());
    assert_eq!(&b[40..48], &1.25f64.to_le_bytes());
    assert_eq!(Cue::decode(&b).unwrap(), cue);
    for n in 0..BYTES {
        assert!(Cue::decode(&b[..n]).is_err());
    }
    let mut bad = b;
    bad[4] = 1;
    assert!(Cue::decode(&bad).is_err());
    let mut bad = b;
    bad[5] = 1;
    assert!(Cue::decode(&bad).is_err());
    let mut bad = b;
    bad[36] = 1;
    assert!(Cue::decode(&bad).is_err());
    assert!(Cue { actor: 0, ..cue }.encode().is_err());
    assert!(Cue { sequence: 0, ..cue }.encode().is_err());
    assert!(Cue { sequence: 8, ..cue }.encode().is_err());
    assert!(
        Cue {
            position: [f64::NAN, 0., 0.],
            ..cue
        }
        .encode()
        .is_err()
    );
}
