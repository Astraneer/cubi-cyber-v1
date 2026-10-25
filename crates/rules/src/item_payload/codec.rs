use super::*;
const HEADER: usize = 16;
fn put_records(out: &mut Vec<u8>, records: &[Record]) -> Result<(), Error> {
    out.extend_from_slice(
        &(u32::try_from(records.len()).map_err(|_| Error::Bounds)?).to_le_bytes(),
    );
    for record in records {
        validate_record(record)?;
        out.extend_from_slice(&record.id.0.to_le_bytes());
        out.extend_from_slice(&(record.payload.bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&record.payload.bytes);
        if out.len() > MAX_PAYLOAD_BYTES {
            return Err(Error::Bounds);
        }
    }
    Ok(())
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let out = self
            .bytes
            .get(self.at..self.at + N)
            .ok_or(Error::Encoding)?
            .try_into()
            .map_err(|_| Error::Encoding)?;
        self.at += N;
        Ok(out)
    }
    fn records(&mut self, maximum: usize) -> Result<Vec<Record>, Error> {
        let n = u32::from_le_bytes(self.take()?) as usize;
        if n > maximum || n > self.bytes.len().saturating_sub(self.at) / 17 {
            return Err(Error::Bounds);
        }
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let id = PayloadId(u64::from_le_bytes(self.take()?));
            let n = u16::from_le_bytes(self.take()?) as usize;
            if n > MAX_RECORD_BYTES {
                return Err(Error::Bounds);
            }
            let bytes = self
                .bytes
                .get(self.at..self.at + n)
                .ok_or(Error::Encoding)?;
            self.at += n;
            let record = Record {
                id,
                payload: Payload::decode(bytes)?,
            };
            validate_record(&record)?;
            out.push(record);
        }
        if out.windows(2).any(|r| r[0].id >= r[1].id) {
            return Err(Error::Encoding);
        }
        Ok(out)
    }
}
impl Snapshot {
    pub fn encode(&self, config: Config) -> Result<Vec<u8>, Error> {
        Ledger::restore(config, self)?;
        let mut out = b"IPL1".to_vec();
        out.extend_from_slice(&self.revision.to_le_bytes());
        put_records(&mut out, &self.records)?;
        Ok(out)
    }
    pub fn decode(bytes: &[u8], config: Config) -> Result<Self, Error> {
        if bytes.len() < HEADER || bytes.len() > MAX_PAYLOAD_BYTES || &bytes[..4] != b"IPL1" {
            return Err(Error::Encoding);
        }
        Ledger::new(config)?;
        let mut read = Reader { bytes, at: 4 };
        let revision = u64::from_le_bytes(read.take()?);
        let records = read.records(config.records)?;
        if read.at != bytes.len() {
            return Err(Error::Encoding);
        }
        let out = Self { revision, records };
        Ledger::restore(config, &out)?;
        Ok(out)
    }
}
impl Delta {
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        if self.insertions.len() + self.removals.len() > MAX_DELTA_RECORDS {
            return Err(Error::Bounds);
        }
        let mut out = b"IPD1".to_vec();
        out.extend_from_slice(&self.revision.to_le_bytes());
        out.extend_from_slice(&self.next_revision.to_le_bytes());
        put_records(&mut out, &self.insertions)?;
        put_records(&mut out, &self.removals)?;
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < 28 || bytes.len() > MAX_PAYLOAD_BYTES || &bytes[..4] != b"IPD1" {
            return Err(Error::Encoding);
        }
        let mut read = Reader { bytes, at: 4 };
        let revision = u64::from_le_bytes(read.take()?);
        let next_revision = u64::from_le_bytes(read.take()?);
        if revision.checked_add(1) != Some(next_revision) {
            return Err(Error::Stale);
        }
        let insertions = read.records(MAX_DELTA_RECORDS)?;
        let removals = read.records(MAX_DELTA_RECORDS.saturating_sub(insertions.len()))?;
        if read.at != bytes.len() {
            return Err(Error::Encoding);
        }
        let inserted: BTreeSet<_> = insertions.iter().map(|r| r.id).collect();
        if removals.iter().any(|r| inserted.contains(&r.id)) {
            return Err(Error::Encoding);
        }
        Ok(Self {
            revision,
            next_revision,
            insertions,
            removals,
        })
    }
}
