//! `ai-account-center dashboard proxy <command>`: the LAN HTTPS reverse proxy, ported from
//! src/commands/dashboard-proxy-command.ts. Same messages, exit codes, validation order and
//! backup behaviour; the config save goes through the product writer (cli/config_write.rs).

use super::{config_write, tls};
use serde_json::{Value, json};
use std::path::Path;

const LAN_KIND: &str = "lan-https-proxy";
const LIVE_NOTE: &str = "The running dashboard reads config.yaml on every request, so this applies to its next request; no restart is needed.\n";

pub fn help() -> String {
    [
        "LAN HTTPS reverse proxy (an HTTPS proxy on another computer in front of the dashboard)",
        "",
        "Usage: ai-account-center dashboard proxy <command>",
        "",
        "Commands:",
        "  status                                   Show whether the proxy is trusted, and why not",
        "  set --address <ip> --origin <https url>  Trust the proxy at <ip> (repeat --address, up to 8)",
        "  off                                      Stop trusting the proxy",
        "",
        "Options for set:",
        "  --address <ip>    The proxy's exact LAN address as the dashboard sees it: a private",
        "                    address, not a range, not loopback and not this computer",
        "  --origin <url>    The public https:// address the proxy serves, with no path",
        "  --replace         Replace a local proxy kind (tailscale-serve, loopback-https-proxy)",
        "",
        "Example:",
        "  ai-account-center dashboard proxy set --address 192.168.1.20 --origin https://aac.example.test",
        "",
        "set and off save the previous config.yaml next to it as config.yaml.bak-proxy-<UTC time> (0600).",
        "The running dashboard applies the change to its next request; no restart is needed.",
        "",
    ]
    .join("\n")
}

fn out(text: &str) {
    print!("{text}");
}

fn err(text: &str) {
    eprint!("{text}");
}

struct SetArgs {
    addresses: Vec<String>,
    origin: Option<String>,
    replace: bool,
}

fn take_value(args: &[String], index: &mut usize, inline: &Option<String>) -> Option<String> {
    if let Some(value) = inline {
        return Some(value.clone());
    }
    let next = args.get(*index + 1)?;
    if next.starts_with("--") {
        return None;
    }
    *index += 1;
    Some(next.clone())
}

fn parse_set_args(args: &[String]) -> Result<SetArgs, String> {
    let mut result = SetArgs {
        addresses: Vec::new(),
        origin: None,
        replace: false,
    };
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let (flag, inline) = match arg.find('=') {
            Some(position) => (&arg[..position], Some(arg[position + 1..].to_string())),
            None => (arg.as_str(), None),
        };
        if flag == "--replace" && inline.is_none() {
            result.replace = true;
        } else if flag == "--address" {
            match take_value(args, &mut index, &inline) {
                Some(address) => result.addresses.push(address),
                None => return Err("--address needs a value".into()),
            }
        } else if flag == "--origin" {
            match take_value(args, &mut index, &inline) {
                Some(origin) => {
                    if result.origin.is_some() {
                        return Err("--origin may be given once".into());
                    }
                    result.origin = Some(origin);
                }
                None => return Err("--origin needs a value".into()),
            }
        } else {
            return Err(format!("unexpected argument: {arg}"));
        }
        index += 1;
    }
    if result.addresses.is_empty() {
        return Err("--address is required".into());
    }
    if result.origin.is_none() {
        return Err("--origin is required".into());
    }
    Ok(result)
}

fn same_list(left: Option<&Value>, right: &[String]) -> bool {
    left.and_then(Value::as_array).is_some_and(|items| {
        items.len() == right.len()
            && items
                .iter()
                .zip(right)
                .all(|(item, address)| item.as_str() == Some(address.as_str()))
    })
}

fn show_status(file: &Path) -> i32 {
    let block = match config_write::read_tls_block(file) {
        Ok(block) => block,
        Err(()) => {
            err("[X] config.yaml cannot be read; nothing is trusted until it can.\n");
            return 1;
        }
    };
    let settings = tls::parse_settings(&block, &tls::this_computer_addresses());
    if settings.trusted_proxy.as_deref() == Some(LAN_KIND) {
        out("LAN HTTPS proxy: on\n");
        out(&format!(
            "  proxy address: {}\n",
            settings.trusted_proxy_addresses.join(", ")
        ));
        out(&format!(
            "  public origin: {}\n",
            settings.public_origin.as_deref().unwrap_or("not set")
        ));
        out(
            "  Requests from that address count as secure only with X-Forwarded-Proto: https and a client IP as the last X-Forwarded-For entry.\n",
        );
        return 0;
    }
    if let Some(problem) = &settings.trusted_proxy_problem {
        out(&format!(
            "LAN HTTPS proxy: off. dashboard_tls.trusted_proxy_addresses was refused: {}.\n",
            tls::describe(problem)
        ));
        out(
            "Fix it with: ai-account-center dashboard proxy set --address <ip> --origin <https url>\n",
        );
        return 0;
    }
    if let Some(kind) = &settings.trusted_proxy {
        out(&format!(
            "LAN HTTPS proxy: off. dashboard_tls.trusted_proxy is {kind} (a local proxy on this computer).\n"
        ));
        return 0;
    }
    out(
        "LAN HTTPS proxy: off. Turn it on: ai-account-center dashboard proxy set --address <ip> --origin <https url>\n",
    );
    0
}

