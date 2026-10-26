//! Public generation handler for the bounded worker pool; no DOM or authority dependency.
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn generate_public_page(
    request: &[u8],
    control: Option<js_sys::Int32Array>,
) -> Result<Vec<u8>, JsValue> {
    let task = terrain_gen::GenerationTask::decode(request)
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    let cancelled = || {
        control
            .as_ref()
            .is_some_and(|flags| js_sys::Atomics::load(flags, 0) != Ok(0))
    };
    task.execute_with_cancellation(cancelled)
        .map(|page| page.encode())
        .map_err(|error| JsValue::from_str(&error))
}

/// Public render data only; this module is initialized inside owned Workers
/// without starting a canvas, socket, audio context or product UI.
#[wasm_bindgen]
pub fn execute_horizon_task(
    request: &[u8],
    control: Option<js_sys::Int32Array>,
) -> Result<Vec<u8>, JsValue> {
    client::view::horizon::execute_background_request(request, || {
        control
            .as_ref()
            .is_some_and(|flags| js_sys::Atomics::load(flags, 0) != Ok(0))
    })
    .map_err(|error| JsValue::from_str(&error.to_string()))
}
