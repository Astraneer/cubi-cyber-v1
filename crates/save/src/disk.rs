use crate::*;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const WAL_MAGIC: &[u8; 8] = b"CUBIWAL!";
const SNAP_MAGIC: &[u8; 8] = b"CUBISNAP";
const HEADER: usize = 82;

pub struct Recovery {
    pub snapshot: Option<Snapshot>,
    pub batches: Vec<(u64, Batch)>,
    pub truncated_tail_bytes: u64,
}

pub struct Store {
    directory: PathBuf,
    _lock: File,
    wal: Option<File>,
    last_sequence: u64,
    last_transaction: u64,
    last_tick: Tick,
    poisoned: bool,
}

fn open_private(path: &Path, truncate: bool, create_new: bool) -> Result<File, SaveError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if create_new {
        options.create_new(true);
    } else {
        options.create(true).truncate(truncate);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn sync_directory(path: &Path) -> Result<(), SaveError> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    // Windows replacements below use MOVEFILE_WRITE_THROUGH explicitly.
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn replace(source: &Path, destination: &Path) -> Result<(), SaveError> {
    #[cfg(not(windows))]
    {
        std::fs::rename(source, destination)?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "Kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
        }
        let mut from: Vec<u16> = source.as_os_str().encode_wide().collect();
        let mut to: Vec<u16> = destination.as_os_str().encode_wide().collect();
        if from.contains(&0) || to.contains(&0) {
            return Err(SaveError::Invalid("NUL in save path"));
        }
        from.push(0);
        to.push(0);
        // SAFETY: Both UTF16 buffers are NUL-terminated, live for this call, and
        // include no interior NUL. REPLACE_EXISTING|WRITE_THROUGH requests an
        // atomic same-volume replacement completed on disk before returning.
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0x1 | 0x8) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    sync_directory(
        destination
            .parent()
            .ok_or(SaveError::Invalid("save parent"))?,
    )
}

fn durable_copy(source: &Path, temporary: &Path, destination: &Path) -> Result<(), SaveError> {
    let mut input = File::open(source)?;
    let mut output = open_private(temporary, true, false)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    drop(output);
    replace(temporary, destination)
}

fn wal_header(base: u64, snapshot_digest: [u8; 32]) -> Vec<u8> {
    let mut header = Vec::with_capacity(HEADER);
    header.extend(WAL_MAGIC);
    header.extend(FORMAT_VERSION.to_le_bytes());
    header.extend(base.to_le_bytes());
    header.extend(snapshot_digest);
    let digest = blake3::hash(&header);
    header.extend(digest.as_bytes());
    header
}

