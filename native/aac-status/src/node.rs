//! The Node binding: one handle, one async `call(method, json)`. Work runs on libuv's thread
//! pool, so no reader call blocks the JavaScript event loop. Copied from native/aac-store.

use crate::ops;
use napi::bindgen_prelude::*;
use napi_derive::napi;

/// Bumped when the binding's methods change; the TypeScript loader refuses a mismatch.
#[allow(dead_code)]
pub const BINDING_VERSION: u32 = 1;

#[napi]
#[allow(dead_code)]
pub fn binding_version() -> u32 {
    BINDING_VERSION
}

#[napi]
pub struct NativeStatus;

#[napi]
impl NativeStatus {
    #[napi(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        NativeStatus
    }

    /// Resolves to the JSON answer; rejects with `<code>: <message>`.
    #[napi]
    pub fn call(&self, method: String, args: String) -> AsyncTask<Call> {
        AsyncTask::new(Call { method, args })
    }
}

pub struct Call {
    method: String,
    args: String,
}

impl Task for Call {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        // A panic must never cross into Node: it becomes an ordinary rejected call.
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ops::dispatch(&self.method, &self.args)));
        match outcome {
            Ok(result) => result.map_err(|e| napi::Error::new(Status::GenericFailure, e.to_string())),
            Err(_) => Err(napi::Error::new(
                Status::GenericFailure,
                "panic: the status module stopped this call".to_string(),
            )),
        }
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output)
    }
}
