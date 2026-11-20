//! The private-file registry shared with TS (L2): `config/private-files.json`. One row per file
//! outside the store, with its cap, mode, folder rule and writer. TS reads the same file
//! (`src/shared/private-files.ts`), resolves each row's owner once at start and passes the result
//! to the service in its boot JSON; [`Owners::from_boot`] checks that both sides agree.
//!
//! A writer claims a row through [`Owners::claim`] (the service) or [`claim_as_agent`] (the
//! agent on another computer); both return an [`OwnedStore`] holding a kernel lease, so the row
//! has one writer at a time. TS's `private-json-store.ts` refuses to write a row it does not own.
use crate::{Class, Lease, OwnedStore, Parents};
use aac_core::{Cause, Failure};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::OnceLock,
};

/// The registry, embedded at build time so the service and the agent carry the rows of their
/// own commit.
pub const SOURCE: &str = include_str!("../../../config/private-files.json");

/// Which root a row's path is relative to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Root {
    /// The AAC private folder (`~/.ccs` or `CCS_HOME`).
    Ccs,
    /// The Status share's agent-reports root.
    Share,
}

/// A process role that may write a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// The TypeScript dashboard.
    Ts,
    /// The Rust service on the dashboard host.
    RustService,
    /// The Rust agent on another computer.
    Agent,
}

impl Role {
    /// The registry and boot spelling.
    pub fn word(self) -> &'static str {
        match self {
            Role::Ts => "ts",
            Role::RustService => "rust-service",
            Role::Agent => "agent",
        }
    }
}

/// One registry row.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Row {
    /// Stable id, shared with TS.
    pub id: String,
    /// The root the path is relative to.
    pub root: Root,
    /// Relative path; one `*` may stand for a plain file name in the last component.
    pub path: String,
    /// Retention/security category.
    pub class: Class,
    /// Byte cap for reads and Rust writes.
    pub max_bytes: u64,
    /// Octal permission bits, as text ("0600").
    pub mode: String,
    /// The writer on the dashboard host while no route group has moved it.
    pub owner: Role,
    /// The agent writes the same layout on the computers it runs on (the Python helper is its
    /// reference there).
    pub agent: bool,
    /// The route group whose `on` makes the Rust service this row's writer.
    pub moves_with: Option<String>,
    /// How missing folders are made and checked on a write.
    pub parents: Parents,
    /// Where the writer's kernel lease is taken.
    pub lease: Lease,
}

const KEYS: [&str; 11] = [
    "id",
    "root",
    "path",
    "class",
    "maxBytes",
    "mode",
    "owner",
    "agent",
    "movesWith",
    "parents",
    "lease",
];

impl Row {
    /// The mode bits.
    pub fn mode_bits(&self) -> u32 {
        u32::from_str_radix(&self.mode, 8).unwrap_or(0)
    }

    /// The relative path of one file of this row: the row's path itself, or with its `*`
    /// replaced by `name`, which must be a plain file name the pattern admits.
    pub fn file(&self, name: Option<&str>) -> Result<String, Failure> {
        let refused = || Failure::new(Cause::Refused, "file is not this registry row");
        match (self.path.split_once('*'), name) {
            (None, None) => Ok(self.path.clone()),
            (Some((head, tail)), Some(name)) => {
                let plain = !name.is_empty()
                    && !name.starts_with('.')
                    && !name.ends_with(".lock")
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
                let (folder, prefix) = head.rsplit_once('/').unwrap_or(("", head));
                let stem = name
                    .strip_prefix(prefix)
                    .and_then(|rest| rest.strip_suffix(tail));
                if !plain || stem.is_none_or(str::is_empty) {
                    return Err(refused());
                }
                Ok(if folder.is_empty() {
                    name.to_owned()
                } else {
                    format!("{folder}/{name}")
                })
            }
            _ => Err(refused()),
        }
    }

    fn open(&self, root: &Path, name: Option<&str>) -> Result<OwnedStore, Failure> {
        let relative = self.file(name)?;
        let max_size = usize::try_from(self.max_bytes)
            .map_err(|_| Failure::new(Cause::Refused, "invalid byte cap"))?;
        Ok(OwnedStore::open(
            root,
            Path::new(&relative),
            max_size,
            self.parents,
            self.lease,
        )?
        .with_mode(self.mode_bits()))
    }
}

