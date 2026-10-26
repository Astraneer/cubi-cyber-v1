//! DOM-free immutable public model classification, executed only on a Worker.
use wasm_bindgen::prelude::*;
#[wasm_bindgen]
pub fn execute_props_task(
    request: &[u8],
    control: Option<js_sys::Int32Array>,
) -> Result<Vec<u8>, JsValue> {
    client::props_source::execute_props_request(request, || {
        control
            .as_ref()
            .is_some_and(|flags| js_sys::Atomics::load(flags, 0) != Ok(0))
    })
    .map_err(|error| JsValue::from_str(&format!("public model worker: {error:?}")))
}
