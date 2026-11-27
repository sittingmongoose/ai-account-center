//! Real-filesystem checks against frozen legacy history-writer receipts.
use aac_files::history_transaction::{self as history, Guard, Record};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};
struct FixtureGuard {
    root: PathBuf,
    registry: PathBuf,
    scenario: String,
    fired: bool,
}
impl Guard for FixtureGuard {
    fn closed(&mut self) -> bool {
        self.scenario != "closed"
    }
    fn private(&mut self, p: &Path, _d: bool) -> bool {
        self.scenario != "private"
            && !(self.scenario == "publication-private"
                && self.fired
                && p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(".history-index-stage-")))
    }
    fn phase(&mut self, phase: &str, path: &Path) {
        if self.fired {
            return;
        }
        let trigger = match self.scenario.as_str() {
            "protected" => "writePrivateContent",
            "stage" => "staged",
            "created" => "created",
            "registry" => "staged",
            "publication-private" => "linkCreateOnly",
            _ => return,
        };
        if phase != trigger {
            return;
        }
        self.fired = true;
        match self.scenario.as_str() {
            "protected" => {
                fs::write(self.root.join("config.json"), b"changed").expect("protected race")
            }
            "stage" => {
                fs::rename(path, path.with_extension("foreign")).expect("save stage");
                fs::write(path, b"replacement").expect("replace stage");
            }
            "created" => fs::write(path, b"replacement").expect("replace created"),
            "registry" => {
                fs::rename(&self.registry, self.root.join("moved")).expect("move root");
                fs::create_dir(&self.registry).expect("replace root");
            }
            _ => (),
        }
    }
}
fn setup() -> (PathBuf, PathBuf) {
    let root = tempfile::tempdir().expect("fixture root").keep();
    let registry = root.join("registry");
    fs::create_dir(&registry).expect("registry");
    fs::write(root.join("config.json"), b"protected").expect("protected file");
    (root, registry)
}
const NAME: &str = "local_12345678-1234-1234-1234-123456789abc.json";
fn native(scenario: &str) -> Value {
    let (root, registry) = setup();
    if scenario == "exists" {
        fs::write(registry.join(NAME), b"foreign").expect("existing target");
    }
    let bytes = b"{\"fixture\":true}".to_vec();
    let mut guard = FixtureGuard {
        root: root.clone(),
        registry: registry.clone(),
        scenario: scenario.into(),
        fired: false,
    };
    history::append(
        &root,
        &registry,
        &[Record {
            name: NAME.into(),
            bytes: Some(bytes),
        }],
        &[Record {
            name: "config.json".into(),
            bytes: Some(b"protected".to_vec()),
        }],
        scenario == "private" || scenario == "publication-private",
        &mut guard,
    )
}
#[test]
fn frozen_receipts_for_success_and_filesystem_races() {
    let corpus: Value = serde_json::from_str(include_str!("fixtures/history-oracle.json"))
        .expect("frozen legacy writer receipts");
    for scenario in [
        "success",
        "exists",
        "closed",
        "private",
        "protected",
        "stage",
        "created",
        "registry",
        "publication-private",
    ] {
        let expected = corpus["scenarios"][scenario].clone();
        assert_eq!(native(scenario), expected, "scenario {scenario}");
    }
}
#[test]
fn invalid_records_never_create_a_snapshot() {
    let (root, registry) = setup();
    let mut guard = FixtureGuard {
        root: root.clone(),
        registry: registry.clone(),
        scenario: "success".into(),
        fired: false,
    };
    let result = history::append(
        &root,
        &registry,
        &[Record {
            name: "../escape".into(),
            bytes: Some(vec![]),
        }],
        &[],
        false,
        &mut guard,
    );
    assert_eq!(result["status"], "refused");
    assert_eq!(fs::read_dir(root).expect("root").count(), 2);
}

#[test]
fn portable_snapshot_cleanup_matches_frozen_receipts_and_preserves_foreign_contents() {
    fn snapshots() -> PathBuf {
        let root = tempfile::tempdir().expect("retention root").keep();
        for i in 0..6 {
            let dir = root.join(format!(".history-index-snapshot-{i:032x}"));
            fs::create_dir(&dir).expect("snapshot");
            fs::write(dir.join("snapshot-manifest.json"), b"[]").expect("manifest");
            if i == 0 {
                fs::write(dir.join("foreign.txt"), b"preserve").expect("foreign file");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        root
    }
    let corpus: Value = serde_json::from_str(include_str!("fixtures/history-oracle.json"))
        .expect("frozen legacy cleanup receipt");
    let expected = &corpus["cleanup"];
    let root = snapshots();
    let result = history::cleanup(&root).expect("portable cleanup");
    assert_eq!(&result, expected);
    assert!(
        root.join(format!(".history-index-snapshot-{:032x}/foreign.txt", 0))
            .is_file()
    );
}
