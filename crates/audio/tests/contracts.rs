use audio::{
    echo::Cavity,
    resources::{ImportError, ImportLimits, LocalResources},
    *,
};
use std::{
    alloc::System,
    io::{Cursor, Write},
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<System> = telemetry::CountingAllocator::new(System);
fn pcm(samples: &[f32]) -> Pcm {
    Pcm::mono(samples.to_vec(), 48000).unwrap()
}
fn mixer() -> Mixer {
    Mixer::new(48000, 4, 32 * 1024 * 1024).unwrap()
}
#[test]
fn equal_power_pan_distance_and_category_volume_are_independent() {
    let mut mixer = mixer();
    let clip = mixer.add_clip(pcm(&[0.5; 16])).unwrap();
    let mut request = VoiceRequest::local(clip, EmitterId(1), Category::Blocks);
    request.position = Some([8.0, 0.0, 0.0]);
    request.range = 16.0;
    mixer.play(request).unwrap();
    let mut output = [0.0; 4];
    mixer.mix_stereo(&mut output);
    assert_eq!(output, [0.0, 0.25, 0.0, 0.25]);
    mixer.volume(Some(Category::Blocks), 0.5).unwrap();
    mixer.volume(None, 0.5).unwrap();
    mixer.mix_stereo(&mut output);
    assert_eq!(output, [0.0, 0.0625, 0.0, 0.0625]);
    mixer
        .listener(Listener {
            position: [16.0, 0.0, 0.0],
            right: [1.0, 0.0, 0.0],
        })
        .unwrap();
    mixer.mix_stereo(&mut output);
    assert_eq!(output, [0.0625, 0.0, 0.0625, 0.0]);
}
#[test]
fn updating_machine_loop_preserves_sample_position_and_handle() {
    let mut mixer = mixer();
    let clip = mixer
        .add_clip(pcm(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7]))
        .unwrap();
    let mut request = VoiceRequest::local(clip, EmitterId(42), Category::Machines);
    request.looping = true;
    let handle = mixer.play(request).unwrap();
    mixer.mix_stereo(&mut [0.0; 6]);
    let cursor = mixer.cursor(handle).unwrap();
    assert_eq!(cursor, 3.0);
    request.position = Some([3.0, 0.0, 0.0]);
    request.gain = 0.2;
    assert_eq!(mixer.play(request).unwrap(), handle);
    assert_eq!(mixer.cursor(handle), Some(cursor));
    assert_eq!(mixer.active_voices(), 1);
    mixer.mix_stereo(&mut [0.0; 20]);
    assert!(mixer.cursor(handle).unwrap() < 7.0);
}
#[test]
fn priority_pool_and_generation_prevent_stale_stop() {
    let mut mixer = Mixer::new(48000, 1, 1000).unwrap();
    let clip = mixer.add_clip(pcm(&[0.2; 32])).unwrap();
    let mut low = VoiceRequest::local(clip, EmitterId(1), Category::Machines);
    low.priority = 10;
    let first = mixer.play(low).unwrap();
    let mut high = low;
    high.emitter = EmitterId(2);
    high.priority = 200;
    let next = mixer.play(high).unwrap();
    assert_ne!(first.generation, next.generation);
    assert!(!mixer.stop(first));
    assert_eq!(mixer.play(low), Err(AudioError::Capacity));
    assert!(mixer.stop(next));
    mixer.mix_stereo(&mut [0.0; 256]);
    assert_eq!(mixer.active_voices(), 0);
}
#[test]
fn resampling_pitch_completion_and_bad_inputs_are_bounded() {
    let mut mixer = mixer();
    let clip = mixer
        .add_clip(Pcm::mono(vec![0.0, 1.0, 0.0], 24000).unwrap())
        .unwrap();
    let request = VoiceRequest::local(clip, EmitterId(1), Category::Ui);
    mixer.play(request).unwrap();
    let mut output = [0.0; 14];
    mixer.mix_stereo(&mut output);
    assert!((output[2] - 0.5 * libm::sqrtf(0.5)).abs() < 1e-6);
    assert_eq!(mixer.active_voices(), 0);
    let mut bad = request;
    bad.pitch = f32::NAN;
    assert_eq!(mixer.play(bad), Err(AudioError::Invalid));
    assert_eq!(mixer.volume(None, 2.0), Err(AudioError::Invalid));
    assert!(Pcm::mono(vec![f32::NAN], 48000).is_err());
    assert!(Pcm::decode_ogg(b"not ogg").is_err());
}
#[test]
fn sixty_four_active_loops_allocate_nothing_for_ten_thousand_callbacks() {
    let mut mixer = Mixer::new(48000, 64, 1000).unwrap();
    let clip = mixer.add_clip(pcm(&[0.001; 8])).unwrap();
    for emitter in 0..64 {
        let mut r = VoiceRequest::local(clip, EmitterId(emitter), Category::Machines);
        r.looping = true;
        mixer.play(r).unwrap();
    }
    let mut output = [0.0; 256];
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10000 {
        mixer.mix_stereo(&mut output);
    }
    let allocations = scope.finish();
    assert_eq!(allocations.allocating_calls(), 0);
    assert_eq!(allocations.dealloc_calls, 0);
    assert!(output.iter().any(|v| *v > 0.0));
    let work = mixer.take_work();
    assert_eq!(work.voice_samples, 64 * 128 * 10000);
    assert_eq!(work.peak_voices, 64);
    let scope = telemetry::AllocationScope::begin().unwrap();
    std::hint::black_box(vec![0u8; 2048]);
    assert!(scope.finish().allocating_calls() > 0);
}
#[test]
fn every_default_sound_decodes_with_embedded_credits_and_catalog_gains() {
    assert!(
        audio::catalog::CREDITS.contains("Eelke") && audio::catalog::CREDITS.contains("Lamoot")
    );
    let mut mixer = Mixer::new(48000, 64, 256 * 1024 * 1024).unwrap();
    let mut catalog = audio::catalog::Catalog::new();
    let events: Vec<_> = audio::catalog::Catalog::events().collect();
    assert_eq!(events.len(), 68);
    for event in events {
        for seed in 0..24 {
            catalog.prepare(&mut mixer, event, seed).unwrap();
        }
    }
    let step = catalog
        .request(
            &mut mixer,
            "block.stone.step",
            EmitterId(1),
            Some([0.0; 3]),
            5,
        )
        .unwrap();
    assert_eq!(step.gain, 0.15);
    assert!((0.95..=1.05).contains(&step.pitch));
    let looped = catalog
        .request(&mut mixer, "machine.loop", EmitterId(2), None, 0)
        .unwrap();
    assert!(looped.looping);
}
#[test]
fn cave_impulse_has_predicted_delay_and_plain_unknown_stays_dry() {
    let cave = Cavity {
        wall_distances: [Some(5.0); 6],
        absorption: 0.3,
    }
    .impulse(48000)
    .unwrap();
    let first = cave.iter().position(|v| *v != 0.0).unwrap();
    assert_eq!(first, (10.0f32 / 343.0 * 48000.0) as usize);
    assert!(cave.iter().sum::<f32>() <= 0.700001);
    assert!(
        Cavity {
            wall_distances: [None; 6],
            absorption: 0.3
        }
        .impulse(48000)
        .unwrap()
        .iter()
        .all(|v| *v == 0.0)
    );
    assert!(
        Cavity {
            wall_distances: [Some(f32::NAN); 6],
            absorption: 0.3
        }
        .impulse(48000)
        .is_err()
    );
}
fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in files {
        zip.start_file(
            *name,
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
#[test]
fn local_labpbr_preserves_notices_and_failed_import_is_atomic() {
    let mut store = LocalResources::new(ImportLimits::default());
    let bytes = archive(&[
        (
            "assets/minecraft/textures/block/stone.png",
            b"LOCAL_PRIVATE_COLOR",
        ),
        (
            "assets/minecraft/textures/block/stone_n.png",
            b"LOCAL_PRIVATE_NORMAL",
        ),
        (
            "assets/minecraft/textures/block/stone_s.png",
            b"LOCAL_PRIVATE_SPECULAR",
        ),
        ("CREDITS.md", b"Local author license"),
    ]);
    let summary = store.import_zip(&bytes).unwrap();
    assert_eq!((summary.textures, summary.notices), (3, 1));
    let pbr = store.labpbr("minecraft", "block/stone").unwrap();
    assert_eq!(pbr.normal, Some(&b"LOCAL_PRIVATE_NORMAL"[..]));
    assert_eq!(
        store.import_zip(&archive(&[("../outside.ogg", b"bad")])),
        Err(ImportError::Path)
    );
    assert_eq!(store.summary().revision, summary.revision);
    assert_eq!(
        store.get_local("CREDITS.md"),
        Some(&b"Local author license"[..])
    );
}
#[test]
fn traversal_case_collision_symlinks_zip64_bombs_and_truncation_are_rejected() {
    for name in [
        "/absolute.ogg",
        "assets/minecraft/../x.ogg",
        "assets\\x.ogg",
        "C:/x.ogg",
        "assets//x.ogg",
    ] {
        assert_eq!(
            LocalResources::new(ImportLimits::default()).import_zip(&archive(&[(name, b"x")])),
            Err(ImportError::Path)
        );
    }
    assert_eq!(
        LocalResources::new(ImportLimits::default())
            .import_zip(&archive(&[("assets/a.ogg", b"x"), ("assets/A.ogg", b"y")])),
        Err(ImportError::Duplicate)
    );
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.add_symlink(
        "assets/escape.ogg",
        "../../outside",
        zip::write::FileOptions::default(),
    )
    .unwrap();
    assert_eq!(
        LocalResources::new(ImportLimits::default())
            .import_zip(&zip.finish().unwrap().into_inner()),
        Err(ImportError::Path)
    );
    let data = archive(&[("assets/a.ogg", b"0123456789012345")]);
    let mut tiny = LocalResources::new(ImportLimits {
        entry_bytes: 8,
        ..ImportLimits::default()
    });
    assert_eq!(tiny.import_zip(&data), Err(ImportError::Limit));
    assert!(
        LocalResources::new(ImportLimits::default())
            .import_zip(&data[..data.len() - 5])
            .is_err()
    );
    let mut zip64 = data.clone();
    let eocd = zip64.len() - 22;
    zip64[eocd + 10..eocd + 12].copy_from_slice(&u16::MAX.to_le_bytes());
    assert_eq!(
        LocalResources::new(ImportLimits::default()).import_zip(&zip64),
        Err(ImportError::Unsupported)
    );
}
#[test]
fn minecraft_sound_index_resolves_only_verified_local_objects() {
    use sha1::{Digest, Sha1};
    let sound = b"LOCAL_MINECRAFT_SOUND_CANARY";
    let hash = format!("{:x}", Sha1::digest(sound));
    let index = format!(
        "{{\"objects\":{{\"minecraft/sounds/test.ogg\":{{\"hash\":\"{hash}\",\"size\":{}}}}}}}",
        sound.len()
    );
    let path = format!(".minecraft/assets/objects/{}/{}", &hash[..2], hash);
    let mut store = LocalResources::new(ImportLimits::default());
    store
        .import_directory(vec![
            (
                ".minecraft/assets/indexes/1.21.json".into(),
                index.into_bytes(),
            ),
            (path, sound.to_vec()),
        ])
        .unwrap();
    assert_eq!(
        store
            .minecraft_sound("indexes/1.21.json", "minecraft/sounds/test.ogg")
            .unwrap(),
        Some(&sound[..])
    );
    assert_eq!(
        store.import_directory(vec![(
            ".minecraft/launcher_accounts.json".into(),
            b"private".to_vec()
        )]),
        Err(ImportError::Unsupported)
    );
}

#[test]
fn step_cadence_matches_donor_and_snow_has_two_distinct_gains() {
    use audio::events::{StepBody, Steps, Surface};
    for (speed, expected) in [(0.215859, 2.59), (0.280617, 3.37)] {
        let mut steps = Steps::default();
        let body = StepBody {
            moved: [speed, 0.0, 0.0],
            grounded: true,
            climbing: false,
            silent: false,
            crouching: false,
            stepping_air: false,
            support: Surface::Stone,
            snow_above_support: false,
        };
        let count = (0..2000).filter(|_| steps.tick(body)[0].is_some()).count();
        assert!((count as f64 / 100.0 - expected).abs() < 0.03);
    }
    let mut steps = Steps::default();
    let body = StepBody {
        moved: [2.0, 0.0, 0.0],
        grounded: true,
        climbing: false,
        silent: true,
        crouching: false,
        stepping_air: false,
        support: Surface::Wood,
        snow_above_support: true,
    };
    for _ in 0..1000 {
        assert_eq!(steps.tick(body), [None, None]);
    }
    let cues = steps.tick(StepBody {
        silent: false,
        ..body
    });
    assert_eq!(cues[0].unwrap().gain, 0.15);
    assert_eq!(cues[1].unwrap().gain, 0.05);
    assert_eq!(cues[1].unwrap().event, "block.wood.step");
}
#[test]
fn native_echo_matches_sparse_impulse_and_callback_allocates_nothing() {
    let mut mixer = mixer();
    let clip = mixer.add_clip(pcm(&[0.5, 0.0])).unwrap();
    mixer
        .cavity(Cavity {
            wall_distances: [Some(5.0); 6],
            absorption: 0.3,
        })
        .unwrap();
    mixer
        .play(VoiceRequest::local(clip, EmitterId(1), Category::Ui))
        .unwrap();
    let mut output = [0.0; 4096];
    let scope = telemetry::AllocationScope::begin().unwrap();
    mixer.mix_stereo(&mut output);
    let allocations = scope.finish();
    assert_eq!(allocations.allocating_calls(), 0);
    assert_eq!(allocations.dealloc_calls, 0);
    let first = (1..2048).find(|frame| output[frame * 2] != 0.0).unwrap();
    assert_eq!(first, 1399);
    let impulse = Cavity {
        wall_distances: [Some(5.0); 6],
        absorption: 0.3,
    }
    .impulse(48000)
    .unwrap();
    let expected = 0.5 * libm::sqrtf(0.5) * impulse[1399] * 0.25;
    assert!((output[1399 * 2] - expected).abs() < 1e-7);
    assert!(mixer.take_work().echo_products <= 4096 * 18);
}
#[test]
fn local_sound_declarations_and_vanilla_stems_feed_the_same_decoder() {
    let ogg = audio::catalog::Catalog::compressed("ui/click_1.ogg").unwrap();
    let json=br#"{"ui.click":{"sounds":[{"name":"privacy:custom/click"}]},"alias":{"sounds":[{"name":"ui.click","type":"event"}]}}"#;
    let mut local = LocalResources::new(ImportLimits::default());
    local
        .import_zip(&archive(&[
            ("assets/minecraft/sounds.json", json),
            ("assets/privacy/sounds/custom/click.ogg", ogg),
        ]))
        .unwrap();
    assert_eq!(local.sound_for_event("alias", 0, None).unwrap(), Some(ogg));
    let mut mixer = mixer();
    let mut catalog = audio::catalog::Catalog::new();
    let clip = catalog
        .prepare_with_local(&mut mixer, &local, "ui.click", 0, None)
        .unwrap();
    assert_eq!(mixer.clip(clip).unwrap().samples().len(), 3648);
    assert_eq!(
        catalog
            .prepare_with_local(&mut mixer, &local, "ui.click", 0, None)
            .unwrap(),
        clip
    );
    let recursive = br#"{"loop":{"sounds":[{"name":"loop","type":"event"}]}}"#;
    local
        .import_zip(&archive(&[("assets/minecraft/sounds.json", recursive)]))
        .unwrap();
    assert_eq!(
        local.sound_for_event("loop", 0, None),
        Err(ImportError::Index)
    );
}
#[test]
fn directory_preflight_never_selects_accounts_or_non_sound_objects() {
    use audio::resources::{minecraft_sound_objects, selected_minecraft_asset};
    assert_eq!(
        selected_minecraft_asset(".minecraft/launcher_accounts.json").unwrap(),
        None
    );
    assert_eq!(
        selected_minecraft_asset(".minecraft/saves/world/CREDITS.md").unwrap(),
        None
    );
    assert_eq!(
        selected_minecraft_asset(".minecraft/assets/indexes/1.21.json").unwrap(),
        Some("indexes/1.21.json".into())
    );
    let json=br#"{"objects":{"minecraft/sounds/random/click.ogg":{"hash":"00112233445566778899aabbccddeeff00112233","size":3},"minecraft/textures/private.png":{"hash":"44112233445566778899aabbccddeeff00112233","size":3}}}"#;
    assert_eq!(
        minecraft_sound_objects(json, 10).unwrap(),
        vec!["objects/00/00112233445566778899aabbccddeeff00112233"]
    );
}
