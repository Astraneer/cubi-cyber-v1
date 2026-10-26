use ui::settings::{Preset, Setting, Settings};

fn legacy(preset: u8) -> Vec<u8> {
    let mut settings = Settings::default();
    settings.set(Setting::Sensitivity, 125).unwrap();
    settings.set(Setting::ShadowResolution, 4096).unwrap();
    let mut bytes = settings.encode();
    bytes[4] = preset;
    bytes[5..7].copy_from_slice(&29u16.to_le_bytes());
    bytes.truncate(7 + 29 * 4);
    bytes
}
#[test]
fn legacy_preferences_preserve_controls_and_explicit_shadow_choices() {
    let default = Settings::decode(&legacy(0)).unwrap();
    assert_eq!(default.get(Setting::Sensitivity), 125);
    assert_eq!(default.get(Setting::ShadowResolution), 2048);
    assert_eq!(default.get(Setting::ViewTime), 0);
    for preset in [1, 2] {
        let settings = Settings::decode(&legacy(preset)).unwrap();
        assert_eq!(settings.get(Setting::ShadowResolution), 4096);
        assert_eq!(settings.get(Setting::Sensitivity), 125);
    }
}
#[test]
fn new_personal_time_round_trips_and_unknown_counts_are_rejected() {
    let mut settings = Settings::default();
    settings.set(Setting::ViewTime, 4).unwrap();
    let decoded = Settings::decode(&settings.encode()).unwrap();
    assert_eq!(decoded.get(Setting::ViewTime), 4);
    assert_eq!(decoded.preset, Preset::Custom);
    let mut bytes = settings.encode();
    bytes[5..7].copy_from_slice(&31u16.to_le_bytes());
    assert!(Settings::decode(&bytes).is_err());
    for length in 0..7 {
        assert!(Settings::decode(&bytes[..length]).is_err());
    }
}

#[test]
fn measured_default_lod_target_migrates_without_changing_explicit_quality() {
    assert_eq!(Settings::default().get(Setting::LodError), 32);
    for count in [29u16, 30] {
        for preset in [0u8, 1, 2] {
            let mut settings = Settings::default();
            settings.set(Setting::LodError, 4).unwrap();
            let mut bytes = settings.encode();
            bytes[4] = preset;
            bytes[5..7].copy_from_slice(&count.to_le_bytes());
            bytes.truncate(7 + usize::from(count) * 4);
            let restored = Settings::decode(&bytes).unwrap();
            assert_eq!(
                restored.get(Setting::LodError),
                if preset == 0 { 32 } else { 4 }
            );
        }
    }
    let mut heavy = Settings::default();
    heavy.apply_preset(Preset::Heavy);
    assert_eq!(heavy.get(Setting::LodError), 4);
}

#[test]
fn visual_choices_append_without_reindexing_and_preserve_legacy_values() {
    for count in [29u16, 30] {
        let mut settings = Settings::default();
        settings.set(Setting::Master, 65).unwrap();
        settings.set(Setting::ViewTime, 4).unwrap();
        let mut bytes = settings.encode();
        bytes[5..7].copy_from_slice(&count.to_le_bytes());
        bytes.truncate(7 + usize::from(count) * 4);
        let restored = Settings::decode(&bytes).unwrap();
        assert_eq!(restored.get(Setting::Master), 65);
        assert_eq!(
            restored.get(Setting::ViewTime),
            if count == 30 { 4 } else { 0 }
        );
        assert_eq!(restored.get(Setting::ExposureMode), 0);
        assert_eq!(restored.get(Setting::SkyStyle), 0);
    }
    let mut settings = Settings::default();
    settings.set(Setting::ExposureMode, 1).unwrap();
    settings.set(Setting::SkyStyle, 2).unwrap();
    assert_eq!(Settings::decode(&settings.encode()).unwrap(), settings);
    assert_eq!(Setting::SkyStyle as u32, 31);
    let mask = 1u32 << Setting::SkyStyle as u32;
    assert_eq!(mask, 0x80000000);
    assert!(settings.set(Setting::ExposureMode, 2).is_err());
    assert!(settings.set(Setting::SkyStyle, 3).is_err());
}
