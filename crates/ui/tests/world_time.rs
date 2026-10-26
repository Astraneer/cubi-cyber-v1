use ui::{
    app::{PersonalTime, UiApp},
    settings::Setting,
};

#[test]
fn physical_sky_choice_survives_preferences_without_authority_intent_or_other_settings_changes() {
    let mut app = UiApp::new().unwrap();
    app.settings.set(Setting::Master, 35).unwrap();
    let initial = app.encode_preferences();
    for (value, expected) in [(1, PersonalTime::Day), (2, PersonalTime::Night)] {
        assert!(app.set_world_time(value));
        assert!(app.take_intent().is_none());
        let mut restored = UiApp::new().unwrap();
        restored
            .load_preferences(&app.encode_preferences())
            .unwrap();
        assert_eq!(restored.personal_time, expected);
        assert_eq!(restored.settings.get(Setting::ViewTime), value);
        assert_eq!(restored.settings.get(Setting::Master), 35);
        assert_eq!(restored.settings.get(Setting::Msaa), 4);
        assert!(!app.set_world_time(value));
    }
    let selected = app.encode_preferences();
    for invalid in [-1, 0, 3, 100] {
        assert!(!app.set_world_time(invalid));
        assert_eq!(app.encode_preferences(), selected);
    }
    assert_ne!(selected, initial);
}
