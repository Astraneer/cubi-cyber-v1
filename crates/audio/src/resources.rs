//! Local-only user resource store. This module has no transport, telemetry or
//! serialization API; neither imported bytes nor names become network messages.
use sha1::{Digest, Sha1};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read},
};
#[derive(Clone, Copy, Debug)]
pub struct ImportLimits {
    pub archive_bytes: usize,
    pub entries: usize,
    pub entry_bytes: usize,
    pub total_bytes: usize,
    pub compression_ratio: u64,
}
impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            archive_bytes: 64 * 1024 * 1024,
            entries: 4096,
            entry_bytes: 16 * 1024 * 1024,
            total_bytes: 256 * 1024 * 1024,
            compression_ratio: 200,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportError {
    InvalidArchive,
    Path,
    Duplicate,
    Unsupported,
    Limit,
    Checksum,
    Index,
}
impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "local import: {self:?}")
    }
}
impl std::error::Error for ImportError {}
pub struct LocalResources {
    files: BTreeMap<String, Vec<u8>>,
    bytes: usize,
    limits: ImportLimits,
    revision: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub files: usize,
    pub bytes: usize,
    pub sounds: usize,
    pub textures: usize,
    pub notices: usize,
    pub revision: u64,
}
pub struct LabPbr<'a> {
    pub albedo: Option<&'a [u8]>,
    pub normal: Option<&'a [u8]>,
    pub specular: Option<&'a [u8]>,
}
impl LocalResources {
    pub fn new(limits: ImportLimits) -> Self {
        Self {
            files: BTreeMap::new(),
            bytes: 0,
            limits,
            revision: 0,
        }
    }
    pub fn summary(&self) -> ImportSummary {
        ImportSummary {
            files: self.files.len(),
            bytes: self.bytes,
            sounds: self
                .files
                .keys()
                .filter(|p| p.ends_with(".ogg") || p.starts_with("objects/"))
                .count(),
            textures: self.files.keys().filter(|p| p.ends_with(".png")).count(),
            notices: self.files.keys().filter(|p| is_notice(p)).count(),
            revision: self.revision,
        }
    }
    pub fn get_local(&self, name: &str) -> Option<&[u8]> {
        self.files.get(name).map(Vec::as_slice)
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Resolve a local resource-pack sound declaration, including bounded event
    /// aliases. No URL is accepted and no missing asset triggers a download.
    pub fn sound_for_event(
        &self,
        event: &str,
        variant: u64,
        index: Option<&str>,
    ) -> Result<Option<&[u8]>, ImportError> {
        self.sound_event_inner(event, variant, index, 0)
    }
    fn sound_event_inner(
        &self,
        event: &str,
        variant: u64,
        index: Option<&str>,
        depth: usize,
    ) -> Result<Option<&[u8]>, ImportError> {
        if depth >= 8 {
            return Err(ImportError::Index);
        }
        let (namespace, event) = event.split_once(':').unwrap_or(("minecraft", event));
        if namespace.is_empty()
            || !namespace
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.-".contains(&c))
            || event.is_empty()
            || !event
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_./-".contains(&c))
        {
            return Err(ImportError::Path);
        }
        let declaration = format!("assets/{namespace}/sounds.json");
        if let Some(data) = self.get_local(&declaration) {
            let value: serde_json::Value =
                serde_json::from_slice(data).map_err(|_| ImportError::Index)?;
            if let Some(sounds) = value
                .get(event)
                .and_then(|event| event.get("sounds"))
                .and_then(serde_json::Value::as_array)
            {
                if sounds.is_empty() {
                    return Ok(None);
                }
                if sounds.len() > 1024 {
                    return Err(ImportError::Limit);
                }
                let selected = &sounds[variant as usize % sounds.len()];
                let name = selected
                    .as_str()
                    .or_else(|| selected.get("name").and_then(serde_json::Value::as_str))
                    .ok_or(ImportError::Index)?;
                if selected.get("type").and_then(serde_json::Value::as_str) == Some("event") {
                    return self.sound_event_inner(name, variant, index, depth + 1);
                }
                let (namespace, path) = name.split_once(':').unwrap_or((namespace, name));
                validate_path(namespace)?;
                validate_path(path)?;
                let suffix = if path.ends_with(".ogg") { "" } else { ".ogg" };
                let path = format!("assets/{namespace}/sounds/{path}{suffix}");
                if let Some(data) = self.get_local(&path) {
                    return Ok(Some(data));
                }
                if let Some(index) = index {
                    let logical = path.strip_prefix("assets/").unwrap();
                    if namespace == "minecraft" {
                        return self.minecraft_sound(index, logical);
                    }
                }
                return Ok(None);
            }
        }
        // Conventional vanilla file stems are facts of the local index. We
        // resolve only files that actually exist and pass their stored SHA-1.
        let stem = vanilla_stem(event);
        if let Some(stem) = stem {
            let mut candidates = Vec::new();
            for number in 0..=8 {
                let suffix = if number == 0 {
                    String::new()
                } else {
                    number.to_string()
                };
                let logical = format!("minecraft/sounds/{stem}{suffix}.ogg");
                let direct = format!("assets/{logical}");
                if let Some(data) = self.get_local(&direct) {
                    candidates.push(data);
                } else if let Some(index) = index
                    && let Some(data) = self.minecraft_sound(index, &logical)?
                {
                    candidates.push(data);
                }
            }
            if !candidates.is_empty() {
                return Ok(Some(candidates[variant as usize % candidates.len()]));
            }
        }
        Ok(None)
    }
    pub fn labpbr(&self, namespace: &str, texture: &str) -> Result<LabPbr<'_>, ImportError> {
        let base = format!("assets/{namespace}/textures/{texture}");
        validate_path(&(base.clone() + ".png"))?;
        Ok(LabPbr {
            albedo: self.get_local(&(base.clone() + ".png")),
            normal: self.get_local(&(base.clone() + "_n.png")),
            specular: self.get_local(&(base + "_s.png")),
        })
    }
    /// Archive replacement is atomic: a malformed last entry preserves the old
    /// resources, names, quotas and revision. ZIP64/encryption are unsupported.
    pub fn import_zip(&mut self, bytes: &[u8]) -> Result<ImportSummary, ImportError> {
        preflight_zip(bytes, self.limits)?;
        let mut zip =
            zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| ImportError::InvalidArchive)?;
        if zip.len() > self.limits.entries {
            return Err(ImportError::Limit);
        }
        let mut files = BTreeMap::new();
        let mut keys = BTreeSet::new();
        let mut total = 0usize;
        for index in 0..zip.len() {
            let mut entry = zip
                .by_index(index)
                .map_err(|_| ImportError::InvalidArchive)?;
            let name = entry.name().to_owned();
            validate_path(&name)?;
            if !keys.insert(name.to_ascii_lowercase()) {
                return Err(ImportError::Duplicate);
            }
            if entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                return Err(ImportError::Path);
            }
            if entry.is_dir() {
                continue;
            }
            if !allowed(&name) {
                return Err(ImportError::Unsupported);
            }
            if entry.size() > self.limits.entry_bytes as u64
                || entry.size() > self.limits.total_bytes.saturating_sub(total) as u64
                || entry.size()
                    > entry
                        .compressed_size()
                        .max(1)
                        .saturating_mul(self.limits.compression_ratio)
            {
                return Err(ImportError::Limit);
            }
            let expected = entry.size() as usize;
            let mut data = Vec::with_capacity(expected.min(1024 * 1024));
            entry
                .by_ref()
                .take((self.limits.entry_bytes + 1) as u64)
                .read_to_end(&mut data)
                .map_err(|_| ImportError::Checksum)?;
            if data.len() != expected || data.len() > self.limits.entry_bytes {
                return Err(ImportError::InvalidArchive);
            }
            total = total.checked_add(data.len()).ok_or(ImportError::Limit)?;
            files.insert(name, data);
        }
        let revision = self.revision.checked_add(1).ok_or(ImportError::Limit)?;
        self.files = files;
        self.bytes = total;
        self.revision = revision;
        Ok(self.summary())
    }
    /// A directory picker supplies only requested files to this local store.
    /// Root .minecraft files (credentials, saves, logs) are rejected by path.
    pub fn import_directory(
        &mut self,
        files: Vec<(String, Vec<u8>)>,
    ) -> Result<ImportSummary, ImportError> {
        if files.len() > self.limits.entries {
            return Err(ImportError::Limit);
        }
        let mut staged = BTreeMap::new();
        let mut keys = BTreeSet::new();
        let mut total = 0usize;
        for (name, data) in files {
            let name = normalize_minecraft_path(&name)?;
            validate_path(&name)?;
            if !allowed(&name) {
                return Err(ImportError::Unsupported);
            }
            if !keys.insert(name.to_ascii_lowercase()) {
                return Err(ImportError::Duplicate);
            }
            if data.len() > self.limits.entry_bytes {
                return Err(ImportError::Limit);
            }
            total = total
                .checked_add(data.len())
                .filter(|n| *n <= self.limits.total_bytes)
                .ok_or(ImportError::Limit)?;
            staged.insert(name, data);
        }
        let revision = self.revision.checked_add(1).ok_or(ImportError::Limit)?;
        self.files = staged;
        self.bytes = total;
        self.revision = revision;
        Ok(self.summary())
    }
    /// Resolve Mojang's local assets/indexes JSON against local content-addressed
    /// objects. SHA-1 validates an existing local object; it is not authentication.
    pub fn minecraft_sound(
        &self,
        index: &str,
        logical_sound: &str,
    ) -> Result<Option<&[u8]>, ImportError> {
        validate_path(logical_sound)?;
        if !logical_sound.starts_with("minecraft/sounds/") || !logical_sound.ends_with(".ogg") {
            return Err(ImportError::Path);
        }
        let index = self.get_local(index).ok_or(ImportError::Index)?;
        if index.len() > 4 * 1024 * 1024 {
            return Err(ImportError::Limit);
        }
        let value: serde_json::Value =
            serde_json::from_slice(index).map_err(|_| ImportError::Index)?;
        let Some(object) = value.get("objects").and_then(|v| v.get(logical_sound)) else {
            return Ok(None);
        };
        let hash = object
            .get("hash")
            .and_then(serde_json::Value::as_str)
            .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or(ImportError::Index)?;
        let path = format!("objects/{}/{}", &hash[..2], hash);
        let Some(bytes) = self.get_local(&path) else {
            return Ok(None);
        };
        let actual = format!("{:x}", Sha1::digest(bytes));
        if actual != hash
            || object.get("size").and_then(serde_json::Value::as_u64) != Some(bytes.len() as u64)
        {
            return Err(ImportError::Checksum);
        }
        Ok(Some(bytes))
    }
}
fn validate_path(path: &str) -> Result<(), ImportError> {
    if path.is_empty()
        || !path.is_ascii()
        || path.len() > 512
        || path.starts_with('/')
        || path.contains(['\\', ':', '\0'])
        || path.chars().any(char::is_control)
        || path
            .trim_end_matches('/')
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(ImportError::Path);
    }
    Ok(())
}
fn normalize_minecraft_path(path: &str) -> Result<String, ImportError> {
    validate_path(path)?;
    let path = path
        .strip_prefix(".minecraft/")
        .or_else(|| path.split_once("/assets/").map(|(_, tail)| tail))
        .unwrap_or(path);
    let path = path
        .strip_prefix("assets/")
        .filter(|p| p.starts_with("indexes/") || p.starts_with("objects/"))
        .unwrap_or(path);
    Ok(path.to_owned())
}
/// Directory-picker preflight. Non-asset files are filtered before reading them;
/// credentials, saves, logs and versions are not read into this subsystem.
pub fn selected_minecraft_asset(path: &str) -> Result<Option<String>, ImportError> {
    validate_path(path)?;
    let tail = if let Some((_, tail)) = path.split_once("/assets/") {
        tail
    } else {
        path.strip_prefix("assets/").unwrap_or(path)
    };
    if ((tail.starts_with("indexes/") && tail.ends_with(".json")) || tail.starts_with("objects/"))
        && allowed(tail)
    {
        return Ok(Some(tail.to_owned()));
    }
    Ok(None)
}
/// Read a chosen local index and return only sound object paths. Object names,
/// indexes and the plan remain local; they never enter a protocol message.
pub fn minecraft_sound_objects(index: &[u8], max: usize) -> Result<Vec<String>, ImportError> {
    if index.len() > 4 * 1024 * 1024 {
        return Err(ImportError::Limit);
    }
    let value: serde_json::Value = serde_json::from_slice(index).map_err(|_| ImportError::Index)?;
    let objects = value
        .get("objects")
        .and_then(serde_json::Value::as_object)
        .ok_or(ImportError::Index)?;
    let mut paths = BTreeSet::new();
    for (name, metadata) in objects {
        if !name.starts_with("minecraft/sounds/") || !name.ends_with(".ogg") {
            continue;
        }
        validate_path(name)?;
        let hash = metadata
            .get("hash")
            .and_then(serde_json::Value::as_str)
            .filter(|s| s.len() == 40 && s.bytes().all(|c| c.is_ascii_hexdigit()))
            .ok_or(ImportError::Index)?;
        paths.insert(format!("objects/{}/{}", &hash[..2], hash));
        if paths.len() > max {
            return Err(ImportError::Limit);
        }
    }
    Ok(paths.into_iter().collect())
}
fn vanilla_stem(event: &str) -> Option<String> {
    let parts: Vec<_> = event.split('.').collect();
    if parts.len() == 3
        && parts[0] == "block"
        && ["stone", "wood", "gravel", "grass", "sand", "snow", "cloth"].contains(&parts[1])
        && ["dig", "step"].contains(&parts[2])
    {
        return Some(format!("{}/{}", parts[2], parts[1]));
    }
    Some(
        match event {
            "ui.click" => "random/click",
            "item.pickup" => "random/pop",
            "player.hurt" => "damage/hit",
            "player.burp" => "random/burp",
            "player.small_fall" => "damage/fallsmall",
            "player.big_fall" => "damage/fallbig",
            "block.glass.break" => "random/glass",
            "block.ladder.step" => "step/ladder",
            "bucket.fill" => "item/bucket/fill",
            "bucket.empty" => "item/bucket/empty",
            "entity.pig.ambient" => "mob/pig/say",
            "entity.pig.hurt" => "mob/pig/say",
            "entity.pig.death" => "mob/pig/death",
            "entity.pig.step" => "mob/pig/step",
            "entity.cow.ambient" => "mob/cow/say",
            "entity.cow.hurt" => "mob/cow/hurt",
            "entity.cow.step" => "mob/cow/step",
            "entity.sheep.ambient" => "mob/sheep/say",
            "entity.sheep.step" => "mob/sheep/step",
            "entity.sheep.shear" => "mob/sheep/shear",
            "entity.chicken.ambient" => "mob/chicken/say",
            "entity.chicken.hurt" => "mob/chicken/hurt",
            "entity.chicken.step" => "mob/chicken/step",
            "entity.chicken.egg" => "mob/chicken/plop",
            "entity.zombie.ambient" => "mob/zombie/say",
            "entity.zombie.hurt" => "mob/zombie/hurt",
            "entity.zombie.death" => "mob/zombie/death",
            "entity.zombie.step" => "mob/zombie/step",
            _ => return None,
        }
        .to_owned(),
    )
}
fn is_notice(path: &str) -> bool {
    let upper = path.to_ascii_uppercase();
    upper.contains("LICENSE")
        || upper.contains("CREDITS")
        || upper.contains("COPYING")
        || upper.contains("NOTICE")
        || upper.ends_with("README.MD")
        || upper.ends_with("README.TXT")
}
fn allowed(path: &str) -> bool {
    if path == "pack.mcmeta" || path == "pack.png" || is_notice(path) {
        return true;
    }
    if path.starts_with("assets/") {
        return path.ends_with(".ogg")
            || path.ends_with(".png")
            || path.ends_with(".json")
            || path.ends_with(".mcmeta");
    }
    if path.starts_with("indexes/") {
        return path.ends_with(".json");
    }
    let pieces: Vec<_> = path.split('/').collect();
    pieces.len() == 3
        && pieces[0] == "objects"
        && pieces[1].len() == 2
        && pieces[2].len() == 40
        && pieces[2].starts_with(pieces[1])
        && pieces[2].bytes().all(|b| b.is_ascii_hexdigit())
}
fn preflight_zip(bytes: &[u8], limits: ImportLimits) -> Result<(), ImportError> {
    if bytes.len() > limits.archive_bytes || bytes.len() < 22 {
        return Err(ImportError::Limit);
    }
    let start = bytes.len().saturating_sub(65557);
    let offset = (start..=bytes.len() - 22)
        .rev()
        .find(|at| bytes[*at..*at + 4] == [0x50, 0x4b, 0x05, 0x06])
        .ok_or(ImportError::InvalidArchive)?;
    let word = |n| u16::from_le_bytes(bytes[offset + n..offset + n + 2].try_into().unwrap());
    let dword = |n| u32::from_le_bytes(bytes[offset + n..offset + n + 4].try_into().unwrap());
    if word(4) != 0
        || word(6) != 0
        || word(8) != word(10)
        || word(10) == u16::MAX
        || dword(12) == u32::MAX
        || dword(16) == u32::MAX
    {
        return Err(ImportError::Unsupported);
    }
    if usize::from(word(10)) > limits.entries
        || dword(12) as usize > limits.archive_bytes
        || offset + 22 + usize::from(word(20)) != bytes.len()
        || dword(16) as u64 + dword(12) as u64 > offset as u64
    {
        return Err(ImportError::Limit);
    }
    Ok(())
}