impl Store {
    pub fn open(directory: impl AsRef<Path>) -> Result<(Self, Recovery), SaveError> {
        let directory = directory.as_ref().to_path_buf();
        std::fs::create_dir_all(&directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        }
        let lock = open_private(&directory.join("writer.lock"), false, false)?;
        lock.try_lock().map_err(|_| SaveError::AlreadyOpen)?;
        let (mut snapshot, snapshot_digest) = read_snapshot(&directory.join("world.snapshot"))?;
        let mut snapshot_sequence = snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.record_sequence);
        let mut last_transaction = snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.world.version.0);
        let mut last_tick = snapshot
            .as_ref()
            .map_or(Tick(0), |snapshot| snapshot.server_tick);
        let wal_path = directory.join("world.wal");
        let exists = wal_path.exists();
        let mut wal = open_private(&wal_path, false, false)?;
        if !exists {
            wal.write_all(&wal_header(snapshot_sequence, snapshot_digest))?;
            wal.sync_all()?;
            sync_directory(&directory)?;
        }
        if wal.metadata()?.len() > MAX_WAL_BYTES {
            return Err(SaveError::Bounds("WAL bytes"));
        }
        wal.seek(SeekFrom::Start(0))?;
        let mut header = [0; HEADER];
        wal.read_exact(&mut header)?;
        if &header[..8] != WAL_MAGIC {
            return Err(SaveError::Invalid("WAL magic"));
        }
        let version = u16::from_le_bytes(header[8..10].try_into().expect("header"));
        if version != FORMAT_VERSION {
            return Err(SaveError::UnsupportedVersion(version));
        }
        if blake3::hash(&header[..50]).as_bytes() != &header[50..] {
            return Err(SaveError::Checksum);
        }
        let base = u64::from_le_bytes(header[10..18].try_into().expect("header"));
        let anchored = header[18..50] != [0; 32];
        if base > snapshot_sequence
            || (anchored && base == snapshot_sequence && header[18..50] != snapshot_digest)
        {
            // A metadata-only checkpoint can have the SAME record frontier as
            // its predecessor. A crash between replacements must retain the
            // previous matching snapshot rather than waive the anchor checksum.
            let previous_path = directory.join("world.snapshot.previous");
            let (previous, digest) = read_snapshot(&previous_path)?;
            let previous = previous
                .filter(|p| p.record_sequence == base)
                .ok_or(SaveError::MissingBase)?;
            if header[18..50] != digest {
                return Err(SaveError::MissingBase);
            }
            snapshot_sequence = previous.record_sequence;
            last_transaction = previous.world.version.0;
            last_tick = previous.server_tick;
            snapshot = Some(previous);
            durable_copy(
                &previous_path,
                &directory.join("world.snapshot.recover.next"),
                &directory.join("world.snapshot"),
            )?;
        }
        let mut sequence = base;
        let mut good_offset = HEADER as u64;
        let file_len = wal.metadata()?.len();
        let mut batches = Vec::new();
        while good_offset < file_len {
            let remaining = file_len - good_offset;
            if remaining < HEADER as u64 {
                break;
            }
            let mut header = [0u8; HEADER];
            wal.read_exact(&mut header)?;
            if &header[..4] != b"CWR!" {
                return Err(SaveError::Invalid("WAL record magic"));
            }
            let version = u16::from_le_bytes(header[4..6].try_into().expect("header"));
            if version != FORMAT_VERSION {
                return Err(SaveError::UnsupportedVersion(version));
            }
            if blake3::hash(&header[..50]).as_bytes() != &header[50..] {
                return Err(SaveError::Checksum);
            }
            let id = u64::from_le_bytes(header[6..14].try_into().expect("header"));
            let bytes = u32::from_le_bytes(header[14..18].try_into().expect("header")) as usize;
            if bytes > MAX_RECORD_BYTES {
                return Err(SaveError::Bounds("record bytes"));
            }
            if remaining < HEADER as u64 + bytes as u64 {
                break;
            }
            if id
                != sequence
                    .checked_add(1)
                    .ok_or(SaveError::Bounds("WAL sequence"))?
            {
                return Err(SaveError::Invalid("WAL sequence gap"));
            }
            let mut payload = vec![0; bytes];
            wal.read_exact(&mut payload)?;
            if blake3::hash(&payload).as_bytes() != &header[18..50] {
                return Err(SaveError::Checksum);
            }
            sequence = id;
            good_offset += HEADER as u64 + bytes as u64;
            if id > snapshot_sequence {
                let batch = decode_batch(&payload)?;
                validate_batch_order(&batch, &mut last_transaction, &mut last_tick)?;
                batches.push((id, batch));
                if batches.len() > 1_000_000 {
                    return Err(SaveError::Bounds("WAL records"));
                }
            }
        }
        if sequence < snapshot_sequence {
            return Err(SaveError::MissingBase);
        }
        let truncated_tail_bytes = file_len - good_offset;
        if truncated_tail_bytes != 0 {
            wal.set_len(good_offset)?;
            wal.sync_all()?;
        }
        wal.seek(SeekFrom::End(0))?;
        Ok((
            Self {
                directory,
                _lock: lock,
                wal: Some(wal),
                last_sequence: sequence,
                last_transaction,
                last_tick,
                poisoned: false,
            },
            Recovery {
                snapshot,
                batches,
                truncated_tail_bytes,
            },
        ))
    }

    pub fn last_sequence(&self) -> u64 {
        self.last_sequence
    }
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// A return of Ok is the durability boundary. Callers may acknowledge only
    /// after this return, and must stop publishing on any error.
    pub fn append(&mut self, batch: &Batch) -> Result<u64, SaveError> {
        if self.poisoned {
            return Err(SaveError::Poisoned);
        }
        let payload = encode_batch(batch)?;
        let mut transaction = self.last_transaction;
        let mut tick = self.last_tick;
        validate_batch_order(batch, &mut transaction, &mut tick)?;
        let sequence = self
            .last_sequence
            .checked_add(1)
            .ok_or(SaveError::Bounds("WAL sequence"))?;
        let mut header = Vec::with_capacity(HEADER);
        header.extend(b"CWR!");
        header.extend(FORMAT_VERSION.to_le_bytes());
        header.extend(sequence.to_le_bytes());
        header.extend((payload.len() as u32).to_le_bytes());
        header.extend(blake3::hash(&payload).as_bytes());
        header.extend(blake3::hash(&header).as_bytes());
        let wal = self.wal.as_mut().ok_or(SaveError::Poisoned)?;
        if wal
            .metadata()?
            .len()
            .saturating_add((header.len() + payload.len()) as u64)
            > MAX_WAL_BYTES
        {
            return Err(SaveError::Bounds("checkpoint required before WAL limit"));
        }
        if let Err(error) = wal
            .write_all(&header)
            .and_then(|_| wal.write_all(&payload))
            .and_then(|_| wal.sync_all())
        {
            self.poisoned = true;
            return Err(error.into());
        }
        self.last_sequence = sequence;
        self.last_transaction = transaction;
        self.last_tick = tick;
        Ok(sequence)
    }

    pub fn checkpoint(&mut self, snapshot: &Snapshot) -> Result<(), SaveError> {
        if self.poisoned {
            return Err(SaveError::Poisoned);
        }
        if snapshot.record_sequence != self.last_sequence
            || snapshot.world.version.0 != self.last_transaction
        {
            // A brand-new store may be initialized with a previously constructed
            // generation world before any accepted client operation exists.
            if self.last_sequence != 0
                || self.last_transaction != 0
                || snapshot.record_sequence != 0
            {
                return Err(SaveError::Invalid("checkpoint frontier mismatch"));
            }
        }
        let payload = encode_snapshot(snapshot)?;
        let digest = *blake3::hash(&payload).as_bytes();
        let current = self.directory.join("world.snapshot");
        if current.exists() {
            durable_copy(
                &current,
                &self.directory.join("world.snapshot.previous.next"),
                &self.directory.join("world.snapshot.previous"),
            )?;
        }
        let temporary = self.directory.join("world.snapshot.next");
        let mut file = open_private(&temporary, true, false)?;
        file.write_all(SNAP_MAGIC)?;
        file.write_all(&FORMAT_VERSION.to_le_bytes())?;
        file.write_all(&(payload.len() as u64).to_le_bytes())?;
        file.write_all(&digest)?;
        file.write_all(&payload)?;
        file.sync_all()?;
        drop(file);
        replace(&temporary, &self.directory.join("world.snapshot"))?;
        let temporary = self.directory.join("world.wal.next");
        let mut new_wal = open_private(&temporary, true, false)?;
        new_wal.write_all(&wal_header(self.last_sequence, digest))?;
        new_wal.sync_all()?;
        drop(new_wal);
        // The durable snapshot is installed before the old WAL is replaced. A
        // crash at either rename recovers from snapshot+oldtail or snapshot+newbase.
        self.wal.take();
        if let Err(error) = replace(&temporary, &self.directory.join("world.wal")) {
            self.poisoned = true;
            return Err(error);
        }
        let mut wal = open_private(&self.directory.join("world.wal"), false, false)?;
        wal.seek(SeekFrom::End(0))?;
        self.wal = Some(wal);
        self.last_transaction = snapshot.world.version.0;
        self.last_tick = snapshot.server_tick;
        Ok(())
    }

    /// The caller supplies OS entropy; this function neither logs nor transmits it.
    pub fn load_or_create_private_entropy(
        &self,
        fresh: impl FnOnce() -> Result<[u8; 32], SaveError>,
    ) -> Result<[u8; 32], SaveError> {
        let path = self.directory.join("generation.key");
        if path.exists() {
            let mut file = File::open(path)?;
            if file.metadata()?.len() != 32 {
                return Err(SaveError::Invalid("private entropy length"));
            }
            let mut bytes = [0; 32];
            file.read_exact(&mut bytes)?;
            return Ok(bytes);
        }
        if self.last_sequence != 0 || self.directory.join("world.snapshot").exists() {
            return Err(SaveError::Invalid(
                "restore missing private generation entropy backup",
            ));
        }
        let entropy = fresh()?;
        let mut file = open_private(&path, false, true)?;
        file.write_all(&entropy)?;
        file.sync_all()?;
        sync_directory(&self.directory)?;
        Ok(entropy)
    }
}

