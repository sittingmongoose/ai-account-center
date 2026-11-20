use std::io::Write;
use tokio::io::{self, AsyncReadExt};

pub(crate) struct Pipe(tokio::sync::Mutex<io::Stdin>);

impl Pipe {
    pub(crate) fn new() -> Self {
        Self(tokio::sync::Mutex::new(io::stdin()))
    }

    pub(crate) async fn perform(&self, effect: &str) -> Result<bool, io::ErrorKind> {
        send(effect).map_err(|e| e.kind())?;
        let response = self.0.lock().await.read_u8().await.map_err(|e| e.kind())?;
        match response {
            b'y' => Ok(true),
            b'n' => Ok(false),
            b'e' => Err(io::ErrorKind::Other),
            _ => Err(io::ErrorKind::InvalidData),
        }
    }
}

fn send(message: &str) -> io::Result<()> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{message}")?;
    out.flush()
}

pub(crate) fn finish(result: Result<bool, io::ErrorKind>) -> io::Result<()> {
    send(match result {
        Ok(true) => "ok",
        Ok(false) => "rejected",
        Err(_) => "failed",
    })
}
