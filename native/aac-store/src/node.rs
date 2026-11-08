//! The Node binding: one handle, one async `call(method, json)`. Work runs on libuv's thread
//! pool, so no store call blocks the JavaScript event loop.

use crate::ops::{self, Slot};
use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::Arc;

/// Bumped when the binding's methods change; the TypeScript loader refuses a mismatch.
#[allow(dead_code)]
pub const BINDING_VERSION: u32 = 1;

#[napi]
#[allow(dead_code)]
pub fn binding_version() -> u32 {
    BINDING_VERSION
}

#[napi]
pub struct NativeStore {
    slot: Arc<Slot>,
}

#[napi]
impl NativeStore {
    #[napi(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        NativeStore {
            slot: Arc::new(Slot::new(None)),
        }
    }

    /// Resolves to the JSON answer; rejects with `<code>: <message>`.
    #[napi]
    pub fn call(&self, method: String, args: String) -> AsyncTask<Call> {
        AsyncTask::new(Call {
            slot: Arc::clone(&self.slot),
            method,
            args,
        })
    }
}

pub struct Call {
    slot: Arc<Slot>,
    method: String,
    args: String,
}

impl Task for Call {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        // A panic must never cross into Node: it becomes an ordinary rejected call.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ops::dispatch(&self.slot, &self.method, &self.args)
        }));
        match outcome {
            Ok(result) => result.map_err(|e| napi::Error::new(Status::GenericFailure, e.to_string())),
            Err(_) => Err(napi::Error::new(
                Status::GenericFailure,
                "panic: the store stopped this call".to_string(),
            )),
        }
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output)
    }
}
