//! Fixed side tables. Changing facing rotates every relative item/gas setting.
use crate::blocks::MekKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Face {
    Down,
    Up,
    North,
    South,
    West,
    East,
}
impl Face {
    pub const ALL: [Self; 6] = [
        Self::Down,
        Self::Up,
        Self::North,
        Self::South,
        Self::West,
        Self::East,
    ];
    pub fn opposite(self) -> Self {
        Self::ALL[self as usize ^ 1]
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RelativeSide {
    Front,
    Left,
    Right,
    Back,
    Top,
    Bottom,
}
impl RelativeSide {
    pub const ALL: [Self; 6] = [
        Self::Front,
        Self::Left,
        Self::Right,
        Self::Back,
        Self::Top,
        Self::Bottom,
    ];
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum SideMode {
    #[default]
    None,
    Input,
    Output,
    Both,
    Energy,
    Extra,
}
pub fn direction(facing: Face, relative: RelativeSide) -> Face {
    use Face::*;
    let table = match facing {
        North => [North, East, West, South, Up, Down],
        South => [South, West, East, North, Up, Down],
        West => [West, North, South, East, Up, Down],
        East => [East, South, North, West, Up, Down],
        Up => [Up, East, West, Down, South, North],
        Down => [Down, East, West, Up, North, South],
    };
    table[relative as usize]
}
pub fn relative(facing: Face, face: Face) -> RelativeSide {
    RelativeSide::ALL
        .into_iter()
        .find(|&r| direction(facing, r) == face)
        .expect("face permutation")
}
pub fn offset(p: [i32; 3], face: Face) -> Option<[i32; 3]> {
    let (axis, sign) = match face {
        Face::Down => (1, -1),
        Face::Up => (1, 1),
        Face::North => (2, -1),
        Face::South => (2, 1),
        Face::West => (0, -1),
        Face::East => (0, 1),
    };
    let mut q = p;
    q[axis] = q[axis].checked_add(sign)?;
    Some(q)
}
pub fn defaults(kind: MekKind) -> [SideMode; 6] {
    use SideMode::*;
    match kind {
        MekKind::Heat | MekKind::Wind | MekKind::Pump => [None; 6],
        MekKind::Cube => [Output, Input, Input, Input, Input, Input],
        MekKind::Separator => [Input, None, None, Energy, None, None],
        MekKind::Infuser | MekKind::Purifier => [Input, Input, Output, Energy, Input, Extra],
        _ => [Input, Input, Output, Energy, Input, Input],
    }
}
pub fn energy_io(kind: MekKind, facing: Face, face: Face) -> (bool, bool) {
    match kind {
        MekKind::Heat => (false, face == facing),
        MekKind::Wind => (false, face == facing || face == Face::Down),
        MekKind::Cube => (face != facing, face == facing),
        MekKind::Pump => (face == direction(facing, RelativeSide::Back), false),
        _ => (true, false),
    }
}
pub fn cycle(kind: MekKind, current: SideMode, backward: bool) -> SideMode {
    use SideMode::*;
    let modes: &[SideMode] = match kind {
        MekKind::Heat | MekKind::Wind | MekKind::Pump => &[None],
        MekKind::Cube => &[None, Input, Output, Both],
        MekKind::Separator => &[None, Input, Energy],
        MekKind::Infuser | MekKind::Purifier => &[None, Input, Output, Both, Energy, Extra],
        _ => &[None, Input, Output, Both, Energy],
    };
    let i = modes.iter().position(|&s| s == current).unwrap_or(0);
    modes[if backward {
        (i + modes.len() - 1) % modes.len()
    } else {
        (i + 1) % modes.len()
    }]
}
