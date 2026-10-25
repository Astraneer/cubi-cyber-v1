//! Реестр (ASP-06 §2, §8): номера блоков и слои атласа (ids), формы (shapes), таблица поведения блоков (blocks),
//! предметы и ломание (items). Номера блоков и предметов не переставляются, новые — только в конец (mc/decisions.md).

pub(crate) mod blocks;
pub(crate) mod ids;
pub(crate) mod items;
pub(crate) mod shapes;
