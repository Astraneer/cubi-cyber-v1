//! Коробки и формы блоков (ASP-04 §2.6, ASP-02): обводка (луч и рамка), коллизия, формы по состоянию.

/// Коробка: в мировых координатах (физика, луч) или в долях блока (формы реестра).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Aabb {
    pub(crate) lo: [f64; 3],
    pub(crate) hi: [f64; 3],
}

impl Aabb {
    #[inline]
    pub(crate) fn moved(&self, d: [f64; 3]) -> Aabb {
        Aabb { lo: [self.lo[0] + d[0], self.lo[1] + d[1], self.lo[2] + d[2]], hi: [self.hi[0] + d[0], self.hi[1] + d[1], self.hi[2] + d[2]] }
    }
    /// Растянуть в сторону знака каждой компоненты (Box.stretch).
    pub(crate) fn stretch(&self, d: [f64; 3]) -> Aabb {
        let mut b = *self;
        for a in 0..3 {
            if d[a] < 0.0 {
                b.lo[a] += d[a];
            } else if d[a] > 0.0 {
                b.hi[a] += d[a];
            }
        }
        b
    }
    pub(crate) fn contract(&self, e: f64) -> Aabb {
        Aabb { lo: [self.lo[0] + e, self.lo[1] + e, self.lo[2] + e], hi: [self.hi[0] - e, self.hi[1] - e, self.hi[2] - e] }
    }
    /// Пересечение с положительным объёмом (Box.intersects — строго).
    #[inline]
    pub(crate) fn intersects(&self, o: &Aabb) -> bool {
        (0..3).all(|a| self.lo[a] < o.hi[a] && self.hi[a] > o.lo[a])
    }
}

/// Коробка в 1/16 блока, как Block.createCuboidShape.
pub(crate) const fn px(x0: f64, y0: f64, z0: f64, x1: f64, y1: f64, z1: f64) -> Aabb {
    Aabb { lo: [x0 / 16.0, y0 / 16.0, z0 / 16.0], hi: [x1 / 16.0, y1 / 16.0, z1 / 16.0] }
}

pub(crate) const CUBE: &[Aabb] = &[Aabb { lo: [0.0; 3], hi: [1.0; 3] }];
/// Трава, папоротник, мёртвый куст.
pub(crate) const SHORT_PLANT: &[Aabb] = &[px(2.0, 0.0, 2.0, 14.0, 13.0, 14.0)];
pub(crate) const FLOWER: &[Aabb] = &[px(5.0, 0.0, 5.0, 11.0, 10.0, 11.0)];
/// Стоячий факел (обводка, коллизии нет).
pub(crate) const TORCH_SHAPE: &[Aabb] = &[px(6.0, 0.0, 6.0, 10.0, 10.0, 10.0)];
pub(crate) const CHEST_SHAPE: &[Aabb] = &[px(1.0, 0.0, 1.0, 15.0, 14.0, 15.0)];
/// FACING north (по умолчанию).
pub(crate) const LADDER_SHAPE: &[Aabb] = &[px(0.0, 0.0, 13.0, 16.0, 16.0, 16.0)];
/// 1 слой; коллизия на слой ниже — пустая.
pub(crate) const SNOW_LAYER_SHAPE: &[Aabb] = &[px(0.0, 0.0, 0.0, 16.0, 2.0, 16.0)];
/// Коллизия кактуса в MC — для опоры факела.
pub(crate) const CACTUS_SIDES: &[Aabb] = &[px(1.0, 0.0, 1.0, 15.0, 15.0, 15.0)];
/// Обводка кактуса (CactusBlock.OUTLINE_SHAPE): на пиксель выше коллизии.
pub(crate) const CACTUS_OUTLINE: &[Aabb] = &[px(1.0, 0.0, 1.0, 15.0, 16.0, 15.0)];

// Формы по состоянию (индекс стороны: N, S, W, E — hidx). Лестница — пластина 3/16 у стены (LadderBlock), настенный
// факел — 5 × 10 × 5 у стены на высоте 3…13 (WallTorchBlock.BOUNDING_SHAPES), снежный слой — 2 пикселя на слой
// (SnowBlock.LAYERS_TO_SHAPE: вид — слоёв, коллизия — на слой ниже).
pub(crate) const LADDER_SHAPES: [&[Aabb]; 4] = [
    &[px(0.0, 0.0, 13.0, 16.0, 16.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 16.0, 3.0)],
    &[px(13.0, 0.0, 0.0, 16.0, 16.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 3.0, 16.0, 16.0)],
];
pub(crate) const WALL_TORCH_SHAPES: [&[Aabb]; 4] = [
    &[px(5.5, 3.0, 11.0, 10.5, 13.0, 16.0)],
    &[px(5.5, 3.0, 0.0, 10.5, 13.0, 5.0)],
    &[px(11.0, 3.0, 5.5, 16.0, 13.0, 10.5)],
    &[px(0.0, 3.0, 5.5, 5.0, 13.0, 10.5)],
];
pub(crate) const SNOW_SHAPES: [&[Aabb]; 9] = [
    &[],
    &[px(0.0, 0.0, 0.0, 16.0, 2.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 4.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 6.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 8.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 10.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 12.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 14.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 16.0, 16.0)],
];
/// Пашня: 16 × 15 × 16 (FarmlandBlock.SHAPE) — и обводка, и коллизия.
pub(crate) const FARMLAND_SHAPE: &[Aabb] = &[px(0.0, 0.0, 0.0, 16.0, 15.0, 16.0)];
/// Обводка посевов по возрасту 0…7: пшеница 2, 4…16 px; морковь и картофель 2, 3…9 px.
pub(crate) const WHEAT_SHAPES: [&[Aabb]; 8] = [
    &[px(0.0, 0.0, 0.0, 16.0, 2.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 4.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 6.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 8.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 10.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 12.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 14.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 16.0, 16.0)],
];
pub(crate) const ROOT_SHAPES: [&[Aabb]; 8] = [
    &[px(0.0, 0.0, 0.0, 16.0, 2.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 3.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 4.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 5.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 6.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 7.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 8.0, 16.0)],
    &[px(0.0, 0.0, 0.0, 16.0, 9.0, 16.0)],
];
/// Центр кабеля 6 × 6 × 6 (BlockSmallTransmitter: 5…11); рукава — registry/blocks.rs cable_shape.
/// Мачта ветряка (ограничивающие блоки, MEK-03 §3.5): столб 8 × 8 px по центру — коллизия и луч.
pub(crate) const MAST: &[Aabb] = &[px(4.0, 0.0, 4.0, 12.0, 16.0, 12.0)];
pub(crate) const CABLE_CORE: &[Aabb] = &[px(5.0, 5.0, 5.0, 11.0, 11.0, 11.0)];
/// Центр большого передатчика 8 × 8 × 8 (BlockLargeTransmitter: 4…12) — транспортёр, механическая труба.
pub(crate) const LARGE_CORE: &[Aabb] = &[px(4.0, 4.0, 4.0, 12.0, 12.0, 12.0)];