fn plain_path(path: &str) -> bool {
    // Existing credential leaves. Wildcards and all other dotted leaves remain refused.
    if matches!(path, ".session-secret" | "bar/.auth-token") {
        return true;
    }
    let parts: Vec<&str> = path.split('/').collect();
    let last = parts.len().saturating_sub(1);
    !path.is_empty()
        && parts.iter().enumerate().all(|(i, part)| {
            !part.is_empty()
                && !part.starts_with('.')
                && !part.ends_with(".lock")
                && part.matches('*').count() <= usize::from(i == last)
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-*".contains(&b))
        })
}

fn id_word(id: &str) -> bool {
    id.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.ends_with('-')
        && !id.contains("--")
}

/// Use a cooperating front door's open-file-description lock for a delegated write.
#[cfg(unix)]
pub fn inherit_writer(
    id: &str,
    root: &Path,
    name: Option<&str>,
    lock: std::fs::File,
) -> Result<OwnedStore, Failure> {
    let row = row(id)?;
    let supports_inherit = matches!(
        (row.parents, row.lease),
        (Parents::Private, Lease::File) | (Parents::Reference, Lease::Folder)
    );
    if row.root != Root::Ccs || !supports_inherit || row.moves_with.as_deref() != Some("writes") {
        return Err(Failure::new(
            Cause::Refused,
            "store cannot inherit a writer lease",
        ));
    }
    OwnedStore::inherit(root, row, name, lock)
}

/// Parse and check a registry document: the same rules TS applies.
pub fn parse(source: &str) -> Result<Vec<Row>, Failure> {
    let damaged = |why: &'static str| Failure::new(Cause::Damaged, why);
    let value: serde_json::Value =
        serde_json::from_str(source).map_err(|_| damaged("registry is not JSON"))?;
    let top = value
        .as_object()
        .filter(|o| o.len() == 2 && o.get("version") == Some(&serde_json::json!(1)))
        .ok_or_else(|| damaged("registry is not version 1"))?;
    let raw = top
        .get("rows")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| damaged("registry has no rows"))?;
    let mut rows = Vec::with_capacity(raw.len());
    let mut ids = BTreeSet::new();
    let mut places = BTreeSet::new();
    for item in raw {
        let keys: BTreeSet<_> = item
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        if keys != KEYS.into_iter().collect() {
            return Err(damaged("registry row has other keys"));
        }
        let row: Row =
            serde_json::from_value(item.clone()).map_err(|_| damaged("registry row is invalid"))?;
        let mode = row.mode_bits();
        let mode_ok = row.mode.len() == 4
            && row.mode.starts_with('0')
            && match row.root {
                Root::Ccs => mode == 0o600,
                Root::Share => mode == 0o600 || mode == 0o644,
            };
        if !id_word(&row.id)
            || !ids.insert(row.id.clone())
            || !plain_path(&row.path)
            || !places.insert((row.root == Root::Ccs, row.path.clone()))
            || !(1..=64 * 1024 * 1024).contains(&row.max_bytes)
            || !mode_ok
            || row.owner == Role::RustService
            || row.owner == Role::Agent
            || row.moves_with.as_deref().is_some_and(|g| !id_word(g))
            || row.agent && row.root != Root::Ccs
        {
            return Err(damaged("registry row breaks a rule"));
        }
        rows.push(row);
    }
    Ok(rows)
}

/// The embedded registry's rows.
pub fn rows() -> Result<&'static [Row], Failure> {
    static ROWS: OnceLock<Result<Vec<Row>, Failure>> = OnceLock::new();
    ROWS.get_or_init(|| parse(SOURCE))
        .as_deref()
        .map_err(Clone::clone)
}

/// One embedded row by id.
pub fn row(id: &str) -> Result<&'static Row, Failure> {
    rows()?
        .iter()
        .find(|r| r.id == id)
        .ok_or_else(|| Failure::new(Cause::Refused, "unregistered private file"))
}

/// The agent's claim on a row it writes on its own computer (`agent: true`), under that
/// computer's root.
pub fn claim_as_agent(id: &str, root: &Path, name: Option<&str>) -> Result<OwnedStore, Failure> {
    let row = row(id)?;
    if !row.agent {
        return Err(Failure::new(
            Cause::Refused,
            "store belongs to another process role",
        ));
    }
    row.open(root, name)
}

