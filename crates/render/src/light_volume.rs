//! Page-local RGB/skylight storage independent of terrain geometry.
//!
//! A one-cell halo is supplied by the owner from *known* world samples. Unknown
//! is dark, never an implicit sky boundary. Slots and generations match terrain;
//! an emission change uploads light data without changing a quad descriptor.

use crate::terrain_gpu::{PageHandle, UploadBudget};

pub const LIGHT_EDGE: usize = 18;
pub const LIGHT_SAMPLES: usize = LIGHT_EDGE * LIGHT_EDGE * LIGHT_EDGE;
pub const LIGHT_PAGE_BYTES: u64 = LIGHT_SAMPLES as u64 * 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct PackedLight(u32);
impl PackedLight {
    pub fn new(sky: u8, block: u8, rgb: [u8; 3]) -> Option<Self> {
        if sky > 15 || block > 15 || rgb.iter().any(|v| *v > 15) {
            return None;
        }
        Some(Self(
            u32::from(rgb[0])
                | (u32::from(rgb[1]) << 4)
                | (u32::from(rgb[2]) << 8)
                | (u32::from(sky) << 12)
                | (u32::from(block) << 16),
        ))
    }
    pub const fn raw(self) -> u32 {
        self.0
    }
    pub const fn sky(self) -> u8 {
        ((self.0 >> 12) & 15) as u8
    }
    pub const fn block(self) -> u8 {
        ((self.0 >> 16) & 15) as u8
    }
    pub const fn rgb(self) -> [u8; 3] {
        [
            (self.0 & 15) as u8,
            ((self.0 >> 4) & 15) as u8,
            ((self.0 >> 8) & 15) as u8,
        ]
    }
}

