//! Offline metric for fenced screenshots. A third PNG supplies a negative control.
#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    fn read(path: &std::ffi::OsStr) -> Result<(u32, u32, Vec<u8>), Box<dyn std::error::Error>> {
        let mut decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path)?));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info()?;
        let mut bytes = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut bytes)?;
        let bytes = &bytes[..info.buffer_size()];
        let rgba = match info.color_type {
            png::ColorType::Rgba => bytes.to_vec(),
            png::ColorType::Rgb => bytes
                .chunks_exact(3)
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect(),
            png::ColorType::Grayscale => bytes.iter().flat_map(|v| [*v, *v, *v, 255]).collect(),
            png::ColorType::GrayscaleAlpha => bytes
                .chunks_exact(2)
                .flat_map(|p| [p[0], p[0], p[0], p[1]])
                .collect(),
            png::ColorType::Indexed => return Err("PNG palette was not expanded".into()),
        };
        Ok((info.width, info.height, rgba))
    }
    fn metric(width: u32, height: u32, reference: &[u8], actual: &[u8]) -> (usize, u8, f32) {
        let differing = reference
            .chunks_exact(4)
            .zip(actual.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        let maximum = reference
            .iter()
            .zip(actual)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        let rgb = |rgba: &[u8]| {
            rgba.chunks_exact(4)
                .flat_map(|p| p[..3].iter().copied())
                .collect::<Vec<_>>()
        };
        let error = nv_flip::flip(
            nv_flip::FlipImageRgb8::with_data(width, height, &rgb(reference)),
            nv_flip::FlipImageRgb8::with_data(width, height, &rgb(actual)),
            nv_flip::DEFAULT_PIXELS_PER_DEGREE,
        );
        (
            differing,
            maximum,
            nv_flip::FlipPool::from_image(&error).mean(),
        )
    }
    let paths = std::env::args_os().skip(1).collect::<Vec<_>>();
    if !(2..=3).contains(&paths.len()) {
        return Err("usage: compare_frames REFERENCE.png ACTUAL.png [NEGATIVE_CONTROL.png]".into());
    }
    let (width, height, reference) = read(&paths[0])?;
    let (actual_width, actual_height, actual) = read(&paths[1])?;
    if (width, height) != (actual_width, actual_height) {
        return Err("image dimensions differ".into());
    }
    let (differing, maximum, mean) = metric(width, height, &reference, &actual);
    let black = vec![0; reference.len()];
    let (_, _, black_mean) = metric(width, height, &reference, &black);
    let negative = if let Some(path) = paths.get(2) {
        let (negative_width, negative_height, negative) = read(path)?;
        if (width, height) != (negative_width, negative_height) {
            return Err("negative-control image dimensions differ".into());
        }
        let (count, max, mean) = metric(width, height, &reference, &negative);
        format!(
            ",\"negativeControl\":{{\"differingPixels\":{count},\"maxDelta\":{max},\"flipMean\":{mean}}}"
        )
    } else {
        String::new()
    };
    println!(
        "{{\"width\":{width},\"height\":{height},\"differingPixels\":{differing},\"maxDelta\":{maximum},\"flipMean\":{mean},\"blackControlFlipMean\":{black_mean}{negative}}}"
    );
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn main() {}
