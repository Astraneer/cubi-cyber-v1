//! Sanitized far-water topology. This contains no raw block IDs, seed, LIGHT,
//! components or private halo versions. A newer halo view may bind the same
//! centre geometry and column when only a sampled neighbour changes.
use core_types::{PageId, SpaceId, Version};

pub const HALO_EDGE: usize = 18;
pub const HALO_CELLS: usize = HALO_EDGE * HALO_EDGE * HALO_EDGE;
pub const TINT_COLUMNS: usize = 256;
pub const MAX_BYTES: usize = 4 + 16 + 4 * 8 + HALO_CELLS + TINT_COLUMNS * 2;
pub const MAX_REQUEST_BYTES: usize = 4 + 16 + 3 * 8 + 1 + 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaterCell {
    Unknown,
    Air,
    Solid,
    Water { amount: u8, falling: bool },
}
impl WaterCell {
    pub fn encode(self) -> Result<u8, Error> {
        Ok(match self {
            Self::Unknown => 0,
            Self::Air => 1,
            Self::Solid => 2,
            Self::Water {
                amount: amount @ 1..=8,
                falling,
            } => 2 + amount + u8::from(falling) * 8,
            Self::Water { .. } => return Err(Error::Cell),
        })
    }
    pub fn decode(value: u8) -> Result<Self, Error> {
        Ok(match value {
            0 => Self::Unknown,
            1 => Self::Air,
            2 => Self::Solid,
            3..=10 => Self::Water {
                amount: value - 2,
                falling: false,
            },
            11..=18 => Self::Water {
                amount: value - 10,
                falling: true,
            },
            _ => return Err(Error::Cell),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaterHalo {
    pub page: PageId,
    pub geometry_source: Version,
    pub geometry_view: u64,
    pub water_view: u64,
    pub column_source: u64,
    /// x+18*z+324*y, where x/y/z encode local[-1..16] by adding one.
    pub cells: Box<[u8; HALO_CELLS]>,
    pub tints: [u16; TINT_COLUMNS],
}
impl WaterHalo {
    pub fn validate(&self) -> Result<(), Error> {
        valid_page(self.page)?;
        valid_binding(self.geometry_source, self.geometry_view, self.column_source)?;
        if self.water_view == 0 {
            return Err(Error::Version);
        }
        if self.cells.iter().any(|&cell| cell > 18) {
            return Err(Error::Cell);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub page: PageId,
    pub geometry_source: Version,
    pub geometry_view: u64,
    pub column_source: u64,
    pub acknowledged: Option<u64>,
}
impl Request {
    pub fn validate(self) -> Result<(), Error> {
        valid_page(self.page)?;
        valid_binding(self.geometry_source, self.geometry_view, self.column_source)?;
        if self.acknowledged == Some(0) {
            return Err(Error::Version);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Length,
    Magic,
    Bounds,
    Version,
    Cell,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
fn valid_page(page: PageId) -> Result<(), Error> {
    if [page.x, page.y, page.z]
        .into_iter()
        .any(|v| !(i32::MIN / 16..=i32::MAX / 16).contains(&v))
    {
        return Err(Error::Bounds);
    }
    Ok(())
}
fn valid_binding(source: Version, view: u64, column: u64) -> Result<(), Error> {
    if source.0 == 0 || view == 0 || column == 0 {
        return Err(Error::Version);
    }
    Ok(())
}
fn write_page(out: &mut Vec<u8>, page: PageId) {
    out.extend_from_slice(&page.space.0.to_le_bytes());
    for v in [page.x, page.y, page.z] {
        out.extend_from_slice(&v.to_le_bytes());
    }
}
pub fn encode(halo: &WaterHalo) -> Result<Vec<u8>, Error> {
    halo.validate()?;
    let mut out = Vec::with_capacity(MAX_BYTES);
    out.extend_from_slice(b"FWT1");
    write_page(&mut out, halo.page);
    for v in [
        halo.geometry_source.0,
        halo.geometry_view,
        halo.water_view,
        halo.column_source,
    ] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(halo.cells.as_slice());
    for tint in halo.tints {
        out.extend_from_slice(&tint.to_le_bytes());
    }
    Ok(out)
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let end = self.at.checked_add(N).ok_or(Error::Length)?;
        let value = self.bytes.get(self.at..end).ok_or(Error::Length)?;
        self.at = end;
        Ok(value.try_into().expect("fixed slice"))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn page(&mut self) -> Result<PageId, Error> {
        let page = PageId {
            space: SpaceId(u32::from_le_bytes(self.take()?)),
            x: i32::from_le_bytes(self.take()?),
            y: i32::from_le_bytes(self.take()?),
            z: i32::from_le_bytes(self.take()?),
        };
        valid_page(page)?;
        Ok(page)
    }
}
pub fn decode(bytes: &[u8]) -> Result<Box<WaterHalo>, Error> {
    if bytes.len() != MAX_BYTES {
        return Err(Error::Length);
    }
    let mut r = Reader { bytes, at: 0 };
    if r.take::<4>()? != *b"FWT1" {
        return Err(Error::Magic);
    }
    let page = r.page()?;
    let geometry_source = Version(r.u64()?);
    let geometry_view = r.u64()?;
    let water_view = r.u64()?;
    let column_source = r.u64()?;
    valid_binding(geometry_source, geometry_view, column_source)?;
    if water_view == 0 {
        return Err(Error::Version);
    }
    let cell_bytes = &bytes[r.at..r.at + HALO_CELLS];
    if cell_bytes.iter().any(|&cell| cell > 18) {
        return Err(Error::Cell);
    }
    let cells: Box<[u8; HALO_CELLS]> = cell_bytes
        .to_vec()
        .into_boxed_slice()
        .try_into()
        .expect("fixed checked length");
    r.at += HALO_CELLS;
    let mut tints = [0; TINT_COLUMNS];
    for tint in &mut tints {
        *tint = u16::from_le_bytes(r.take()?);
    }
    Ok(Box::new(WaterHalo {
        page,
        geometry_source,
        geometry_view,
        water_view,
        column_source,
        cells,
        tints,
    }))
}
pub fn encode_request(request: Request) -> Result<Vec<u8>, Error> {
    request.validate()?;
    let mut out = Vec::with_capacity(MAX_REQUEST_BYTES);
    out.extend_from_slice(b"FWQ1");
    write_page(&mut out, request.page);
    for v in [
        request.geometry_source.0,
        request.geometry_view,
        request.column_source,
    ] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.push(u8::from(request.acknowledged.is_some()));
    if let Some(version) = request.acknowledged {
        out.extend_from_slice(&version.to_le_bytes());
    }
    Ok(out)
}
pub fn decode_request(bytes: &[u8]) -> Result<Request, Error> {
    if ![MAX_REQUEST_BYTES - 8, MAX_REQUEST_BYTES].contains(&bytes.len()) {
        return Err(Error::Length);
    }
    let mut r = Reader { bytes, at: 0 };
    if r.take::<4>()? != *b"FWQ1" {
        return Err(Error::Magic);
    }
    let page = r.page()?;
    let geometry_source = Version(r.u64()?);
    let geometry_view = r.u64()?;
    let column_source = r.u64()?;
    let acknowledged = match r.take::<1>()?[0] {
        0 => None,
        1 => Some(r.u64()?),
        _ => return Err(Error::Version),
    };
    if r.at != bytes.len() {
        return Err(Error::Length);
    }
    let request = Request {
        page,
        geometry_source,
        geometry_view,
        column_source,
        acknowledged,
    };
    request.validate()?;
    Ok(request)
}