/// Each row's writer on the dashboard host, as TS resolved it at start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owners(BTreeMap<String, Role>);

impl Owners {
    /// The owners when no route group has moved a row: each row's own `owner`.
    pub fn defaults(rows: &[Row]) -> Self {
        Self(rows.iter().map(|r| (r.id.clone(), r.owner)).collect())
    }

    /// The boot's `files` map: every row exactly once, each owner its row's own or, for a row
    /// that moves with a group, the Rust service.
    pub fn from_boot(rows: &[Row], files: &BTreeMap<String, String>) -> Result<Self, Failure> {
        let refused = || Failure::new(Cause::Refused, "boot owners differ from the registry");
        if files.len() != rows.len() {
            return Err(refused());
        }
        let mut owners = BTreeMap::new();
        for row in rows {
            let word = files.get(&row.id).ok_or_else(refused)?;
            let owner = if word == row.owner.word() {
                row.owner
            } else if row.moves_with.is_some() && word == Role::RustService.word() {
                Role::RustService
            } else {
                return Err(refused());
            };
            owners.insert(row.id.clone(), owner);
        }
        Ok(Self(owners))
    }

    /// A row's writer.
    pub fn owner(&self, id: &str) -> Option<Role> {
        self.0.get(id).copied()
    }

    /// The service's claim on a row it owns, under the canonical private root.
    pub fn claim(
        &self,
        rows: &[Row],
        id: &str,
        root: &Path,
        name: Option<&str>,
    ) -> Result<OwnedStore, Failure> {
        let row = rows
            .iter()
            .find(|r| r.id == id)
            .ok_or_else(|| Failure::new(Cause::Refused, "unregistered private file"))?;
        if self.owner(id) != Some(Role::RustService) {
            return Err(Failure::new(
                Cause::Refused,
                "store belongs to another process role",
            ));
        }
        row.open(root, name)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn with(source: &str, edit: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut value: serde_json::Value = serde_json::from_str(source).unwrap();
        edit(&mut value);
        value.to_string()
    }

    #[test]
    fn the_embedded_registry_parses_and_names_every_reader() {
        let rows = rows().unwrap();
        assert!(rows.len() >= 10);
        for id in [
            "account-keys",
            "account-visibility",
            "dashboard-preferences",
            "dashboard-preferences-extra-sources",
            "claude-desktop-profiles",
            "account-refresh-settings",
        ] {
            assert!(row(id).is_ok(), "{id}");
        }
        assert!(row("unknown").is_err());
        assert_eq!(
            row("readers-check").unwrap().moves_with.as_deref(),
            Some("status")
        );
        assert_eq!(
            row("catalog-seed").unwrap().moves_with.as_deref(),
            Some("scoreboard")
        );
        // The default owner map keeps all groups with TS.
        let defaults = Owners::defaults(rows);
        for row in rows {
            assert_eq!(row.owner, Role::Ts);
            assert_eq!(defaults.owner(&row.id), Some(Role::Ts));
        }
        let moved: Vec<_> = rows
            .iter()
            .filter(|r| r.moves_with.as_deref() == Some("collection"))
            .map(|r| (r.id.as_str(), "collection"))
            .collect();
        assert_eq!(
            moved,
            [
                ("opencode-wallet-account", "collection"),
                ("cursor-account-observation", "collection"),
                ("cursor-quota-observation", "collection"),
                ("account-usage-cooldown", "collection"),
                ("account-usage-observations", "collection"),
                ("claude-live-snapshots", "collection"),
            ]
        );
    }

    #[test]
    fn every_moves_with_group_is_a_route_group() {
        let routes: serde_json::Value =
            serde_json::from_str(include_str!("../../../config/rust-service-routes.json")).unwrap();
        let groups: BTreeSet<_> = routes
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["group"].as_str())
            .collect();
        for row in rows().unwrap() {
            if let Some(group) = &row.moves_with {
                assert!(
                    groups.contains(group.as_str()) || group == "public-auth",
                    "{}",
                    row.id
                );
            }
        }
    }

