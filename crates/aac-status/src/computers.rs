//! Package-owned computers; never accepts host aliases, paths or commands from callers.
use serde_json::Value;
use std::sync::LazyLock;

static COMPUTERS: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../web-dashboard/public/computers.json"))
        .expect("packaged computer registry")
});

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
        assert!(!known_machine("vm", true));
        assert!(known_machine("vm", false));
        assert!(!known_machine("unregistered", false));
        assert_eq!(machine_label("alexis"), "Alexis PC");
        assert!(crate::validate::is_writer_name("alexis.fixture"));
        assert!(crate::pins::parse_remote_pins("alexis", "[]").unwrap().is_empty());
    }
}
