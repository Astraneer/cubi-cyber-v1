use ui::settings::{COUNT, SCHEMA, Setting, Settings, pending_graphics_mask};

#[test]
fn retired_taa_values_survive_every_supported_storage_version_without_selecting_aa() {
    assert_eq!(COUNT, 32);
    for stored in [29, 30, COUNT] {
        for legacy in [0, 1] {
            for canonical in [1, 4] {
                let mut settings = Settings::default();
                settings.set(Setting::Msaa, canonical).unwrap();
                settings.set(Setting::Taa, legacy).unwrap();
                let mut bytes = settings.encode();
                bytes[5..7].copy_from_slice(&(stored as u16).to_le_bytes());
                bytes.truncate(7 + stored * 4);
                let restored = Settings::decode(&bytes).unwrap();
                assert_eq!(restored.get(Setting::Taa), legacy);
                assert_eq!(restored.get(Setting::Msaa), canonical);
                let again = restored.encode();
                let offset = 7 + Setting::Taa as usize * 4;
                assert_eq!(&again[offset..offset + 4], &legacy.to_le_bytes());
            }
        }
    }
}

#[test]
fn retirement_removes_pending_work_without_claiming_any_backend_receipt() {
    let retired = 1u32 << Setting::Taa as u32;
    assert!(Setting::Taa.is_retired());
    assert!(!Setting::Msaa.is_retired());
    assert_eq!(SCHEMA.iter().filter(|def| def.id.is_retired()).count(), 1);
    assert_eq!(pending_graphics_mask(0) & retired, 0);
    // No TAA receipt exists, but every active backend was acknowledged.
    let actual_receipts = u32::MAX & !retired;
    assert_eq!(actual_receipts & retired, 0);
    assert_eq!(pending_graphics_mask(actual_receipts), 0);
    let msaa = 1u32 << Setting::Msaa as u32;
    assert_eq!(pending_graphics_mask(actual_receipts & !msaa), msaa);
    assert!(!SCHEMA[Setting::Msaa as usize].domain.accepts(2));
}

#[test]
fn graphics_screen_keeps_one_canonical_antialiasing_control() {
    let atlas = ui::atlas::UiAtlas::load().unwrap();
    let mut app = ui::app::UiApp::new().unwrap();
    app.settings.set(Setting::Taa, 1).unwrap();
    app.open(ui::app::Screen::Settings(ui::settings::Section::Graphics));
    app.paint(&atlas, &mut ui::paint::PaintList::default(), 900, 900);
    assert!(app.hit(ui::app::Widget::Setting(Setting::Msaa)).is_some());
    assert!(app.hit(ui::app::Widget::Setting(Setting::Taa)).is_none());
    assert_eq!(app.settings.get(Setting::Taa), 1);
}
