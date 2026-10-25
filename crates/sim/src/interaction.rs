//! Bounded DDA ray queries and authoritative transaction preparation.
use crate::{Aabb, GameMode, Player, QueryWorld};
use core_types::{BlockPos, Cell, SpaceId, Tick};
use world::CellRead;

/// Ray-box intersection t in[0,1] plus outward face(DOWN,UP,NORTH,SOUTH,WEST,EAST).
pub(crate) fn segment_box(start: [f64; 3], end: [f64; 3], shape: Aabb) -> Option<(f64, u8)> {
    let delta = std::array::from_fn::<_, 3, _>(|axis| end[axis] - start[axis]);
    if delta.iter().map(|value| value * value).sum::<f64>() < 1e-7 {
        return None;
    }
    let probe = std::array::from_fn::<_, 3, _>(|axis| start[axis] + delta[axis] * 0.001);
    if (0..3).all(|axis| probe[axis] >= shape.lo[axis] && probe[axis] < shape.hi[axis]) {
        let mut best = (f32::MIN_POSITIVE, 2u8);
        for face in 0..6u8 {
            let direction = rules::cell::DIRS[face as usize];
            let projection = (0..3)
                .map(|axis| delta[axis] * f64::from(direction[axis]))
                .sum::<f64>() as f32;
            if projection > best.0 {
                best = (projection, face);
            }
        }
        return Some((0.001, best.1 ^ 1));
    }
    let mut best = 1.0;
    let mut face = None;
    for axis in 0..3 {
        let (plane, side) = if delta[axis] > 1e-7 {
            (shape.lo[axis], [4, 0, 2][axis])
        } else if delta[axis] < -1e-7 {
            (shape.hi[axis], [5, 1, 3][axis])
        } else {
            continue;
        };
        let t = (plane - start[axis]) / delta[axis];
        if !(0.0 < t && t < best) {
            continue;
        }
        if (0..3).filter(|&other| other != axis).all(|other| {
            let point = start[other] + t * delta[other];
            shape.lo[other] - 1e-7 < point && point < shape.hi[other] + 1e-7
        }) {
            best = t;
            face = Some(side);
        }
    }
    face.map(|face| (best, face))
}
pub(crate) struct RayCells {
    cell: [i32; 3],
    step: [i32; 3],
    delta: [f64; 3],
    next: [f64; 3],
    entry: f64,
    remaining: u32,
    first: bool,
}
pub(crate) fn ray_cells(start: [f64; 3], end: [f64; 3]) -> RayCells {
    let d = std::array::from_fn::<_, 3, _>(|axis| end[axis] - start[axis]);
    let step = std::array::from_fn(|axis| {
        if d[axis] > 0.0 {
            1
        } else if d[axis] < 0.0 {
            -1
        } else {
            0
        }
    });
    let delta = std::array::from_fn(|axis| {
        if step[axis] == 0 {
            f64::INFINITY
        } else {
            (1.0 / d[axis]).abs()
        }
    });
    let next = std::array::from_fn(|axis| {
        if step[axis] == 0 {
            return f64::INFINITY;
        }
        let fraction = start[axis] - start[axis].floor();
        delta[axis]
            * if step[axis] > 0 {
                1.0 - fraction
            } else {
                fraction
            }
    });
    RayCells {
        cell: std::array::from_fn(|axis| start[axis].floor() as i32),
        step,
        delta,
        next,
        entry: 0.0,
        remaining: 128,
        first: true,
    }
}
impl Iterator for RayCells {
    type Item = ([i32; 3], f64);
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 || !self.entry.is_finite() || self.entry > 1.0 {
            return None;
        }
        self.remaining -= 1;
        if self.first {
            self.first = false;
            return Some((self.cell, 0.0));
        }
        let axis = if self.next[0] < self.next[1] {
            if self.next[0] < self.next[2] { 0 } else { 2 }
        } else if self.next[1] < self.next[2] {
            1
        } else {
            2
        };
        self.entry = self.next[axis];
        if self.entry > 1.0 || !self.entry.is_finite() {
            return None;
        }
        self.cell[axis] = self.cell[axis].saturating_add(self.step[axis]);
        self.next[axis] += self.delta[axis];
        Some((self.cell, self.entry))
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockHit {
    pub cell: [i32; 3],
    pub point: [f64; 3],
    pub face: u8,
    pub distance: f64,
    pub block: Cell,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RayResult {
    Miss,
    Unknown([i32; 3]),
    Hit(BlockHit),
}
pub fn raycast(
    world: &impl QueryWorld,
    eye: [f64; 3],
    direction: [f64; 3],
    reach: f64,
) -> RayResult {
    if !eye
        .iter()
        .chain(direction.iter())
        .all(|value| value.is_finite())
        || !(0.0..=16.0).contains(&reach)
    {
        return RayResult::Miss;
    }
    let length =
        (direction[0] * direction[0] + direction[1] * direction[1] + direction[2] * direction[2])
            .sqrt();
    if !length.is_finite() || length < 1e-9 || eye.iter().any(|value| value.abs() >= 30_000_000.0) {
        return RayResult::Miss;
    }
    let end = std::array::from_fn(|axis| eye[axis] + direction[axis] / length * reach);
    for (position, _) in ray_cells(eye, end) {
        let cell = match world.cell(position) {
            CellRead::Unknown => return RayResult::Unknown(position),
            CellRead::Known(cell) => cell,
        };
        let offset = position.map(f64::from);
        let mut best = None;
        for shape in rules::blocks::outline_of(cell.raw()) {
            if let Some((t, face)) = segment_box(eye, end, shape.moved(offset))
                && best.is_none_or(|(old, _)| t < old)
            {
                best = Some((t, face));
            }
        }
        if let Some((t, face)) = best
            && t < 1.0
        {
            let point = std::array::from_fn(|axis| eye[axis] + (end[axis] - eye[axis]) * t);
            return RayResult::Hit(BlockHit {
                cell: position,
                point,
                face,
                distance: t * reach,
                block: cell,
            });
        }
    }
    RayResult::Miss
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditError {
    Spectator,
    Unknown,
    OutOfReach,
    Occluded,
    Protected,
    Unbreakable,
    Occupied,
    Stale,
    InvalidCell,
    Unsupported,
    DuplicateTick,
}
#[derive(Clone, Copy, Debug)]
pub enum EditAction {
    Break {
        position: [i32; 3],
        expected: Cell,
    },
    Place {
        position: [i32; 3],
        expected: Cell,
        cell: Cell,
    },
}
/// Produces a guarded transaction. The caller must still apply server inventory,
/// permissions, action-rate and idempotency checks; this never edits world directly.
pub fn prepare_edit(
    world: &world::World,
    space: SpaceId,
    player: &Player,
    action: EditAction,
    tick: Tick,
    protected: impl Fn([i32; 3]) -> bool,
) -> Result<world::Transaction, EditError> {
    if player.mode == GameMode::Spectator {
        return Err(EditError::Spectator);
    }
    let position = match action {
        EditAction::Break { position, .. } | EditAction::Place { position, .. } => position,
    };
    if protected(position) {
        return Err(EditError::Protected);
    }
    let query = crate::WorldQuery::new(world, space);
    let current = match query.cell(position) {
        CellRead::Unknown => return Err(EditError::Unknown),
        CellRead::Known(cell) => cell,
    };
    let expected = match action {
        EditAction::Break { expected, .. } | EditAction::Place { expected, .. } => expected,
    };
    if current != expected {
        return Err(EditError::Stale);
    }
    let reach = if player.mode == GameMode::Creative || player.noclip {
        5.0
    } else {
        4.5
    };
    let eye = player.eye(1.0);
    let target = position.map(|value| f64::from(value) + 0.5);
    let distance = (0..3)
        .map(|axis| (target[axis] - eye[axis]).powi(2))
        .sum::<f64>()
        .sqrt();
    if distance >= reach + 0.87 {
        return Err(EditError::OutOfReach);
    }
    let ray = raycast(&query, eye, player.look(), reach);
    let new = match action {
        EditAction::Break { .. } => {
            let RayResult::Hit(hit) = ray else {
                return Err(EditError::Occluded);
            };
            if hit.cell != position {
                return Err(EditError::Occluded);
            }
            if rules::blocks::bdef(current.id()).be.is_some() {
                return Err(EditError::Unsupported);
            }
            if player.mode != GameMode::Creative
                && !player.noclip
                && rules::blocks::bdef(current.id()).hardness < 0.0
            {
                return Err(EditError::Unbreakable);
            }
            Cell::AIR
        }
        EditAction::Place { cell, .. } => {
            if cell.id() == 0 || rules::block(cell.id()).is_none() {
                return Err(EditError::InvalidCell);
            }
            if !rules::blocks::replaceable_by(current.raw(), rules::items::block_item(cell.id())) {
                return Err(EditError::Occupied);
            }
            let RayResult::Hit(hit) = ray else {
                return Err(EditError::Occluded);
            };
            let dir = rules::cell::DIRS[hit.face as usize];
            let adjacent = std::array::from_fn(|axis| hit.cell[axis] + dir[axis]);
            if position != adjacent && hit.cell != position {
                return Err(EditError::Occluded);
            }
            let cell = placement_cell(&query, player, hit, position, cell, current)?;
            let offset = position.map(f64::from);
            if rules::blocks::collision_of(cell.raw()).iter().any(|shape| {
                shape
                    .moved(offset)
                    .intersects(&player.bbox().contract(1e-7))
            }) {
                return Err(EditError::Occupied);
            }
            cell
        }
    };
    let block = BlockPos {
        space,
        x: position[0],
        y: position[1],
        z: position[2],
    };
    let mut transaction = world::Transaction::new(tick, space);
    // Guard every page observed along visibility and support, not only writes.
    let end = std::array::from_fn(|axis| eye[axis] + player.look()[axis] * reach);
    let visible_cell = match ray {
        RayResult::Hit(hit) => Some(hit.cell),
        _ => None,
    };
    for (p, _) in ray_cells(eye, end) {
        let observed = BlockPos {
            space,
            x: p[0],
            y: p[1],
            z: p[2],
        };
        transaction.expect_page(observed.page(), world.page_version(observed.page()));
        if Some(p) == visible_cell {
            break;
        }
    }
    transaction.expect_page(block.page(), world.page_version(block.page()));
    if matches!(action, EditAction::Place { .. }) {
        for direction in rules::cell::DIRS {
            let p = BlockPos {
                space,
                x: position[0] + direction[0],
                y: position[1] + direction[1],
                z: position[2] + direction[2],
            };
            transaction.expect_page(p.page(), world.page_version(p.page()));
        }
    }
    transaction.set_cell(block, new);
    Ok(transaction)
}

/// Hand mining progress uses f32 accumulation (e.g. an oak log takes61 ticks).
#[derive(Default, Debug)]
pub struct Mining {
    pub target: Option<[i32; 3]>,
    pub progress: f32,
    pub cooldown: u8,
}
impl Mining {
    pub fn cancel(&mut self) {
        self.target = None;
        self.progress = 0.0;
    }
    pub fn tick_cell(
        &mut self,
        target: [i32; 3],
        cell: Cell,
        item: u16,
        on_ground: bool,
        eyes_in_water: bool,
        creative: bool,
    ) -> bool {
        let definition = rules::blocks::bdef(cell.id());
        if self.cooldown > 0 {
            self.cooldown -= 1;
            return false;
        }
        if self.target != Some(target) {
            self.target = Some(target);
            self.progress = 0.0;
        }
        if definition.hardness < 0.0 && !creative {
            return false;
        }
        if creative || definition.hardness == 0.0 {
            self.cancel();
            self.cooldown = if creative { 5 } else { 0 };
            return true;
        }
        self.progress += rules::items::break_delta(item, definition, on_ground, eyes_in_water);
        if self.progress >= 1.0 {
            self.cancel();
            self.cooldown = 5;
            true
        } else {
            false
        }
    }
    pub fn tick(
        &mut self,
        target: [i32; 3],
        hardness: f32,
        speed: f32,
        can_harvest: bool,
        creative: bool,
    ) -> bool {
        if self.cooldown > 0 {
            self.cooldown -= 1;
            return false;
        }
        if self.target != Some(target) {
            self.target = Some(target);
            self.progress = 0.0;
        }
        if hardness < 0.0 && !creative {
            return false;
        }
        if creative || hardness == 0.0 {
            self.cancel();
            self.cooldown = if creative { 5 } else { 0 };
            return true;
        }
        self.progress += speed / hardness / if can_harvest { 30.0 } else { 100.0 };
        if self.progress >= 1.0 {
            self.cancel();
            self.cooldown = 5;
            true
        } else {
            false
        }
    }
}

fn placement_cell(
    world: &impl QueryWorld,
    player: &Player,
    hit: BlockHit,
    position: [i32; 3],
    requested: Cell,
    current: Cell,
) -> Result<Cell, EditError> {
    use rules::blocks::{Place, Support};
    let definition = rules::blocks::bdef(requested.id());
    if definition.be.is_some() || definition.fluid != 0 {
        return Err(EditError::Unsupported);
    }
    let read = |p| match world.cell(p) {
        CellRead::Known(cell) => Ok(cell),
        CellRead::Unknown => Err(EditError::Unknown),
    };
    let facing = {
        let direction = player.look();
        if direction[0].abs() > direction[2].abs() {
            if direction[0] > 0.0 { 4 } else { 5 }
        } else if direction[2] > 0.0 {
            2
        } else {
            3
        }
    };
    let mut id = requested.id();
    let state = match definition.place {
        Place::Plain => 0,
        Place::Axis => match hit.face {
            0 | 1 => 0,
            2 | 3 => 2,
            _ => 1,
        },
        Place::FacePlayer => facing,
        Place::Persistent => rules::blocks::ST_PERSIST,
        Place::Layers => {
            if current.id() == id {
                (current.state() & 7) + 1
            } else {
                0
            }
        }
        Place::Torch { wall } => {
            if hit.face == 0 {
                return Err(EditError::Unsupported);
            }
            if hit.face >= 2 {
                id = wall;
                u16::from(hit.face)
            } else {
                0
            }
        }
        Place::Ladder => {
            if hit.face >= 2 {
                u16::from(hit.face)
            } else {
                facing
            }
        }
    };
    let cell = Cell::from_parts(id, state);
    match rules::blocks::bdef(id).support {
        Support::None => {}
        Support::Wall => {
            let face = rules::blocks::facing_of(cell.raw());
            let direction = rules::cell::DIRS[face as usize];
            let behind = std::array::from_fn(|axis| position[axis] - direction[axis]);
            let support = read(behind)?;
            if !rules::blocks::face_full(support.raw(), face)
                || id == rules::ids::LADDER && rules::blocks::is_leaves(support.id())
            {
                return Err(EditError::Unsupported);
            }
        }
        support => {
            let below = read([position[0], position[1] - 1, position[2]])?;
            let valid = match support {
                Support::Crop => rules::blocks::is_farmland(below.id()),
                Support::Layers => rules::blocks::face_full(below.raw(), 1),
                Support::Cactus => matches!(below.id(), rules::ids::SAND | rules::ids::CACTUS),
                Support::Floor if id == rules::ids::TORCH => rules::blocks::top_center(below.raw()),
                _ => rules::blocks::plant_supported(id, below.id()),
            };
            if !valid {
                return Err(EditError::Unsupported);
            }
            if support == Support::Cactus {
                for [dx, _, dz] in [[-1, 0, 0], [1, 0, 0], [0, 0, -1], [0, 0, 1]] {
                    let neighbor = read([position[0] + dx, position[1], position[2] + dz])?;
                    if !rules::blocks::collision_of(neighbor.raw()).is_empty()
                        || neighbor.id() == rules::ids::LAVA
                    {
                        return Err(EditError::Unsupported);
                    }
                }
            }
        }
    }
    Ok(cell)
}

/// Per-player server/client mining clock. Repeated requests cannot accumulate more
/// than one progress update per20Hz tick; the caller checks inventory entitlement.
#[derive(Default)]
pub struct InteractionController {
    pub mining: Mining,
    last_tick: Option<Tick>,
    key: Option<([i32; 3], u16, u16, u16)>,
}
impl InteractionController {
    #[allow(clippy::too_many_arguments)]
    pub fn tick_break(
        &mut self,
        world: &world::World,
        space: SpaceId,
        player: &Player,
        tick: Tick,
        item: u16,
        tool_damage: u16,
        held: bool,
        protected: impl Fn([i32; 3]) -> bool,
    ) -> Result<Option<world::Transaction>, EditError> {
        if player.mode == GameMode::Spectator {
            return Err(EditError::Spectator);
        }
        if self.last_tick.is_some_and(|old| tick.0 <= old.0) {
            return Err(EditError::DuplicateTick);
        }
        self.last_tick = Some(tick);
        // The post-break delay belongs to the fixed clock, including ticks aimed
        // at air/Unknown/protected cells. Mining::tick_cell keeps its standalone
        // cooldown behavior, but the controller never calls it while cooling.
        if self.mining.cooldown > 0 {
            self.mining.cooldown -= 1;
            self.mining.cancel();
            self.key = None;
            return Ok(None);
        }
        if !held {
            self.mining.cancel();
            self.key = None;
            return Ok(None);
        }
        let query = crate::WorldQuery::new(world, space);
        let reach = if player.mode == GameMode::Creative || player.noclip {
            5.0
        } else {
            4.5
        };
        let hit = match raycast(&query, player.eye(1.0), player.look(), reach) {
            RayResult::Hit(hit) => hit,
            RayResult::Unknown(_) => {
                self.mining.cancel();
                return Err(EditError::Unknown);
            }
            RayResult::Miss => {
                self.mining.cancel();
                return Ok(None);
            }
        };
        if protected(hit.cell) {
            self.mining.cancel();
            return Err(EditError::Protected);
        }
        let key = (hit.cell, hit.block.id(), item, tool_damage);
        if self.key != Some(key) {
            self.mining.cancel();
            self.key = Some(key);
        }
        if self.mining.tick_cell(
            hit.cell,
            hit.block,
            item,
            player.on_ground,
            player.eye_water,
            player.mode == GameMode::Creative || player.noclip,
        ) {
            prepare_edit(
                world,
                space,
                player,
                EditAction::Break {
                    position: hit.cell,
                    expected: hit.block,
                },
                tick,
                protected,
            )
            .map(Some)
        } else {
            Ok(None)
        }
    }
}
