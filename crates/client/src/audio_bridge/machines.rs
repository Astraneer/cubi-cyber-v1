//! Machine loops follow only exact confirmed public visual bindings. Discovery
//! visits at most two headers/64 hints; revoked active emitters stop immediately.
use super::BlockCue;
use crate::{ClientScene, props_source::PropsSource};
use core_types::{BlockPos, Cell, LocalIdx, PageId, SpaceId, Version};
use net::props::{ACTIVE, Frame, Hint, Kind};
use world::CellRead;
const MAX_LOOPS: usize = 32;
#[derive(Clone, Copy, Debug)]
pub enum MachineCommand {
    Start(BlockCue),
    Stop(audio::EmitterId),
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MachineSoundWork {
    pub headers: u32,
    pub hints: u32,
    pub revalidated: u32,
    pub started: u32,
    pub stopped: u32,
    pub capacity: bool,
}
#[derive(Clone, Copy)]
struct Loop {
    position: BlockPos,
    emitter: audio::EmitterId,
    event: &'static str,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    public: u64,
    world: Version,
    eye: [u64; 3],
    space: SpaceId,
}
pub struct MachineSounds {
    session: Option<u64>,
    key: Option<Key>,
    pass: Option<Key>,
    scanning: bool,
    cursor: Option<(PageId, usize)>,
    active: Vec<Loop>,
}
impl Default for MachineSounds {
    fn default() -> Self {
        Self {
            session: None,
            key: None,
            pass: None,
            scanning: false,
            cursor: None,
            active: Vec::with_capacity(MAX_LOOPS),
        }
    }
}
impl MachineSounds {
    pub fn active(&self) -> usize {
        self.active.len()
    }
    pub fn clear(&mut self, mut emit: impl FnMut(MachineCommand)) {
        for row in self.active.drain(..) {
            emit(MachineCommand::Stop(row.emitter));
        }
        self.session = None;
        self.key = None;
        self.pass = None;
        self.scanning = false;
        self.cursor = None;
    }
    /// The caller uses the authority's session ID and actual listener eye. A
    /// bank swap calls clear, then current confirmed loops are rediscovered.
    pub fn visit(
        &mut self,
        session: u64,
        scene: &ClientScene,
        source: &PropsSource,
        eye: [f64; 3],
        mut emit: impl FnMut(MachineCommand),
    ) -> MachineSoundWork {
        let mut work = MachineSoundWork::default();
        if eye.iter().any(|value| !value.is_finite()) {
            work.stopped += self.active.len() as u32;
            self.clear(&mut emit);
            return work;
        }
        if self.session != Some(session) {
            work.stopped += self.active.len() as u32;
            self.clear(&mut emit);
            self.session = Some(session);
        }
        let key = Key {
            public: source.public_revision(),
            world: scene.world().version(),
            eye: eye.map(f64::to_bits),
            space: scene.config().center.space,
        };
        // A raw cell-view can change without changing the derived cell bytes.
        // Recheck at most32 exact bindings even when World.version is unchanged.
        {
            let mut index = 0;
            while index < self.active.len() {
                let row = self.active[index];
                work.revalidated += 1;
                let valid = source
                    .public_frame(row.position.page())
                    .and_then(|frame| {
                        frame
                            .hints
                            .binary_search_by_key(&row.position.local_index(), |hint| hint.index)
                            .ok()
                            .and_then(|index| eligible(scene, frame, &frame.hints[index], eye))
                    })
                    .is_some_and(|(_, event)| event == row.event);
                if valid {
                    index += 1;
                } else {
                    self.active.swap_remove(index);
                    emit(MachineCommand::Stop(row.emitter));
                    work.stopped += 1;
                }
            }
        }
        if self.key != Some(key) {
            self.key = Some(key);
            if !self.scanning {
                self.scanning = true;
                self.cursor = None;
                self.pass = Some(key);
            }
        }
        if !self.scanning {
            return work;
        }
        while work.headers < 2 && work.hints < 64 {
            let (frame, start) = match self.cursor {
                Some((page, index)) => match source
                    .public_frame(page)
                    .filter(|frame| index < frame.hints.len())
                {
                    Some(frame) => (frame, index),
                    None => match source.public_frames_after(Some(page)).next() {
                        Some(frame) => (frame, 0),
                        None => {
                            self.finish_pass(key);
                            break;
                        }
                    },
                },
                None => match source.public_frames_after(None).next() {
                    Some(frame) => (frame, 0),
                    None => {
                        self.finish_pass(key);
                        break;
                    }
                },
            };
            work.headers += 1;
            if scene
                .authoritative_version(frame.page)
                .is_none_or(|version| version.view != frame.cell_view)
            {
                self.cursor = Some((frame.page, frame.hints.len()));
                continue;
            }
            let mut index = start;
            while index < frame.hints.len() && work.hints < 64 {
                let hint = &frame.hints[index];
                index += 1;
                work.hints += 1;
                let Some((position, event)) = eligible(scene, frame, hint, eye) else {
                    continue;
                };
                if self.active.iter().any(|row| row.position == position) {
                    continue;
                }
                if self.active.len() == MAX_LOOPS {
                    work.capacity = true;
                    continue;
                }
                let emitter = emitter(position);
                emit(MachineCommand::Start(BlockCue {
                    event,
                    gain: 0.6,
                    position: [
                        f64::from(position.x) + 0.5,
                        f64::from(position.y) + 0.5,
                        f64::from(position.z) + 0.5,
                    ],
                    emitter,
                    seed: emitter.0,
                }));
                self.active.push(Loop {
                    position,
                    emitter,
                    event,
                });
                work.started += 1;
            }
            self.cursor = Some((frame.page, index));
        }
        work
    }
    fn finish_pass(&mut self, key: Key) {
        self.cursor = None;
        if self.pass == Some(key) {
            self.scanning = false;
        } else {
            self.pass = Some(key);
        }
    }
}
fn eligible(
    scene: &ClientScene,
    frame: &Frame,
    hint: &Hint,
    eye: [f64; 3],
) -> Option<(BlockPos, &'static str)> {
    if frame.page.space != scene.config().center.space
        || hint.flags & ACTIVE == 0
        || !matches!(hint.kind, Kind::Machine | Kind::Wind)
        || scene.authoritative_version(frame.page)?.view != frame.cell_view
    {
        return None;
    }
    let position = position(frame.page, hint.index)?;
    let distance = [
        f64::from(position.x) + 0.5,
        f64::from(position.y) + 0.5,
        f64::from(position.z) + 0.5,
    ]
    .into_iter()
    .zip(eye)
    .map(|(a, b)| (a - b) * (a - b))
    .sum::<f64>();
    if distance > 32. * 32. {
        return None;
    }
    let CellRead::Known(cell) = scene.world().read(position) else {
        return None;
    };
    match hint.kind {
        Kind::Wind if cell.id() == rules::ids::WIND_GENERATOR => {
            Some((position, "machine.generator_loop"))
        }
        Kind::Machine if cell.id() != rules::ids::WIND_GENERATOR && machine(cell) => Some((
            position,
            if cell.id() == rules::ids::HEAT_GENERATOR {
                "machine.generator_loop"
            } else {
                "machine.loop"
            },
        )),
        _ => None,
    }
}
fn machine(cell: Cell) -> bool {
    rules::block(cell.id()).is_some_and(|block| {
        matches!(
            block.render,
            rules::blocks::Render::Model(rules::Model::Mek)
        )
    })
}
fn position(page: PageId, index: LocalIdx) -> Option<BlockPos> {
    let (x, y, z) = index.xyz();
    Some(BlockPos {
        space: page.space,
        x: i32::try_from(i64::from(page.x) * 16 + i64::from(x)).ok()?,
        y: i32::try_from(i64::from(page.y) * 16 + i64::from(y)).ok()?,
        z: i32::try_from(i64::from(page.z) * 16 + i64::from(z)).ok()?,
    })
}
fn emitter(position: BlockPos) -> audio::EmitterId {
    let mut hash = 0x4d414348_u64;
    for word in [
        position.space.0,
        position.x as u32,
        position.y as u32,
        position.z as u32,
    ] {
        hash = (hash ^ u64::from(word)).wrapping_mul(0x100000001b3);
    }
    audio::EmitterId((hash << 2) | 3)
}