fn set_proxy(args: &[String], file: &Path) -> i32 {
    let parsed = match parse_set_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            err(&format!("[X] proxy set: {message}.\n{}", help()));
            return 1;
        }
    };
    let raw = Value::Array(
        parsed
            .addresses
            .iter()
            .map(|address| Value::String(address.clone()))
            .collect(),
    );
    let list = match tls::parse_trusted_proxy_addresses(&raw, &tls::this_computer_addresses()) {
        Ok(list) => list,
        Err(problem) => {
            err(&format!(
                "[X] Not saved: {}. Give the proxy's exact private LAN address (up to {}).\n",
                tls::describe(&problem),
                tls::ADDRESSES_MAX
            ));
            return 1;
        }
    };
    let Some(origin) = parsed
        .origin
        .clone()
        .and_then(|text| tls::parse_public_origin(&Value::String(text)))
    else {
        err(
            "[X] Not saved: --origin must be an https:// address with no path, user or query, like https://aac.example.test\n",
        );
        return 1;
    };
    let before = match config_write::read_tls_block(file) {
        Ok(block) => block,
        Err(()) => {
            err("[X] config.yaml cannot be read, so nothing was changed.\n");
            return 1;
        }
    };
    let kind = before.get("trusted_proxy").and_then(Value::as_str);
    if let Some(kind) = kind
        && matches!(kind, "tailscale-serve" | "loopback-https-proxy")
        && !parsed.replace
    {
        err(&format!(
            "[X] Not saved: dashboard_tls.trusted_proxy is {kind}, and only one proxy kind works at a time. Add --replace to switch to the LAN HTTPS proxy.\n"
        ));
        return 1;
    }
    if kind == Some(LAN_KIND)
        && same_list(before.get("trusted_proxy_addresses"), &list)
        && before.get("public_origin").and_then(Value::as_str) == Some(origin.as_str())
    {
        out("[i] The LAN HTTPS proxy is already set to these values; nothing changed.\n");
        return 0;
    }
    let saved = config_write::mutate(file, |config| {
        let mut existing = config
            .get("dashboard_tls")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        existing.insert("trusted_proxy".into(), json!(LAN_KIND));
        existing.insert(
            "trusted_proxy_addresses".into(),
            Value::Array(list.iter().map(|address| json!(address)).collect()),
        );
        existing.insert("public_origin".into(), json!(origin));
        config.insert("dashboard_tls".into(), Value::Object(existing));
    });
    let backup = match saved {
        Ok(backup) => backup,
        Err(_) => {
            err("[X] config.yaml could not be saved; the previous settings stay in place.\n");
            return 1;
        }
    };
    out(&format!(
        "[OK] LAN HTTPS proxy on: address {}, public origin {origin}.\n",
        list.join(", ")
    ));
    match backup {
        Some(path) => out(&format!(
            "[i] The previous config.yaml is saved as {} (0600).\n",
            config_write::display_path(&path)
        )),
        None => out("[i] There was no config.yaml before, so there is no backup.\n"),
    }
    out(LIVE_NOTE);
    0
}

fn proxy_off(file: &Path) -> i32 {
    let before = match config_write::read_tls_block(file) {
        Ok(block) => block,
        Err(()) => {
            err("[X] config.yaml cannot be read, so nothing was changed.\n");
            return 1;
        }
    };
    let lan_kind = before.get("trusted_proxy").and_then(Value::as_str) == Some(LAN_KIND);
    let has_addresses = before
        .as_object()
        .is_some_and(|block| block.contains_key("trusted_proxy_addresses"));
    if !lan_kind && !has_addresses {
        out("[i] The LAN HTTPS proxy is already off; nothing changed.\n");
        return 0;
    }
    let saved = config_write::mutate(file, |config| {
        let mut block = config
            .get("dashboard_tls")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if block.get("trusted_proxy").and_then(Value::as_str) == Some(LAN_KIND) {
            block.remove("trusted_proxy");
            // The public origin was the proxy's address; it means nothing without it.
            block.remove("public_origin");
        }
        block.remove("trusted_proxy_addresses");
        if block.is_empty() {
            config.remove("dashboard_tls");
        } else {
            config.insert("dashboard_tls".into(), Value::Object(block));
        }
    });
    let backup = match saved {
        Ok(backup) => backup,
        Err(_) => {
            err("[X] config.yaml could not be saved; the previous settings stay in place.\n");
            return 1;
        }
    };
    out("[OK] LAN HTTPS proxy off: requests from its address are ordinary LAN requests again.\n");
    if let Some(path) = backup {
        out(&format!(
            "[i] The previous config.yaml is saved as {} (0600).\n",
            config_write::display_path(&path)
        ));
    }
    out(LIVE_NOTE);
    0
}

pub fn run(args: &[String], root: &Path) -> i32 {
    let file = root.join("config.yaml");
    let Some((first, rest)) = args.split_first() else {
        out(&help());
        return 0;
    };
    if matches!(first.as_str(), "help" | "--help" | "-h") {
        out(&help());
        return 0;
    }
    if !matches!(first.as_str(), "status" | "set" | "off") {
        err(&format!("[X] Unknown proxy command: {first}\n{}", help()));
        return 1;
    }
    if rest
        .iter()
        .any(|arg| matches!(arg.as_str(), "help" | "--help" | "-h"))
    {
        out(&help());
        return 0;
    }
    if first == "set" {
        return set_proxy(rest, &file);
    }
    if !rest.is_empty() {
        err(&format!(
            "[X] Unexpected arguments for proxy {first}: {}\n",
            rest.join(" ")
        ));
        return 1;
    }
    if first == "status" {
        return show_status(&file);
    }
    proxy_off(&file)
}
