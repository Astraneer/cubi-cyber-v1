//! Thin WebGPU shell. No WebGL backend and no downloaded visual assets.
#![cfg(target_arch = "wasm32")]

use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::{JsCast, prelude::*};
use wasm_bindgen_futures::JsFuture;
use web_sys::HtmlCanvasElement;

#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);

type AnimationCallback = Rc<RefCell<Option<Closure<dyn FnMut()>>>>;

struct WebState {
    canvas: HtmlCanvasElement,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    output_format: wgpu::TextureFormat,
    gpu: render::Gpu,
    sky: render::SkyRenderer,
    recorder: telemetry::FrameRecorder<600>,
    frames: u64,
}

impl WebState {
    fn resize(&mut self) {
        let window = web_sys::window().expect("window exists during animation");
        let ratio = window.device_pixel_ratio();
        let width = ((self.canvas.client_width().max(1) as f64) * ratio).round() as u32;
        let height = ((self.canvas.client_height().max(1) as f64) * ratio).round() as u32;
        if width == self.config.width && height == self.config.height {
            return;
        }
        self.canvas.set_width(width);
        self.canvas.set_height(height);
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.gpu.device, &self.config);
        self.sky.resize(&self.gpu.device, width, height);
    }

    fn draw(&mut self) -> Result<bool, JsValue> {
        let performance = web_sys::window().and_then(|window| window.performance());
        let started = performance.as_ref().map_or(0.0, |clock| clock.now());
        self.resize();
        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.gpu.device, &self.config);
                return Ok(false);
            }
            Err(wgpu::SurfaceError::Timeout) => return Ok(false),
            Err(error) => return Err(JsValue::from_str(&format!("WebGPU frame failed: {error}"))),
        };
        let output = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("sRGB canvas output"),
            format: Some(self.output_format),
            ..Default::default()
        });
        self.sky.render(&self.gpu.device, &self.gpu.queue, &output);
        frame.present();
        self.frames += 1;
        let cpu_ns = performance
            .as_ref()
            .map_or(0, |clock| ((clock.now() - started) * 1_000_000.0) as u64);
        self.recorder.push(telemetry::FrameRecord {
            frame: self.frames,
            cpu_ns,
            gpu_ns: None,
            work: self.sky.take_work(),
        });
        Ok(true)
    }
}

fn animate(state: WebState) -> Result<(), JsValue> {
    let callback: AnimationCallback = Rc::new(RefCell::new(None));
    let retained = callback.clone();
    let mut state = state;
    *callback.borrow_mut() = Some(Closure::wrap(Box::new(move || {
        if let Err(error) = state.draw() {
            web_sys::console::error_1(&error);
            if let Some(document) = web_sys::window().and_then(|window| window.document())
                && let Some(status) = document.get_element_by_id("status")
            {
                status.set_text_content(Some("WebGPU: ошибка кадра. Перезагрузите страницу."));
            }
            return;
        }
        if let Some(callback) = retained.borrow().as_ref() {
            let _ = web_sys::window()
                .expect("window")
                .request_animation_frame(callback.as_ref().unchecked_ref());
        }
    }) as Box<dyn FnMut()>));
    web_sys::window()
        .ok_or("window unavailable")?
        .request_animation_frame(
            callback
                .borrow()
                .as_ref()
                .expect("animation callback initialized")
                .as_ref()
                .unchecked_ref(),
        )?;
    // The single callback cycle intentionally lives for this page's lifetime.
    Ok(())
}

