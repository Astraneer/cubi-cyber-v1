//! Worker-only exterior exposure policy for public canonical geometry. Private
//! generation stays exact; closed underground AIR/lava is replaced only in the
//! public view. Unknown cannot open a path into a hidden component.
use core_types::Cell;

pub const WIDTH: usize = 48;
pub const HEIGHT: usize = 128;
pub const VOLUME: usize = WIDTH * WIDTH * HEIGHT;
const PASSABLE: u8 = 1;
const EXTERIOR: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Bounds,
    Cancelled,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub authority_reads: u64,
    pub baseline_reads: u64,
    pub flood_cells: u64,
    pub closed_transparent_cells: u64,
    pub mask_bytes: u64,
    pub peak_queue_bytes: u64,
}
pub struct PublicProjection {
    origin: [i32; 2],
    flags: Box<[u8]>,
    work: Work,
}
impl PublicProjection {
    /// `origin` is the minimum X/Z block coordinate of three complete profile
    /// columns. Caller certifies all72 WHOLE profile pages, not only halo faces.
    pub fn build(
        origin: [i32; 2],
        mut authority: impl FnMut([i32; 3]) -> Option<Cell>,
        mut baseline: impl FnMut([i32; 3]) -> Cell,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, Error> {
        if origin
            .iter()
            .any(|v| v.checked_add(WIDTH as i32 - 1).is_none())
        {
            return Err(Error::Bounds);
        }
        let mut flags = vec![0; VOLUME].into_boxed_slice();
        let mut queue = Vec::with_capacity(VOLUME);
        let mut work = Work {
            mask_bytes: VOLUME as u64,
            peak_queue_bytes: (VOLUME * std::mem::size_of::<u32>()) as u64,
            ..Work::default()
        };
        for y in 0..HEIGHT {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            for z in 0..WIDTH {
                for x in 0..WIDTH {
                    let position = [origin[0] + x as i32, y as i32, origin[1] + z as i32];
                    let i = index(x, y, z);
                    work.authority_reads += 1;
                    let Some(actual) = authority(position) else {
                        continue;
                    };
                    if rules::properties(actual).opaque {
                        continue;
                    }
                    flags[i] = PASSABLE;
                    work.baseline_reads += 1;
                    if !rules::properties(baseline(position)).opaque {
                        flags[i] |= EXTERIOR;
                        queue.push(i as u32);
                    }
                }
            }
        }
        let mut head = 0;
        while head < queue.len() {
            if head & 4095 == 0 && cancelled() {
                return Err(Error::Cancelled);
            }
            let i = queue[head] as usize;
            head += 1;
            work.flood_cells += 1;
            let x = i % WIDTH;
            let y = i / (WIDTH * WIDTH);
            let z = i / WIDTH % WIDTH;
            for j in [
                (x > 0).then(|| i - 1),
                (x + 1 < WIDTH).then_some(i + 1),
                (z > 0).then(|| i - WIDTH),
                (z + 1 < WIDTH).then_some(i + WIDTH),
                (y > 0).then(|| i - WIDTH * WIDTH),
                (y + 1 < HEIGHT).then_some(i + WIDTH * WIDTH),
            ]
            .into_iter()
            .flatten()
            {
                if flags[j] == PASSABLE {
                    flags[j] |= EXTERIOR;
                    queue.push(j as u32);
                }
            }
        }
        work.closed_transparent_cells = flags.iter().filter(|&&f| f == PASSABLE).count() as u64;
        Ok(Self {
            origin,
            flags,
            work,
        })
    }
    pub fn work(&self) -> Work {
        self.work
    }
    /// The actual World value is never mutated. Sources outside this finite
    /// canonical profile have no generated secret layer and remain as supplied.
    pub fn project(&self, position: [i32; 3], actual: Cell, baseline: Cell) -> Cell {
        let x = i64::from(position[0]) - i64::from(self.origin[0]);
        let z = i64::from(position[2]) - i64::from(self.origin[1]);
        let inside = (0..WIDTH as i64).contains(&x)
            && (0..WIDTH as i64).contains(&z)
            && (0..HEIGHT as i32).contains(&position[1]);
        let cell = if inside
            && !rules::properties(actual).opaque
            && self.flags[index(x as usize, position[1] as usize, z as usize)] & EXTERIOR == 0
        {
            baseline
        } else {
            actual
        };
        crate::lod::redact_private_material(cell)
    }
}
fn index(x: usize, y: usize, z: usize) -> usize {
    x + WIDTH * (z + WIDTH * y)
}
