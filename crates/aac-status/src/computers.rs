//! Package-owned computers; never accepts host aliases, paths or commands from callers.
use serde_json::Value;
use std::sync::LazyLock;

static COMPUTERS: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../web-dashboard/public/computers.json"))
        .expect("packaged computer registry")
});

/// The packaged registry in its shipped order (`DASHBOARD_HOSTS` is its key list).
pub fn registry() -> &'static Value {
    &COMPUTERS
}

fn find_computer<'a>(registry: &'a Value, machine: &str) -> Option<&'a Value> {
    registry
        .as_object()?
        .values()
        .find(|row| row["machineId"].as_str() == Some(machine))
}

pub fn computer(machine: &str) -> Option<&'static Value> {
    find_computer(&COMPUTERS, machine)
}

pub fn known_machine(machine: &str, remote_only: bool) -> bool {
    computer(machine).is_some_and(|row| !remote_only || row["sshAlias"].is_string())
}

/// Every registered computer in its shipped order: `(machineId, os, sshAlias)`.
#[must_use]
pub fn machine_rows() -> Vec<(&'static str, &'static str, Option<&'static str>)> {
    COMPUTERS
        .as_object()
        .map(|rows| {
            rows.values()
                .filter_map(|row| {
                    Some((
                        row["machineId"].as_str()?,
                        row["os"].as_str()?,
                        row["sshAlias"].as_str(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `MACHINE_IDS`: the machine ids in the registry's order.
#[must_use]
pub fn machine_ids() -> Vec<&'static str> {
    machine_rows().into_iter().map(|row| row.0).collect()
}

/// `MACHINE_LABELS`: the machine's `machineLabel`.
#[must_use]
pub fn machine_labels() -> Vec<(&'static str, &'static str)> {
    COMPUTERS
        .as_object()
        .map(|rows| {
            rows.values()
                .filter_map(|row| Some((row["machineId"].as_str()?, row["machineLabel"].as_str()?)))
                .collect()
        })
        .unwrap_or_default()
}

/// Match an exact registry id, machine id, alias or hostname, ignoring case.
#[must_use]
pub fn exact_computer(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    COMPUTERS.as_object()?.iter().find_map(|(id, row)| {
        let machine = row["machineId"].as_str()?;
        let exact = id.to_lowercase() == lower
            || machine.to_lowercase() == lower
            || row["sshAlias"]
                .as_str()
                .is_some_and(|alias| alias.to_lowercase() == lower)
            || row["names"].as_array().is_some_and(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|value| value.to_lowercase() == lower)
            });
        exact.then_some(machine)
    })
}

/// `namedComputer`: an exact id, alias or name, else the first non-legacy computer
/// whose name the host contains.
#[must_use]
pub fn named_computer(name: &str) -> Option<&'static str> {
    if let Some(machine) = exact_computer(name) {
        return Some(machine);
    }
    let lower = name.to_lowercase();
    let mut fallback: Vec<_> = COMPUTERS.as_object()?.iter().collect();
    fallback.sort_by_key(|(_, row)| row["legacy"] == Value::Bool(true));
    fallback
        .into_iter()
        .find(|(_, row)| {
            row["names"].as_array().is_some_and(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|value| lower.contains(&value.to_lowercase()))
            })
        })
        .and_then(|(_, row)| row["machineId"].as_str())
}

/// `localComputer`: this host and platform's computer id, else the platform's default host.
#[must_use]
pub fn local_computer(host: &str, platform: &str) -> &'static str {
    let kind = match platform {
        "windows" | "win32" => "windows",
        "mac" | "darwin" | "macos" => "mac",
        _ => "linux",
    };
    match named_computer(host) {
        Some(id) if computer(id).is_some_and(|row| row["os"] == kind) => id,
        _ if kind == "linux" => "vm",
        _ => kind,
    }
}

pub fn machine_label(machine: &str) -> &str {
    computer(machine)
        .and_then(|row| row["sentenceLabel"].as_str())
        .unwrap_or(machine)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_sixth_linux_row_needs_no_new_allowlist() {
        let mut registry = COMPUTERS.clone();
        registry["future"] = serde_json::json!({"machineId":"future", "os":"linux", "sshAlias":"future-linux", "sentenceLabel":"Future Linux"});
        let row = find_computer(&registry, "future").unwrap();
        assert_eq!(row["os"], "linux");
        assert!(row["sshAlias"].is_string());
    }

    #[test]
    fn registry_covers_alexis_and_rejects_unlisted_machines() {
        assert!(known_machine("alexis", true));
        // The VM has an SSH route too (`ubuntu-vm`), for a dashboard that runs elsewhere.
        assert!(known_machine("vm", true));
        assert!(known_machine("vm", false));
        assert!(!known_machine("unregistered", false));
        assert_eq!(machine_label("alexis"), "Alexis PC");
        assert!(crate::validate::is_writer_name("alexis.fixture"));
        assert!(
            crate::pins::parse_remote_pins("alexis", "[]")
                .unwrap()
                .pinned
                .is_empty()
        );
    }
}
