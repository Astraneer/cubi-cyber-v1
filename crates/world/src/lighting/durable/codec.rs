use super::*;
struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], LightingError> {
        let (a, b) = self
            .0
            .split_at_checked(n)
            .ok_or(invalid("truncated continuation"))?;
        self.0 = b;
        Ok(a)
    }
    fn byte(&mut self) -> Result<u8, LightingError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, LightingError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("length"),
        ))
    }
    fn u64(&mut self) -> Result<u64, LightingError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("length"),
        ))
    }
    fn count(&mut self, maximum: usize) -> Result<usize, LightingError> {
        let count = self.u32()? as usize;
        if count > maximum {
            return Err(invalid("continuation count"));
        }
        Ok(count)
    }
    fn index(&mut self) -> Result<Option<usize>, LightingError> {
        Ok(match self.u32()? {
            u32::MAX => None,
            index => Some(index as usize),
        })
    }
    fn optional_u64(&mut self) -> Result<Option<u64>, LightingError> {
        let flag = self.byte()?;
        let value = self.u64()?;
        match (flag, value) {
            (0, 0) => Ok(None),
            (1, value) => Ok(Some(value)),
            _ => Err(invalid("continuation option")),
        }
    }
    fn priority(&mut self) -> Result<LightPriority, LightingError> {
        match self.byte()? {
            0 => Ok(LightPriority::Interactive),
            1 => Ok(LightPriority::Normal),
            2 => Ok(LightPriority::Background),
            _ => Err(invalid("priority")),
        }
    }
    fn position(&mut self) -> Result<BlockPos, LightingError> {
        Ok(BlockPos {
            space: SpaceId(self.u32()?),
            x: self.u32()? as i32,
            y: self.u32()? as i32,
            z: self.u32()? as i32,
        })
    }
    fn page(&mut self) -> Result<PageId, LightingError> {
        let p = self.position()?;
        Ok(PageId {
            space: p.space,
            x: p.x,
            y: p.y,
            z: p.z,
        })
    }
    fn seed(&mut self) -> Result<PageSeed, LightingError> {
        let page = self.page()?;
        let flags = self.byte()?;
        if flags > 7 {
            return Err(invalid("seed flags"));
        }
        let seed = PageSeed {
            page,
            whole: flags & 1 != 0,
            top: flags & 2 != 0,
            halo: flags & 4 != 0,
            next: self.u32()? as usize,
            queued_at: Tick(self.u64()?),
            priority: self.priority()?,
        };
        validate_seed(seed)?;
        Ok(seed)
    }
    fn seeds(&mut self) -> Result<Vec<PageSeed>, LightingError> {
        let n = self.count(65_536)?;
        if n > self.0.len() / 30 {
            return Err(invalid("seed bytes"));
        }
        let mut seeds = Vec::with_capacity(n);
        let mut previous = None;
        for _ in 0..n {
            let seed = self.seed()?;
            if previous.is_some_and(|old| old >= seed.page) {
                return Err(invalid("seed order"));
            }
            previous = Some(seed.page);
            seeds.push(seed);
        }
        Ok(seeds)
    }
    fn input(&mut self) -> Result<Arc<InputRecord>, LightingError> {
        let tx = self.u64()?;
        let n = self.count(65_536)?;
        if n > self.0.len() / 17 {
            return Err(invalid("input page bytes"));
        }
        let mut pages = Vec::with_capacity(n);
        for _ in 0..n {
            pages.push((self.page()?, self.byte()?));
        }
        let groups = self.count(65_536)?;
        let mut cells = Vec::new();
        let mut previous = None;
        for _ in 0..groups {
            let page = self.u32()?;
            if page as usize >= pages.len() || previous.is_some_and(|old| old >= page) {
                return Err(invalid("input group"));
            }
            previous = Some(page);
            let n = self.count(4096)?;
            if n == 0 || n > self.0.len() / 2 {
                return Err(invalid("input cell bytes"));
            }
            if cells.len() + n > MAX_INPUT_BYTES / 2 {
                return Err(invalid("input cell capacity"));
            }
            for _ in 0..n {
                let value = u16::from_le_bytes(self.take(2)?.try_into().expect("length"));
                if value > 8191 {
                    return Err(invalid("input cell bits"));
                }
                cells.push(page << 13 | u32::from(value));
            }
        }
        let input = Arc::new(InputRecord::new(tx, pages, cells));
        validate_input(&input)?;
        Ok(input)
    }
    fn inputs(&mut self) -> Result<Vec<Arc<InputRecord>>, LightingError> {
        let n = self.count(MAX_INPUT_BYTES / 16)?;
        if n > self.0.len() / 16 {
            return Err(invalid("input bytes"));
        }
        let mut result = Vec::with_capacity(n);
        let mut bytes = 0usize;
        for _ in 0..n {
            let input = self.input()?;
            bytes += input.encoded_len();
            if bytes > MAX_INPUT_BYTES {
                return Err(invalid("input capacity"));
            }
            result.push(input);
        }
        Ok(result)
    }
    fn record(&mut self) -> Result<Option<RecordCursor>, LightingError> {
        match self.byte()? {
            0 => Ok(None),
            1 => {
                let page = self.u32()? as usize;
                let cell = self.u32()? as usize;
                let neighbor = self.byte()? as usize;
                let interested = match self.byte()? {
                    0 => None,
                    1 => Some(false),
                    2 => Some(true),
                    _ => return Err(invalid("input interest")),
                };
                Ok(Some(RecordCursor {
                    page,
                    cell,
                    neighbor,
                    interested,
                    queued_at: Tick(self.u64()?),
                }))
            }
            _ => Err(invalid("record option")),
        }
    }
}
fn u32(out: &mut Vec<u8>, n: usize) -> Result<(), LightingError> {
    out.extend_from_slice(
        &u32::try_from(n)
            .map_err(|_| invalid("encoded count"))?
            .to_le_bytes(),
    );
    Ok(())
}
fn index(out: &mut Vec<u8>, n: Option<usize>) -> Result<(), LightingError> {
    match n {
        None => out.extend_from_slice(&u32::MAX.to_le_bytes()),
        Some(n) => u32(out, n)?,
    }
    Ok(())
}
fn optional_u64(out: &mut Vec<u8>, n: Option<u64>) {
    out.push(u8::from(n.is_some()));
    out.extend_from_slice(&n.unwrap_or(0).to_le_bytes());
}
fn position(out: &mut Vec<u8>, p: BlockPos) {
    out.extend_from_slice(&p.space.0.to_le_bytes());
    out.extend_from_slice(&p.x.to_le_bytes());
    out.extend_from_slice(&p.y.to_le_bytes());
    out.extend_from_slice(&p.z.to_le_bytes());
}
fn page(out: &mut Vec<u8>, p: PageId) {
    position(
        out,
        BlockPos {
            space: p.space,
            x: p.x,
            y: p.y,
            z: p.z,
        },
    );
}
fn seeds(out: &mut Vec<u8>, seeds: &[PageSeed]) -> Result<(), LightingError> {
    u32(out, seeds.len())?;
    for seed in seeds {
        page(out, seed.page);
        out.push(u8::from(seed.whole) | u8::from(seed.top) << 1 | u8::from(seed.halo) << 2);
        u32(out, seed.next)?;
        out.extend_from_slice(&seed.queued_at.0.to_le_bytes());
        out.push(seed.priority.index() as u8);
    }
    Ok(())
}
fn inputs(out: &mut Vec<u8>, inputs: &[Arc<InputRecord>]) -> Result<(), LightingError> {
    u32(out, inputs.len())?;
    for input in inputs {
        out.extend_from_slice(&input.tx.to_le_bytes());
        u32(out, input.pages.len())?;
        for &(p, flags) in &input.pages {
            page(out, p);
            out.push(flags);
        }
        let groups = input
            .cells
            .iter()
            .enumerate()
            .filter(|(i, cell)| *i == 0 || input.cells[*i - 1] >> 13 != **cell >> 13)
            .count();
        u32(out, groups)?;
        let mut start = 0;
        while start < input.cells.len() {
            let p = input.cells[start] >> 13;
            let mut end = start + 1;
            while end < input.cells.len() && input.cells[end] >> 13 == p {
                end += 1;
            }
            out.extend_from_slice(&p.to_le_bytes());
            u32(out, end - start)?;
            for &cell in &input.cells[start..end] {
                out.extend_from_slice(&((cell & 8191) as u16).to_le_bytes());
            }
            start = end;
        }
    }
    Ok(())
}
fn record(out: &mut Vec<u8>, cursor: Option<RecordCursor>) -> Result<(), LightingError> {
    out.push(u8::from(cursor.is_some()));
    if let Some(cursor) = cursor {
        u32(out, cursor.page)?;
        u32(out, cursor.cell)?;
        out.push(cursor.neighbor as u8);
        out.push(match cursor.interested {
            None => 0,
            Some(false) => 1,
            Some(true) => 2,
        });
        out.extend_from_slice(&cursor.queued_at.0.to_le_bytes());
    }
    Ok(())
}
fn finished(out: Vec<u8>) -> Result<Vec<u8>, LightingError> {
    if out.len() > MAX_BYTES {
        Err(invalid("continuation byte capacity"))
    } else {
        Ok(out)
    }
}
impl Snapshot {
    pub fn encode(&self) -> Result<Vec<u8>, LightingError> {
        let mut out = Vec::new();
        out.extend_from_slice(b"WLS1");
        out.extend_from_slice(&self.revision.to_le_bytes());
        u32(&mut out, self.limits.reserved_interactive_cells)?;
        u32(&mut out, self.limits.max_seed_pages)?;
        out.extend_from_slice(&self.limits.max_queue_age_ticks.to_le_bytes());
        optional_u64(&mut out, self.watermark);
        u32(&mut out, self.entries.len())?;
        u32(
            &mut out,
            self.entries.iter().filter(|e| e.is_some()).count(),
        )?;
        for (i, entry) in self.entries.iter().enumerate() {
            if let Some(entry) = entry {
                u32(&mut out, i)?;
                position(&mut out, entry.position);
                out.push(entry.priority.index() as u8);
                out.extend_from_slice(&entry.queued_at.0.to_le_bytes());
                index(&mut out, entry.previous)?;
                index(&mut out, entry.next)?;
            }
        }
        u32(&mut out, self.free.len())?;
        for &i in &self.free {
            u32(&mut out, i)?;
        }
        for chain in &self.chains {
            index(&mut out, chain.first)?;
            index(&mut out, chain.last)?;
        }
        seeds(&mut out, &self.seeds)?;
        inputs(&mut out, &self.inputs)?;
        record(&mut out, self.record)?;
        finished(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, LightingError> {
        if bytes.len() > MAX_BYTES {
            return Err(invalid("snapshot bytes"));
        }
        let mut r = Reader(bytes);
        if r.take(4)? != b"WLS1" {
            return Err(invalid("snapshot header"));
        }
        let revision = r.u64()?;
        let reserved_interactive_cells = r.count(65_536)?;
        let max_seed_pages = r.count(65_536)?;
        let max_queue_age_ticks = r.u64()?;
        let watermark = r.optional_u64()?;
        let capacity = r.count(65_536)?;
        let limits = LightingLimits {
            max_frontier_cells: capacity,
            reserved_interactive_cells,
            max_seed_pages,
            max_queue_age_ticks,
        };
        validate_limits(limits)?;
        let count = r.count(capacity)?;
        if count > r.0.len() / 37 {
            return Err(invalid("entry bytes"));
        }
        let mut rows = Vec::with_capacity(count);
        let mut seen = BTreeSet::new();
        for _ in 0..count {
            let i = r.count(capacity.saturating_sub(1))?;
            if i >= capacity || !seen.insert(i) {
                return Err(invalid("entry index"));
            }
            rows.push((
                i,
                Entry {
                    position: r.position()?,
                    priority: r.priority()?,
                    queued_at: Tick(r.u64()?),
                    previous: r.index()?,
                    next: r.index()?,
                },
            ));
        }
        let n = r.count(capacity)?;
        if n + count != capacity || n > r.0.len() / 4 {
            return Err(invalid("free count"));
        }
        let mut free = Vec::with_capacity(n);
        for _ in 0..n {
            free.push(r.u32()? as usize);
        }
        let mut chains = [Chain::default(); 3];
        for chain in &mut chains {
            chain.first = r.index()?;
            chain.last = r.index()?;
        }
        let seeds = r.seeds()?;
        let inputs = r.inputs()?;
        let record = r.record()?;
        if !r.0.is_empty() {
            return Err(invalid("snapshot trailing bytes"));
        }
        let mut entries = vec![None; capacity];
        for (i, entry) in rows {
            entries[i] = Some(entry);
        }
        Ok(Self {
            limits,
            revision,
            watermark,
            entries,
            free,
            chains,
            seeds,
            inputs,
            record,
        })
    }
}
impl Delta {
    pub fn encode(&self) -> Result<Vec<u8>, LightingError> {
        let mut out = Vec::new();
        out.extend_from_slice(b"WLD1");
        out.extend_from_slice(&self.before.to_le_bytes());
        out.extend_from_slice(&self.after.to_le_bytes());
        optional_u64(&mut out, self.watermark);
        u32(&mut out, self.operations.len())?;
        for op in &self.operations {
            match *op {
                FrontierOp::Insert(p, priority, at) => {
                    out.push(0);
                    position(&mut out, p);
                    out.push(priority.index() as u8);
                    out.extend_from_slice(&at.0.to_le_bytes());
                }
                FrontierOp::Remove(i) => {
                    out.push(1);
                    u32(&mut out, i)?;
                }
                FrontierOp::Rotate(i) => {
                    out.push(2);
                    u32(&mut out, i)?;
                }
            }
        }
        out.push(u8::from(self.seeds.is_some()));
        if let Some(values) = &self.seeds {
            seeds(&mut out, values)?;
        }
        inputs(&mut out, &self.appended)?;
        u32(&mut out, self.consumed)?;
        record(&mut out, self.record)?;
        finished(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, LightingError> {
        if bytes.len() > MAX_BYTES {
            return Err(invalid("delta bytes"));
        }
        let mut r = Reader(bytes);
        if r.take(4)? != b"WLD1" {
            return Err(invalid("delta header"));
        }
        let before = r.u64()?;
        let after = r.u64()?;
        let watermark = r.optional_u64()?;
        if before.checked_add(1) != Some(after) {
            return Err(invalid("delta revision"));
        }
        let n = r.count(MAX_OPERATIONS)?;
        if n > r.0.len() / 5 {
            return Err(invalid("operation bytes"));
        }
        let mut operations = Vec::with_capacity(n);
        for _ in 0..n {
            operations.push(match r.byte()? {
                0 => FrontierOp::Insert(r.position()?, r.priority()?, Tick(r.u64()?)),
                1 => FrontierOp::Remove(r.u32()? as usize),
                2 => FrontierOp::Rotate(r.u32()? as usize),
                _ => return Err(invalid("operation tag")),
            });
        }
        let seeds = match r.byte()? {
            0 => None,
            1 => Some(r.seeds()?),
            _ => return Err(invalid("seed option")),
        };
        let appended = r.inputs()?;
        let consumed = r.count(MAX_INPUT_BYTES / 16)?;
        let record = r.record()?;
        if !r.0.is_empty() {
            return Err(invalid("delta trailing bytes"));
        }
        Ok(Self {
            before,
            after,
            watermark,
            operations,
            seeds,
            appended,
            consumed,
            record,
        })
    }
}
