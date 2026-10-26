//! The pipeline worker owns all heavy GPU/asset preparation before handing one
//! heap-owned shared view to the window thread. No frame loop compiles shaders.
use client::{
    lighting::ClientLightingConfig,
    view::{ClientView, NearRasterization},
};
use render::terrain_gpu::{AtlasImage, TerrainGpu};

#[derive(Clone, Copy)]
pub struct Preparation {
    pub format: wgpu::TextureFormat,
    pub width: u32,
    pub height: u32,
    pub scene: client::SceneConfig,
    pub raster: NearRasterization,
    pub entities: bool,
    pub props: bool,
    pub lighting: Option<ClientLightingConfig>,
    pub materials: bool,
    pub environment: bool,
    pub horizon: bool,
}

pub fn prepare(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    preparation: Preparation,
) -> Result<Box<ClientView>, String> {
    let atlas = AtlasImage::donor()?;
    let terrain = TerrainGpu::new(
        device,
        queue,
        preparation.format,
        preparation.width,
        preparation.height,
        render::OutputTransfer::Attachment,
        client::view::terrain_gpu_config(),
        &atlas,
    )
    .map_err(|error| error.to_string())?;
    let mut view =
        ClientView::new_with_rasterization(terrain, preparation.scene, preparation.raster)?;
    if let Some(mut config) = preparation.lighting {
        config.shadows.dynamic_casters |= preparation.entities || preparation.props;
        view.prepare_lighting(device, queue, config)?;
    }
    if preparation.environment {
        view.prepare_environment(
            device,
            queue,
            preparation.width,
            preparation.height,
            render::OutputTransfer::Attachment,
            client::view::EnvironmentProfile::default(),
            &atlas,
        )?;
    }
    if preparation.entities {
        view.prepare_entities(device, queue)?;
    }
    if preparation.props {
        view.prepare_props(device, queue, Default::default())?;
    }
    if preparation.horizon {
        view.prepare_horizon(device, queue, client::view::horizon::Config::default())?;
    }
    if preparation.materials {
        let prepared =
            render::material_pack::PreparedMaterials::prepare(&atlas, &[], 0, Default::default())
                .map_err(|error| format!("material defaults: {error:?}"))?;
        let pack = render::material_pack::MaterialPackGpu::upload(device, queue, &prepared)
            .map_err(|error| format!("material GPU defaults: {error:?}"))?;
        view.renderer_mut()
            .prepare_material_pack(device, queue, pack, false)
            .map_err(|error| error.to_string())?;
    }
    Ok(Box::new(view))
}

/// Explicit startup profile: server LIGHT is authoritative and both shadow
/// targets fit the selected 2048 resolution. Runtime quality does not change it.
pub fn product_lighting() -> ClientLightingConfig {
    ClientLightingConfig {
        source: client::lighting::LightSource::Authoritative,
        shadows: render::shadows::ShadowConfig {
            resolution: 2048,
            ..Default::default()
        },
        ..Default::default()
    }
}
