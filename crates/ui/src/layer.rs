use crate::{
    app::UiApp,
    gpu::{UiRenderer, UiWork},
    paint::PaintList,
};
/// Platform shells own only events/surface submission. All layout, widgets,
/// settings, inventory/NEI state and pixels use this same native/browser type.
pub struct UiLayer {
    pub app: UiApp,
    renderer: UiRenderer,
    list: PaintList,
    last: Option<(u64, u32, u32)>,
}
impl UiLayer {
    pub fn prepare(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        shader_srgb: bool,
    ) -> Result<Self, &'static str> {
        Ok(Self {
            app: UiApp::new()?,
            renderer: UiRenderer::prepare(device, queue, format, shader_srgb)?,
            list: PaintList::default(),
            last: None,
        })
    }
    pub fn needs_redraw(&self, width: u32, height: u32) -> bool {
        self.last != Some((self.app.revision(), width, height))
    }
    pub fn upload(
        &mut self,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
    ) -> Result<bool, &'static str> {
        if !self.needs_redraw(width, height) {
            return Ok(false);
        }
        self.app
            .paint(&self.renderer.atlas, &mut self.list, width, height);
        let changed = self.renderer.upload(queue, &self.list)?;
        self.last = Some((self.app.revision(), width, height));
        Ok(changed)
    }
    pub fn encode(&mut self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        self.renderer.encode(encoder, target);
    }
    pub fn take_work(&mut self) -> UiWork {
        self.renderer.take_work()
    }
}
