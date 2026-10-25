#![cfg(not(target_arch = "wasm32"))]
use audio::local_files::import_minecraft_directory;
#[cfg(unix)]
use audio::resources::ImportError;
use sha1::{Digest, Sha1};
#[test]
fn native_directory_import_keeps_only_existing_local_sound_objects() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local")
        .join(format!("native-import-test-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    let sound = b"LOCAL_NATIVE_SOUND";
    let hash = format!("{:x}", Sha1::digest(sound));
    let object = format!("objects/{}/{}", &hash[..2], hash);
    std::fs::create_dir_all(root.join("assets/indexes")).unwrap();
    std::fs::create_dir_all(root.join("assets").join(&object).parent().unwrap()).unwrap();
    std::fs::write(root.join("assets").join(&object), sound).unwrap();
    std::fs::write(
        root.join("launcher_accounts.json"),
        b"PRIVATE_ACCOUNT_CANARY",
    )
    .unwrap();
    std::fs::write(root.join("assets/indexes/test.json"), format!("{{\"objects\":{{\"minecraft/sounds/test.ogg\":{{\"hash\":\"{hash}\",\"size\":{}}},\"minecraft/sounds/missing.ogg\":{{\"hash\":\"0000000000000000000000000000000000000000\",\"size\":1}},\"minecraft/textures/private.png\":{{\"hash\":\"1111111111111111111111111111111111111111\",\"size\":1}}}}}}", sound.len())).unwrap();
    let store = import_minecraft_directory(&root).unwrap();
    assert_eq!(store.summary().files, 2);
    assert_eq!(
        store
            .minecraft_sound("indexes/test.json", "minecraft/sounds/test.ogg")
            .unwrap(),
        Some(&sound[..])
    );
    assert!(store.get_local("launcher_accounts.json").is_none());
    #[cfg(unix)]
    {
        let linked = root.with_extension("link");
        if linked.symlink_metadata().is_ok() {
            std::fs::remove_file(&linked).unwrap();
        }
        std::os::unix::fs::symlink(&root, &linked).unwrap();
        assert!(matches!(
            import_minecraft_directory(&linked),
            Err(ImportError::Path)
        ));
        std::fs::remove_file(linked).unwrap();
        std::fs::remove_file(root.join("assets").join(&object)).unwrap();
        std::os::unix::fs::symlink(
            root.join("launcher_accounts.json"),
            root.join("assets").join(&object),
        )
        .unwrap();
        assert!(matches!(
            import_minecraft_directory(&root),
            Err(ImportError::Path)
        ));
    }
    std::fs::remove_dir_all(root).unwrap();
}
