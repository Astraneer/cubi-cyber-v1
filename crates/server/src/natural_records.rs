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
