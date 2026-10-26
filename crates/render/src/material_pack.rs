//! Local material preparation. Call during explicit loading, never from a frame.
//! Inputs are borrowed PNG bytes; this module has no network/file/export API.
use crate::terrain_gpu::{AtlasImage, AtlasMip, TerrainGeometry};
use std::io::Cursor;
pub const WGSL: &str = include_str!("material_pack.wgsl");
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub resolution: u32,
    pub gpu_bytes: u64,
    pub encoded_bytes: usize,
    pub decoded_bytes: usize,
    pub layers: u32,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            resolution: 128,
            gpu_bytes: 64 * 1024 * 1024,
            encoded_bytes: 64 * 1024 * 1024,
            decoded_bytes: 64 * 1024 * 1024,
            layers: 256,
        }
    }
}
#[derive(Clone, Copy)]
pub struct LayerSource<'a> {
    pub layer: u32,
    pub albedo_png: Option<&'a [u8]>,
    pub normal_png: Option<&'a [u8]>,
    pub specular_png: Option<&'a [u8]>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterialError {
    Limit,
    Dimensions,
    Atlas,
    Layer,
    Duplicate,
    Png,
    Target,
    UnsupportedMetal,
}
pub struct PreparedMaterials {
    atlas: AtlasImage,
    normal: Vec<Vec<u8>>,
    specular: Vec<Vec<u8>>,
    material_metadata: Vec<[u32; 4]>,
    pub revision: u64,
    pub imported_layers: usize,
    pub decoded_pngs: usize,
    pub animation_strips: usize,
    pub gpu_bytes: u64,
}
/// The shared 16-byte table also serves far proxies, without another binding.
/// Bit 0 enables albedo-scaled emission; bit 1 selects the builtin fallback in
/// the near PBR shader. An explicitly supplied `_s` always replaces that fallback.
pub(crate) fn builtin_material(layer: u32) -> [u32; 4] {
    let lava = layer == 185;
    [
        layer,
        u32::from(layer == 40 || layer == 41),
        if lava { 3 } else { 0 },
        if lava { 1.0f32.to_bits() } else { 0 },
    ]
}
struct Image {
    size: u32,
    rgba: Vec<u8>,
}
fn storage(length: usize, value: u8) -> Result<Vec<u8>, MaterialError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| MaterialError::Limit)?;
    bytes.resize(length, value);
    Ok(bytes)
}
fn dimensions(bytes: &[u8], limits: Limits) -> Result<(u32, u32), MaterialError> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(MaterialError::Limit);
    }
    if bytes.len() < 33 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return Err(MaterialError::Png);
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    if width == 0
        || !width.is_power_of_two()
        || width > 4096
        || height < width
        || height % width != 0
        || height / width > 256
    {
        return Err(MaterialError::Dimensions);
    }
    let decoded = u64::from(width) * u64::from(height) * 4;
    if decoded > limits.decoded_bytes as u64 {
        return Err(MaterialError::Limit);
    }
    Ok((width, height))
}
fn decode(bytes: &[u8], limits: Limits) -> Result<(Image, bool), MaterialError> {
    let (width, height) = dimensions(bytes, limits)?;
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_limits(png::Limits {
        bytes: limits.decoded_bytes,
    });
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|_| MaterialError::Png)?;
    if reader.info().width != width
        || reader.info().height != height
        || reader.info().animation_control.is_some()
    {
        return Err(MaterialError::Dimensions);
    }
    let (color, depth) = reader.output_color_type();
    if depth != png::BitDepth::Eight {
        return Err(MaterialError::Png);
    }
    let channels = match color {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        _ => return Err(MaterialError::Png),
    };
    let length = width as usize * height as usize * channels;
    if reader.output_buffer_size() != length || length > limits.decoded_bytes {
        return Err(MaterialError::Limit);
    }
    let mut raw = storage(length, 0)?;
    let frame = reader
        .next_frame(&mut raw)
        .map_err(|_| MaterialError::Png)?;
    if frame.buffer_size() != length || frame.width != width || frame.height != height {
        return Err(MaterialError::Png);
    }
    reader.finish().map_err(|_| MaterialError::Png)?;
    // Minecraft's ordinary animation strips are static PNG rows; use their first
    // complete frame. APNG is deliberately not accepted as a static material.
    let mut rgba = storage(width as usize * width as usize * 4, 0)?;
    for (source, destination) in raw[..width as usize * width as usize * channels]
        .chunks_exact(channels)
        .zip(rgba.chunks_exact_mut(4))
    {
        destination.copy_from_slice(&match color {
            png::ColorType::Grayscale => [source[0], source[0], source[0], 255],
            png::ColorType::GrayscaleAlpha => [source[0], source[0], source[0], source[1]],
            png::ColorType::Rgb => [source[0], source[1], source[2], 255],
            png::ColorType::Rgba => [source[0], source[1], source[2], source[3]],
            _ => unreachable!(),
        });
    }
    Ok((Image { size: width, rgba }, height != width))
}
fn linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}
fn srgb(value: f32) -> f32 {
    if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}
