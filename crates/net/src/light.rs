//! Public near-page light, bound to a confirmed sanitized cell view. Source and
//! light-view versions are independent of cell/mesh versions. No cells, seed,
//! credentials or block-entity contents are present in this protocol.
use core_types::{PAGE_VOLUME, PageId, SpaceId, Version};
use std::collections::BTreeMap;
pub const MAX_BYTES: usize = 32 * 1024;
pub const MAX_REQUEST_BYTES: usize = 37;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub page: PageId,
    pub cell_view: u64,
    pub version: u64,
    pub source: Version,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub header: Header,
    pub values: Box<[u32; PAGE_VOLUME]>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delta {
    pub header: Header,
    pub base: u64,
    pub changes: Vec<(u16, u32)>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Update {
    Snapshot(Snapshot),
    Delta(Delta),
}
impl Update {
    pub fn header(&self) -> Header {
        match self {
            Self::Snapshot(s) => s.header,
            Self::Delta(d) => d.header,
        }
    }
    pub fn validate(&self) -> Result<(), Error> {
        valid_header(self.header())?;
        if let Self::Delta(delta) = self
            && (delta.base == 0
                || delta.header.version <= delta.base
                || delta.changes.len() > PAGE_VOLUME
                || delta
                    .changes
                    .iter()
                    .any(|(index, _)| usize::from(*index) >= PAGE_VOLUME)
                || delta.changes.windows(2).any(|v| v[0].0 >= v[1].0))
        {
            return Err(Error::InvalidDelta);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub page: PageId,
    pub cell_view: u64,
    pub acknowledged: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Bounds,
    Malformed,
    Truncated,
    Trailing,
    InvalidVersion,
    InvalidDelta,
    ConflictingVersion,
    Capacity,
    MissingCells {
        page: PageId,
        have: Option<u64>,
        expected: u64,
    },
    MissingBase {
        page: PageId,
        have: Option<u64>,
        expected: u64,
    },
}
/// World permits full eight-bit visible RGB; MC scalar levels remain four-bit.
pub const fn pack(sky: u8, block: u8, rgb: [u8; 3]) -> Option<u32> {
    if sky > 15 || block > 15 {
        None
    } else {
        Some(
            sky as u32
                | ((block as u32) << 4)
                | ((rgb[0] as u32) << 8)
                | ((rgb[1] as u32) << 16)
                | ((rgb[2] as u32) << 24),
        )
    }
}
pub const fn unpack(value: u32) -> (u8, u8, [u8; 3]) {
    (
        (value & 15) as u8,
        ((value >> 4) & 15) as u8,
        [(value >> 8) as u8, (value >> 16) as u8, (value >> 24) as u8],
    )
}
fn valid_page(page: PageId) -> Result<(), Error> {
    if [page.x, page.y, page.z]
        .iter()
        .any(|axis| !(i32::MIN / 16..=i32::MAX / 16).contains(axis))
    {
        return Err(Error::Bounds);
    }
    Ok(())
}
fn valid_header(header: Header) -> Result<(), Error> {
    valid_page(header.page)?;
    if header.cell_view == 0 || header.version == 0 {
        return Err(Error::InvalidVersion);
    }
    Ok(())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Apply {
    Applied,
    Stale,
}
pub struct Replica {
    pages: BTreeMap<PageId, Snapshot>,
    maximum: usize,
}
impl Replica {
    pub fn new(maximum: usize) -> Result<Self, Error> {
        if maximum == 0 || maximum > 4096 {
            return Err(Error::Capacity);
        }
        Ok(Self {
            pages: BTreeMap::new(),
            maximum,
        })
    }
    pub fn page(&self, page: PageId) -> Option<&Snapshot> {
        self.pages.get(&page)
    }
    pub fn len(&self) -> usize {
        self.pages.len()
    }
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }
    pub fn remove(&mut self, page: PageId) -> bool {
        self.pages.remove(&page).is_some()
    }
    pub fn apply(
        &mut self,
        update: Update,
        confirmed_cell_view: Option<u64>,
    ) -> Result<Apply, Error> {
        update.validate()?;
        let header = update.header();
        if confirmed_cell_view.is_some_and(|view| header.cell_view < view) {
            return Ok(Apply::Stale);
        }
        if confirmed_cell_view != Some(header.cell_view) {
            return Err(Error::MissingCells {
                page: header.page,
                have: confirmed_cell_view,
                expected: header.cell_view,
            });
        }
        let old = self.pages.get(&header.page);
        if old.is_some_and(|old| old.header.version > header.version) {
            return Ok(Apply::Stale);
        }
        if let Some(old) = old {
            if old.header.version == header.version {
                let agrees = old.header == header
                    && match &update {
                        Update::Snapshot(s) => s.values == old.values,
                        Update::Delta(d) => d
                            .changes
                            .iter()
                            .all(|(i, v)| old.values[usize::from(*i)] == *v),
                    };
                return if agrees {
                    Ok(Apply::Stale)
                } else {
                    Err(Error::ConflictingVersion)
                };
            }
            if header.source < old.header.source || header.cell_view < old.header.cell_view {
                return Err(Error::InvalidVersion);
            }
        }
        match update {
            Update::Snapshot(snapshot) => {
                if old.is_none() && self.pages.len() == self.maximum {
                    return Err(Error::Capacity);
                }
                self.pages.insert(header.page, snapshot);
            }
            Update::Delta(delta) => {
                let have = old.map(|s| s.header.version);
                if have != Some(delta.base) {
                    return Err(Error::MissingBase {
                        page: header.page,
                        have,
                        expected: delta.base,
                    });
                }
                let state = self
                    .pages
                    .get_mut(&header.page)
                    .expect("checked light base");
                for (index, value) in delta.changes {
                    state.values[usize::from(index)] = value;
                }
                state.header = header;
            }
        }
        Ok(Apply::Applied)
    }
}
pub fn encode(update: &Update) -> Result<Vec<u8>, Error> {
    update.validate()?;
    let header = update.header();
    let mut out = Vec::new();
    out.extend_from_slice(b"LGT1");
    out.push(u8::from(matches!(update, Update::Delta(_))));
    write_header(&mut out, header);
    match update {
        Update::Snapshot(s) => {
            for value in s.values.iter() {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        Update::Delta(d) => {
            out.extend_from_slice(&d.base.to_le_bytes());
            out.extend_from_slice(&(d.changes.len() as u16).to_le_bytes());
            for (index, value) in &d.changes {
                out.extend_from_slice(&index.to_le_bytes());
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
    }
    if out.len() > MAX_BYTES {
        return Err(Error::Bounds);
    }
    Ok(out)
}
pub fn decode(bytes: &[u8]) -> Result<Update, Error> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::Bounds);
    }
    let mut input = Reader { bytes, at: 0 };
    if input.take(4)? != b"LGT1" {
        return Err(Error::Malformed);
    }
    let kind = input.byte()?;
    let header = input.header()?;
    let update = match kind {
        0 => {
            let mut values = Box::new([0; PAGE_VOLUME]);
            for value in values.iter_mut() {
                *value = input.u32()?;
            }
            Update::Snapshot(Snapshot { header, values })
        }
        1 => {
            let base = input.u64()?;
            let count = usize::from(input.u16()?);
            if count > PAGE_VOLUME {
                return Err(Error::Bounds);
            }
            let mut changes = Vec::with_capacity(count);
            for _ in 0..count {
                changes.push((input.u16()?, input.u32()?));
            }
            Update::Delta(Delta {
                header,
                base,
                changes,
            })
        }
        _ => return Err(Error::Malformed),
    };
    if input.at != bytes.len() {
        return Err(Error::Trailing);
    }
    update.validate()?;
    Ok(update)
}
pub fn encode_request(request: Request) -> Result<Vec<u8>, Error> {
    valid_page(request.page)?;
    if request.cell_view == 0 || request.acknowledged == Some(0) {
        return Err(Error::InvalidVersion);
    }
    let mut out = Vec::with_capacity(MAX_REQUEST_BYTES);
    out.extend_from_slice(b"LGR1");
    write_page(&mut out, request.page);
    out.extend_from_slice(&request.cell_view.to_le_bytes());
    out.push(u8::from(request.acknowledged.is_some()));
    if let Some(value) = request.acknowledged {
        out.extend_from_slice(&value.to_le_bytes());
    }
    Ok(out)
}
pub fn decode_request(bytes: &[u8]) -> Result<Request, Error> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(Error::Bounds);
    }
    let mut input = Reader { bytes, at: 0 };
    if input.take(4)? != b"LGR1" {
        return Err(Error::Malformed);
    }
    let page = input.page()?;
    let cell_view = input.u64()?;
    let acknowledged = match input.byte()? {
        0 => None,
        1 => Some(input.u64()?),
        _ => return Err(Error::Malformed),
    };
    let request = Request {
        page,
        cell_view,
        acknowledged,
    };
    if input.at != bytes.len() {
        return Err(Error::Trailing);
    }
    if cell_view == 0 || acknowledged == Some(0) {
        return Err(Error::InvalidVersion);
    }
    Ok(request)
}
fn write_header(out: &mut Vec<u8>, header: Header) {
    write_page(out, header.page);
    out.extend_from_slice(&header.cell_view.to_le_bytes());
    out.extend_from_slice(&header.version.to_le_bytes());
    out.extend_from_slice(&header.source.0.to_le_bytes());
}
fn write_page(out: &mut Vec<u8>, page: PageId) {
    out.extend_from_slice(&page.space.0.to_le_bytes());
    for value in [page.x, page.y, page.z] {
        out.extend_from_slice(&value.to_le_bytes());
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take(&mut self, len: usize) -> Result<&[u8], Error> {
        let end = self.at.checked_add(len).ok_or(Error::Bounds)?;
        let out = self.bytes.get(self.at..end).ok_or(Error::Truncated)?;
        self.at = end;
        Ok(out)
    }
    fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn page(&mut self) -> Result<PageId, Error> {
        let page = PageId {
            space: SpaceId(self.u32()?),
            x: self.u32()? as i32,
            y: self.u32()? as i32,
            z: self.u32()? as i32,
        };
        valid_page(page)?;
        Ok(page)
    }
    fn header(&mut self) -> Result<Header, Error> {
        let header = Header {
            page: self.page()?,
            cell_view: self.u64()?,
            version: self.u64()?,
            source: Version(self.u64()?),
        };
        valid_header(header)?;
        Ok(header)
    }
}
