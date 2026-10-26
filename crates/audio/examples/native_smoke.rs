//! Optional real endpoint check. Exit2 means unavailable, never successful audio.
fn main() {
    let prepared = audio::scene::PreparedSounds::prepare(None, None)
        .expect("prepare default sound bank before starting endpoint");
    match audio::scene::NativeScene::open(prepared) {
        Ok(mut scene) => {
            scene
                .event("ui.click", audio::EmitterId(1), None, 1)
                .unwrap();
            let output = scene.output();
            std::thread::sleep(std::time::Duration::from_millis(500));
            let work = output.with_mixer(|mixer| mixer.take_work()).unwrap();
            if output.callback_errors() != 0 || work.frames == 0 || work.voice_samples == 0 {
                eprintln!(
                    "NATIVE_AUDIO_FAILED callback_errors={} missed={}",
                    output.callback_errors(),
                    output.missed_callbacks()
                );
                std::process::exit(1);
            }
            let prepared = audio::scene::PreparedSounds::prepare(None, None).unwrap();
            scene.replace_bank(prepared).unwrap();
            scene
                .event("ui.click", audio::EmitterId(2), None, 2)
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(100));
            let output = scene.output();
            let replacement = output.with_mixer(|mixer| mixer.take_work()).unwrap();
            assert!(replacement.frames > 0 && replacement.voice_samples > 0);
            assert_eq!(output.callback_errors(), 0);
            println!(
                "NATIVE_AUDIO_ENDPOINT_OK virtual={} frames={} voice_samples={} missed_callbacks={} replaced_bank_voice_samples={}",
                std::env::var_os("CUBI_AUDIO_VIRTUAL").is_some(),
                work.frames,
                work.voice_samples,
                output.missed_callbacks(),
                replacement.voice_samples,
            );
        }
        Err(error) => {
            eprintln!("NATIVE_AUDIO_ENDPOINT_UNAVAILABLE {error}");
            std::process::exit(2);
        }
    }
}
