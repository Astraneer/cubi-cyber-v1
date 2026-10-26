//! Opaque recipe cards prepared once during loading from the same licensed art
//! and compiled recipes as the shared UI. This module performs no GPU work.
use rules::{
    ItemId,
    crafting::RecipeBook,
    crossroads::{self, BOARD_COUNT, SHEETS, Sheet},
};
use ui::{
    atlas::{ATLAS_SIZE, UiAtlas},
    paint::Rect,
};

pub const WIDTH: u32 = 256;
pub const HEIGHT: u32 = 192;
pub const LAYERS: u32 = BOARD_COUNT as u32;
pub const BYTES: usize = WIDTH as usize * HEIGHT as usize * 4 * BOARD_COUNT;
const PAPER: [u8; 3] = [242, 231, 205];
const INK: [u8; 3] = [53, 40, 30];
const FRAME: [u8; 3] = [99, 72, 46];
const ACCENT: [u8; 3] = [44, 104, 76];

pub struct BoardImages {
    pub rgba: Vec<u8>,
    pub recipes: u32,
    pub icons: u32,
    pub glyphs: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    AtlasPixels,
    AtlasBounds,
    MissingItem(ItemId),
    Recipe(crossroads::Error),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "recipe board image: {self:?}")
    }
}
impl std::error::Error for Error {}

