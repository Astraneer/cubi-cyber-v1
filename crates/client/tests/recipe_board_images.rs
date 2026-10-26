use client::recipe_board_images::{self as images, Error};
use rules::{crafting::RecipeBook, crossroads};
use std::hash::{Hash, Hasher};

#[test]
fn all_public_cards_use_opaque_art_and_the_actual_twelve_compiled_recipes() {
    let atlas = ui::atlas::UiAtlas::load().unwrap();
    let book = RecipeBook::standard().unwrap();
    let prepared = images::prepare(&atlas, &book).unwrap();
    assert_eq!(prepared.rgba.len(), images::BYTES);
    assert_eq!(prepared.recipes, 12);
    assert!(prepared.icons > 60 && prepared.glyphs > 200);
    assert!(prepared.rgba.chunks_exact(4).all(|pixel| pixel[3] == 255));
    let layer_bytes = (images::WIDTH * images::HEIGHT * 4) as usize;
    let mut hashes = std::collections::BTreeSet::new();
    for (index, layer) in prepared.rgba.chunks_exact(layer_bytes).enumerate() {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        layer.hash(&mut hasher);
        assert!(
            hashes.insert(hasher.finish()),
            "duplicate physical card {index}"
        );
        assert!(layer.chunks_exact(4).any(|p| p[1] > p[0] && p[1] > p[2]));
        if let Some(folder) = std::env::var_os("CUBI_BOARD_PREVIEW") {
            let folder = std::path::PathBuf::from(folder);
            std::fs::create_dir_all(&folder).unwrap();
            let mut bytes = format!("P6\n{} {}\n255\n", images::WIDTH, images::HEIGHT).into_bytes();
            for pixel in layer.chunks_exact(4) {
                bytes.extend_from_slice(&pixel[..3]);
            }
            std::fs::write(folder.join(format!("layer-{index:02}.ppm")), bytes).unwrap();
        }
    }
    assert_eq!(hashes.len(), crossroads::BOARD_COUNT);
    // An absent compiled recipe must stop preparation rather than draw guessed
    // ingredients or silently advertise a different machine recipe.
    let empty = RecipeBook::build(std::iter::empty()).unwrap();
    assert!(matches!(
        images::prepare(&atlas, &empty),
        Err(Error::Recipe(crossroads::Error::MissingRecipe))
    ));
    let mut uploaded = atlas;
    uploaded.pixels.clear();
    assert!(matches!(
        images::prepare(&uploaded, &book),
        Err(Error::AtlasPixels)
    ));
}
