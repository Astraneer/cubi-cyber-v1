use crate::atlas::UiAtlas;
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}
impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
    pub fn intersection(self, other: Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let w = (self.x + self.w).min(other.x + other.w) - x;
        let h = (self.y + self.h).min(other.y + other.h) - y;
        (w > 0.0 && h > 0.0).then_some(Self { x, y, w, h })
    }
    pub fn scaled(self, scale: f32) -> Self {
        Self::new(
            (self.x * scale).round(),
            (self.y * scale).round(),
            (self.w * scale).round(),
            (self.h * scale).round(),
        )
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Quad {
    pub rect: Rect,
    pub uv: Rect,
    pub color: u32,
}
pub const MAX_UI_QUADS: usize = 16384;
pub struct PaintList {
    pub quads: Vec<Quad>,
    pub revision: u64,
    pub width: u32,
    pub height: u32,
    pub clipped: u64,
}
impl Default for PaintList {
    fn default() -> Self {
        Self {
            quads: Vec::with_capacity(MAX_UI_QUADS),
            revision: 0,
            width: 1,
            height: 1,
            clipped: 0,
        }
    }
}
impl PaintList {
    pub fn clear(&mut self, revision: u64, width: u32, height: u32) {
        self.quads.clear();
        self.revision = revision;
        self.width = width;
        self.height = height;
        self.clipped = 0;
    }
}
pub struct Painter<'a> {
    pub atlas: &'a UiAtlas,
    pub list: &'a mut PaintList,
    pub clip: Rect,
    pub scale: f32,
}
impl<'a> Painter<'a> {
    pub fn new(atlas: &'a UiAtlas, list: &'a mut PaintList) -> Self {
        let clip = Rect::new(0.0, 0.0, list.width as f32, list.height as f32);
        Self {
            atlas,
            list,
            clip,
            scale: 1.0,
        }
    }
    pub fn quad(&mut self, rect: Rect, uv: Rect, color: u32) {
        if self.list.quads.len() == MAX_UI_QUADS {
            self.list.clipped += 1;
            return;
        }
        let Some(clipped) = rect.intersection(self.clip) else {
            return;
        };
        let uv = Rect::new(
            uv.x + (clipped.x - rect.x) / rect.w * uv.w,
            uv.y + (clipped.y - rect.y) / rect.h * uv.h,
            uv.w * clipped.w / rect.w,
            uv.h * clipped.h / rect.h,
        );
        self.list.quads.push(Quad {
            rect: clipped.scaled(self.scale),
            uv,
            color,
        });
    }
    pub fn rect(&mut self, rect: Rect, color: u32) {
        self.quad(rect, self.atlas.white, color);
    }
    pub fn border(&mut self, rect: Rect, color: u32) {
        self.rect(Rect::new(rect.x, rect.y, rect.w, 1.0), color);
        self.rect(Rect::new(rect.x, rect.y + rect.h - 1.0, rect.w, 1.0), color);
        self.rect(Rect::new(rect.x, rect.y, 1.0, rect.h), color);
        self.rect(Rect::new(rect.x + rect.w - 1.0, rect.y, 1.0, rect.h), color);
    }
    pub fn text(&mut self, x: f32, y: f32, text: &str, color: u32) {
        let mut cursor = x;
        for ch in text.chars() {
            if ch == '\n' {
                continue;
            }
            let glyph = self.atlas.glyph(ch);
            if glyph.size[0] > 0.0 {
                let rect = Rect::new(
                    cursor + glyph.offset[0],
                    y + glyph.offset[1],
                    glyph.size[0],
                    glyph.size[1],
                );
                self.quad(rect, glyph.uv, color);
            }
            cursor += glyph.advance;
        }
    }
    pub fn text_width(&self, text: &str) -> f32 {
        text.chars().map(|ch| self.atlas.glyph(ch).advance).sum()
    }
    pub fn centered(&mut self, rect: Rect, text: &str, color: u32) {
        let width = self.text_width(text);
        self.text(
            (rect.x + (rect.w - width) * 0.5).floor(),
            (rect.y + (rect.h - 18.0) * 0.5).floor(),
            text,
            color,
        );
    }
    pub fn item(&mut self, rect: Rect, item: rules::ItemId) {
        if let Some(uv) = self.atlas.item(item) {
            self.quad(rect, uv, 0xffffffff);
        }
    }
}
pub struct TextBuffer {
    bytes: [u8; 256],
    len: usize,
}
impl Default for TextBuffer {
    fn default() -> Self {
        Self {
            bytes: [0; 256],
            len: 0,
        }
    }
}
impl TextBuffer {
    pub fn clear(&mut self) {
        self.len = 0;
    }
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).expect("formatted UTF8")
    }
}
impl std::fmt::Write for TextBuffer {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        if self.len + s.len() > self.bytes.len() {
            return Err(std::fmt::Error);
        }
        self.bytes[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
        self.len += s.len();
        Ok(())
    }
}
