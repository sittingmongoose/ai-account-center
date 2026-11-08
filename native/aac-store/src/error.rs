//! One error type for the store. `code` is a fixed word the TypeScript side matches on;
//! `message` is for logs and never carries payload content.

use std::fmt;

pub struct Error {
    pub code: &'static str,
    pub message: String,
}

impl Error {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Error {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// Every library error (I/O, redb, serde) becomes `io` unless a call site maps it first.
/// `Error` itself does not implement `std::error::Error`, so this blanket impl is coherent.
impl<E: std::error::Error + Send + Sync + 'static> From<E> for Error {
    fn from(error: E) -> Self {
        Error::new("io", error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[macro_export]
macro_rules! fail {
    ($code:expr, $($arg:tt)*) => {
        return Err($crate::error::Error::new($code, format!($($arg)*)))
    };
}
