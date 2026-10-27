//! Loading preflight. Resource limits come from the actual prepared backend;
//! this module never allocates a GPU resource or changes the active session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Smaa,
    Temporal,
    Msaa4,
}
impl Mode {
    pub fn from_setting(value: i32) -> Result<Self, Error> {
        match value {
            1 => Ok(Self::Smaa),
            2 => Ok(Self::Temporal),
            4 => Ok(Self::Msaa4),
            _ => Err(Error::InvalidMode),
        }
    }
    pub const fn samples(self) -> u32 {
        match self {
            Self::Smaa | Self::Temporal => 1,
            Self::Msaa4 => 4,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct TemporalLimits {
    pub available: bool,
    pub bytes_per_pixel: u64,
    pub constant_bytes: u64,
    pub max_pixels: u64,
    pub max_bytes: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub mode: Mode,
    pub samples: u32,
    pub render_extent: [u32; 2],
    pub temporal_bytes: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidMode,
    InvalidExtent,
    InvalidScale,
    TemporalUnavailable,
    TemporalPixels { requested: u64, maximum: u64 },
    TemporalMemory { required: u64, maximum: u64 },
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::InvalidMode => f.write_str("Неизвестный режим сглаживания"),
            Self::InvalidExtent => f.write_str("Некорректный размер изображения"),
            Self::InvalidScale => f.write_str("Масштаб изображения должен быть от 85 до 100 %"),
            Self::TemporalUnavailable => f.write_str("TAA недоступно на этом устройстве"),
            Self::TemporalPixels { requested, maximum } => write!(
                f,
                "TAA: {requested} пикселей превышают предел {maximum}; прежний режим сохранён"
            ),
            Self::TemporalMemory { required, maximum } => write!(
                f,
                "TAA требует {} МиБ при пределе {} МиБ; прежний режим сохранён",
                required.div_ceil(1024 * 1024),
                maximum.div_ceil(1024 * 1024)
            ),
        }
    }
}
impl std::error::Error for Error {}

/// Call before dropping the existing View/socket or advancing its epoch.
/// The rounded extent matches ClientView::resize exactly. Disabled temporal
/// mode does not inspect its limits, preserving the ordinary MSAA/SMAA path.
pub fn preflight(
    value: i32,
    surface: [u32; 2],
    scale: u32,
    temporal: TemporalLimits,
) -> Result<Plan, Error> {
    let mut plan = base_plan(value, surface, scale)?;
    if plan.mode != Mode::Temporal {
        return Ok(plan);
    }
    if !temporal.available || temporal.bytes_per_pixel == 0 {
        return Err(Error::TemporalUnavailable);
    }
    let pixels = u64::from(plan.render_extent[0]) * u64::from(plan.render_extent[1]);
    if pixels > temporal.max_pixels {
        return Err(Error::TemporalPixels {
            requested: pixels,
            maximum: temporal.max_pixels,
        });
    }
    let required = pixels
        .checked_mul(temporal.bytes_per_pixel)
        .and_then(|bytes| bytes.checked_add(temporal.constant_bytes))
        .ok_or(Error::TemporalMemory {
            required: u64::MAX,
            maximum: temporal.max_bytes,
        })?;
    if required > temporal.max_bytes {
        return Err(Error::TemporalMemory {
            required,
            maximum: temporal.max_bytes,
        });
    }
    plan.temporal_bytes = required;
    Ok(plan)
}

fn base_plan(value: i32, surface: [u32; 2], scale: u32) -> Result<Plan, Error> {
    let mode = Mode::from_setting(value)?;
    if surface.contains(&0) {
        return Err(Error::InvalidExtent);
    }
    if !(85..=100).contains(&scale) {
        return Err(Error::InvalidScale);
    }
    let extent = surface.map(|axis| (u64::from(axis) * u64::from(scale)).div_ceil(100) as u32);
    Ok(Plan {
        mode,
        samples: mode.samples(),
        render_extent: extent,
        temporal_bytes: 0,
    })
}

/// The capacities of the actual default product constructors, including both
/// front and staging banks for the distant geometry owners.
pub fn product_capacities() -> crate::view::temporal::Capacities {
    let horizon = crate::view::horizon::Config::default();
    crate::view::temporal::Capacities {
        terrain_pages: crate::view::terrain_gpu_config().max_pages,
        entity_instances: Some(render::entity_gpu::EntityConfig::default().max_entities),
        prop_instances: Some(render::block_models::BlockConfig::default().max_instances),
        far: Some((
            horizon.max_tiles as u32 * 2,
            horizon.max_proxy_pages as u32 * 2,
        )),
        recipe_boards: Some(rules::crossroads::BOARD_COUNT as u32),
        optical_mask: true,
    }
}

/// Exact whole-graph preflight shared by both window shells. Querying the
/// one-pixel graph obtains its fixed banks/parameters without allocating any
/// resources; the image term is counted once at the selected scaled extent.
pub fn preflight_graph(
    value: i32,
    surface: [u32; 2],
    scale: u32,
    max_dimension: u32,
    timestamps: bool,
    capacities: crate::view::temporal::Capacities,
) -> Result<Plan, String> {
    let plan = base_plan(value, surface, scale).map_err(|error| error.to_string())?;
    if plan.mode != Mode::Temporal {
        return Ok(plan);
    }
    let config = render::taa::TaaConfig::default();
    let fixed = crate::view::temporal::required_bytes(
        config,
        [1, 1],
        max_dimension,
        timestamps,
        capacities,
    )?
    .checked_sub(render::taa::BYTES_PER_PIXEL)
    .ok_or("temporal image accounting")?;
    let plan = preflight(
        value,
        surface,
        scale,
        TemporalLimits {
            available: true,
            bytes_per_pixel: render::taa::BYTES_PER_PIXEL,
            constant_bytes: fixed,
            max_pixels: config.max_pixels,
            max_bytes: config
                .memory_budget_bytes
                .min(crate::view::temporal::MAX_TEMPORAL_BYTES),
        },
    )
    .map_err(|error| error.to_string())?;
    let bytes = crate::view::temporal::required_bytes(
        config,
        plan.render_extent,
        max_dimension,
        timestamps,
        capacities,
    )?;
    if bytes != plan.temporal_bytes {
        return Err("temporal preflight allocation mismatch".into());
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn limits() -> TemporalLimits {
        TemporalLimits {
            available: true,
            bytes_per_pixel: 88,
            constant_bytes: 576,
            max_pixels: 3840 * 2160,
            max_bytes: 256 * 1024 * 1024,
        }
    }
    #[test]
    fn known_modes_keep_msaa_default_and_temporal_uses_one_sample() {
        let unavailable = TemporalLimits {
            available: false,
            bytes_per_pixel: 0,
            ..limits()
        };
        for (value, samples) in [(1, 1), (4, 4)] {
            let plan = preflight(value, [960, 540], 85, unavailable).unwrap();
            assert_eq!(plan.samples, samples);
            assert_eq!(plan.render_extent, [816, 459]);
            assert_eq!(plan.temporal_bytes, 0);
        }
        let plan = preflight(2, [1920, 1080], 100, limits()).unwrap();
        assert_eq!(plan.mode, Mode::Temporal);
        assert_eq!(plan.samples, 1);
        assert_eq!(plan.temporal_bytes, 182_477_376);
        assert_eq!(
            preflight(2, [1, 1], 100, unavailable),
            Err(Error::TemporalUnavailable)
        );
        assert_eq!(preflight(3, [1, 1], 100, limits()), Err(Error::InvalidMode));
    }
    #[test]
    fn exact_scaled_cost_rejects_ultrawide_before_any_active_state_changes() {
        for surface in [[3440, 1440], [3840, 2160]] {
            for scale in [85, 100] {
                let error = preflight(2, surface, scale, limits()).unwrap_err();
                let Error::TemporalMemory { required, maximum } = error else {
                    panic!("unexpected preflight error: {error:?}");
                };
                assert!(required > maximum);
                assert_eq!(maximum, 256 * 1024 * 1024);
                assert!(error.to_string().contains("прежний режим сохранён"));
            }
        }
        let rounded = preflight(2, [65, 65], 85, limits()).unwrap();
        assert_eq!(rounded.render_extent, [56, 56]);
        let exact = rounded.temporal_bytes;
        assert_eq!(
            preflight(
                2,
                [65, 65],
                85,
                TemporalLimits {
                    max_bytes: exact,
                    ..limits()
                }
            )
            .unwrap(),
            rounded
        );
        assert!(matches!(
            preflight(
                2,
                [65, 65],
                85,
                TemporalLimits {
                    max_bytes: exact - 1,
                    ..limits()
                }
            ),
            Err(Error::TemporalMemory { .. })
        ));
        assert_eq!(
            preflight(2, [0, 65], 100, limits()),
            Err(Error::InvalidExtent)
        );
        assert_eq!(
            preflight(2, [65, 65], 84, limits()),
            Err(Error::InvalidScale)
        );
        assert!(matches!(
            preflight(2, [4096, 2160], 100, limits()),
            Err(Error::TemporalPixels { .. })
        ));
    }
    #[test]
    fn product_graph_preflight_uses_physical_banks_and_preserves_other_modes() {
        let caps = product_capacities();
        let horizon = crate::view::horizon::Config::default();
        assert_eq!(
            caps.prop_instances,
            Some(crate::props_source::PropsLimits::default().max_instances as u32)
        );
        assert_eq!(
            caps.far,
            Some((
                horizon.max_tiles as u32 * 2,
                horizon.max_proxy_pages as u32 * 2
            ))
        );
        for timestamps in [false, true] {
            for scale in [85, 100] {
                let plan =
                    preflight_graph(2, [1920, 1080], scale, 16384, timestamps, caps).unwrap();
                assert_eq!(plan.samples, 1);
                assert_eq!(
                    plan.temporal_bytes,
                    crate::view::temporal::required_bytes(
                        render::taa::TaaConfig::default(),
                        plan.render_extent,
                        16384,
                        timestamps,
                        caps
                    )
                    .unwrap()
                );
                assert!(plan.temporal_bytes < crate::view::temporal::MAX_TEMPORAL_BYTES);
            }
        }
        for extent in [[3440, 1440], [3840, 2160]] {
            let error = preflight_graph(2, extent, 85, 16384, true, caps).unwrap_err();
            assert!(error.contains("МиБ"));
            assert!(error.contains("прежний режим сохранён"));
        }
        assert!(preflight_graph(2, [1920, 1080], 100, 1024, true, caps).is_err());
        for value in [1, 4] {
            assert_eq!(
                preflight_graph(value, [3840, 2160], 100, 0, true, caps)
                    .unwrap()
                    .temporal_bytes,
                0
            );
        }
    }
}
