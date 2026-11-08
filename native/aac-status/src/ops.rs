//! JSON method dispatch shared by the Node binding and the tests: one method name and a JSON
//! argument in, one JSON answer out. The readers are stateless, so there is no open/close and
//! no handle state (unlike aac-store's engine slot).

use crate::error::Result;
use crate::fail;

pub fn dispatch(method: &str, args: &str) -> Result<String> {
    let _ = (method, args);
    fail!("unknown_method", "unknown method")
}