fn validate_batch_order(
    batch: &Batch,
    transaction: &mut u64,
    tick: &mut Tick,
) -> Result<(), SaveError> {
    if batch.tick < *tick {
        return Err(SaveError::Invalid("batch tick went backwards"));
    }
    for change in &batch.changes {
        if change.tx.0
            != transaction
                .checked_add(1)
                .ok_or(SaveError::Bounds("transaction ID"))?
            || change.tick > batch.tick
        {
            return Err(SaveError::Invalid("transaction order"));
        }
        *transaction = change.tx.0;
    }
    if batch
        .actions
        .iter()
        .any(|(_, reply)| matches!(reply.status, net::ActionStatus::Pending) || reply.id.0 == 0)
    {
        return Err(SaveError::Invalid("nonterminal action result"));
    }
    *tick = batch.tick;
    Ok(())
}

fn read_snapshot(path: &Path) -> Result<(Option<Snapshot>, [u8; 32]), SaveError> {
    if !path.exists() {
        return Ok((None, [0; 32]));
    }
    let mut file = File::open(path)?;
    if file.metadata()?.len() > MAX_SNAPSHOT_BYTES as u64 + 50 {
        return Err(SaveError::Bounds("snapshot bytes"));
    }
    let mut header = [0u8; 50];
    file.read_exact(&mut header)?;
    if &header[..8] != SNAP_MAGIC {
        return Err(SaveError::Invalid("snapshot magic"));
    }
    let version = u16::from_le_bytes(header[8..10].try_into().expect("header"));
    if version != FORMAT_VERSION {
        return Err(SaveError::UnsupportedVersion(version));
    }
    let len = u64::from_le_bytes(header[10..18].try_into().expect("header"));
    if len > MAX_SNAPSHOT_BYTES as u64 || len + 50 != file.metadata()?.len() {
        return Err(SaveError::Bounds("snapshot payload length"));
    }
    let mut payload = vec![0; len as usize];
    file.read_exact(&mut payload)?;
    let digest = *blake3::hash(&payload).as_bytes();
    if digest != header[18..50] {
        return Err(SaveError::Checksum);
    }
    Ok((Some(decode_snapshot(&payload)?), digest))
}
