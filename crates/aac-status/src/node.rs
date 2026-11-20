//! The Node binding: one handle, one async `call(method, json)`. Work runs on libuv's thread
//! pool, so no reader call blocks the JavaScript event loop. Copied from crates/aac-store.

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

// The one secret filter (owner, 2026-11-19: exactly one, the Rust one). TypeScript calls these
// synchronously until the cut (src/shared/secret-policy.ts, typed-secrets.ts): each is a pure
// scan of the text it is given, linear and bounded, so it runs on the caller's thread. A panic
// never crosses into Node: it is an error the caller treats as "could not be checked".

#[allow(dead_code)]
fn guarded<T>(work: impl FnOnce() -> T) -> napi::Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
        .map_err(|_| napi::Error::new(Status::GenericFailure, "panic: the secret filter stopped".to_string()))
}

/// `findSecret`: the kind of secret a text contains, or null.
#[napi]
#[allow(dead_code)]
pub fn find_secret(text: String) -> napi::Result<Option<String>> {
    guarded(|| crate::secret_policy::find_secret(&text).map(str::to_string))
}

/// `findSecretIn`: the check over a file's text and its parsed value (passed as JSON text).
#[napi]
#[allow(dead_code)]
pub fn find_secret_in(source: String, value_json: String) -> napi::Result<Option<String>> {
    let value: serde_json::Value = serde_json::from_str(&value_json)
        .map_err(|_| napi::Error::new(Status::InvalidArg, "the value is not JSON text".to_string()))?;
    guarded(|| crate::secret_policy::find_secret_in(&source, &value).map(str::to_string))
}

/// `findTypedSecret`: a secret typed into an owner's note, or null.
#[napi]
#[allow(dead_code)]
pub fn find_typed_secret(text: String) -> napi::Result<Option<String>> {
    guarded(|| crate::typed_secrets::find_typed_secret(&text).map(str::to_string))
}
