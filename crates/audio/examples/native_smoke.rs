//! Optional real endpoint check. Exit2 means unavailable, never successful audio.
fn main() {
    match audio::native::NativeOutput::open(32, 32 * 1024 * 1024) {
        Ok(output) => {
            let mut catalog = audio::catalog::Catalog::new();
            output
                .with_mixer(|mixer| {
                    let request = catalog
                        .request(mixer, "ui.click", audio::EmitterId(1), None, 1)
                        .unwrap();
                    mixer.play(request).unwrap();
                })
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(500));
            let work = output.with_mixer(|mixer| mixer.take_work()).unwrap();
            if output.callback_errors() != 0 || work.frames == 0 {
                eprintln!(
                    "NATIVE_AUDIO_FAILED callback_errors={} missed={}",
                    output.callback_errors(),
                    output.missed_callbacks()
                );
                std::process::exit(1);
            }
            println!(
                "NATIVE_AUDIO_ENDPOINT_OK virtual={} frames={} voice_samples={} missed_callbacks={}",
                std::env::var_os("CUBI_AUDIO_VIRTUAL").is_some(),
                work.frames,
                work.voice_samples,
                output.missed_callbacks()
            );
        }
        Err(error) => {
            eprintln!("NATIVE_AUDIO_ENDPOINT_UNAVAILABLE {error}");
            std::process::exit(2);
        }
    }
}
