//! Optional natural metadata travels in the same kind2 record as mob births.
use crate::{entities, natural};
const MAX_MOBS: usize = 5000;
fn split<'a>(bytes: &'a [u8], magic: &[u8; 4]) -> Result<(&'a [u8], &'a [u8]), &'static str> {
    if bytes.len() < 12 || bytes.len() > save::MAX_RUNTIME_BYTES || &bytes[..4] != magic {
        return Err("natural owner record header");
    }
    let a = u32::from_le_bytes(
        bytes[4..8]
            .try_into()
            .map_err(|_| "natural owner lengths")?,
    ) as usize;
    let b = u32::from_le_bytes(
        bytes[8..12]
            .try_into()
            .map_err(|_| "natural owner lengths")?,
    ) as usize;
    let end = 12usize.checked_add(a).ok_or("natural owner bounds")?;
    if end.checked_add(b) != Some(bytes.len()) {
        return Err("natural owner lengths");
    }
    Ok((&bytes[12..end], &bytes[end..]))
}
fn join(magic: &[u8; 4], a: &[u8], b: &[u8]) -> Result<Vec<u8>, &'static str> {
    let size = 12usize
        .checked_add(a.len())
        .and_then(|v| v.checked_add(b.len()))
        .ok_or("natural owner bounds")?;
    if size > save::MAX_RUNTIME_BYTES {
        return Err("natural owner capacity");
    }
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(magic);
    out.extend_from_slice(&(a.len() as u32).to_le_bytes());
    out.extend_from_slice(&(b.len() as u32).to_le_bytes());
    out.extend_from_slice(a);
    out.extend_from_slice(b);
    Ok(out)
}
pub(super) fn snapshot(
    mobs: &entities::MobRuntime,
    state: &natural::State,
) -> Result<Vec<u8>, &'static str> {
    let mobs = mobs.snapshot().encode().map_err(|_| "mob snapshot codec")?;
    join(b"MNS1", &mobs, &state.encode())
}
pub(super) fn restore(
    bytes: &[u8],
) -> Result<(entities::Snapshot, Option<natural::State>), &'static str> {
    if bytes.starts_with(b"MOR1") {
        return Ok((
            entities::Snapshot::decode(bytes, MAX_MOBS).map_err(|_| "legacy mob snapshot codec")?,
            None,
        ));
    }
    let (mobs, state) = split(bytes, b"MNS1")?;
    Ok((
        entities::Snapshot::decode(mobs, MAX_MOBS).map_err(|_| "mob snapshot codec")?,
        Some(natural::State::decode(state).map_err(|_| "natural snapshot codec")?),
    ))
}
pub(super) fn delta(
    mobs: Option<&entities::MobDelta>,
    state: Option<natural::Delta>,
) -> Result<Vec<u8>, &'static str> {
    if state.is_none() {
        return mobs
            .ok_or("empty natural owner delta")?
            .encode()
            .map_err(|_| "mob delta codec");
    }
    let mobs = mobs
        .map(|d| d.encode())
        .transpose()
        .map_err(|_| "mob delta codec")?
        .unwrap_or_default();
    join(b"MND1", &mobs, &state.expect("checked delta").encode())
}
pub(super) fn decode_delta(
    bytes: &[u8],
) -> Result<(Option<entities::MobDelta>, Option<natural::Delta>), &'static str> {
    if bytes.starts_with(b"MOD1") {
        return Ok((
            Some(
                entities::MobDelta::decode(bytes, MAX_MOBS)
                    .map_err(|_| "legacy mob delta codec")?,
            ),
            None,
        ));
    }
    let (mobs, state) = split(bytes, b"MND1")?;
    if state.is_empty() {
        return Err("missing natural delta");
    }
    Ok((
        if mobs.is_empty() {
            None
        } else {
            Some(entities::MobDelta::decode(mobs, MAX_MOBS).map_err(|_| "mob delta codec")?)
        },
        Some(natural::Delta::decode(state).map_err(|_| "natural delta codec")?),
    ))
}

