//! Worker entry uses public classified water inputs only; no DOM or network API.
use wasm_bindgen::prelude::*;
#[wasm_bindgen]
pub fn execute_water_task(
    request: &[u8],
    control: Option<js_sys::Int32Array>,
) -> Result<Vec<u8>, JsValue> {
    let cancelled = || {
        control
            .as_ref()
            .is_some_and(|flags| js_sys::Atomics::load(flags, 0) != Ok(0))
    };
    client::water_source::execute_water_request(request, cancelled)
        .map_err(|error| JsValue::from_str(&error.to_string()))
}
