//! Loopback HTTP/1.1 keep-alive client with truncation detection and content decoding.

use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use std::collections::BTreeMap;
use std::error::Error;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const IO_SLICE: Duration = Duration::from_millis(200);

/// Raw HTTP/1.1 response returned by [`HttpClient`].
#[derive(Debug, Clone)]
pub struct HttpResponse {
    /// Numeric HTTP status code (e.g. `200`).
    pub status: u16,
    /// Lowercased response headers (multi-value headers joined or collected in `set_cookies`).
    pub headers: BTreeMap<String, String>,
    /// All `Set-Cookie` header values in arrival order.
    pub set_cookies: Vec<String>,
    /// Exact wire response body bytes (before content-encoding decompression).
    pub body: Vec<u8>,
}

/// Single-socket HTTP/1.1 keep-alive client targeting `127.0.0.1:<port>`.
#[derive(Debug)]
pub struct HttpClient {
    port: u16,
    keep_alive: bool,
    stream: Option<TcpStream>,
    buf: Vec<u8>,
}

impl HttpClient {
    /// Create a client targeting loopback `port`.
    pub fn new(port: u16, keep_alive: bool) -> Self {
        Self {
            port,
            keep_alive,
            stream: None,
            buf: Vec::new(),
        }
    }

    /// Execute one HTTP/1.1 request, honoring `timeout` and `abort`.
    pub fn request(
        &mut self,
        method: &str,
        req_path: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
        timeout: Duration,
        abort: &AtomicBool,
    ) -> Result<HttpResponse, Box<dyn Error>> {
        if abort.load(Ordering::Acquire) {
            return Err(format!("HTTP request aborted ({method} {req_path})").into());
        }
        let deadline = Instant::now() + timeout;
        let reused = self.stream.is_some();

        match self.request_once(method, req_path, headers, body, deadline, timeout, abort) {
            Ok(resp) => Ok(resp),
            Err(first_err)
                if reused && !abort.load(Ordering::Acquire) && Instant::now() < deadline =>
            {
                // Retry once on a fresh socket if the server closed an idle keep-alive socket.
                self.reset_socket();
                self.request_once(method, req_path, headers, body, deadline, timeout, abort)
                    .map_err(|_| first_err)
            }
            Err(err) => {
                self.reset_socket();
                Err(err)
            }
        }
    }

    fn reset_socket(&mut self) {
        self.stream = None;
        self.buf.clear();
    }