/// Actual finite owner-cohort progress, including visited Unknown actors. A
/// continuation must not infer this count from successful body timestamps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Progress {
    pub tick: core_types::Tick,
    pub remaining: usize,
}
pub(super) fn with_progress(
    inner: Vec<u8>,
    progress: Option<Progress>,
) -> Result<Vec<u8>, &'static str> {
    let Some(progress) = progress else {
        return Ok(inner);
    };
    if progress.remaining > MAX_MOBS || inner.len() > save::MAX_RUNTIME_BYTES - 20 {
        return Err("mob owner progress capacity");
    }
    let mut bytes = Vec::with_capacity(20 + inner.len());
    bytes.extend_from_slice(b"MOP1");
    bytes.extend_from_slice(&progress.tick.0.to_le_bytes());
    bytes.extend_from_slice(&(progress.remaining as u32).to_le_bytes());
    bytes.extend_from_slice(&(inner.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&inner);
    Ok(bytes)
}
pub(super) fn progress(bytes: &[u8]) -> Result<(&[u8], Option<Progress>), &'static str> {
    if !bytes.starts_with(b"MOP1") {
        return Ok((bytes, None));
    }
    if bytes.len() < 20 || bytes.len() > save::MAX_RUNTIME_BYTES {
        return Err("mob owner progress header");
    }
    let remaining = u32::from_le_bytes(bytes[12..16].try_into().expect("fixed remaining")) as usize;
    let len = u32::from_le_bytes(bytes[16..20].try_into().expect("fixed length")) as usize;
    if remaining > MAX_MOBS
        || 20usize.checked_add(len) != Some(bytes.len())
        || bytes[20..].starts_with(b"MOP1")
    {
        return Err("mob owner progress bounds");
    }
    Ok((
        &bytes[20..],
        Some(Progress {
            tick: core_types::Tick(u64::from_le_bytes(
                bytes[4..12].try_into().expect("fixed tick"),
            )),
            remaining,
        }),
    ))
}

#[cfg(test)]
mod progress_tests {
    use super::*;
    #[test]
    fn progress_preserves_legacy_inner_and_supports_unknown_only_portions() {
        let legacy = b"MOD1fixture".to_vec();
        assert_eq!(with_progress(legacy.clone(), None).unwrap(), legacy);
        assert_eq!(progress(&legacy).unwrap(), (legacy.as_slice(), None));
        for inner in [legacy, Vec::new()] {
            let value = Progress {
                tick: core_types::Tick(u64::MAX),
                remaining: 5000,
            };
            let bytes = with_progress(inner.clone(), Some(value)).unwrap();
            assert_eq!(progress(&bytes).unwrap(), (inner.as_slice(), Some(value)));
            assert_eq!(bytes.len(), 20 + inner.len());
        }
    }
    #[test]
    fn progress_rejects_corruption_trailing_bytes_nested_envelopes_and_quota() {
        let value = Progress {
            tick: core_types::Tick(7),
            remaining: 1,
        };
        let bytes = with_progress(b"MOD1".to_vec(), Some(value)).unwrap();
        for end in 4..bytes.len() {
            assert!(progress(&bytes[..end]).is_err());
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(progress(&trailing).is_err());
        let mut count = bytes.clone();
        count[12..16].copy_from_slice(&5001u32.to_le_bytes());
        assert!(progress(&count).is_err());
        let mut length = bytes.clone();
        length[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(progress(&length).is_err());
        assert!(progress(&with_progress(bytes, Some(value)).unwrap()).is_err());
        assert!(
            with_progress(
                Vec::new(),
                Some(Progress {
                    remaining: 5001,
                    ..value
                })
            )
            .is_err()
        );
        assert!(with_progress(vec![0; save::MAX_RUNTIME_BYTES - 19], Some(value)).is_err());
    }
}
