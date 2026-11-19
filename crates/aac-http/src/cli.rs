//! Native public entry points. The shipped TS command remains the fallback until cutover.
use std::{
    env,
    net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs},
    path::{Path, PathBuf},
    process::Command,
};

const ROOT_HELP: &str = include_str!("cli/root-help.txt");
const DASHBOARD_HELP: &str = include_str!("cli/dashboard-help.txt");
const LEGACY_BINS: &[&str] = &["ccs-droid", "ccsd", "ccs-codex", "ccsx", "ccsxp"];

/// Dispatches the product CLI or a compatibility name chosen by argv[0].
pub fn run() -> i32 {
    let mut args = env::args_os();
    let binary = args.next().unwrap_or_default();
    let name = Path::new(&binary)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if LEGACY_BINS.contains(&name) {
        eprintln!("[X] {name} is a retired upstream CCS launcher.");
        eprintln!("    Use ai-account-center dashboard for accounts and usage.");
        eprintln!("    Use ai-account-center codex-auth show to inspect saved Codex logins.");
        eprintln!(
            "    Use ai-account-center codex-auth activate <saved-login> to activate the shared login."
        );
        eprintln!(
            "    Shell exports, isolated CODEX_HOME launches, Droid routing, and CLIProxy launches are retired."
        );
        return 1;
    }
    let Ok(raw) = args
        .map(|arg| arg.into_string())
        .collect::<Result<Vec<_>, _>>()
    else {
        return failure("Command arguments must be valid Unicode.");
    };
    match dispatch(&raw) {
        Ok(code) => code,
        Err(message) => failure(&message),
    }
}

fn failure(message: &str) -> i32 {
    eprintln!("[X] {message}");
    1
}

fn config_directory(args: &mut Vec<String>) -> Result<Option<PathBuf>, String> {
    let flags = args
        .iter()
        .enumerate()
        .filter(|(_, arg)| *arg == "--config-dir" || arg.starts_with("--config-dir="))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let index = match flags.as_slice() {
        [] => return Ok(None),
        [index] => *index,
        _ => return Err("--config-dir may be provided only once".into()),
    };
    let (value, count) = if let Some((_, value)) = args[index].split_once('=') {
        (Some(value), 1)
    } else {
        (args.get(index + 1).map(String::as_str), 2)
    };
    let value = value
        .filter(|value| !value.is_empty() && !value.starts_with('-'))
        .ok_or("--config-dir requires a path argument")?;
    let path = PathBuf::from(value);
    match path.metadata() {
        Ok(meta) if meta.is_dir() => (),
        Ok(_) => return Err(format!("Not a directory: {value}")),
        Err(_) => {
            eprintln!("[X] Config directory not found: {value}");
            eprintln!("[i] Create the directory first, then copy your config files into it.");
            return Err(String::new());
        }
    }
    let absolute = std::path::absolute(path).map_err(|error| error.to_string())?;
    args.drain(index..index + count);
    Ok(Some(absolute))
}

fn root(config_dir: Option<PathBuf>) -> Option<PathBuf> {
    config_dir
        .or_else(|| {
            env::var_os("CCS_DIR")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| {
            env::var_os("CCS_HOME")
                .filter(|value| !value.is_empty())
                .or_else(|| env::home_dir().map(PathBuf::into_os_string))
                .map(|home| PathBuf::from(home).join(".ccs"))
        })
}

fn dispatch(raw: &[String]) -> Result<i32, String> {
    let mut args = raw.to_vec();
    let config_dir = match config_directory(&mut args) {
        Err(message) if message.is_empty() => return Ok(1),
        result => result?,
    };
    let Some((command, rest)) = args.split_first() else {
        print!("{ROOT_HELP}");
        return Ok(0);
    };
    match (command.as_str(), rest) {
        ("help" | "--help" | "-h", []) => print!("{ROOT_HELP}"),
        ("help", [topic]) if matches!(topic.as_str(), "dashboard" | "config") => {
            print!("{DASHBOARD_HELP}");
        }
        ("version" | "--version" | "-v", []) => version(root(config_dir))?,
        ("version" | "--version" | "-v", args) => {
            return Err(format!("Unexpected version arguments: {}", args.join(" ")));
        }
        ("dashboard" | "config", args) => {
            let subcommand = args.first().map(String::as_str);
            if matches!(
                subcommand,
                Some("auth" | "usage-hub" | "usage-snapshot" | "proxy" | "store" | "cursor-key")
            ) {
                return typescript(raw);
            }
            if subcommand == Some("help")
                || args
                    .iter()
                    .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
            {
                print!("{DASHBOARD_HELP}");
            } else if let Some(name @ ("channels" | "image-analysis" | "thinking")) = subcommand {
                return Ok(retired(&format!("config {name}")));
            } else if env::var("AAC_RUST_DASHBOARD").as_deref() == Ok("1")
                && env::var("AAC_RUST_COMPOSE").as_deref() != Ok("1")
            {
                launch(args, root(config_dir))?;
            } else {
                return typescript(raw);
            }
        }
        ("codex-auth" | "antigravity" | "bar" | "help" | "-h" | "--help", _) => {
            return typescript(raw);
        }
        (command, _) => return Ok(retired(command)),
    }
    Ok(0)
}

fn version(root: Option<PathBuf>) -> Result<(), String> {
    let bin = env::current_exe().map_err(|error| error.to_string())?;
    let root =
        root.ok_or("Set CCS_HOME or CCS_DIR to select an account configuration directory.")?;
    println!("AI Account Center v{}", env!("AAC_VERSION"));
    println!("Installation: {}", bin.display());
    println!("Account configuration: {}", root.display());
    println!("Config: {}", root.join("config.json").display());
    println!("License: MIT; derived from CCS by Tam Nhu Tran (Kai).");
    println!("Run 'ai-account-center --help' for usage information.");
    Ok(())
}

fn retired(command: &str) -> i32 {
    if matches!(command, "update" | "--update") {
        eprintln!("[X] Upstream CCS self-update is retired in AI Account Center.");
        eprintln!("    Update AI Account Center from its own release or checkout.");
        eprintln!("    Installed application updates remain available in the dashboard.");
    } else {
        eprintln!("[X] \"{command}\" is an unsupported command or retired upstream CCS workflow.");
        eprintln!(
            "    AI Account Center manages accounts and usage; profile/model CLI launches are retired."
        );
    }
    eprintln!("    Use ai-account-center dashboard, codex-auth, bar, or --help.");
    1
}

fn typescript(args: &[String]) -> Result<i32, String> {
    let script = env::var_os("AAC_TS_CLI")
        .map(PathBuf::from)
        .or_else(|| {
            let exe = env::current_exe().ok()?;
            let directory = exe.parent()?;
            [
                directory.join("ccs.js"),
                directory.join("../dist/ccs.js"),
                directory.join("../../dist/ccs.js"),
            ]
            .into_iter()
            .find(|candidate| candidate.is_file())
        })
        .ok_or("Set AAC_TS_CLI to the shipped TypeScript CLI (dist/ccs.js).")?;
    let mut command = Command::new("node");
    command.arg(script).args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(format!(
            "Cannot start the TypeScript CLI: {}",
            command.exec()
        ))
    }
    #[cfg(not(unix))]
    {
        command
            .status()
            .map(|status| status.code().unwrap_or(1))
            .map_err(|error| format!("Cannot start the TypeScript CLI: {error}"))
    }
}

