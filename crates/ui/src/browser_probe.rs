//! Debug-only standalone browser acceptance shell. Production UI is UiLayer;
//! release builds do not expose these isolated GPU/input probe entry points.
use crate::{
    app::{Focus, Modifiers, Screen, Snapshot},
    layer::UiLayer,
};
use std::cell::RefCell;
use wasm_bindgen::{JsCast, prelude::*};
struct Probe {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    device: wgpu::Device,
    queue: wgpu::Queue,
    ui: UiLayer,
}
thread_local! {static PROBE:RefCell<Option<Probe>>=const{RefCell::new(None)};}
fn error(message: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&message.to_string())
}
#[wasm_bindgen]
pub async fn ui_browser_start() -> Result<(), JsValue> {
    let document = web_sys::window()
        .and_then(|window| window.document())
        .ok_or_else(|| error("document"))?;
    let canvas = document
        .get_element_by_id("ui-canvas")
        .ok_or_else(|| error("UI canvas"))?
        .dyn_into::<web_sys::HtmlCanvasElement>()?;
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::BROWSER_WEBGPU,
        ..Default::default()
    });
    let surface = instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
        .map_err(error)?;
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
        })
        .await
        .map_err(error)?;
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("UI browser acceptance"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        })
        .await
        .map_err(error)?;
    let mut config = surface
        .get_default_config(&adapter, canvas.width(), canvas.height())
        .ok_or_else(|| error("surface config"))?;
    config.present_mode = wgpu::PresentMode::Fifo;
    config.desired_maximum_frame_latency = 1;
    surface.configure(&device, &config);
    let ui = UiLayer::prepare(&device, &queue, config.format, !config.format.is_srgb())
        .map_err(error)?;
    PROBE.with(|probe| {
        *probe.borrow_mut() = Some(Probe {
            surface,
            config,
            device,
            queue,
            ui,
        })
    });
    draw()?;
    Ok(())
}
fn draw() -> Result<(), JsValue> {
    PROBE.with(|probe| {
        let mut probe = probe.borrow_mut();
        let probe = probe
            .as_mut()
            .ok_or_else(|| error("UI probe not started"))?;
        if !probe
            .ui
            .needs_redraw(probe.config.width, probe.config.height)
        {
            return Ok(());
        }
        probe
            .ui
            .upload(&probe.queue, probe.config.width, probe.config.height)
            .map_err(error)?;
        let frame = probe.surface.get_current_texture().map_err(error)?;
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = probe.device.create_command_encoder(&Default::default());
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("UI probe clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.04,
                            g: 0.06,
                            b: 0.07,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        probe.ui.encode(&mut encoder, &view);
        probe.queue.submit([encoder.finish()]);
        frame.present();
        Ok(())
    })
}
#[wasm_bindgen]
pub fn ui_browser_pointer(x: f32, y: f32) -> Result<(), JsValue> {
    PROBE.with(|probe| {
        if let Some(probe) = probe.borrow_mut().as_mut() {
            probe.ui.app.pointer(x, y);
        }
    });
    draw()
}
#[wasm_bindgen]
pub fn ui_browser_click(
    x: f32,
    y: f32,
    right: bool,
    shift: bool,
    ctrl: bool,
) -> Result<(), JsValue> {
    PROBE.with(|probe| {
        if let Some(probe) = probe.borrow_mut().as_mut() {
            probe.ui.app.click(x, y, right, Modifiers { shift, ctrl });
        }
    });
    draw()
}
#[wasm_bindgen]
pub fn ui_browser_key(
    code: &str,
    key: &str,
    pressed: bool,
    shift: bool,
    ctrl: bool,
) -> Result<bool, JsValue> {
    let captured = PROBE.with(|probe| {
        let mut probe = probe.borrow_mut();
        let Some(probe) = probe.as_mut() else {
            return false;
        };
        let text_focus = matches!(
            probe.ui.app.focus,
            Focus::Name | Focus::Address | Focus::Search | Focus::Chat
        );
        let captured = probe.ui.app.key(code, pressed, Modifiers { shift, ctrl });
        if pressed && text_focus && !ctrl && key.chars().count() == 1 {
            probe.ui.app.text(key);
        }
        captured
    });
    draw()?;
    Ok(captured)
}
#[wasm_bindgen]
pub fn ui_browser_wheel(delta: i32) -> Result<(), JsValue> {
    PROBE.with(|probe| {
        if let Some(probe) = probe.borrow_mut().as_mut() {
            probe.ui.app.wheel(delta);
        }
    });
    draw()
}
#[wasm_bindgen]
pub fn ui_browser_fixture_inventory() -> Result<(), JsValue> {
    PROBE.with(|probe| {
        if let Some(probe) = probe.borrow_mut().as_mut() {
            let _ = probe.ui.app.set_snapshot(Snapshot {
                connected: true,
                ..Snapshot::default()
            });
            probe.ui.app.open(Screen::Inventory);
        }
    });
    draw()
}
#[wasm_bindgen]
pub fn ui_browser_status() -> String {
    PROBE.with(|probe| {
        let probe = probe.borrow();
        let Some(probe) = probe.as_ref() else {
            return "{}".into();
        };
        format!(
            "{{\"screen\":\"{:?}\",\"revision\":{},\"filteredItems\":{},\"connected\":{},\"masterVolume\":{},\"debug\":{}}}",
            probe.ui.app.screen,
            probe.ui.app.revision(),
            probe.ui.app.filtered_items().len(),
            probe.ui.app.snapshot.connected,
            probe.ui.app.settings.get(crate::settings::Setting::Master),
            probe.ui.app.show_debug
        )
    })
}
#[wasm_bindgen]
pub async fn ui_browser_fence() -> Result<(), JsValue> {
    let queue = PROBE
        .with(|probe| probe.borrow().as_ref().map(|probe| probe.queue.clone()))
        .ok_or_else(|| error("UI probe"))?;
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let completed = done.clone();
    queue.on_submitted_work_done(move || {
        completed.store(true, std::sync::atomic::Ordering::Release)
    });
    for _ in 0..600 {
        if done.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }
        let promise = js_sys::Promise::new(&mut |resolve, reject| {
            if let Some(window) = web_sys::window()
                && let Err(error) = window.request_animation_frame(&resolve)
            {
                let _ = reject.call1(&JsValue::UNDEFINED, &error);
            }
        });
        wasm_bindgen_futures::JsFuture::from(promise).await?;
    }
    Err(error("UI queue completion timeout"))
}