fn quantize(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}
#[derive(Clone, Copy)]
enum Filter {
    Color,
    Normal,
    Specular,
}
fn downsample(
    source: &[u8],
    size: u32,
    layers: u32,
    filter: Filter,
) -> Result<Vec<u8>, MaterialError> {
    let next = (size / 2).max(1);
    let mut out = storage(next as usize * next as usize * layers as usize * 4, 0)?;
    for layer in 0..layers as usize {
        for y in 0..next as usize {
            for x in 0..next as usize {
                let pixels = std::array::from_fn::<_, 4, _>(|index| {
                    let sx = x * 2 + index % 2;
                    let sy = y * 2 + index / 2;
                    let at = ((layer * size as usize + sy) * size as usize + sx) * 4;
                    [source[at], source[at + 1], source[at + 2], source[at + 3]]
                });
                let at = ((layer * next as usize + y) * next as usize + x) * 4;
                let average = |channel: usize| {
                    pixels
                        .iter()
                        .map(|p| u32::from(p[channel]))
                        .sum::<u32>()
                        .div_ceil(4) as u8
                };
                let mut value: [u8; 4] = std::array::from_fn(average);
                match filter {
                    Filter::Color => {
                        let alpha = pixels.iter().map(|p| f32::from(p[3]) / 255.0).sum::<f32>();
                        for channel in 0..3 {
                            let color = pixels
                                .iter()
                                .map(|p| {
                                    linear(f32::from(p[channel]) / 255.0) * f32::from(p[3]) / 255.0
                                })
                                .sum::<f32>();
                            value[channel] =
                                quantize(srgb(if alpha > 0.0 { color / alpha } else { 0.0 }));
                        }
                    }
                    Filter::Normal => {}
                    Filter::Specular => {
                        // Metal IDs are categorical; averaging 230 with 10 would invent
                        // a dielectric F0. Retain the modal real metal code instead.
                        if let Some(pixel) = pixels
                            .iter()
                            .filter(|p| p[1] >= 230)
                            .max_by_key(|p| pixels.iter().filter(|other| other[1] == p[1]).count())
                        {
                            value[1] = pixel[1];
                        }
                        let emission = pixels
                            .iter()
                            .map(|p| if p[3] == 255 { 0 } else { u32::from(p[3]) })
                            .sum::<u32>();
                        value[3] = (emission / 4).min(254) as u8;
                    }
                }
                out[at..at + 4].copy_from_slice(&value);
            }
        }
    }
    Ok(out)
}
fn resized(mut image: Image, target: u32, filter: Filter) -> Result<Vec<u8>, MaterialError> {
    while image.size > target {
        image.rgba = downsample(&image.rgba, image.size, 1, filter)?;
        image.size /= 2;
    }
    if image.size == target {
        return Ok(image.rgba);
    }
    let mut result = storage(target as usize * target as usize * 4, 0)?;
    for y in 0..target as usize {
        for x in 0..target as usize {
            let source = ((y * image.size as usize / target as usize) * image.size as usize
                + x * image.size as usize / target as usize)
                * 4;
            let destination = (y * target as usize + x) * 4;
            result[destination..destination + 4].copy_from_slice(&image.rgba[source..source + 4]);
        }
    }
    Ok(result)
}
fn fill_layer(destination: &mut [u8], layer: u32, size: u32, pixel: [u8; 4]) {
    let first = layer as usize * size as usize * size as usize * 4;
    for target in destination[first..first + size as usize * size as usize * 4].chunks_exact_mut(4)
    {
        target.copy_from_slice(&pixel);
    }
}
impl PreparedMaterials {
    pub fn prepare(
        base: &AtlasImage,
        sources: &[LayerSource<'_>],
        revision: u64,
        limits: Limits,
    ) -> Result<Self, MaterialError> {
        if limits.resolution == 0
            || !limits.resolution.is_power_of_two()
            || limits.resolution > 4096
            || base.tile_size == 0
            || !base.tile_size.is_power_of_two()
            || base.tile_size > 4096
            || base.layers == 0
            || base.layers > limits.layers
            || base.layers > 256
            || base.mips.is_empty()
        {
            return Err(MaterialError::Atlas);
        }
        let pixels = base.tile_size as usize * base.tile_size as usize * base.layers as usize;
        if base.mips[0].size != base.tile_size
            || base.mips[0].rgba.len() != pixels * 4
            || base.mips[0].tint_mask.len() != pixels
        {
            return Err(MaterialError::Atlas);
        }
        let mut encoded = 0usize;
        let mut decoded = 0u64;
        let mut seen = [false; 256];
        let mut resolution = base.tile_size;
        for source in sources {
            if source.layer >= base.layers {
                return Err(MaterialError::Layer);
            }
            if seen[source.layer as usize] {
                return Err(MaterialError::Duplicate);
            }
            seen[source.layer as usize] = true;
            for bytes in [source.albedo_png, source.normal_png, source.specular_png]
                .into_iter()
                .flatten()
            {
                encoded = encoded
                    .checked_add(bytes.len())
                    .filter(|n| *n <= limits.encoded_bytes)
                    .ok_or(MaterialError::Limit)?;
                let (width, height) = dimensions(bytes, limits)?;
                decoded = decoded
                    .checked_add(u64::from(width) * u64::from(height) * 4)
                    .filter(|n| *n <= limits.decoded_bytes as u64)
                    .ok_or(MaterialError::Limit)?;
                resolution = resolution.max(width.min(limits.resolution));
            }
        }
        let mut all_pixels = 0u64;
        let mut level = resolution;
        loop {
            all_pixels += u64::from(level) * u64::from(level) * u64::from(base.layers);
            if level == 1 {
                break;
            }
            level /= 2;
        }
        let gpu_bytes = all_pixels
            .checked_mul(13)
            .filter(|n| *n <= limits.gpu_bytes)
            .ok_or(MaterialError::Limit)?;
        let length = resolution as usize * resolution as usize * base.layers as usize;
        let mut color = storage(length * 4, 0)?;
        let mut mask = storage(length, 255)?;
        let mut normal = storage(length * 4, 0)?;
        let mut specular = storage(length * 4, 0)?;
        for layer in 0..base.layers {
            let first = layer as usize * base.tile_size as usize * base.tile_size as usize;
            let source = Image {
                size: base.tile_size,
                rgba: base.mips[0].rgba
                    [first * 4..(first + base.tile_size as usize * base.tile_size as usize) * 4]
                    .to_vec(),
            };
            let resized = resized(source, resolution, Filter::Color)?;
            let offset = layer as usize * resolution as usize * resolution as usize * 4;
            color[offset..offset + resized.len()].copy_from_slice(&resized);
            for y in 0..resolution as usize {
                for x in 0..resolution as usize {
                    let sx = x * base.tile_size as usize / resolution as usize;
                    let sy = y * base.tile_size as usize / resolution as usize;
                    mask[(layer as usize * resolution as usize + y) * resolution as usize + x] =
                        base.mips[0].tint_mask[first + sy * base.tile_size as usize + sx];
                }
            }
            fill_layer(&mut normal, layer, resolution, [128, 128, 255, 255]);
            fill_layer(&mut specular, layer, resolution, [0, 10, 0, 255]);
        }
        let mut decoded_pngs = 0;
        let mut animation_strips = 0;
        let mut material_metadata: Vec<_> = (0..base.layers).map(builtin_material).collect();
        for source in sources {
            for (bytes, destination, filter) in [
                (source.albedo_png, &mut color, Filter::Color),
                (source.normal_png, &mut normal, Filter::Normal),
                (source.specular_png, &mut specular, Filter::Specular),
            ] {
                if let Some(bytes) = bytes {
                    let (image, animated) = decode(bytes, limits)?;
                    decoded_pngs += 1;
                    animation_strips += usize::from(animated);
                    let data = resized(image, resolution, filter)?;
                    let offset =
                        source.layer as usize * resolution as usize * resolution as usize * 4;
                    destination[offset..offset + data.len()].copy_from_slice(&data);
                }
            }
            if source.specular_png.is_some() {
                let pixels = resolution as usize * resolution as usize;
                let offset = source.layer as usize * pixels * 4;
                let sum: u64 = specular[offset..offset + pixels * 4]
                    .chunks_exact(4)
                    .map(|pixel| u64::from(if pixel[3] == 255 { 0 } else { pixel[3] }))
                    .sum();
                // Proxies use a layer average. Near PBR retains the original
                // per-texel channel, including both 0/255 non-emission sentinels.
                let emission = (sum as f64 / (pixels as f64 * 254.0)) as f32;
                let entry = &mut material_metadata[source.layer as usize];
                entry[2] = u32::from(emission > 0.0);
                entry[3] = emission.to_bits();
            }
        }
        let mut atlas = AtlasImage {
            tile_size: resolution,
            layers: base.layers,
            mips: vec![AtlasMip {
                size: resolution,
                rgba: color,
                tint_mask: mask,
            }],
        };
        let mut normals = vec![normal];
        let mut speculars = vec![specular];
        while atlas.mips.last().unwrap().size > 1 {
            let old = atlas.mips.last().unwrap();
            let size = old.size / 2;
            let rgba = downsample(&old.rgba, old.size, base.layers, Filter::Color)?;
            let mut tint_mask = storage(size as usize * size as usize * base.layers as usize, 0)?;
            for layer in 0..base.layers as usize {
                for y in 0..size as usize {
                    for x in 0..size as usize {
                        let mut sum = 0u32;
                        for dy in 0..2 {
                            for dx in 0..2 {
                                sum += u32::from(
                                    old.tint_mask[(layer * old.size as usize + y * 2 + dy)
                                        * old.size as usize
                                        + x * 2
                                        + dx],
                                );
                            }
                        }
                        tint_mask[(layer * size as usize + y) * size as usize + x] =
                            (sum / 4) as u8;
                    }
                }
            }
            normals.push(downsample(
                normals.last().unwrap(),
                old.size,
                base.layers,
                Filter::Normal,
            )?);
            speculars.push(downsample(
                speculars.last().unwrap(),
                old.size,
                base.layers,
                Filter::Specular,
            )?);
            atlas.mips.push(AtlasMip {
                size,
                rgba,
                tint_mask,
            });
        }
        Ok(Self {
            atlas,
            normal: normals,
            specular: speculars,
            material_metadata,
            revision,
            imported_layers: sources.len(),
            decoded_pngs,
            animation_strips,
            gpu_bytes,
        })
    }
    pub fn atlas(&self) -> &AtlasImage {
        &self.atlas
    }
    /// Layer-average far emission and near builtin-fallback flags. Import is
    /// local; this table contains no resource bytes or network state.
    pub fn material_metadata(&self) -> &[[u32; 4]] {
        &self.material_metadata
    }
}
pub struct MaterialPackGpu {
    pub(crate) material_metadata: Vec<[u32; 4]>,
    pub albedo: wgpu::TextureView,
    pub normal: wgpu::TextureView,
    pub specular: wgpu::TextureView,
    pub tint: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
    pub revision: u64,
    pub layers: u32,
    pub resolution: u32,
    pub upload_bytes: u64,
    /// Undefined standard IDs are retained for inspection, but not activated.
    pub reserved_metals: bool,
}
fn texture(
    device: &wgpu::Device,
    label: &str,
    format: wgpu::TextureFormat,
    prepared: &PreparedMaterials,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: prepared.atlas.tile_size,
            height: prepared.atlas.tile_size,
            depth_or_array_layers: prepared.atlas.layers,
        },
        mip_level_count: prepared.atlas.mips.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}
