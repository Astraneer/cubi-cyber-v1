//! Bounded WOFF1 unpacking for the donor's CC0 VoxelPixel font. No platform fonts
//! or font enumeration are used, so native and browser receive identical glyphs.
use std::io::Read;
pub const FONT_SOURCE: &[u8] = include_bytes!("../../../donor/web/ui/pixel.woff");
pub const FONT_CREDITS: &str =
    "VoxelPixel — CC0 1.0; license preserved in the embedded WOFF metadata.";
pub fn unpack_woff(bytes: &[u8]) -> Result<Vec<u8>, &'static str> {
    let u16_at = |offset: usize| -> Result<u16, &'static str> {
        Ok(u16::from_be_bytes(
            bytes
                .get(offset..offset + 2)
                .ok_or("truncated WOFF")?
                .try_into()
                .unwrap(),
        ))
    };
    let u32_at = |offset: usize| -> Result<u32, &'static str> {
        Ok(u32::from_be_bytes(
            bytes
                .get(offset..offset + 4)
                .ok_or("truncated WOFF")?
                .try_into()
                .unwrap(),
        ))
    };
    if bytes.get(..4) != Some(b"wOFF") || u32_at(8)? as usize != bytes.len() {
        return Err("invalid WOFF1 header");
    }
    let tables = usize::from(u16_at(12)?);
    let size = u32_at(16)? as usize;
    if tables == 0 || tables > 128 || size > 8 * 1024 * 1024 || size < 12 + tables * 16 {
        return Err("WOFF font budget");
    }
    let mut output = vec![0u8; size];
    output[..4].copy_from_slice(&bytes[4..8]);
    output[4..6].copy_from_slice(&(tables as u16).to_be_bytes());
    let power = 1usize << (usize::BITS - tables.leading_zeros() - 1);
    output[6..8].copy_from_slice(&((power * 16) as u16).to_be_bytes());
    output[8..10].copy_from_slice(&((power.trailing_zeros()) as u16).to_be_bytes());
    output[10..12].copy_from_slice(&((tables * 16 - power * 16) as u16).to_be_bytes());
    let mut next = 12 + tables * 16;
    let mut tags = std::collections::BTreeSet::new();
    for table in 0..tables {
        let record = 44 + table * 20;
        let tag = bytes.get(record..record + 4).ok_or("WOFF table")?;
        if !tags.insert(tag.to_vec()) {
            return Err("duplicate WOFF table");
        }
        let offset = u32_at(record + 4)? as usize;
        let compressed = u32_at(record + 8)? as usize;
        let original = u32_at(record + 12)? as usize;
        let checksum = u32_at(record + 16)?;
        if original == 0
            || compressed > original
            || original > size
            || next.checked_add(original).is_none_or(|end| end > size)
        {
            return Err("WOFF table bound");
        }
        let data = bytes
            .get(offset..offset.checked_add(compressed).ok_or("WOFF offset")?)
            .ok_or("WOFF table offset")?;
        if compressed == original {
            output[next..next + original].copy_from_slice(data);
        } else {
            let mut decoded = Vec::with_capacity(original);
            flate2::read::ZlibDecoder::new(data)
                .take(original as u64 + 1)
                .read_to_end(&mut decoded)
                .map_err(|_| "WOFF deflate")?;
            if decoded.len() != original {
                return Err("WOFF inflated length");
            }
            output[next..next + original].copy_from_slice(&decoded);
        }
        let target = 12 + table * 16;
        output[target..target + 4].copy_from_slice(tag);
        output[target + 4..target + 8].copy_from_slice(&checksum.to_be_bytes());
        output[target + 8..target + 12].copy_from_slice(&(next as u32).to_be_bytes());
        output[target + 12..target + 16].copy_from_slice(&(original as u32).to_be_bytes());
        next = (next + original + 3) & !3;
    }
    Ok(output)
}
