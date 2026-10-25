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
