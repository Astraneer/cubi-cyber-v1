fn main() {
    if std::env::args().nth(1).as_deref() == Some("--find-forest") {
        let generator = terrain_gen::Generator::new(terrain_gen::TerrainSeed(1337));
        for (x, z) in [
            (96, 96),
            (128, 128),
            (128, 0),
            (0, 128),
            (-128, 128),
            (256, 256),
            (-128, -128),
        ] {
            let info = generator.column_info(x, z);
            let column = generator
                .generate_column(x.div_euclid(16), z.div_euclid(16))
                .expect("bounded coordinates");
            let leaves = column
                .raw_blocks()
                .iter()
                .filter(|value| rules::blocks::is_leaves(u16::from(**value)))
                .count();
            println!(
                "FOREST_CANDIDATE x={x} y={} z={z} biome={} leaves={leaves}",
                info.height, info.biome
            );
        }
        return;
    }
    println!("{}", determinism_runner::run_trace());
}
