//! Local immutable source construction only; no DOM, network or generation.
use wasm_bindgen::prelude::*;
#[wasm_bindgen]
pub fn execute_gi_task(
    request: &[u8],
    control: Option<js_sys::Int32Array>,
) -> Result<Vec<u8>, JsValue> {
    client::gi_source::execute_gi_request(request, || {
        control
            .as_ref()
            .is_some_and(|flags| js_sys::Atomics::load(flags, 0) != Ok(0))
    })
    .map_err(|error| JsValue::from_str(&error.to_string()))
}
