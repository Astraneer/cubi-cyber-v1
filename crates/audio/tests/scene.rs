use audio::{
    EmitterId, Pcm,
    catalog::Catalog,
    events::{StepBody, Surface},
    resources::{ImportLimits, LocalResources},
    scene::PreparedSounds,
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
#[test]
fn loading_prepares_every_event_and_fixed_tick_requests_allocate_nothing() {
    let prepared = PreparedSounds::prepare(None, None).unwrap();
    let (mut mixer, mut events) = prepared.into_parts();
    let body = StepBody {
        moved: [0.3, 0.0, 0.0],
        grounded: true,
        climbing: false,
        silent: false,
        crouching: false,
        stepping_air: false,
        support: Surface::Stone,
        snow_above_support: false,
    };
    for name in Catalog::events() {
        let request = events.request(name, EmitterId(1), None, 0).unwrap();
        assert!(mixer.clip(request.clip).is_some());
    }
    let scope = telemetry::AllocationScope::begin().unwrap();
    let mut footsteps = 0;
    for tick in 0..10_000 {
        let cue = events.steps(tick, body, EmitterId(1), [0.0; 3]).unwrap();
        footsteps += cue.iter().flatten().count();
        assert!(
            events
                .steps(tick, body, EmitterId(1), [0.0; 3])
                .unwrap()
                .iter()
                .all(Option::is_none)
        );
        let request = events
            .request("ui.click", EmitterId(2), None, tick)
            .unwrap();
        std::hint::black_box(request);
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    assert!(footsteps > 1500 && footsteps < 1900);
    let fast = StepBody {
        moved: [2.0, 0.0, 0.0],
        ..body
    };
    for id in 2..=64 {
        assert!(events.steps(0, fast, EmitterId(id), [0.0; 3]).unwrap()[0].is_some());
    }
    assert!(matches!(
        events.steps(0, fast, EmitterId(65), [0.0; 3]),
        Err(audio::AudioError::Capacity)
    ));
    events.forget_emitter(EmitterId(2));
    assert!(events.steps(0, fast, EmitterId(65), [0.0; 3]).unwrap()[0].is_some());
    let request = events
        .request("machine.loop", EmitterId(3), None, 0)
        .unwrap();
    let handle = mixer.play(request).unwrap();
    mixer.mix_stereo(&mut [0.0; 256]);
    assert_eq!(mixer.play(request).unwrap(), handle);
    assert_eq!(mixer.stop_emitter(EmitterId(3)), 1);
    mixer.mix_stereo(&mut [0.0; 256]);
    assert_eq!(mixer.active_voices(), 0);
}
#[test]
fn local_override_is_decoded_before_activation_and_not_in_event_storage() {
    let bytes = Catalog::files().next().unwrap().1;
    let reference = Pcm::decode_ogg(bytes).unwrap();
    let mut local = LocalResources::new(ImportLimits::default());
    local
        .import_directory(vec![
            (
                "assets/minecraft/sounds.json".into(),
                br#"{"block.stone.step":{"sounds":["local/test"]}}"#.to_vec(),
            ),
            (
                "assets/minecraft/sounds/local/test.ogg".into(),
                bytes.to_vec(),
            ),
        ])
        .unwrap();
    let prepared = PreparedSounds::prepare(Some(&local), None).unwrap();
    let (mixer, events) = prepared.into_parts();
    drop(local);
    for seed in 0..12 {
        let request = events
            .request(
                "block.stone.step",
                EmitterId(1),
                Some([3.0, 2.0, 1.0]),
                seed,
            )
            .unwrap();
        assert_eq!(
            mixer.clip(request.clip).unwrap().samples(),
            reference.samples()
        );
        assert_eq!(request.gain, 0.15);
    }
    assert!(events.request("missing", EmitterId(1), None, 0).is_err());
}
