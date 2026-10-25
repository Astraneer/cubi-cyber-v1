//! Real-browser contract harness. It invokes the application's public generator
//! through the Rust worker transport and compares every byte with a native fixture.

#[cfg(target_arch = "wasm32")]
mod probe {
    use jobs::{
        BrowserEvent, BrowserOutcome, BrowserSubmitError, BrowserTransport, BrowserWorkerPool,
        JobId,
    };
    use js_sys::{Date, Object, Promise, Reflect};
    use wasm_bindgen::{JsValue, prelude::*};
    use wasm_bindgen_futures::JsFuture;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = globalThis, js_name = setTimeout)]
        fn set_timeout(callback: &js_sys::Function, milliseconds: i32) -> i32;
    }

    async fn turn() -> Result<(), JsValue> {
        JsFuture::from(Promise::new(&mut |resolve, _| {
            set_timeout(&resolve, 1);
        }))
        .await?;
        Ok(())
    }

    async fn ready(pool: &mut BrowserWorkerPool) -> Result<(), JsValue> {
        let deadline = Date::now() + 10_000.0;
        while pool.available_workers() == 0 {
            if let Some(event) = pool.try_recv() {
                return Err(JsValue::from_str(&format!(
                    "unexpected boot event: {event:?}"
                )));
            }
            if Date::now() >= deadline {
                return Err("snapshot worker boot timed out".into());
            }
            turn().await?;
        }
        Ok(())
    }

    async fn completed(pool: &BrowserWorkerPool) -> Result<(), JsValue> {
        let deadline = Date::now() + 10_000.0;
        while pool.completed_len() == 0 {
            if Date::now() >= deadline {
                return Err("snapshot worker completion timed out".into());
            }
            turn().await?;
        }
        Ok(())
    }

    fn submit(pool: &mut BrowserWorkerPool, id: u64, bytes: &[u8]) -> Result<(), JsValue> {
        pool.try_submit(JobId(id), bytes, 1024 * 1024)
            .map_err(|error| format!("snapshot submit failed: {error:?}").into())
    }

    fn check_bytes(pool: &mut BrowserWorkerPool, id: u64, expected: &[u8]) -> Result<(), JsValue> {
        match pool.try_recv() {
            Some(BrowserEvent::Completion {
                id: actual,
                outcome: BrowserOutcome::Bytes(bytes),
            }) if actual == JobId(id) => {
                if bytes == expected {
                    return Ok(());
                }
                let first = bytes.iter().zip(expected).position(|(a, b)| a != b);
                Err(JsValue::from_str(&format!(
                    "snapshot differs: actual bytes {}, expected {}, first mismatch {first:?}",
                    bytes.len(),
                    expected.len()
                )))
            }
            event => Err(JsValue::from_str(&format!(
                "snapshot result differs from native fixture: {event:?}"
            ))),
        }
    }

    #[wasm_bindgen]
    pub async fn run_probe(
        worker_url: String,
        module_url: String,
        request: Vec<u8>,
        expected: Vec<u8>,
    ) -> Result<JsValue, JsValue> {
        if request.len() != 33 || expected.len() != 16_961 {
            return Err("invalid native fixture lengths".into());
        }
        let mut pool = BrowserWorkerPool::new(
            &worker_url,
            &module_url,
            "generate_public_page",
            1,
            2 * 1024 * 1024,
        )?;
        ready(&mut pool).await?;
        let transport = match pool.transport() {
            BrowserTransport::Shared => "shared",
            BrowserTransport::Transfer => "transfer",
        };
        submit(&mut pool, 1, &request)?;
        if pool.try_submit(JobId(2), &request, 1024 * 1024)
            != Err(BrowserSubmitError::NoReadyWorker)
        {
            return Err("worker capacity was not enforced".into());
        }
        completed(&pool).await?;
        if pool.pending_len() != 1
            || pool.retained_bytes() != 1024 * 1024
            || pool.available_workers() != 0
        {
            return Err("completed job released its quota before collection".into());
        }
        check_bytes(&mut pool, 1, &expected)?;
        if pool.pending_len() != 0 || pool.retained_bytes() != 0 || pool.available_workers() != 1 {
            return Err("collection did not release worker quota".into());
        }
        // Reusing the worker must preserve the exact native page bytes.
        submit(&mut pool, 2, &request)?;
        completed(&pool).await?;
        check_bytes(&mut pool, 2, &expected)?;
        // Invalid snapshots fail as jobs, then a valid snapshot still works.
        submit(&mut pool, 3, &[0])?;
        completed(&pool).await?;
        if !matches!(
            pool.try_recv(),
            Some(BrowserEvent::Completion {
                id: JobId(3),
                outcome: BrowserOutcome::Failed(_)
            })
        ) {
            return Err("malformed snapshot did not report a job failure".into());
        }
        submit(&mut pool, 4, &request)?;
        if !pool.cancel(JobId(4))? {
            return Err("active snapshot could not be cancelled".into());
        }
        completed(&pool).await?;
        if !matches!(
            pool.try_recv(),
            Some(BrowserEvent::Completion {
                id: JobId(4),
                outcome: BrowserOutcome::Cancelled
            })
        ) {
            return Err("cancelled snapshot was published".into());
        }
        ready(&mut pool).await?;
        submit(&mut pool, 5, &request)?;
        completed(&pool).await?;
        check_bytes(&mut pool, 5, &expected)?;
        let result = Object::new();
        Reflect::set(&result, &"transport".into(), &transport.into())?;
        Reflect::set(
            &result,
            &"bytesCompared".into(),
            &JsValue::from_f64((expected.len() * 3) as f64),
        )?;
        Reflect::set(&result, &"jobsCompleted".into(), &5.into())?;
        Reflect::set(&result, &"cancelled".into(), &JsValue::TRUE)?;
        Reflect::set(&result, &"recovered".into(), &JsValue::TRUE)?;
        Ok(result.into())
    }
}