    #[test]
    fn rows_that_break_a_rule_are_refused() {
        type Edit = Box<dyn Fn(&mut serde_json::Value)>;
        let edits: Vec<Edit> = vec![
            Box::new(|v| v["rows"][0]["path"] = "../escape".into()),
            Box::new(|v| v["rows"][0]["path"] = "/absolute".into()),
            Box::new(|v| v["rows"][0]["path"] = "a\\b".into()),
            Box::new(|v| v["rows"][0]["path"] = "x.lock".into()),
            Box::new(|v| v["rows"][0]["path"] = "*/x.json".into()),
            Box::new(|v| v["rows"][0]["path"] = "a/*.*".into()),
            Box::new(|v| v["rows"][0]["mode"] = "0644".into()),
            Box::new(|v| v["rows"][0]["mode"] = "600".into()),
            Box::new(|v| v["rows"][0]["maxBytes"] = 0.into()),
            Box::new(|v| v["rows"][0]["owner"] = "rust-service".into()),
            Box::new(|v| v["rows"][0]["owner"] = "python".into()),
            Box::new(|v| v["rows"][0]["id"] = "Upper".into()),
            Box::new(|v| v["rows"][1]["id"] = v["rows"][0]["id"].clone()),
            Box::new(|v| v["rows"][1]["path"] = v["rows"][0]["path"].clone()),
            Box::new(|v| v["rows"][0]["extra"] = true.into()),
            Box::new(|v| {
                v["rows"][0].as_object_mut().unwrap().remove("movesWith");
            }),
            Box::new(|v| v["version"] = 2.into()),
        ];
        assert!(parse(SOURCE).is_ok());
        for (i, edit) in edits.iter().enumerate() {
            assert!(parse(&with(SOURCE, |v| edit(v))).is_err(), "edit {i}");
        }
    }

    #[test]
    fn pattern_rows_admit_only_their_plain_names() {
        let keys = row("account-keys").unwrap();
        assert_eq!(
            keys.file(Some("zai-12345678.json")).unwrap(),
            "account-usage/keys/zai-12345678.json"
        );
        for bad in [
            ".zai.json",
            "a/b.json",
            "zai.json.lock",
            ".json",
            "zai.txt",
            "z ai.json",
        ] {
            assert!(keys.file(Some(bad)).is_err(), "{bad}");
        }
        assert!(keys.file(None).is_err());
        let visibility = row("account-visibility").unwrap();
        assert_eq!(visibility.file(None).unwrap(), "account-visibility.json");
        assert!(visibility.file(Some("x.json")).is_err());
    }

    #[test]
    fn boot_owners_must_match_the_registry() {
        let source = with(SOURCE, |v| v["rows"][1]["movesWith"] = "scoreboard".into());
        let rows = parse(&source).unwrap();
        let mut files: BTreeMap<String, String> = rows
            .iter()
            .map(|r| (r.id.clone(), "ts".to_owned()))
            .collect();
        assert_eq!(
            Owners::from_boot(&rows, &files).unwrap(),
            Owners::defaults(&rows)
        );
        // A row that moves with a group may be the service's; no other row may.
        files.insert(rows[1].id.clone(), "rust-service".into());
        let owners = Owners::from_boot(&rows, &files).unwrap();
        assert_eq!(owners.owner(&rows[1].id), Some(Role::RustService));
        let mut flipped = files.clone();
        flipped.insert("usage-hub-key".into(), "rust-service".into());
        assert!(Owners::from_boot(&rows, &flipped).is_err());
        let mut missing = files.clone();
        missing.remove(&rows[2].id);
        assert!(Owners::from_boot(&rows, &missing).is_err());
        let mut unknown = files.clone();
        unknown.insert("zz-unknown".into(), "ts".into());
        assert!(Owners::from_boot(&rows, &unknown).is_err());
        // Only the owner claims: TS's row is never the service's.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert!(owners.claim(&rows, &rows[0].id, &root, None).is_err());
        let store = owners.claim(&rows, &rows[1].id, &root, None).unwrap();
        store.write(b"{}").unwrap();
        assert_eq!(store.read().unwrap(), b"{}");
    }

    #[test]
    fn only_agent_rows_are_the_agents() {
        let dir = tempfile::tempdir().unwrap();
        assert!(claim_as_agent("account-visibility", dir.path(), None).is_err());
        assert!(claim_as_agent("account-keys", dir.path(), None).is_err());
        assert!(claim_as_agent("account-keys", dir.path(), Some("zai-12345678.json")).is_ok());
    }
}
