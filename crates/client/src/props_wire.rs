//! Browser transport for immutable, already-public near-page model snapshots.
//! Encoding copies adaptive storage buffers; cell classification stays on worker.
use super::*;
use world::PageStorage;
pub const MAX_REQUEST_BYTES: usize = 18_000;
pub const MAX_RESULT_BYTES: usize = 42 + 4096 * 10;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropsWireError {
    Length,
    Magic,
    Storage,
    Inputs,
    Sample,
    Cancelled,
}
impl PropsTask {
    pub fn encode(&self) -> Result<Vec<u8>, PropsWireError> {
        if self.id == 0 || self.page.space != SpaceId(0) {
            return Err(PropsWireError::Inputs);
        }
        let mut out = Vec::with_capacity(1600 + self.data.storage().payload_bytes());
        out.extend_from_slice(b"CPJ1");
        context_write(&mut out, self.context());
        for word in self.data.masks().occupancy.words() {
            w64(&mut out, *word);
        }
        for tint in self.tints.iter() {
            w32(&mut out, *tint);
        }
        match self.data.storage() {
            PageStorage::Uniform(cell) => {
                out.push(0);
                w32(&mut out, cell.raw());
            }
            PageStorage::Paletted {
                palette,
                bits,
                indices,
                ..
            } => {
                out.push(1);
                out.push(*bits);
                w16(&mut out, palette.len() as u16);
                w16(&mut out, indices.len() as u16);
                for cell in palette {
                    w32(&mut out, cell.raw());
                }
                for word in indices {
                    w64(&mut out, *word);
                }
            }
            PageStorage::Dense { cells, .. } => {
                out.push(2);
                for cell in cells.iter() {
                    w32(&mut out, cell.raw());
                }
            }
        }
        if out.len() > MAX_REQUEST_BYTES {
            return Err(PropsWireError::Length);
        }
        Ok(out)
    }
}
impl PropsBuilt {
    pub fn encode(&self) -> Result<Vec<u8>, PropsWireError> {
        if self.id == 0
            || self.page.space != SpaceId(0)
            || self.mask_words != 64
            || self.cells > 4096
            || self.samples.len() > self.cells as usize
        {
            return Err(PropsWireError::Inputs);
        }
        let mut out = Vec::with_capacity(42 + self.samples.len() * 10);
        out.extend_from_slice(b"CPR1");
        context_write(
            &mut out,
            PropsContext {
                id: self.id,
                page: self.page,
                source: self.source,
            },
        );
        w16(&mut out, self.mask_words as u16);
        w16(&mut out, self.cells as u16);
        w16(&mut out, self.samples.len() as u16);
        let mut seen = [0u64; 64];
        for sample in &self.samples {
            if sample.position.page() != self.page
                || !eligible(sample.cell)
                || sample.light != 0
                || sample.animation != Animation::default()
            {
                return Err(PropsWireError::Sample);
            }
            let index = sample.position.local_index();
            let i = usize::from(index.get());
            let bit = 1 << (i % 64);
            if seen[i / 64] & bit != 0 {
                return Err(PropsWireError::Sample);
            }
            seen[i / 64] |= bit;
            w16(&mut out, index.get());
            w32(&mut out, sample.cell.raw());
            w32(&mut out, sample.tint);
        }
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Box<Self>, PropsWireError> {
        if !(42..=MAX_RESULT_BYTES).contains(&bytes.len()) {
            return Err(PropsWireError::Length);
        }
        let mut r = Reader { bytes, at: 0 };
        if r.take::<4>()? != *b"CPR1" {
            return Err(PropsWireError::Magic);
        }
        let context = r.context()?;
        let mask_words = u64::from(r.u16()?);
        let cells = u64::from(r.u16()?);
        let n = usize::from(r.u16()?);
        if mask_words != 64 || cells > 4096 || n > cells as usize || bytes.len() != 42 + n * 10 {
            return Err(PropsWireError::Length);
        }
        let mut samples = Vec::with_capacity(n);
        let mut seen = [0u64; 64];
        for _ in 0..n {
            let index = LocalIdx::new(r.u16()?).ok_or(PropsWireError::Sample)?;
            let i = usize::from(index.get());
            let bit = 1 << (i % 64);
            if seen[i / 64] & bit != 0 {
                return Err(PropsWireError::Sample);
            }
            seen[i / 64] |= bit;
            let cell = Cell::from_raw(r.u32()?);
            let tint = r.u32()?;
            if !eligible(cell) {
                return Err(PropsWireError::Sample);
            }
            samples.push(BlockSample {
                position: position(context.page, index).ok_or(PropsWireError::Inputs)?,
                cell,
                tint,
                light: 0,
                animation: Animation::default(),
            });
        }
        Ok(Box::new(Self {
            id: context.id,
            page: context.page,
            source: context.source,
            samples,
            mask_words,
            cells,
            worker_thread: std::thread::current().id(),
        }))
    }
}
pub fn execute_props_request(
    bytes: &[u8],
    cancel: impl Fn() -> bool,
) -> Result<Vec<u8>, PropsWireError> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(PropsWireError::Length);
    }
    let mut r = Reader { bytes, at: 0 };
    if r.take::<4>()? != *b"CPJ1" {
        return Err(PropsWireError::Magic);
    }
    let context = r.context()?;
    let mut occupancy = [0; 64];
    for word in &mut occupancy {
        *word = r.u64()?;
    }
    let mut tints = Box::new([0; 256]);
    for tint in tints.iter_mut() {
        *tint = r.u32()?;
    }
    let storage = match r.u8()? {
        0 => PageStorage::Uniform(Cell::from_raw(r.u32()?)),
        1 => {
            let bits = r.u8()?;
            let n = usize::from(r.u16()?);
            let words = usize::from(r.u16()?);
            if !(1..=8).contains(&bits)
                || !(2..=256).contains(&n)
                || n > 1 << bits
                || words != 64 * usize::from(bits)
                || bytes.len().saturating_sub(r.at) != n * 4 + words * 8
            {
                return Err(PropsWireError::Storage);
            }
            let mut palette = Vec::with_capacity(n);
            for _ in 0..n {
                palette.push(Cell::from_raw(r.u32()?));
            }
            let mut indices = Vec::with_capacity(words);
            for _ in 0..words {
                indices.push(r.u64()?);
            }
            // Validate only on the worker, before the storage accessor can index.
            for i in 0..4096 {
                if cancel() {
                    return Err(PropsWireError::Cancelled);
                }
                let bit = i * usize::from(bits);
                let offset = bit % 64;
                let mut value = indices[bit / 64] >> offset;
                if offset + usize::from(bits) > 64 {
                    value |= indices[bit / 64 + 1] << (64 - offset);
                }
                if (value & ((1 << bits) - 1)) as usize >= n {
                    return Err(PropsWireError::Storage);
                }
            }
            PageStorage::Paletted {
                palette,
                counts: Vec::new(),
                bits,
                indices,
            }
        }
        2 => {
            if bytes.len().saturating_sub(r.at) != 4096 * 4 {
                return Err(PropsWireError::Storage);
            }
            let mut cells = Box::new([Cell::AIR; 4096]);
            for cell in cells.iter_mut() {
                *cell = Cell::from_raw(r.u32()?);
            }
            PageStorage::Dense {
                cells,
                counts: std::collections::BTreeMap::new(),
            }
        }
        _ => return Err(PropsWireError::Storage),
    };
    if r.at != bytes.len() {
        return Err(PropsWireError::Length);
    }
    build_snapshot(
        context,
        &tints,
        &occupancy,
        |index| storage.get(index),
        cancel,
    )
    .ok_or(PropsWireError::Cancelled)?
    .encode()
}
fn w16(out: &mut Vec<u8>, n: u16) {
    out.extend_from_slice(&n.to_le_bytes());
}
fn w32(out: &mut Vec<u8>, n: u32) {
    out.extend_from_slice(&n.to_le_bytes());
}
fn w64(out: &mut Vec<u8>, n: u64) {
    out.extend_from_slice(&n.to_le_bytes());
}
fn context_write(out: &mut Vec<u8>, c: PropsContext) {
    w64(out, c.id);
    w32(out, c.page.space.0);
    for n in [c.page.x, c.page.y, c.page.z] {
        w32(out, n as u32);
    }
    w64(out, c.source.0);
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], PropsWireError> {
        let end = self.at.checked_add(N).ok_or(PropsWireError::Length)?;
        let n = self.bytes.get(self.at..end).ok_or(PropsWireError::Length)?;
        self.at = end;
        Ok(n.try_into().expect("fixed range"))
    }
    fn u8(&mut self) -> Result<u8, PropsWireError> {
        Ok(self.take::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16, PropsWireError> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32, PropsWireError> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64, PropsWireError> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn context(&mut self) -> Result<PropsContext, PropsWireError> {
        let id = self.u64()?;
        let page = PageId {
            space: SpaceId(self.u32()?),
            x: self.u32()? as i32,
            y: self.u32()? as i32,
            z: self.u32()? as i32,
        };
        let source = Version(self.u64()?);
        if id == 0
            || page.space != SpaceId(0)
            || position(page, LocalIdx::new(4095).expect("page")).is_none()
        {
            return Err(PropsWireError::Inputs);
        }
        Ok(PropsContext { id, page, source })
    }
}
