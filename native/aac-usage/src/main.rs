//! `aac-usage-scan --version` prints `{name, version, protocol, target}`. Otherwise one JSON request
//! on stdin, one JSON response on stdout (protocol.rs). A refused request exits 1 with one line on
//! stderr; no path or log content is ever printed.

use std::io::Read;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--version") {
        println!("{}", aac_usage::protocol::version_json());
        return;
    }
    let mut raw = Vec::new();
    if std::io::stdin()
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)
        .is_err()
    {
        eprintln!("aac-usage-scan: request could not be read");
        std::process::exit(1);
    }
    let request = match aac_usage::protocol::parse_request(&raw) {
        Ok(request) => request,
        Err(message) => {
            eprintln!("aac-usage-scan: {message}");
            std::process::exit(1);
        }
    };
    aac_usage::lower_priority();
    // Windows names variables in any case (`Path`): keys are upper-cased there, and every lookup
    // uses upper case (FIX-ROUND-BIG 12).
    let env = aac_usage::discover::env_from(
        std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))),
    );
    let Some(home) = aac_usage::home_dir(&env).filter(|h| h.is_dir()) else {
        eprintln!("aac-usage-scan: home directory is unavailable");
        std::process::exit(1);
    };
    // The last guard (FIX-ROUND-BIG 9): should anything still block past the deadline (a mount
    // that dies mid-scan), the watchdog answers for the scan, every kind "pending", so the server
    // keeps what it had and names the stall, and the process ends without waiting for it.
    aac_usage::protocol::watchdog(&request);
    let response = aac_usage::protocol::scan(&request, &home, &env);
    aac_usage::protocol::answer(&response);
}
