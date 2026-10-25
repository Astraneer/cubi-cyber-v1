//! Optional sparse references preserve every byte of legacy zero-reference
//! encodings. The enclosing codec owns its prefix and any subsequent sections.
use super::{Error, PayloadId};
pub const MAX_REFERENCE_SLOTS: usize = 1024;
pub fn append(
    out: &mut Vec<u8>,
    refs: impl IntoIterator<Item = (usize, Option<PayloadId>)>,
) -> Result<(), Error> {
    let mut sparse = Vec::new();
    for (slot, id) in refs {
        if let Some(id) = id {
            if id.0 == 0
                || slot > u16::MAX as usize
                || sparse.last().is_some_and(|(last, _)| *last >= slot)
            {
                return Err(Error::Encoding);
            }
            sparse.push((slot, id));
            if sparse.len() > MAX_REFERENCE_SLOTS {
                return Err(Error::Bounds);
            }
        }
    }
    if sparse.is_empty() {
        return Ok(());
    }
    out.extend_from_slice(b"XRF1");
    out.extend_from_slice(&(sparse.len() as u16).to_le_bytes());
    for (slot, id) in sparse {
        out.extend_from_slice(&(slot as u16).to_le_bytes());
        out.extend_from_slice(&id.0.to_le_bytes());
    }
    Ok(())
}
/// Missing XRF1 is the legacy empty reference set, consuming zero bytes.
/// A recognized section must be complete and canonical before allocation.
pub fn read(bytes: &[u8], slots: usize) -> Result<(Vec<(usize, PayloadId)>, usize), Error> {
    if !bytes.starts_with(b"XRF1") {
        return Ok((Vec::new(), 0));
    }
    if bytes.len() < 6 || slots > MAX_REFERENCE_SLOTS {
        return Err(Error::Encoding);
    }
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    let used = 6 + count * 10;
    if count == 0 || count > slots || bytes.len() < used {
        return Err(Error::Encoding);
    }
    let mut refs = Vec::with_capacity(count);
    for raw in bytes[6..used].chunks_exact(10) {
        let slot = u16::from_le_bytes([raw[0], raw[1]]) as usize;
        let id = PayloadId(u64::from_le_bytes(
            raw[2..10].try_into().map_err(|_| Error::Encoding)?,
        ));
        if id.0 == 0 || slot >= slots || refs.last().is_some_and(|(last, _)| *last >= slot) {
            return Err(Error::Encoding);
        }
        refs.push((slot, id));
    }
    Ok((refs, used))
}
