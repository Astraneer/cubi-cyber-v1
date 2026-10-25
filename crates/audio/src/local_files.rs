//! Native local-only file IO. User-selected bytes never enter a network type.
//! Run directory/archive preparation on a loading/background task, not a frame.
use crate::resources::{
    ImportError, ImportLimits, ImportSummary, LocalResources, minecraft_sound_objects,
};
use std::{
    io::Read,
    path::{Path, PathBuf},
};
fn safe_file(path: &Path, limit: usize) -> Result<Vec<u8>, ImportError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| ImportError::Path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(ImportError::Limit);
    }
    let file = std::fs::File::open(path).map_err(|_| ImportError::Path)?;
    let mut bytes = Vec::with_capacity((metadata.len() as usize).min(1024 * 1024));
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ImportError::Path)?;
    if bytes.len() > limit {
        return Err(ImportError::Limit);
    }
    Ok(bytes)
}
fn checked_child(root: &Path, relative: &str) -> Result<PathBuf, ImportError> {
    if relative
        .split('/')
        .any(|s| s.is_empty() || s == ".." || s == ".")
    {
        return Err(ImportError::Path);
    }
    let mut path = root.to_owned();
    for segment in relative.split('/') {
        path.push(segment);
        let metadata = std::fs::symlink_metadata(&path).map_err(|_| ImportError::Path)?;
        if metadata.file_type().is_symlink() {
            return Err(ImportError::Path);
        }
    }
    Ok(path)
}
pub fn import_pack_file(path: &Path) -> Result<LocalResources, ImportError> {
    let bytes = safe_file(path, ImportLimits::default().archive_bytes)?;
    let mut resources = LocalResources::new(ImportLimits::default());
    resources.import_zip(&bytes)?;
    Ok(resources)
}
pub fn import_minecraft_directory(root: &Path) -> Result<LocalResources, ImportError> {
    let metadata = std::fs::symlink_metadata(root).map_err(|_| ImportError::Path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ImportError::Path);
    }
    if root.to_string_lossy().starts_with("\\\\") || root.to_string_lossy().starts_with("//") {
        return Err(ImportError::Path);
    }
    let index_dir = checked_child(root, "assets/indexes")?;
    let mut indexes = Vec::new();
    for entry in std::fs::read_dir(index_dir).map_err(|_| ImportError::Path)? {
        let entry = entry.map_err(|_| ImportError::Path)?;
        if entry.path().extension().is_some_and(|e| e == "json") {
            if indexes.len() >= 32 {
                return Err(ImportError::Limit);
            }
            let modified = entry
                .metadata()
                .map_err(|_| ImportError::Path)?
                .modified()
                .unwrap_or(std::time::UNIX_EPOCH);
            indexes.push((modified, entry.path()));
        }
    }
    indexes.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let (_, path) = indexes.into_iter().next().ok_or(ImportError::Index)?;
    let index = safe_file(&path, 4 * 1024 * 1024)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(ImportError::Path)?;
    let objects = minecraft_sound_objects(&index, ImportLimits::default().entries - 1)?;
    let mut files = vec![(format!("indexes/{name}"), index)];
    let mut total = files[0].1.len();
    for object in objects {
        if matches!(std::fs::symlink_metadata(root.join("assets").join(&object)), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        {
            continue;
        }
        let path = checked_child(root, &format!("assets/{object}"))?;
        let limit = ImportLimits::default()
            .entry_bytes
            .min(ImportLimits::default().total_bytes.saturating_sub(total));
        let bytes = safe_file(&path, limit)?;
        total += bytes.len();
        files.push((object, bytes));
    }
    let mut resources = LocalResources::new(ImportLimits::default());
    resources.import_directory(files)?;
    Ok(resources)
}
pub fn summary(resources: &LocalResources) -> ImportSummary {
    resources.summary()
}
#[cfg(target_os = "windows")]
pub async fn choose_pack() -> Result<Option<LocalResources>, ImportError> {
    let Some(file) = rfd::AsyncFileDialog::new()
        .add_filter("Minecraft resource pack", &["zip"])
        .pick_file()
        .await
    else {
        return Ok(None);
    };
    import_pack_file(file.path()).map(Some)
}
#[cfg(target_os = "windows")]
pub async fn choose_minecraft() -> Result<Option<LocalResources>, ImportError> {
    let Some(directory) = rfd::AsyncFileDialog::new()
        .set_title("Выберите свою папку .minecraft")
        .pick_folder()
        .await
    else {
        return Ok(None);
    };
    import_minecraft_directory(directory.path()).map(Some)
}