struct Listen {
    addresses: Vec<SocketAddr>,
    browser: bool,
}

fn listen(args: &[String]) -> Result<Listen, String> {
    let mut host = "localhost";
    let mut ports = vec![3000, 3001, 3002, 8000, 8080, 0];
    let mut browser = !cfg!(target_os = "linux");
    let mut unexpected_flags = Vec::new();
    let mut unexpected_words = Vec::new();
    let mut iter = args.iter().map(String::as_str).peekable();
    while let Some(arg) = iter.next() {
        let (flag, inline) = arg
            .split_once('=')
            .map_or((arg, None), |(flag, value)| (flag, Some(value)));
        match flag {
            "--port" | "-p" => {
                let value = inline.or_else(|| iter.next_if(|arg| !arg.starts_with('-')));
                let port = value
                    .filter(|value| value.bytes().all(|ch| ch.is_ascii_digit()))
                    .and_then(|value| value.parse::<u16>().ok())
                    .filter(|port| *port > 0)
                    .ok_or("Invalid port number")?;
                ports = vec![port];
            }
            "--host" | "-H" => {
                host = inline
                    .or_else(|| iter.next_if(|arg| !arg.starts_with('-')))
                    .map(str::trim)
                    .filter(|host| !host.is_empty())
                    .ok_or("Invalid host value")?;
            }
            "--no-open" if inline.is_none() => browser = false,
            "--dev" if inline.is_none() => {
                return Err("--dev is retired; use the AI Account Center dashboard.".into());
            }
            _ if arg.starts_with('-') => unexpected_flags.push(arg),
            _ => unexpected_words.push(arg),
        }
    }
    unexpected_flags.extend(unexpected_words);
    if !unexpected_flags.is_empty() {
        return Err(format!(
            "Unexpected arguments: {}",
            unexpected_flags.join(" ")
        ));
    }
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let ips = if host.eq_ignore_ascii_case("localhost") || host.eq_ignore_ascii_case("localhost.") {
        vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]
    } else {
        (host, 0)
            .to_socket_addrs()
            .map_err(|error| format!("Invalid dashboard host: {error}"))?
            .map(|address| address.ip())
            .collect()
    };
    let addresses = ports
        .into_iter()
        .flat_map(|port| ips.iter().map(move |ip| SocketAddr::new(*ip, port)))
        .collect();
    Ok(Listen { addresses, browser })
}

fn launch(args: &[String], root: Option<PathBuf>) -> Result<(), String> {
    let Listen { addresses, browser } = listen(args)?;
    let root = root
        .filter(|path| path.is_absolute())
        .ok_or("Rust dashboard mode requires an absolute CCS_DIR, CCS_HOME or --config-dir.")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(20)
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    println!("AI Account Center\n\n[i] Starting dashboard server...");
    let result = runtime.block_on(crate::public_auth::run_public_at(&addresses, root, browser));
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    result.map_err(|error| format!("Dashboard could not start: {error}"))
}

pub(crate) fn open_browser(address: SocketAddr) {
    let address = if address.ip().is_unspecified() {
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), address.port())
    } else {
        address
    };
    let url = format!("http://{address}");
    #[cfg(target_os = "windows")]
    let result = Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", &url])
        .spawn();
    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg(&url).spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let result = Command::new("xdg-open").arg(&url).spawn();
    if result.is_err() {
        eprintln!("[!] Open {url} in your browser.");
    }
}