struct Canvas<'a> {
    atlas: &'a UiAtlas,
    pixels: &'a mut [u8],
    width: u32,
    clip: [i32; 4],
    icons: u32,
    glyphs: u32,
}
impl Canvas<'_> {
    fn pixel(&mut self, x: i32, y: i32, color: [u8; 3], alpha: u8) {
        let [cx, cy, cw, ch] = self.clip;
        if x < cx || y < cy || x >= cx + cw || y >= cy + ch || alpha == 0 {
            return;
        }
        let offset = (y as usize * self.width as usize + x as usize) * 4;
        let a = u32::from(alpha);
        for (channel, value) in color.into_iter().enumerate() {
            let old = u32::from(self.pixels[offset + channel]);
            self.pixels[offset + channel] =
                ((u32::from(value) * a + old * (255 - a) + 127) / 255) as u8;
        }
        self.pixels[offset + 3] = 255;
    }
    fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: [u8; 3]) {
        for py in y.max(self.clip[1])..(y + h).min(self.clip[1] + self.clip[3]) {
            for px in x.max(self.clip[0])..(x + w).min(self.clip[0] + self.clip[2]) {
                self.pixel(px, py, color, 255);
            }
        }
    }
    fn blit(
        &mut self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        uv: Rect,
        tint: [u8; 3],
    ) -> Result<(), Error> {
        let values = [uv.x, uv.y, uv.w, uv.h];
        if values.iter().any(|v| !v.is_finite() || *v < 0.) {
            return Err(Error::AtlasBounds);
        }
        let [sx, sy, sw, sh] = values.map(|v| (v * ATLAS_SIZE as f32).round() as u32);
        if w == 0 || h == 0 || sw == 0 || sh == 0 || sx + sw > ATLAS_SIZE || sy + sh > ATLAS_SIZE {
            return Err(Error::AtlasBounds);
        }
        for dy in 0..h {
            let py = sy + ((2 * dy + 1) * sh / (2 * h)).min(sh - 1);
            for dx in 0..w {
                let px = sx + ((2 * dx + 1) * sw / (2 * w)).min(sw - 1);
                let offset = (py as usize * ATLAS_SIZE as usize + px as usize) * 4;
                let source = &self.atlas.pixels[offset..offset + 4];
                let color = std::array::from_fn(|channel| {
                    ((u16::from(source[channel]) * u16::from(tint[channel]) + 127) / 255) as u8
                });
                self.pixel(x + dx as i32, y + dy as i32, color, source[3]);
            }
        }
        Ok(())
    }
    fn icon(&mut self, x: i32, y: i32, size: u32, item: ItemId) -> Result<(), Error> {
        let uv = self.atlas.item(item).ok_or(Error::MissingItem(item))?;
        self.blit(x, y, size, size, uv, [255; 3])?;
        self.icons += 1;
        Ok(())
    }
    fn width(&self, text: &str) -> i32 {
        text.chars()
            .map(|ch| (self.atlas.glyph(ch).advance).round() as i32)
            .sum()
    }
    fn text(&mut self, x: i32, y: i32, text: &str, color: [u8; 3]) -> Result<(), Error> {
        let mut cursor = x;
        for ch in text.chars() {
            let glyph = self.atlas.glyph(ch);
            if glyph.size[0] > 0. && glyph.size[1] > 0. {
                self.blit(
                    cursor + (glyph.offset[0]).round() as i32,
                    y + (glyph.offset[1]).round() as i32,
                    (glyph.size[0]).round().max(1.) as u32,
                    (glyph.size[1]).round().max(1.) as u32,
                    glyph.uv,
                    color,
                )?;
                self.glyphs += 1;
            }
            cursor += (glyph.advance).round() as i32;
        }
        Ok(())
    }
    fn centered(&mut self, y: i32, text: &str, color: [u8; 3]) -> Result<(), Error> {
        let x = (self.width as i32 - self.width(text)) / 2;
        self.text(x, y, text, color)
    }
    fn fit(&self, text: &str, width: i32) -> String {
        let mut result = text.to_owned();
        if self.width(text) > width {
            while !result.is_empty() && self.width(&result) + self.width("…") > width {
                result.pop();
            }
            result.push('…');
        }
        result
    }
    fn two_lines(&mut self, x: i32, y: i32, width: i32, text: &str) -> Result<(), Error> {
        let mut line = String::new();
        let mut row = 0;
        for word in text.split_whitespace() {
            let separator = i32::from(!line.is_empty()) * self.width(" ");
            if row == 0
                && !line.is_empty()
                && self.width(&line) + separator + self.width(word) > width
            {
                self.text(x, y, &line, INK)?;
                line.clear();
                row = 1;
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        self.text(x, y + row * 16, &self.fit(&line, width), INK)
    }
}

pub fn prepare(atlas: &UiAtlas, book: &RecipeBook) -> Result<BoardImages, Error> {
    if atlas.pixels.len() != ATLAS_SIZE as usize * ATLAS_SIZE as usize * 4 {
        return Err(Error::AtlasPixels);
    }
    let mut images = BoardImages {
        rgba: vec![255; BYTES],
        recipes: 0,
        icons: 0,
        glyphs: 0,
    };
    let layer_bytes = WIDTH as usize * HEIGHT as usize * 4;
    // Rasterize at square physical texels first; the final nearest resample
    // compensates for each world panel aspect while retaining one array shape.
    let mut scratch = vec![255; 576 * HEIGHT as usize * 4];
    for (index, sheet) in SHEETS.into_iter().enumerate() {
        let logical_width = match sheet {
            Sheet::Show(_) | Sheet::Craft(_) => 320,
            Sheet::Sky(_) => 192,
            Sheet::SkySign => 576,
        };
        let mut card = Canvas {
            atlas,
            pixels: &mut scratch[..logical_width as usize * HEIGHT as usize * 4],
            width: logical_width,
            clip: [0, 0, logical_width as i32, HEIGHT as i32],
            icons: 0,
            glyphs: 0,
        };
        card.rect(0, 0, logical_width as i32, HEIGHT as i32, FRAME);
        card.rect(3, 3, logical_width as i32 - 6, HEIGHT as i32 - 6, PAPER);
        match sheet {
            Sheet::Show(hall) => {
                let show = hall.show();
                card.centered(10, show.title, INK)?;
                card.rect(12, 33, logical_width as i32 - 24, 1, FRAME);
                let start = (logical_width as i32 - (show.chain.len() as i32 * 40 - 16)) / 2;
                for (i, &(item, count)) in show.chain.iter().enumerate() {
                    let x = start + i as i32 * 40;
                    card.icon(x, 49, 24, item)?;
                    if count > 1 {
                        card.text(x, 77, &format!("×{count}"), INK)?;
                    }
                    if i + 1 < show.chain.len() {
                        card.text(x + 27, 52, "→", ACCENT)?;
                    }
                }
                for (i, &(item, description)) in show.extra.iter().enumerate() {
                    let y = 93 + i as i32 * 32;
                    card.icon(15, y + 6, 20, item)?;
                    card.two_lines(43, y, logical_width as i32 - 55, description)?;
                }
                card.centered(167, &card.fit(show.why, logical_width as i32 - 24), ACCENT)?;
            }
            Sheet::Craft(hall) => {
                card.centered(9, &format!("Рецепты: {}", hall.title()), INK)?;
                for (i, entry) in hall.craft().iter().enumerate() {
                    let recipe =
                        crossroads::recipe_cells(book, entry.name).map_err(Error::Recipe)?;
                    let card_width = (logical_width as i32 - 24) / 2;
                    let x = 8 + (i % 2) as i32 * (card_width + 8);
                    let y = 37 + (i / 2) as i32 * 76;
                    card.rect(x, y, card_width, 70, [222, 210, 182]);
                    card.clip = [x, y, card_width, 70];
                    card.text(x + 5, y + 2, &card.fit(entry.caption, card_width - 10), INK)?;
                    for (at, item) in recipe.grid.into_iter().enumerate() {
                        let gx = x + 5 + (at % 3) as i32 * 14;
                        let gy = y + 19 + (at / 3) as i32 * 14;
                        card.rect(gx, gy, 13, 13, [199, 186, 157]);
                        if item != 0 {
                            card.icon(gx, gy, 12, item)?;
                        }
                    }
                    card.text(x + 60, y + 29, "→", ACCENT)?;
                    card.icon(x + card_width - 35, y + 23, 24, recipe.output)?;
                    card.text(
                        x + card_width - 35,
                        y + 50,
                        &format!("×{}", recipe.count),
                        INK,
                    )?;
                    card.clip = [0, 0, logical_width as i32, HEIGHT as i32];
                    images.recipes += 1;
                }
            }
            Sheet::Sky(day) => {
                card.centered(16, if day { "День" } else { "Ночь" }, INK)?;
                // Original simple geometric marks; no external image asset.
                for y in 0i32..64 {
                    for x in 0i32..64 {
                        let disk = (x - 32).pow(2) + (y - 32).pow(2) <= 25 * 25;
                        let crescent = (x - 42).pow(2) + (y - 23).pow(2) > 23 * 23;
                        if disk && (day || crescent) {
                            card.pixel(
                                x + (logical_width as i32 - 64) / 2,
                                y + 52,
                                if day { [218, 156, 46] } else { [73, 91, 129] },
                                255,
                            );
                        }
                    }
                }
                card.centered(143, "ПКМ — выбрать", INK)?;
                card.centered(171, "Только для вас", ACCENT)?;
            }
            Sheet::SkySign => {
                card.centered(57, "Смена времени", INK)?;
                card.centered(91, "День / Ночь", ACCENT)?;
                card.centered(127, "Серверный мир не меняется", INK)?;
            }
        }
        for y in 0..HEIGHT as usize {
            for x in 0..WIDTH as usize {
                let sx = (2 * x + 1) * logical_width as usize / (2 * WIDTH as usize);
                let source = (y * logical_width as usize + sx) * 4;
                let target = index * layer_bytes + (y * WIDTH as usize + x) * 4;
                images.rgba[target..target + 4].copy_from_slice(&card.pixels[source..source + 4]);
            }
        }
        images.icons += card.icons;
        images.glyphs += card.glyphs;
    }
    Ok(images)
}