    #[allow(clippy::too_many_arguments)]
    fn request_once(
        &mut self,
        method: &str,
        req_path: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
        deadline: Instant,
        timeout: Duration,
        abort: &AtomicBool,
    ) -> Result<HttpResponse, Box<dyn Error>> {
        if self.stream.is_none() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "HTTP deadline exceeded after {} ms ({method} {req_path})",
                    timeout.as_millis()
                )
                .into());
            }
            let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
            let stream = TcpStream::connect_timeout(&addr, remaining.min(Duration::from_secs(5)))?;
            let _ = stream.set_nodelay(true);
            self.stream = Some(stream);
            self.buf.clear();
        }

        let mut req = Vec::with_capacity(256 + body.map_or(0, <[u8]>::len));
        write!(req, "{method} {req_path} HTTP/1.1\r\n")?;
        let mut has_host = false;
        let mut has_connection = false;
        let mut has_content_length = false;
        for (name, value) in headers {
            if name.eq_ignore_ascii_case("host") {
                has_host = true;
            } else if name.eq_ignore_ascii_case("connection") {
                has_connection = true;
            } else if name.eq_ignore_ascii_case("content-length") {
                has_content_length = true;
            }
            write!(req, "{name}: {value}\r\n")?;
        }
        if !has_host {
            write!(req, "Host: 127.0.0.1:{}\r\n", self.port)?;
        }
        if !has_connection {
            if self.keep_alive {
                req.extend_from_slice(b"Connection: keep-alive\r\n");
            } else {
                req.extend_from_slice(b"Connection: close\r\n");
            }
        }
        if let Some(payload) = body
            && !has_content_length
        {
            write!(req, "Content-Length: {}\r\n", payload.len())?;
        }
        req.extend_from_slice(b"\r\n");
        if let Some(payload) = body {
            req.extend_from_slice(payload);
        }

        self.write_all_checked(&req, method, req_path, deadline, timeout, abort)?;
        let header_end = self.read_until_header_end(method, req_path, deadline, timeout, abort)?;
        let header_bytes = self.buf[..header_end].to_vec();
        self.buf.drain(..header_end + 4);

        let header_text = String::from_utf8_lossy(&header_bytes);
        let mut lines = header_text.split("\r\n");
        let status_line = lines
            .next()
            .ok_or_else(|| format!("Malformed HTTP status line ({method} {req_path})"))?;
        let status = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(|| format!("Invalid HTTP status line: {status_line}"))?;

        let mut resp_headers = BTreeMap::new();
        let mut set_cookies = Vec::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            if let Some((name, val)) = line.split_once(':') {
                let key = name.trim().to_ascii_lowercase();
                let value = val.trim().to_string();
                if key == "set-cookie" {
                    set_cookies.push(value.clone());
                }
                resp_headers
                    .entry(key)
                    .and_modify(|existing: &mut String| {
                        existing.push_str(", ");
                        existing.push_str(&value);
                    })
                    .or_insert(value);
            }
        }

        let is_chunked = resp_headers
            .get("transfer-encoding")
            .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
        let content_length = resp_headers
            .get("content-length")
            .and_then(|v| v.trim().parse::<usize>().ok());
        let connection_close = resp_headers
            .get("connection")
            .is_some_and(|v| v.to_ascii_lowercase().contains("close"));

        let resp_body = if method.eq_ignore_ascii_case("HEAD")
            || status == 204
            || status == 304
            || (100..200).contains(&status)
        {
            Vec::new()
        } else if is_chunked {
            self.read_chunked_body(method, req_path, deadline, timeout, abort)?
        } else if let Some(len) = content_length {
            self.read_exact_body(len, method, req_path, deadline, timeout, abort)?
        } else {
            let body = self.read_to_eof(method, req_path, deadline, timeout, abort)?;
            self.reset_socket();
            body
        };

        if !self.keep_alive || connection_close {
            self.reset_socket();
        }

        Ok(HttpResponse {
            status,
            headers: resp_headers,
            set_cookies,
            body: resp_body,
        })
    }

    fn write_all_checked(
        &mut self,
        data: &[u8],
        method: &str,
        req_path: &str,
        deadline: Instant,
        timeout: Duration,
        abort: &AtomicBool,
    ) -> Result<(), Box<dyn Error>> {
        let mut written = 0;
        while written < data.len() {
            if abort.load(Ordering::Acquire) {
                return Err(format!("HTTP request aborted ({method} {req_path})").into());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "HTTP deadline exceeded after {} ms ({method} {req_path})",
                    timeout.as_millis()
                )
                .into());
            }
            let stream = self
                .stream
                .as_mut()
                .ok_or("HTTP socket unexpectedly absent")?;
            stream.set_write_timeout(Some(remaining.min(IO_SLICE)))?;
            match stream.write(&data[written..]) {
                Ok(0) => {
                    return Err(format!(
                        "HTTP connection closed while writing ({method} {req_path})"
                    )
                    .into());
                }
                Ok(n) => written += n,
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(err) => return Err(err.into()),
            }
        }
        Ok(())
    }

    fn read_more(
        &mut self,
        method: &str,
        req_path: &str,
        deadline: Instant,
        timeout: Duration,
        abort: &AtomicBool,
    ) -> Result<usize, Box<dyn Error>> {
        let mut tmp = [0_u8; 8192];
        loop {
            if abort.load(Ordering::Acquire) {
                return Err(format!("HTTP request aborted ({method} {req_path})").into());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "HTTP deadline exceeded after {} ms ({method} {req_path})",
                    timeout.as_millis()
                )
                .into());
            }
            let stream = self
                .stream
                .as_mut()
                .ok_or("HTTP socket unexpectedly absent")?;
            stream.set_read_timeout(Some(remaining.min(IO_SLICE)))?;
            match stream.read(&mut tmp) {
                Ok(0) => return Ok(0),
                Ok(n) => {
                    self.buf.extend_from_slice(&tmp[..n]);
                    return Ok(n);
                }
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(err) => return Err(err.into()),
            }
        }
    }

    fn read_until_header_end(
        &mut self,
        method: &str,
        req_path: &str,
        deadline: Instant,
        timeout: Duration,
        abort: &AtomicBool,
    ) -> Result<usize, Box<dyn Error>> {
        loop {
            if let Some(pos) = find_subslice(&self.buf, b"\r\n\r\n") {
                return Ok(pos);
            }
            if self.buf.len() > 256 * 1024 {
                return Err(
                    format!("HTTP response headers too large ({method} {req_path})").into(),
                );
            }
            if self.read_more(method, req_path, deadline, timeout, abort)? == 0 {
                return Err(format!(
                    "HTTP response closed before completion ({method} {req_path})"
                )
                .into());
            }
        }
    }

    fn read_exact_body(
        &mut self,
        len: usize,
        method: &str,
        req_path: &str,
        deadline: Instant,
        timeout: Duration,
        abort: &AtomicBool,
    ) -> Result<Vec<u8>, Box<dyn Error>> {
        while self.buf.len() < len {
            if self.read_more(method, req_path, deadline, timeout, abort)? == 0 {
                return Err(format!(
                    "HTTP response closed before completion ({method} {req_path})"
                )
                .into());
            }
        }
        let body = self.buf[..len].to_vec();
        self.buf.drain(..len);
        Ok(body)
    }

    fn read_to_eof(
        &mut self,
        method: &str,
        req_path: &str,
        deadline: Instant,
        timeout: Duration,
        abort: &AtomicBool,
    ) -> Result<Vec<u8>, Box<dyn Error>> {
        loop {
            if self.read_more(method, req_path, deadline, timeout, abort)? == 0 {
                return Ok(std::mem::take(&mut self.buf));
            }
        }
    }

    fn read_line_crlf(
        &mut self,
        method: &str,
        req_path: &str,
        deadline: Instant,
        timeout: Duration,
        abort: &AtomicBool,
    ) -> Result<String, Box<dyn Error>> {
        loop {
            if let Some(pos) = find_subslice(&self.buf, b"\r\n") {
                let line = String::from_utf8_lossy(&self.buf[..pos]).into_owned();
                self.buf.drain(..pos + 2);
                return Ok(line);
            }
            if self.read_more(method, req_path, deadline, timeout, abort)? == 0 {
                return Err(format!(
                    "HTTP response closed before completion ({method} {req_path})"
                )
                .into());
            }
        }
    }

    fn read_chunked_body(
        &mut self,
        method: &str,
        req_path: &str,
        deadline: Instant,
        timeout: Duration,
        abort: &AtomicBool,
    ) -> Result<Vec<u8>, Box<dyn Error>> {
        let mut out = Vec::new();
        loop {
            let size_line = self.read_line_crlf(method, req_path, deadline, timeout, abort)?;
            let hex_part = size_line.split(';').next().unwrap_or("").trim();
            let chunk_len = usize::from_str_radix(hex_part, 16).map_err(|_| {
                format!("Invalid HTTP chunk size '{size_line}' ({method} {req_path})")
            })?;
            if chunk_len == 0 {
                loop {
                    let trailer =
                        self.read_line_crlf(method, req_path, deadline, timeout, abort)?;
                    if trailer.is_empty() {
                        break;
                    }
                }
                return Ok(out);
            }
            let chunk =
                self.read_exact_body(chunk_len + 2, method, req_path, deadline, timeout, abort)?;
            if &chunk[chunk_len..] != b"\r\n" {
                return Err(
                    format!("Malformed HTTP chunk terminator ({method} {req_path})").into(),
                );
            }
            out.extend_from_slice(&chunk[..chunk_len]);
        }
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Decode a compressed Analytics response body (`""`, `"identity"`, `"gzip"`, `"deflate"`, `"br"`).
pub fn decode_content_encoding(encoding: &str, body: &[u8]) -> Result<Vec<u8>, Box<dyn Error>> {
    let normalized = encoding.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "" | "identity" => Ok(body.to_vec()),
        "gzip" => {
            let mut decoder = GzDecoder::new(body);
            let mut out = Vec::new();
            decoder.read_to_end(&mut out)?;
            Ok(out)
        }
        "deflate" => {
            let mut decoder = ZlibDecoder::new(body);
            let mut out = Vec::new();
            if decoder.read_to_end(&mut out).is_ok() {
                return Ok(out);
            }
            let mut raw_decoder = DeflateDecoder::new(body);
            let mut raw_out = Vec::new();
            raw_decoder.read_to_end(&mut raw_out)?;
            Ok(raw_out)
        }
        "br" => {
            let mut decoder = brotli_decompressor::Decompressor::new(body, 4096);
            let mut out = Vec::new();
            decoder.read_to_end(&mut out)?;
            Ok(out)
        }
        other => Err(format!("Unsupported Analytics content encoding: {other}").into()),
    }
}