/// Resolves after a presented frame and a verified readback of the shared MSAA4 shader.
#[wasm_bindgen]
pub async fn start() -> Result<JsValue, JsValue> {
    console_error_panic_hook::set_once();
    let window = web_sys::window().ok_or("window unavailable")?;
    let document = window.document().ok_or("document unavailable")?;
    let canvas = document
        .get_element_by_id("game")
        .ok_or("canvas #game unavailable")?
        .dyn_into::<HtmlCanvasElement>()?;
    let instance = render::instance();
    let surface = instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    let gpu = render::Gpu::request(&instance, Some(&surface), false)
        .await
        .map_err(|error| JsValue::from_str(&error))?;
    let ratio = window.device_pixel_ratio();
    let width = ((canvas.client_width().max(1) as f64) * ratio).round() as u32;
    let height = ((canvas.client_height().max(1) as f64) * ratio).round() as u32;
    canvas.set_width(width);
    canvas.set_height(height);
    let mut config = surface
        .get_default_config(&gpu.adapter, width, height)
        .ok_or("WebGPU surface has no compatible format")?;
    config.desired_maximum_frame_latency = render::MAX_FRAMES_IN_FLIGHT;
    config.present_mode = wgpu::PresentMode::Fifo;
    let capabilities = surface.get_capabilities(&gpu.adapter);
    if let Some(format) = capabilities.formats.iter().find(|format| format.is_srgb()) {
        config.format = *format;
    }
    // Canvas configurations expose unorm base formats. An explicitly allowed
    // sRGB view applies the same linear-to-sRGB transfer as the native surface.
    let output_format = config.format.add_srgb_suffix();
    if !output_format.is_srgb() {
        return Err(JsValue::from_str(
            "WebGPU canvas requires an sRGB-compatible format",
        ));
    }
    if output_format != config.format {
        config.view_formats.push(output_format);
    }
    surface.configure(&gpu.device, &config);
    let mut sky = render::SkyRenderer::new(&gpu.device, output_format, width, height);
    let startup = sky.take_work();
    let mut state = WebState {
        canvas,
        surface,
        config,
        output_format,
        gpu,
        sky,
        recorder: telemetry::FrameRecorder::new(),
        frames: 0,
    };
    // A recoverable surface error must never be reported as a presented frame.
    for _ in 0..8 {
        if state.draw()? {
            break;
        }
        let next_frame = js_sys::Promise::new(&mut |resolve, reject| {
            if let Err(error) = window.request_animation_frame(&resolve) {
                let _ = reject.call1(&JsValue::UNDEFINED, &error);
            }
        });
        JsFuture::from(next_frame).await?;
    }
    if state.frames == 0 {
        return Err(JsValue::from_str(
            "WebGPU surface did not present after 8 attempts",
        ));
    }
    let probe = render::render_probe(&state.gpu.device, &state.gpu.queue);
    let buffer = probe.buffer.clone();
    let mapped = js_sys::Promise::new(&mut |resolve, reject| {
        let resolve = resolve.clone();
        let reject = reject.clone();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| match result {
                Ok(()) => {
                    let _ = resolve.call0(&JsValue::UNDEFINED);
                }
                Err(error) => {
                    let _ =
                        reject.call1(&JsValue::UNDEFINED, &JsValue::from_str(&error.to_string()));
                }
            });
    });
    JsFuture::from(mapped).await?;
    let bytes = probe.buffer.slice(..).get_mapped_range();
    let colors = probe
        .validate(&bytes)
        .map_err(|error| JsValue::from_str(&error))?;
    drop(bytes);
    probe.buffer.unmap();
    let result = js_sys::Object::new();
    js_sys::Reflect::set(&result, &"ok".into(), &JsValue::TRUE)?;
    js_sys::Reflect::set(&result, &"colorEncoding".into(), &"srgb".into())?;
    js_sys::Reflect::set(
        &result,
        &"outputFormat".into(),
        &format!("{:?}", state.output_format).into(),
    )?;
    js_sys::Reflect::set(
        &result,
        &"canvasFormat".into(),
        &format!("{:?}", state.config.format).into(),
    )?;
    js_sys::Reflect::set(
        &result,
        &"frames".into(),
        &JsValue::from_f64(state.frames as f64),
    )?;
    js_sys::Reflect::set(
        &result,
        &"startupPipelineCompilations".into(),
        &JsValue::from_f64(startup.pipeline_compilations as f64),
    )?;
    // Keep diagnostic work visible without attributing it to a steady-state frame.
    js_sys::Reflect::set(
        &result,
        &"probePipelineCompilations".into(),
        &JsValue::from_f64(probe.setup_work.pipeline_compilations as f64),
    )?;
    js_sys::Reflect::set(
        &result,
        &"totalStartupPipelineCompilations".into(),
        &JsValue::from_f64(
            (startup.pipeline_compilations + probe.setup_work.pipeline_compilations) as f64,
        ),
    )?;
    let latest = state
        .recorder
        .iter()
        .next_back()
        .expect("presented frame recorded");
    js_sys::Reflect::set(
        &result,
        &"framePipelineCompilations".into(),
        &JsValue::from_f64(latest.work.pipeline_compilations as f64),
    )?;
    js_sys::Reflect::set(&result, &"sampleCount".into(), &render::MSAA_SAMPLES.into())?;
    for (name, pixel) in [("top", colors.top), ("bottom", colors.bottom)] {
        let array = js_sys::Array::new();
        for channel in pixel {
            array.push(&channel.into());
        }
        js_sys::Reflect::set(&result, &name.into(), &array)?;
    }
    animate(state)?;
    Ok(result.into())
}
