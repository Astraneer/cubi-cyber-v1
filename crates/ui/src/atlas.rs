use crate::paint::Rect;
use std::{collections::BTreeMap, io::Cursor};
pub const ATLAS_SIZE: u32 = 1024;
#[derive(Clone, Copy)]
pub struct Glyph {
    pub uv: Rect,
    pub size: [f32; 2],
    pub offset: [f32; 2],
    pub advance: f32,
}
pub struct UiAtlas {
    pub pixels: Vec<u8>,
    glyphs: BTreeMap<char, Glyph>,
    items: BTreeMap<rules::ItemId, Rect>,
    pub white: Rect,
}
struct Packer {
    pixels: Vec<u8>,
    x: usize,
    y: usize,
    row: usize,
}
impl Packer {
    fn new() -> Self {
        Self {
            pixels: vec![0; ATLAS_SIZE as usize * ATLAS_SIZE as usize * 4],
            x: 2,
            y: 2,
            row: 0,
        }
    }
    fn image(&mut self, width: usize, height: usize, rgba: &[u8]) -> Result<Rect, &'static str> {
        if width == 0 || height == 0 {
            return Ok(Rect::default());
        }
        if rgba.len() != width * height * 4 {
            return Err("UI atlas image shape");
        }
        if self.x + width + 1 > ATLAS_SIZE as usize {
            self.x = 2;
            self.y += self.row + 2;
            self.row = 0;
        }
        if self.y + height + 1 > ATLAS_SIZE as usize {
            return Err("UI atlas capacity");
        }
        for row in 0..height {
            let target = ((self.y + row) * ATLAS_SIZE as usize + self.x) * 4;
            self.pixels[target..target + width * 4]
                .copy_from_slice(&rgba[row * width * 4..(row + 1) * width * 4]);
        }
        let uv = Rect::new(
            self.x as f32 / ATLAS_SIZE as f32,
            self.y as f32 / ATLAS_SIZE as f32,
            width as f32 / ATLAS_SIZE as f32,
            height as f32 / ATLAS_SIZE as f32,
        );
        self.x += width + 2;
        self.row = self.row.max(height);
        Ok(uv)
    }
}
impl UiAtlas {
    pub fn load() -> Result<Self, &'static str> {
        let ttf = crate::font::unpack_woff(crate::font::FONT_SOURCE)?;
        let font = fontdue::Font::from_bytes(ttf, fontdue::FontSettings::default())
            .map_err(|_| "UI font parse")?;
        let ascent = font
            .horizontal_line_metrics(16.0)
            .ok_or("UI font metrics")?
            .ascent
            .ceil();
        let mut pack = Packer::new();
        pack.pixels[..4].fill(255);
        let white = Rect::new(0.5 / ATLAS_SIZE as f32, 0.5 / ATLAS_SIZE as f32, 0.0, 0.0);
        let mut glyphs = BTreeMap::new();
        for ch in (32..=126)
            .chain(0xa0..=0xff)
            .chain(0x400..=0x45f)
            .filter_map(char::from_u32)
            .chain("♥★●↔↑↓←→×…•—–−±".chars())
        {
            let (metrics, alpha) = font.rasterize(ch, 16.0);
            let rgba: Vec<_> = alpha
                .into_iter()
                .flat_map(|a| [255, 255, 255, if a >= 128 { 255 } else { 0 }])
                .collect();
            let uv = pack.image(metrics.width, metrics.height, &rgba)?;
            glyphs.insert(
                ch,
                Glyph {
                    uv,
                    size: [metrics.width as f32, metrics.height as f32],
                    offset: [
                        metrics.xmin as f32,
                        ascent - (metrics.ymin + metrics.height as i32) as f32,
                    ],
                    advance: metrics.advance_width.round(),
                },
            );
        }
        let (_, height, terrain) = decode_png(include_bytes!("../../../donor/web/atlas.png"))?;
        if height % 16 != 0 {
            return Err("terrain atlas height");
        }
        let mut items = BTreeMap::new();
        for (id, definition) in rules::all_items() {
            let image = match definition.kind {
                rules::items::ItemKind::Block(block) => {
                    let layer = rules::blocks::layer(block, 2) as usize;
                    let start = layer * 16 * 16 * 4;
                    if start + 1024 > terrain.len() {
                        return Err("item block texture layer");
                    }
                    let mut image = terrain[start..start + 1024].to_vec();
                    if layer == 40 || layer == 41 {
                        for pixel in image.chunks_exact_mut(4) {
                            pixel[3] = 255;
                        }
                    }
                    if rules::blocks::tinted(block, 2) {
                        for pixel in image.chunks_exact_mut(4) {
                            pixel[0] = (u16::from(pixel[0]) * 115 / 255) as u8;
                            pixel[1] = (u16::from(pixel[1]) * 190 / 255) as u8;
                            pixel[2] = (u16::from(pixel[2]) * 80 / 255) as u8;
                        }
                    }
                    image
                }
                _ => item_symbol(id, definition),
            };
            items.insert(id, pack.image(16, 16, &image)?);
        }
        Ok(Self {
            pixels: pack.pixels,
            glyphs,
            items,
            white,
        })
    }
    pub fn glyph(&self, ch: char) -> Glyph {
        *self.glyphs.get(&ch).unwrap_or(&self.glyphs[&'?'])
    }
    pub fn item(&self, item: rules::ItemId) -> Option<Rect> {
        self.items.get(&item).copied()
    }
    pub fn take_pixels(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pixels)
    }
}
fn decode_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), &'static str> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|_| "UI PNG header")?;
    if reader.info().width > 4096 || reader.info().height > 4096 {
        return Err("UI PNG budget");
    }
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels).map_err(|_| "UI PNG data")?;
    if info.color_type != png::ColorType::Rgba {
        return Err("UI RGBA texture expected");
    }
    pixels.truncate(info.buffer_size());
    Ok((info.width, info.height, pixels))
}
fn item_symbol(id: rules::ItemId, definition: &rules::ItemDef) -> Vec<u8> {
    use rules::items::ItemKind;
    let mut pixels = vec![0; 16 * 16 * 4];
    let mut dot = |x: usize, y: usize, color: [u8; 4]| {
        if x < 16 && y < 16 {
            pixels[(x + y * 16) * 4..(x + y * 16) * 4 + 4].copy_from_slice(&color);
        }
    };
    let wood = [134, 91, 52, 255];
    let metal = if definition.mc.contains("wooden") {
        [165, 119, 61, 255]
    } else if definition.mc.contains("stone") {
        [147, 151, 156, 255]
    } else if definition.mc.contains("steel") {
        [113, 138, 152, 255]
    } else {
        [220, 231, 233, 255]
    };
    match definition.kind {
        ItemKind::Tool { .. } | ItemKind::Sword { .. } => {
            let mask = match definition.kind {
                ItemKind::Tool { mask, .. } => mask,
                _ => 0,
            };
            for n in 2..13 {
                dot(n, 15 - n, wood);
                dot(n + 1, 15 - n, wood);
            }
            if matches!(definition.kind, ItemKind::Sword { .. }) {
                for n in 2..10 {
                    dot(15 - n, n, metal);
                    dot(14 - n, n, metal);
                }
            } else {
                let width = if mask & rules::blocks::PICKAXE != 0 {
                    9
                } else {
                    5
                };
                for x in 4..4 + width {
                    dot(x, 3, metal);
                    dot(x, 4, metal);
                }
                for y in 5..8 {
                    dot(12, y, metal);
                }
            }
        }
        ItemKind::Armor { slot, .. } => {
            for y in 3..13 {
                for x in 3..13 {
                    let visible = match slot {
                        0 => y < 8 && (y < 5 || !(5..=10).contains(&x)),
                        1 => y < 12 && (!(5..=10).contains(&x) || y > 4),
                        2 => y < 7 || !(6..=9).contains(&x),
                        _ => y > 8 && !(7..=8).contains(&x),
                    };
                    if visible {
                        dot(x, y, metal);
                    }
                }
            }
        }
        ItemKind::Bucket { fluid } => {
            for y in 4..13 {
                for x in 4..12 {
                    let edge = x == 4 || x == 11 || y == 12;
                    if edge {
                        dot(x, y, metal);
                    } else if fluid != 0 && y > 7 {
                        dot(
                            x,
                            y,
                            if fluid == 1 {
                                [59, 126, 225, 255]
                            } else {
                                [247, 101, 22, 255]
                            },
                        );
                    }
                }
            }
        }
        ItemKind::Food { .. } | ItemKind::Seed { .. } => {
            for y in 5..13 {
                for x in 4..12 {
                    if (x as i32 - 8).pow(2) + (y as i32 - 9).pow(2) < 20 {
                        dot(
                            x,
                            y,
                            if definition.mc.contains("carrot") {
                                [240, 134, 36, 255]
                            } else {
                                [202, 52, 45, 255]
                            },
                        );
                    }
                }
            }
            dot(8, 3, wood);
            dot(9, 3, [82, 150, 58, 255]);
            dot(10, 2, [82, 150, 58, 255]);
        }
        _ => {
            let color = if definition.mc.contains("gold") {
                [238, 192, 59, 255]
            } else if definition.mc.contains("copper") {
                [194, 110, 62, 255]
            } else if definition.mc.contains("coal") {
                [51, 56, 63, 255]
            } else if definition.mc.contains("redstone") {
                [206, 43, 40, 255]
            } else {
                [110 + (id % 80) as u8, 138, 156, 255]
            };
            for y in 5..12 {
                for x in 3..13 {
                    if x + y > 8 && x + y < 23 {
                        dot(x, y, color);
                    }
                }
            }
        }
    }
    pixels
}
