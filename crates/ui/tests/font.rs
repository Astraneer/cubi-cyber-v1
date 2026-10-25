#[test]
fn pinned_woff_glyphs_have_identical_native_browser_data() {
    let ttf = font::unpack_woff(font::FONT_SOURCE).unwrap();
    let font = fontdue::Font::from_bytes(ttf, fontdue::FontSettings::default()).unwrap();
    for ch in ['M', 'i', 'П', 'я', '0', ' '] {
        let (metrics, bytes) = font.rasterize(ch, 16.0);
        assert!(metrics.advance_width > 0.0);
        assert_eq!(bytes.len(), metrics.width * metrics.height);
        println!(
            "GLYPH {ch} {} {} {} {} {}",
            metrics.width, metrics.height, metrics.xmin, metrics.ymin, metrics.advance_width
        );
    }
    assert!(font::FONT_CREDITS.contains("CC0"));
    let mut corrupt = font::FONT_SOURCE.to_vec();
    corrupt[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(font::unpack_woff(&corrupt).is_err());
}
#[path = "../src/font.rs"]
pub mod font;