/// Coordinates include the neighbor halo: -1..=16 on each axis.
pub fn light_index(position: [i32; 3]) -> Option<usize> {
    if position.iter().any(|p| !(-1..=16).contains(p)) {
        return None;
    }
    Some(
        (position[0] + 1) as usize
            + LIGHT_EDGE * ((position[2] + 1) as usize + LIGHT_EDGE * (position[1] + 1) as usize),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LightUpdate {
    pub index: u16,
    pub value: PackedLight,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightUploadError {
    InvalidSlot,
    StaleHandle,
    StaleVersion,
    InvalidIndex,
    UploadBudget,
    Capacity,
}
#[derive(Clone, Copy, Debug, Default)]
struct Slot {
    generation: u32,
    version: u64,
    initialized: bool,
    alias: Option<PageHandle>,
    uniform: bool,
}

pub struct LightVolumeGpu {
    buffer: wgpu::Buffer,
    slots: Vec<Slot>,
    staging: Box<[u32; LIGHT_SAMPLES]>,
    upload_bytes: u64,
}
impl LightVolumeGpu {
    pub fn new(device: &wgpu::Device, max_pages: u32) -> Result<Self, LightUploadError> {
        let size = u64::from(max_pages) * LIGHT_PAGE_BYTES;
        if max_pages == 0
            || size > u64::from(device.limits().max_storage_buffer_binding_size)
            || size > device.limits().max_buffer_size
        {
            return Err(LightUploadError::Capacity);
        }
        Ok(Self {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("RGB + sky volumes, no geometry dependency"),
                size,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            slots: vec![Slot::default(); max_pages as usize],
            staging: Box::new([0; LIGHT_SAMPLES]),
            upload_bytes: 0,
        })
    }
    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }
    pub fn take_upload_bytes(&mut self) -> u64 {
        std::mem::take(&mut self.upload_bytes)
    }
    /// An overlay shares its base's *current* volume with one 4-byte forwarding
    /// header. No duplicate 23KB upload is needed after each light change. Alias
    /// chains are forbidden; the source must be an initialized live base slot.
    pub fn alias_page(
        &mut self,
        queue: &wgpu::Queue,
        handle: PageHandle,
        source: PageHandle,
        budget: &mut UploadBudget,
    ) -> Result<u64, LightUploadError> {
        if handle.slot == source.slot {
            return Err(LightUploadError::InvalidSlot);
        }
        let source_state = *self
            .slots
            .get(source.slot as usize)
            .ok_or(LightUploadError::InvalidSlot)?;
        if !source_state.initialized
            || source_state.generation != source.generation
            || source_state.alias.is_some()
        {
            return Err(LightUploadError::StaleHandle);
        }
        let slot = self
            .slots
            .get_mut(handle.slot as usize)
            .ok_or(LightUploadError::InvalidSlot)?;
        if slot.initialized && handle.generation < slot.generation {
            return Err(LightUploadError::StaleHandle);
        }
        if slot.initialized && slot.generation == handle.generation && slot.alias == Some(source) {
            return Ok(0);
        }
        if budget.remaining_bytes < 4 {
            return Err(LightUploadError::UploadBudget);
        }
        let header = 0x8000_0000 | source.slot;
        queue.write_buffer(
            &self.buffer,
            u64::from(handle.slot) * LIGHT_PAGE_BYTES,
            bytemuck::bytes_of(&header),
        );
        *slot = Slot {
            generation: handle.generation,
            version: source_state.version,
            initialized: true,
            alias: Some(source),
            uniform: false,
        };
        budget.remaining_bytes -= 4;
        self.upload_bytes += 4;
        Ok(4)
    }
    /// Small rigid model parts can use one known light sample with a 4-byte
    /// uniform header. Their vertices/meshes and the terrain volumes stay intact.
    pub fn upload_uniform(
        &mut self,
        queue: &wgpu::Queue,
        handle: PageHandle,
        version: u64,
        value: PackedLight,
        budget: &mut UploadBudget,
    ) -> Result<u64, LightUploadError> {
        let slot = self
            .slots
            .get_mut(handle.slot as usize)
            .ok_or(LightUploadError::InvalidSlot)?;
        if slot.initialized && handle.generation < slot.generation {
            return Err(LightUploadError::StaleHandle);
        }
        if slot.initialized && slot.generation == handle.generation && version < slot.version {
            return Err(LightUploadError::StaleVersion);
        }
        if slot.initialized
            && slot.generation == handle.generation
            && slot.uniform
            && version == slot.version
        {
            return Ok(0);
        }
        if budget.remaining_bytes < 4 {
            return Err(LightUploadError::UploadBudget);
        }
        let header = 0x4000_0000 | value.raw();
        queue.write_buffer(
            &self.buffer,
            u64::from(handle.slot) * LIGHT_PAGE_BYTES,
            bytemuck::bytes_of(&header),
        );
        *slot = Slot {
            generation: handle.generation,
            version,
            initialized: true,
            alias: None,
            uniform: true,
        };
        budget.remaining_bytes -= 4;
        self.upload_bytes += 4;
        Ok(4)
    }
    /// Full snapshots are accepted only for a new slot generation or a newer
    /// version. A reused terrain slot cannot inherit a previous page's light.
    pub fn upload_page(
        &mut self,
        queue: &wgpu::Queue,
        handle: PageHandle,
        version: u64,
        samples: &[PackedLight; LIGHT_SAMPLES],
        budget: &mut UploadBudget,
    ) -> Result<u64, LightUploadError> {
        let slot = self
            .slots
            .get_mut(handle.slot as usize)
            .ok_or(LightUploadError::InvalidSlot)?;
        if slot.initialized && handle.generation < slot.generation {
            return Err(LightUploadError::StaleHandle);
        }
        if slot.initialized && handle.generation == slot.generation && version < slot.version {
            return Err(LightUploadError::StaleVersion);
        }
        if slot.initialized && handle.generation == slot.generation && version == slot.version {
            return Ok(0);
        }
        if budget.remaining_bytes < LIGHT_PAGE_BYTES {
            return Err(LightUploadError::UploadBudget);
        }
        for (output, sample) in self.staging.iter_mut().zip(samples) {
            *output = sample.raw();
        }
        queue.write_buffer(
            &self.buffer,
            u64::from(handle.slot) * LIGHT_PAGE_BYTES,
            bytemuck::cast_slice(&*self.staging),
        );
        *slot = Slot {
            generation: handle.generation,
            version,
            initialized: true,
            alias: None,
            uniform: false,
        };
        budget.remaining_bytes -= LIGHT_PAGE_BYTES;
        self.upload_bytes += LIGHT_PAGE_BYTES;
        Ok(LIGHT_PAGE_BYTES)
    }
    /// Strictly increasing indices permit atomic validation and bounded writes
    /// with no temporary allocation. Adjacent updates share one queue upload.
    pub fn upload_patch(
        &mut self,
        queue: &wgpu::Queue,
        handle: PageHandle,
        version: u64,
        updates: &[LightUpdate],
        budget: &mut UploadBudget,
    ) -> Result<u64, LightUploadError> {
        let slot = self
            .slots
            .get_mut(handle.slot as usize)
            .ok_or(LightUploadError::InvalidSlot)?;
        if !slot.initialized
            || slot.generation != handle.generation
            || slot.alias.is_some()
            || slot.uniform
        {
            return Err(LightUploadError::StaleHandle);
        }
        if version <= slot.version {
            return Err(LightUploadError::StaleVersion);
        }
        if updates
            .iter()
            .any(|u| usize::from(u.index) >= LIGHT_SAMPLES)
            || updates
                .windows(2)
                .any(|pair| pair[0].index >= pair[1].index)
        {
            return Err(LightUploadError::InvalidIndex);
        }
        let bytes = updates.len() as u64 * 4;
        if bytes > budget.remaining_bytes {
            return Err(LightUploadError::UploadBudget);
        }
        for update in updates {
            self.staging[usize::from(update.index)] = update.value.raw();
        }
        let mut start = 0;
        while start < updates.len() {
            let mut end = start + 1;
            while end < updates.len() && updates[end].index == updates[end - 1].index + 1 {
                end += 1;
            }
            let first = usize::from(updates[start].index);
            let last = usize::from(updates[end - 1].index) + 1;
            queue.write_buffer(
                &self.buffer,
                u64::from(handle.slot) * LIGHT_PAGE_BYTES + first as u64 * 4,
                bytemuck::cast_slice(&self.staging[first..last]),
            );
            start = end;
        }
        slot.version = version;
        budget.remaining_bytes -= bytes;
        self.upload_bytes += bytes;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packed_channels_are_exact_and_bounded() {
        for level in 0..16 {
            let value = PackedLight::new(level, 15 - level, [level, 7, 15]).unwrap();
            assert_eq!(value.sky(), level);
            assert_eq!(value.block(), 15 - level);
            assert_eq!(value.rgb(), [level, 7, 15]);
        }
        assert!(PackedLight::new(16, 0, [0; 3]).is_none());
        assert!(PackedLight::new(0, 16, [0; 3]).is_none());
        assert!(PackedLight::new(0, 0, [0, 16, 0]).is_none());
    }
    #[test]
    fn halo_indices_match_cell_yzx_layout() {
        assert_eq!(light_index([-1; 3]), Some(0));
        assert_eq!(light_index([16; 3]), Some(LIGHT_SAMPLES - 1));
        assert_eq!(light_index([0, 0, 0]), Some(1 + 18 + 18 * 18));
        assert!(light_index([17, 0, 0]).is_none());
    }
}
