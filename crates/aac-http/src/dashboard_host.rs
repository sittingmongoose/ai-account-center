//! The computer this dashboard runs on (`thisComputer`). That computer is read in place, as a
//! local child with the dashboard's own `~/.ccs`; every other registered computer is reached
//! over its packaged SSH alias through the pinned agent.
//!
//! One rule for every reader (usage collection, sign-ins and renewal, Analytics, Status, Update
//! apps), from one switch, `AAC_DASHBOARD_COMPUTER`:
//! - unset or empty: this OS's legacy computer, today's placement (the VM on Linux);
//! - a registry id or machine id of this OS: that computer (a dashboard on Nas1 says `nas1`);
//! - `none`, or any value the registry does not know: no registered computer. The dashboard
//!   still reads its own `~/.ccs` in place, and reaches all seven computers, the VM included
//!   (`ubuntu-vm`), through the agent. The container image sets it (the Unraid dashboard).
//!
//! The host name is never consulted: build and test hosts named like a computer (`1070ti`)
//! keep the same placement as every other host.
use serde_json::Value;
use std::sync::LazyLock;

/// The switch.
pub(crate) const ENV: &str = "AAC_DASHBOARD_COMPUTER";

/// This build's computer-table OS.
pub(crate) fn this_os() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}

/// The registry id of the computer this dashboard runs on, once per process.
pub(crate) fn this_computer() -> Option<&'static str> {
    static HERE: LazyLock<Option<&'static str>> =
        LazyLock::new(|| resolve(std::env::var(ENV).ok().as_deref(), this_os()));
    *HERE
}

fn rows() -> impl Iterator<Item = (&'static str, &'static Value)> {
    aac_status::computers::registry()
        .as_object()
        .into_iter()
        .flatten()
        .map(|(id, row)| (id.as_str(), row))
}

/// The rule above, with its inputs given.
pub(crate) fn resolve(explicit: Option<&str>, os: &str) -> Option<&'static str> {
    if let Some(value) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        // A computer of another OS cannot be the one this build runs on.
        return rows()
            .find(|(id, row)| {
                row["os"] == os
                    && (id.eq_ignore_ascii_case(value)
                        || row["machineId"]
                            .as_str()
                            .is_some_and(|machine| machine.eq_ignore_ascii_case(value)))
            })
            .map(|(id, _)| id);
    }
    rows()
        .find(|(_, row)| row["os"] == os && row["legacy"] == Value::Bool(true))
        .map(|(id, _)| id)
}

/// Whether `alias` (a packaged SSH alias, registry id or machine id) names `here`.
pub(crate) fn names(here: Option<&str>, alias: &str) -> bool {
    let Some(here) = here else {
        return false;
    };
    rows().any(|(id, row)| {
        id == here
            && (id.eq_ignore_ascii_case(alias)
                || row["sshAlias"].as_str() == Some(alias)
                || row["machineId"].as_str() == Some(alias))
    })
}

/// The SSH alias a reader uses for registry computer `id`: none for the computer this
/// dashboard runs on (it is read in place), else its packaged alias.
pub(crate) fn route_from<'a>(
    here: Option<&str>,
    id: &str,
    alias: Option<&'a str>,
) -> Option<&'a str> {
    if here == Some(id) { None } else { alias }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_keeps_todays_placement() {
        assert_eq!(resolve(None, "linux"), Some("ubuntu"));
        assert_eq!(resolve(Some(""), "linux"), Some("ubuntu"));
        assert_eq!(resolve(Some(" "), "linux"), Some("ubuntu"));
        assert_eq!(resolve(None, "mac"), Some("mac"));
        assert_eq!(resolve(None, "windows"), Some("windows"));
    }

    #[test]
    fn the_switch_names_a_computer_or_none() {
        assert_eq!(resolve(Some("none"), "linux"), None);
        assert_eq!(resolve(Some("not-a-computer"), "linux"), None);
        assert_eq!(resolve(Some("ubuntu"), "linux"), Some("ubuntu"));
        assert_eq!(resolve(Some("VM"), "linux"), Some("ubuntu"));
        assert_eq!(resolve(Some("nas1"), "linux"), Some("nas1"));
        assert_eq!(resolve(Some("mac"), "linux"), None);
    }

    #[test]
    fn the_vm_is_remote_only_when_the_dashboard_runs_elsewhere() {
        let vm = aac_status::computers::registry()["ubuntu"]["sshAlias"].as_str();
        assert_eq!(vm, Some("ubuntu-vm"), "the VM has a packaged SSH alias");
        let on_vm = resolve(None, "linux");
        assert_eq!(
            route_from(on_vm, "ubuntu", vm),
            None,
            "the VM reads itself in place"
        );
        assert_eq!(
            route_from(on_vm, "nas1", Some("nas1-agent")),
            Some("nas1-agent")
        );
        let elsewhere = resolve(Some("none"), "linux");
        assert_eq!(route_from(elsewhere, "ubuntu", vm), Some("ubuntu-vm"));
        assert!(names(on_vm, "ubuntu-vm"));
        assert!(names(on_vm, "ubuntu"));
        assert!(!names(elsewhere, "ubuntu-vm"));
        assert!(!names(on_vm, "nas1-agent"));
    }
}
