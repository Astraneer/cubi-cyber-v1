//! Хранилище ключ-значение (design.md §2.5): ключ — строка из частей через «/» («meta», «chunk/-3,5», «player/вася»),
//! значение — байты целиком. Реализации: MemStorage (проверки) и FileStorage (сервер; только натив): ключ → файл в
//! папке мира, запись атомарная — временный файл, сброс на диск, rename поверх старого.

use std::collections::BTreeMap;
use std::io;

/// Частей в ключе не больше.
pub(crate) const MAX_PARTS: usize = 4;
/// Байт в части ключа не больше (в имени файла — до ×3 после кодирования, предел NTFS — 255).
pub(crate) const MAX_PART: usize = 80;

pub(crate) trait Storage {
    /// Значение по ключу; None — нет такого.
    fn get(&self, key: &str) -> io::Result<Option<Vec<u8>>>;
    /// Записать значение: читатель увидит старое или новое целиком, не смесь.
    fn put(&mut self, key: &str, value: &[u8]) -> io::Result<()>;
    /// Удалить (нет ключа — не ошибка).
    fn delete(&mut self, key: &str) -> io::Result<()>;
    /// Ключи, начинающиеся с prefix, по возрастанию.
    fn list(&self, prefix: &str) -> io::Result<Vec<String>>;
}

/// Ключ годен: 1…MAX_PARTS непустых частей через «/», в части до MAX_PART байт и нет управляющих символов.
pub(crate) fn check_key(key: &str) -> io::Result<()> {
    let bad = |m: &str| Err(io::Error::new(io::ErrorKind::InvalidInput, format!("ключ «{}»: {m}", key.escape_debug())));
    if key.split('/').count() > MAX_PARTS {
        return bad("больше 4 частей");
    }
    for p in key.split('/') {
        if p.is_empty() {
            return bad("пустая часть");
        }
        if p.len() > MAX_PART {
            return bad("часть длиннее 80 байт");
        }
        if p.chars().any(char::is_control) {
            return bad("управляющий символ");
        }
    }
    Ok(())
}

/// В памяти (проверки и, позже, зеркало клиента): упорядочено по ключу.
#[derive(Default)]
pub(crate) struct MemStorage {
    pub(crate) map: BTreeMap<String, Vec<u8>>,
}

impl Storage for MemStorage {
    fn get(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
        check_key(key)?;
        Ok(self.map.get(key).cloned())
    }
    fn put(&mut self, key: &str, value: &[u8]) -> io::Result<()> {
        check_key(key)?;
        self.map.insert(key.to_string(), value.to_vec());
        Ok(())
    }
    fn delete(&mut self, key: &str) -> io::Result<()> {
        check_key(key)?;
        self.map.remove(key);
        Ok(())
    }
    fn list(&self, prefix: &str) -> io::Result<Vec<String>> {
        Ok(self.map.range(prefix.to_string()..).take_while(|(k, _)| k.starts_with(prefix)).map(|(k, _)| k.clone()).collect())
    }
}

// ---------------------------------------------------------------- имена файлов

/// Символ части ключа, который идёт в имя файла как есть: латинские строчные, цифры, «-», «,», «_» и строчная кириллица
/// без «й» и «ё» (у них есть разложение NFD — ФС с нормализацией вернули бы другое имя).
fn raw_ok(c: char) -> bool {
    matches!(c, 'a'..='z' | '0'..='9' | '-' | ',' | '_') || (('а'..='я').contains(&c) && c != 'й')
}

/// Имена устройств Windows: файл с таким именем открыть нельзя.
fn reserved(p: &str) -> bool {
    matches!(p, "con" | "prn" | "aux" | "nul")
        || ((p.starts_with("com") || p.starts_with("lpt")) && p.len() == 4 && p.as_bytes()[3].is_ascii_digit())
}

/// Часть ключа → имя файла: годные символы как есть, остальные — %XX по байтам UTF-8 (заглавные шестнадцатеричные).
/// Точки в имени не бывает никогда: «..» → «%2E%2E» (путь не выходит из папки мира), «.tmp» — только у временных
/// файлов. Заглавные буквы тоже кодируются: разные ключи не совпадут на ФС без учёта регистра (Windows). У имени
/// устройства Windows («con», «nul», «com1»…) кодируется первая буква.
pub(crate) fn encode_part(p: &str) -> String {
    use std::fmt::Write;
    let dev = reserved(p);
    let mut out = String::with_capacity(p.len());
    for (i, c) in p.chars().enumerate() {
        if raw_ok(c) && !(i == 0 && dev) {
            out.push(c);
        } else {
            let mut b = [0u8; 4];
            for x in c.encode_utf8(&mut b).bytes() {
                let _ = write!(out, "%{x:02X}");
            }
        }
    }
    out
}

