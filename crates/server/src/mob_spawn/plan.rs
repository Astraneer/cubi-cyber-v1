use super::*;
pub(super) struct Rng(pub u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
    fn float(&mut self) -> f32 {
        (self.next() >> 40) as f32 / 16777216.
    }
    fn below(&mut self, n: i32) -> i32 {
        (self.next() % n as u64) as i32
    }
    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        (i64::from(lo) + (self.next() % (i64::from(hi) - i64::from(lo) + 1) as u64) as i64) as i32
    }
    fn color(&mut self) -> u8 {
        match self.below(100) {
            0..=4 => 1,
            5..=9 => 2,
            10..=14 => 3,
            15..=17 => 4,
            _ => {
                if self.below(500) == 0 {
                    5
                } else {
                    0
                }
            }
        }
    }
}
pub(super) struct Context<'a, W> {
    pub world: &'a mut W,
    pub request: Request,
    pub budget: Budget,
    pub work: Work,
}
impl<W: SpawnWorld> Context<'_, W> {
    fn p(&self, x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos {
            space: self.request.space,
            x,
            y,
            z,
        }
    }
    fn cell(&mut self, p: BlockPos) -> Result<Cell, Error> {
        if self.work.cell_reads == self.budget.cells {
            return Err(Error::Budget);
        }
        self.work.cell_reads += 1;
        self.world.cell(p)
    }
    fn column(&mut self, x: i32, z: i32) -> Result<Column, Error> {
        if self.work.column_queries == self.budget.columns {
            return Err(Error::Budget);
        }
        self.work.column_queries += 1;
        self.world.column(x, z)
    }
    fn light(&mut self, p: BlockPos) -> Result<Light, Error> {
        if self.work.light_reads == self.budget.lights {
            return Err(Error::Budget);
        }
        self.work.light_reads += 1;
        let light = self.world.light(p)?;
        if light.sky > 15 || light.block > 15 {
            return Err(Error::Encoding);
        }
        Ok(light)
    }
    fn ground(&mut self, p: BlockPos, kind: MobKind) -> Result<bool, Error> {
        if p.y < self.budget.min_y || p.y >= self.budget.max_y_exclusive {
            return Ok(false);
        }
        let below = self.cell(self.p(p.x, p.y - 1, p.z))?;
        let def = blocks::bdef(below.id());
        if def.leaves || def.fluid != 0 || !blocks::face_full(below.raw(), 1) {
            return Ok(false);
        }
        for dy in 0..2 {
            let cell = self.cell(self.p(p.x, p.y + dy, p.z))?;
            let def = blocks::bdef(cell.id());
            if def.fluid != 0 || def.suffocates && !blocks::collision_of(cell.raw()).is_empty() {
                return Ok(false);
            }
        }
        let def = kind.definition();
        let radius = def.width / 2.;
        let body = Aabb {
            lo: [
                f64::from(p.x) + 0.5 - radius,
                f64::from(p.y),
                f64::from(p.z) + 0.5 - radius,
            ],
            hi: [
                f64::from(p.x) + 0.5 + radius,
                f64::from(p.y) + def.height,
                f64::from(p.z) + 0.5 + radius,
            ],
        };
        let lo = body.lo.map(|v| v.floor() as i32);
        let hi = body.hi.map(|v| v.ceil() as i32);
        for y in lo[1]..hi[1] {
            for z in lo[2]..hi[2] {
                for x in lo[0]..hi[0] {
                    let cell = self.cell(self.p(x, y, z))?;
                    for shape in blocks::collision_of(cell.raw()) {
                        if body.intersects(&shape.moved([f64::from(x), f64::from(y), f64::from(z)]))
                        {
                            return Ok(false);
                        }
                    }
                }
            }
        }
        Ok(true)
    }
    fn birth(
        &mut self,
        rng: &mut Rng,
        out: &mut Vec<Candidate>,
        kind: MobKind,
        p: BlockPos,
        baby: bool,
    ) -> Option<usize> {
        if out.len() >= self.request.available.min(MAX_CANDIDATES) {
            return None;
        }
        let yaw = rng.float() * 360.;
        let color = if kind == MobKind::Sheep {
            rng.color()
        } else {
            0
        };
        let index = out.len();
        out.push(Candidate {
            kind,
            position: [f64::from(p.x) + 0.5, f64::from(p.y), f64::from(p.z) + 0.5],
            baby,
            yaw,
            color,
        });
        self.work.candidates += 1;
        Some(index)
    }
    pub fn herd(&mut self, global: &mut Rng, out: &mut Vec<Candidate>) -> Result<(), Error> {
        let [cx, cz] = self.request.column;
        let (x0, z0) = (cx * 16, cz * 16);
        let Some(table) = herd_table(self.column(x0 + 8, z0 + 8)?.biome) else {
            return Ok(());
        };
        let seed = self.request.map_seed
            ^ (cx as i64 as u64).wrapping_mul(0x9e3779b97f4a7c15)
            ^ (cz as i64 as u64).wrapping_mul(0xc2b2ae3d27d4eb4f);
        let mut rng = Rng(seed ^ 0x4e7d5eed);
        let mut guard = 0;
        while rng.float() < table.chance && guard < 8 {
            guard += 1;
            let mut pick = rng.below(i32::from(table.weights.iter().sum::<u16>() + table.empty));
            let mut kind = None;
            for (k, weight) in [MobKind::Sheep, MobKind::Pig, MobKind::Chicken, MobKind::Cow]
                .into_iter()
                .zip(table.weights)
            {
                if pick < i32::from(weight) {
                    kind = Some(k);
                    break;
                }
                pick -= i32::from(weight);
            }
            let Some(kind) = kind else {
                continue;
            };
            let (ox, oz) = (x0 + rng.below(16), z0 + rng.below(16));
            let (mut x, mut z) = (ox, oz);
            for member in 0..4 {
                for _ in 0..4 {
                    self.work.attempts += 1;
                    let height = self.column(x, z)?.height;
                    let mut y = height
                        .saturating_add(1)
                        .min(self.budget.max_y_exclusive - 1);
                    let mut k = 0;
                    while k < 8
                        && y < self.budget.max_y_exclusive
                        && !blocks::collision_of(self.cell(self.p(x, y, z))?.raw()).is_empty()
                    {
                        y += 1;
                        k += 1;
                    }
                    while y > self.budget.min_y && k < 16 {
                        let below = self.cell(self.p(x, y - 1, z))?;
                        if !blocks::collision_of(below.raw()).is_empty()
                            || blocks::bdef(below.id()).fluid != 0
                        {
                            break;
                        }
                        y -= 1;
                        k += 1;
                    }
                    let below = self.cell(self.p(x, y - 1, z))?;
                    if blocks::bdef(below.id()).grass && self.ground(self.p(x, y, z), kind)? {
                        let baby = member > 0 && rng.float() < 0.05;
                        if let Some(index) = self.birth(global, out, kind, self.p(x, y, z), baby) {
                            out[index].yaw = rng.float() * 360.;
                        }
                        break;
                    }
                    let mut tries = 0;
                    loop {
                        x += rng.below(5) - rng.below(5);
                        z += rng.below(5) - rng.below(5);
                        if (x0..x0 + 16).contains(&x) && (z0..z0 + 16).contains(&z) || tries > 8 {
                            break;
                        }
                        x = ox;
                        z = oz;
                        tries += 1;
                    }
                    x = x.clamp(x0, x0 + 15);
                    z = z.clamp(z0, z0 + 15);
                }
            }
        }
        Ok(())
    }
    pub fn monsters(
        &mut self,
        rng: &mut Rng,
        out: &mut Vec<Candidate>,
        players: &[Player],
    ) -> Result<(), Error> {
        if self.request.available == 0 || !players.iter().any(|p| p.space == self.request.space) {
            return Ok(());
        }
        let [cx, cz] = self.request.column;
        let (x, z) = (cx * 16 + rng.below(16), cz * 16 + rng.below(16));
        let top = self.column(x, z)?.height.saturating_add(1);
        let y = rng.range(-79, top.max(1));
        if y < self.budget.min_y || y >= self.budget.max_y_exclusive {
            return Ok(());
        }
        let cell = self.cell(self.p(x, y, z))?;
        if blocks::bdef(cell.id()).suffocates && !blocks::collision_of(cell.raw()).is_empty() {
            return Ok(());
        }
        let darkness = ambient_darkness(self.request.day);
        for _ in 0..3 {
            let (mut px, mut pz) = (x, z);
            let mut chosen = None;
            let steps = (rng.float() * 4.).ceil() as usize;
            for _ in 0..steps {
                self.work.attempts += 1;
                px += rng.below(6) - rng.below(6);
                pz += rng.below(6) - rng.below(6);
                let p = self.p(px, y, pz);
                let pos = [f64::from(px) + 0.5, f64::from(y), f64::from(pz) + 0.5];
                let near = players
                    .iter()
                    .filter(|p| p.space == self.request.space)
                    .map(|p| {
                        (0..3)
                            .map(|i| (p.position[i] - pos[i]).powi(2))
                            .sum::<f64>()
                    })
                    .fold(f64::INFINITY, f64::min);
                if near <= 576. || near > 128. * 128. || self.world.protected(p) {
                    continue;
                }
                if !*chosen.get_or_insert_with(|| rng.below(515) < 95) {
                    break;
                }
                if !self.ground(p, MobKind::Zombie)? {
                    continue;
                }
                let light = self.light(p)?;
                if i32::from(light.sky) > rng.below(32) {
                    continue;
                }
                let combined = light.sky.saturating_sub(darkness).max(light.block);
                if i32::from(combined) > rng.below(8) {
                    continue;
                }
                let baby = rng.float() < 0.05;
                self.birth(rng, out, MobKind::Zombie, p, baby);
                if out.len() >= 4 || out.len() >= self.request.available {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}