fn write(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    mip: u32,
    size: u32,
    layers: u32,
    channels: u32,
    bytes: &[u8],
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: mip,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size * channels),
            rows_per_image: Some(size),
        },
        wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: layers,
        },
    );
}
impl MaterialPackGpu {
    pub fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        prepared: &PreparedMaterials,
    ) -> Result<Self, MaterialError> {
        if prepared.atlas.layers > device.limits().max_texture_array_layers
            || prepared.atlas.tile_size > device.limits().max_texture_dimension_2d
        {
            return Err(MaterialError::Limit);
        }
        let albedo = texture(
            device,
            "local sRGB albedo",
            wgpu::TextureFormat::Rgba8UnormSrgb,
            prepared,
        );
        let normal = texture(
            device,
            "local linear normal AO height",
            wgpu::TextureFormat::Rgba8Unorm,
            prepared,
        );
        let specular = texture(
            device,
            "local linear smoothness F0 metal emission",
            wgpu::TextureFormat::Rgba8Unorm,
            prepared,
        );
        let tint = texture(
            device,
            "local tint coverage",
            wgpu::TextureFormat::R8Unorm,
            prepared,
        );
        for (mip, image) in prepared.atlas.mips.iter().enumerate() {
            write(
                queue,
                &albedo,
                mip as u32,
                image.size,
                prepared.atlas.layers,
                4,
                &image.rgba,
            );
            write(
                queue,
                &normal,
                mip as u32,
                image.size,
                prepared.atlas.layers,
                4,
                &prepared.normal[mip],
            );
            write(
                queue,
                &specular,
                mip as u32,
                image.size,
                prepared.atlas.layers,
                4,
                &prepared.specular[mip],
            );
            write(
                queue,
                &tint,
                mip as u32,
                image.size,
                prepared.atlas.layers,
                1,
                &image.tint_mask,
            );
        }
        let view = |texture: &wgpu::Texture| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            })
        };
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("sharp local material sampling"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        Ok(Self {
            material_metadata: prepared.material_metadata.clone(),
            albedo: view(&albedo),
            normal: view(&normal),
            specular: view(&specular),
            tint: view(&tint),
            sampler,
            revision: prepared.revision,
            layers: prepared.atlas.layers,
            resolution: prepared.atlas.tile_size,
            upload_bytes: prepared.gpu_bytes,
            reserved_metals: prepared.specular[0]
                .chunks_exact(4)
                .any(|pixel| (238..=254).contains(&pixel[1])),
        })
    }
}
/// Existing 16px arrays can receive the matching prepared mip without changing
/// any geometry, LOD, model or shadow binding. Higher-resolution views stay ready
/// for a loading-time renderer replacement; this path reports its actual size.
pub fn apply_albedo_in_place(
    queue: &wgpu::Queue,
    geometry: TerrainGeometry<'_>,
    prepared: &PreparedMaterials,
) -> Result<(u32, u64), MaterialError> {
    let color = geometry.colors.texture();
    let tint = geometry.tint_masks.texture();
    let size = color.width();
    let layers = color.depth_or_array_layers();
    if color.width() != color.height()
        || tint.width() != size
        || tint.height() != size
        || tint.depth_or_array_layers() != layers
        || layers != prepared.atlas.layers
        || color.mip_level_count() != tint.mip_level_count()
        || color.format() != wgpu::TextureFormat::Rgba8UnormSrgb
        || tint.format() != wgpu::TextureFormat::R8Unorm
        || !color.usage().contains(wgpu::TextureUsages::COPY_DST)
        || !tint.usage().contains(wgpu::TextureUsages::COPY_DST)
    {
        return Err(MaterialError::Target);
    }
    let first = prepared
        .atlas
        .mips
        .iter()
        .position(|mip| mip.size == size)
        .ok_or(MaterialError::Target)?;
    if prepared.atlas.mips.len() - first != color.mip_level_count() as usize {
        return Err(MaterialError::Target);
    }
    let mut bytes = 0;
    for (mip, image) in prepared.atlas.mips[first..].iter().enumerate() {
        write(queue, color, mip as u32, image.size, layers, 4, &image.rgba);
        write(
            queue,
            tint,
            mip as u32,
            image.size,
            layers,
            1,
            &image.tint_mask,
        );
        bytes += image.rgba.len() as u64 + image.tint_mask.len() as u64;
    }
    Ok((size, bytes))
}