/// Имя файла → часть ключа; None — не наше имя (временный файл, чужой файл, не каноническая запись).
pub(crate) fn decode_part(name: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(name.len());
    let mut it = name.chars();
    while let Some(c) = it.next() {
        if c == '%' {
            let h = it.next()?.to_digit(16)?;
            let l = it.next()?.to_digit(16)?;
            bytes.push((h * 16 + l) as u8);
        } else if raw_ok(c) {
            let mut b = [0u8; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
        } else {
            return None;
        }
    }
    let s = String::from_utf8(bytes).ok()?;
    (!s.is_empty() && encode_part(&s) == name).then_some(s)
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(unused_imports)] // сервер (MP-2) и проверки
pub(crate) use file::FileStorage;

#[cfg(not(target_arch = "wasm32"))]
mod file {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    /// Папка мира: ключ «chunk/-3,5» → <папка>/chunk/-3,5, части — encode_part. Один писатель на папку (Clone — тот же
    /// каталог для чтения из потока симуляции, пока пишет поток записи: save/writer.rs).
    #[derive(Clone)]
    pub(crate) struct FileStorage {
        root: PathBuf,
    }

    impl FileStorage {
        /// Открыть (и создать) папку мира.
        pub(crate) fn open(root: impl Into<PathBuf>) -> io::Result<FileStorage> {
            let root = root.into();
            fs::create_dir_all(&root)?;
            Ok(FileStorage { root })
        }
        #[allow(dead_code)]
        pub(crate) fn root(&self) -> &Path {
            &self.root
        }
        /// Путь файла ключа — всегда внутри папки мира.
        pub(crate) fn path(&self, key: &str) -> io::Result<PathBuf> {
            check_key(key)?;
            let mut p = self.root.clone();
            for part in key.split('/') {
                p.push(encode_part(part));
            }
            Ok(p)
        }
    }

    /// Временный файл записи рядом с целевым (точка бывает только здесь — list такие имена не видит).
    fn tmp_of(path: &Path) -> PathBuf {
        let mut s = path.as_os_str().to_owned();
        s.push(".tmp");
        PathBuf::from(s)
    }

    /// Сбросить запись каталога (rename) на диск: на Linux без этого после сбоя питания файл может «откатиться».
    #[cfg(unix)]
    fn sync_dir(d: &Path) -> io::Result<()> {
        fs::File::open(d)?.sync_all()
    }
    #[cfg(not(unix))]
    fn sync_dir(_: &Path) -> io::Result<()> {
        Ok(())
    }

    fn not_found_ok<T: Default>(r: io::Result<T>) -> io::Result<T> {
        match r {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(T::default()),
            r => r,
        }
    }

    impl Storage for FileStorage {
        fn get(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
            match fs::read(self.path(key)?) {
                Ok(v) => Ok(Some(v)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        }
        fn put(&mut self, key: &str, value: &[u8]) -> io::Result<()> {
            let path = self.path(key)?;
            let dir = path.parent().unwrap_or(&self.root).to_path_buf();
            fs::create_dir_all(&dir)?;
            let tmp = tmp_of(&path);
            {
                let mut f = fs::File::create(&tmp)?;
                f.write_all(value)?;
                f.sync_all()?; // байты на диске до rename: после сбоя — старый файл или новый целиком
            }
            fs::rename(&tmp, &path)?; // замена поверх старого — атомарная (на Windows — MoveFileEx с заменой)
            sync_dir(&dir)
        }
        fn delete(&mut self, key: &str) -> io::Result<()> {
            not_found_ok(fs::remove_file(self.path(key)?))
        }
        fn list(&self, prefix: &str) -> io::Result<Vec<String>> {
            // Полные части префикса — сразу папкой, хвост — обходом и сравнением.
            let (dir_key, depth) = match prefix.rfind('/') {
                Some(i) => (&prefix[..i], prefix[..i].split('/').count()),
                None => ("", 0),
            };
            let mut dir = self.root.clone();
            let mut base = String::new();
            if !dir_key.is_empty() {
                if check_key(dir_key).is_err() {
                    return Ok(Vec::new()); // таких ключей не бывает
                }
                for part in dir_key.split('/') {
                    dir.push(encode_part(part));
                }
                base = format!("{dir_key}/");
            }
            let mut out = Vec::new();
            walk(&dir, &base, MAX_PARTS - depth, &mut out)?;
            out.retain(|k| k.starts_with(prefix));
            out.sort();
            Ok(out)
        }
    }

    /// Ключи файлов под dir (не глубже depth частей); временные и чужие имена пропускаются.
    fn walk(dir: &Path, base: &str, depth: usize, out: &mut Vec<String>) -> io::Result<()> {
        if depth == 0 {
            return Ok(());
        }
        let rd = match fs::read_dir(dir) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        for ent in rd {
            let ent = ent?;
            let Some(part) = ent.file_name().to_str().and_then(decode_part) else { continue };
            let key = format!("{base}{part}");
            let ft = ent.file_type()?;
            if ft.is_dir() {
                walk(&ent.path(), &format!("{key}/"), depth - 1, out)?;
            } else if ft.is_file() {
                out.push(key);
            }
        }
        Ok(())
    }
}
