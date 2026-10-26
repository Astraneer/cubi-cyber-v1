use super::*;
struct Write {
    bytes: Vec<u8>,
}
impl Write {
    fn raw(&mut self, b: &[u8]) {
        self.bytes.extend_from_slice(b);
    }
    fn u8(&mut self, n: u8) {
        self.raw(&[n]);
    }
    fn u16(&mut self, n: u16) {
        self.raw(&n.to_le_bytes());
    }
    fn u32(&mut self, n: u32) {
        self.raw(&n.to_le_bytes());
    }
    fn u64(&mut self, n: u64) {
        self.raw(&n.to_le_bytes());
    }
    fn pos(&mut self, p: BlockPos) {
        self.u32(p.space.0);
        for n in [p.x, p.y, p.z] {
            self.u32(n as u32);
        }
    }
    fn packet(&mut self, p: &Packet) {
        self.u64(p.id);
        self.u16(p.stack.item());
        self.u16(p.stack.count());
        self.u16(p.stack.damage());
        self.u64(p.stack.payload().map_or(0, |id| id.0));
        self.u8(p.color);
        self.pos(p.from);
        self.u16(p.step);
        self.u8(p.progress);
        self.u8(match p.goal {
            Goal::Destination => 0,
            Goal::Home => 1,
            Goal::Idle => 2,
        });
        self.u8(u8::from(p.home.is_some()));
        if let Some(home) = p.home {
            self.pos(home);
        }
        self.u8(p.idle_direction.map_or(255, |f| f as u8));
        self.u8(u8::from(p.destination.is_some()));
        if let Some(id) = p.destination {
            self.u64(id.0);
        }
        self.u64(p.last_tick.0);
        self.u64(p.due.0);
        self.u64(p.first_due.0);
        self.u16(p.path.len() as u16);
        for &p in &p.path {
            self.pos(p);
        }
    }
    fn pull(&mut self, p: Pull) {
        self.pos(p.position);
        self.u64(p.order);
        self.u8(p.failures);
        self.u64(p.due.0);
        self.u64(p.first_due.0);
    }
    fn finish(self, maximum: usize) -> Result<Vec<u8>, MachineError> {
        if self.bytes.len() > maximum {
            Err(MachineError::Bounds)
        } else {
            Ok(self.bytes)
        }
    }
}
struct Read<'a> {
    bytes: &'a [u8],
    at: usize,
    config: Config,
}
impl Read<'_> {
    fn raw<const N: usize>(&mut self) -> Result<[u8; N], MachineError> {
        let value = self
            .bytes
            .get(self.at..self.at + N)
            .ok_or(MachineError::Encoding)?
            .try_into()
            .map_err(|_| MachineError::Encoding)?;
        self.at += N;
        Ok(value)
    }
    fn u8(&mut self) -> Result<u8, MachineError> {
        Ok(self.raw::<1>()?[0])
    }
    fn boolean(&mut self) -> Result<bool, MachineError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(MachineError::Encoding),
        }
    }
    fn u16(&mut self) -> Result<u16, MachineError> {
        Ok(u16::from_le_bytes(self.raw()?))
    }
    fn u32(&mut self) -> Result<u32, MachineError> {
        Ok(u32::from_le_bytes(self.raw()?))
    }
    fn u64(&mut self) -> Result<u64, MachineError> {
        Ok(u64::from_le_bytes(self.raw()?))
    }
    fn pos(&mut self) -> Result<BlockPos, MachineError> {
        Ok(BlockPos {
            space: SpaceId(self.u32()?),
            x: self.u32()? as i32,
            y: self.u32()? as i32,
            z: self.u32()? as i32,
        })
    }
    fn count(&mut self, max: usize, min: usize) -> Result<usize, MachineError> {
        let n = self.u32()? as usize;
        if n > max || n > self.bytes.len().saturating_sub(self.at) / min {
            return Err(MachineError::Bounds);
        }
        Ok(n)
    }
    fn packet(&mut self) -> Result<Packet, MachineError> {
        let id = self.u64()?;
        let mut stack = ItemStack::from_parts(self.u16()?, self.u16()?, self.u16()?)
            .map_err(|_| MachineError::Encoding)?;
        let payload = self.u64()?;
        if payload != 0 {
            stack = stack
                .with_payload(rules::item_payload::PayloadId(payload))
                .map_err(|_| MachineError::Encoding)?;
        }
        let color = self.u8()?;
        let from = self.pos()?;
        let step = self.u16()?;
        let progress = self.u8()?;
        let goal = match self.u8()? {
            0 => Goal::Destination,
            1 => Goal::Home,
            2 => Goal::Idle,
            _ => return Err(MachineError::Encoding),
        };
        let home = if self.boolean()? {
            Some(self.pos()?)
        } else {
            None
        };
        let idle_direction = match self.u8()? {
            255 => None,
            n => Some(*Face::ALL.get(n as usize).ok_or(MachineError::Encoding)?),
        };
        let destination = if self.boolean()? {
            Some(ComponentId(self.u64()?))
        } else {
            None
        };
        let last_tick = Tick(self.u64()?);
        let due = Tick(self.u64()?);
        let first_due = Tick(self.u64()?);
        let count = self.u16()? as usize;
        if count == 0
            || count > MAX_PACKET_PATH
            || count > self.bytes.len().saturating_sub(self.at) / 16
        {
            return Err(MachineError::Bounds);
        }
        let mut path = Vec::with_capacity(count);
        for _ in 0..count {
            path.push(self.pos()?);
        }
        let packet = Packet {
            id,
            stack,
            color,
            from,
            path,
            step,
            progress,
            goal,
            home,
            idle_direction,
            destination,
            last_tick,
            due,
            first_due,
        };
        packet.validate()?;
        Ok(packet)
    }
    fn pull(&mut self) -> Result<Pull, MachineError> {
        Ok(Pull {
            position: self.pos()?,
            order: self.u64()?,
            failures: self.u8()?,
            due: Tick(self.u64()?),
            first_due: Tick(self.u64()?),
        })
    }
    fn finish(&self) -> Result<(), MachineError> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(MachineError::Encoding)
        }
    }
}
impl TransportSnapshot {
    pub fn encode(&self, config: Config) -> Result<Vec<u8>, MachineError> {
        let normalized = TransportOwner::restore(config, self)?.snapshot();
        let snapshot = &normalized;
        let mut w = Write {
            bytes: b"MTR1".to_vec(),
        };
        w.u64(snapshot.next_packet);
        w.u64(snapshot.next_pull_order);
        w.u32(snapshot.packets.len() as u32);
        for p in &snapshot.packets {
            w.packet(p);
        }
        w.u32(snapshot.pulls.len() as u32);
        for &p in &snapshot.pulls {
            w.pull(p);
        }
        w.raw(b"XPAG");
        w.u32(snapshot.clocks.len() as u32);
        for record in &snapshot.clocks {
            w.page(record.page);
            w.clock(record.clock);
        }
        w.finish(MAX_TRANSPORT_BYTES)
    }
    pub fn decode(bytes: &[u8], config: Config) -> Result<Self, MachineError> {
        if bytes.len() < 28 || bytes.len() > MAX_TRANSPORT_BYTES {
            return Err(MachineError::Bounds);
        }
        let mut r = Read {
            bytes,
            at: 0,
            config,
        };
        if r.raw::<4>()? != *b"MTR1" {
            return Err(MachineError::Encoding);
        }
        let next_packet = r.u64()?;
        let next_pull_order = r.u64()?;
        let count = r.count(config.packets, 72)?;
        let mut packets = Vec::with_capacity(count);
        for _ in 0..count {
            packets.push(r.packet()?);
        }
        let count = r.count(r.config.transmitters, 25)?;
        let mut pulls = Vec::with_capacity(count);
        for _ in 0..count {
            pulls.push(r.pull()?);
        }
        let mut clocks;
        if r.peek(b"XPAG") {
            r.raw::<4>()?;
            let count = r.count(config.packets.saturating_add(config.transmitters), 33)?;
            clocks = Vec::with_capacity(count);
            for _ in 0..count {
                clocks.push(ClockRecord {
                    page: r.page()?,
                    clock: r.clock()?,
                });
            }
            if clocks.is_empty() && (!packets.is_empty() || !pulls.is_empty()) {
                return Err(MachineError::Encoding);
            }
        } else {
            let pages: BTreeSet<_> = packets
                .iter()
                .filter_map(Packet::at)
                .map(|p| p.page())
                .chain(pulls.iter().map(|p| p.position.page()))
                .collect();
            clocks = pages
                .into_iter()
                .map(|page| ClockRecord {
                    page,
                    clock: PageClock::default(),
                })
                .collect();
        }
        if clocks.windows(2).any(|p| p[0].page >= p[1].page) {
            return Err(MachineError::Encoding);
        }
        r.finish()?;
        if packets.windows(2).any(|p| p[0].id >= p[1].id)
            || pulls.windows(2).any(|p| p[0].position >= p[1].position)
        {
            return Err(MachineError::Encoding);
        }
        let out = Self {
            next_packet,
            next_pull_order,
            packets,
            pulls,
            clocks,
        };
        TransportOwner::restore(config, &out)?;
        Ok(out)
    }
}
impl TransportDelta {
    pub fn encode(&self) -> Result<Vec<u8>, MachineError> {
        if self
            .packets
            .iter()
            .fold(self.pulls.len().saturating_mul(52), |total, p| {
                total
                    .saturating_add(p.before.as_ref().map_or(0, Packet::bytes))
                    .saturating_add(p.after.as_ref().map_or(0, Packet::bytes))
            })
            > MAX_TRANSPORT_DELTA_BYTES - 44
        {
            return Err(MachineError::Bounds);
        }
        let mut w = Write {
            bytes: b"MTD1".to_vec(),
        };
        for n in [
            self.next_packet_before,
            self.next_packet_after,
            self.next_pull_before,
            self.next_pull_after,
        ] {
            w.u64(n);
        }
        w.u32(self.packets.len() as u32);
        for p in &self.packets {
            w.u8(u8::from(p.before.is_some()));
            if let Some(before) = &p.before {
                w.packet(before);
            }
            w.u8(u8::from(p.after.is_some()));
            if let Some(after) = &p.after {
                w.packet(after);
            }
        }
        w.u32(self.pulls.len() as u32);
        for p in &self.pulls {
            w.u8(u8::from(p.before.is_some()));
            if let Some(before) = p.before {
                w.pull(before);
            }
            w.u8(u8::from(p.after.is_some()));
            if let Some(after) = p.after {
                w.pull(after);
            }
        }
        w.raw(b"XPAG");
        w.u32(self.clocks.len() as u32);
        for patch in &self.clocks {
            w.page(patch.page);
            w.u8(u8::from(patch.before.is_some()));
            if let Some(c) = patch.before {
                w.clock(c);
            }
            w.u8(u8::from(patch.after.is_some()));
            if let Some(c) = patch.after {
                w.clock(c);
            }
        }
        w.finish(MAX_TRANSPORT_DELTA_BYTES)
    }
    pub fn decode(bytes: &[u8], config: Config) -> Result<Self, MachineError> {
        if bytes.len() < 44 || bytes.len() > MAX_TRANSPORT_DELTA_BYTES {
            return Err(MachineError::Bounds);
        }
        let mut r = Read {
            bytes,
            at: 0,
            config,
        };
        if r.raw::<4>()? != *b"MTD1" {
            return Err(MachineError::Encoding);
        }
        let next_packet_before = r.u64()?;
        let next_packet_after = r.u64()?;
        let next_pull_before = r.u64()?;
        let next_pull_after = r.u64()?;
        let count = r.count(config.packets, 74)?;
        let mut packets = Vec::with_capacity(count);
        for _ in 0..count {
            let before = if r.boolean()? {
                Some(r.packet()?)
            } else {
                None
            };
            let after = if r.boolean()? {
                Some(r.packet()?)
            } else {
                None
            };
            if before.is_none() && after.is_none() {
                return Err(MachineError::Encoding);
            }
            packets.push(PacketPatch { before, after });
        }
        let count = r.count(config.transmitters, 27)?;
        let mut pulls = Vec::with_capacity(count);
        for _ in 0..count {
            let before = if r.boolean()? { Some(r.pull()?) } else { None };
            let after = if r.boolean()? { Some(r.pull()?) } else { None };
            if before.is_none() && after.is_none() {
                return Err(MachineError::Encoding);
            }
            pulls.push(PullPatch { before, after });
        }
        let legacy = !r.peek(b"XPAG");
        let mut clocks = Vec::new();
        if !legacy {
            r.raw::<4>()?;
            let count = r.count(config.packets.saturating_add(config.transmitters), 35)?;
            clocks = Vec::with_capacity(count);
            for _ in 0..count {
                let page = r.page()?;
                let before = if r.boolean()? { Some(r.clock()?) } else { None };
                let after = if r.boolean()? { Some(r.clock()?) } else { None };
                if before.is_none() && after.is_none() {
                    return Err(MachineError::Encoding);
                }
                clocks.push(ClockPatch {
                    page,
                    before,
                    after,
                });
            }
        }
        r.finish()?;
        Ok(Self {
            next_packet_before,
            next_packet_after,
            next_pull_before,
            next_pull_after,
            packets,
            pulls,
            clocks,
            legacy,
        })
    }
}
impl Write {
    fn page(&mut self, p: PageId) {
        self.u32(p.space.0);
        self.u32(p.x as u32);
        self.u32(p.y as u32);
        self.u32(p.z as u32);
    }
    fn clock(&mut self, c: PageClock) {
        self.u8(u8::from(c.active));
        self.u64(c.offset);
        self.u64(c.paused_since.0);
    }
}
impl Read<'_> {
    fn peek(&self, tag: &[u8; 4]) -> bool {
        self.bytes.get(self.at..self.at.saturating_add(4)) == Some(tag.as_slice())
    }
    fn page(&mut self) -> Result<PageId, MachineError> {
        Ok(PageId {
            space: SpaceId(self.u32()?),
            x: self.u32()? as i32,
            y: self.u32()? as i32,
            z: self.u32()? as i32,
        })
    }
    fn clock(&mut self) -> Result<PageClock, MachineError> {
        let clock = PageClock {
            active: self.boolean()?,
            offset: self.u64()?,
            paused_since: Tick(self.u64()?),
        };
        clock.validate()?;
        Ok(clock)
    }
}
