//! The public owners one boot runs: the store host, the owned Status/Scoreboard readers and
//! producers, the answers store and the shared readers-check writer.
//!
//! One construction for both boots that host them: the private service (`unix::serve_inner`)
//! and the public listener's owner host when no private service exists (Windows). The owners
//! themselves are platform-neutral; only their transport differs.

use crate::{
    answers_owned::AnswersOwned,
    feed::Feeds,
    scoreboard_owned::ScoreboardOwned,
    status_owned::StatusOwned,
    status_produce::Produce,
    store_host::{StoreBoot, StoreHost},
};
use aac_files::registry::{self, Owners as Registry};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    io,
    path::PathBuf,
    sync::{Arc, Mutex, atomic::AtomicBool},
};

/// R4: which owners this boot runs, and the share roots they read. The supervisor resolves the
/// roots and the group switches once at start; the store host must be on for either owner.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnedBoot {
    /// The agent-reports root (`AAC_AGENT_REPORTS_DIR`, else the shared folder).
    pub reports_root: PathBuf,
    /// The pm-mail registry folder (`AAC_PM_MAIL_REGISTRY_DIR`, else the shared inbox).
    pub registry_dir: PathBuf,
    /// The owned Status readers (the status group on, under the store host).
    pub status: bool,
    /// The owned Scoreboard (the scoreboard group on, under the store host).
    pub scoreboard: bool,
}

/// The boot's inputs for the owner stack.
#[derive(Clone)]
pub(crate) struct Config {
    /// The canonical private root.
    pub ccs_dir: PathBuf,
    /// The store host's config; absent keeps the store in the caller.
    pub store: Option<StoreBoot>,
    /// The owned readers' config; absent keeps the share readers in the caller.
    pub owned: Option<OwnedBoot>,
    /// Each registry row's writer, when the caller resolved it (the service's boot does).
    pub files: Option<BTreeMap<String, String>>,
}

/// The owners, built once.
pub(crate) struct Owners {
    #[cfg(not(unix))]
    pub(crate) feeds: Feeds,
    pub(crate) store: Option<Arc<StoreHost>>,
    pub(crate) status_owned: Option<Arc<StatusOwned>>,
    pub(crate) status_produce: Option<Arc<Produce>>,
    pub(crate) answers_owned: Option<Arc<AnswersOwned>>,
    pub(crate) scoreboard_owned: Option<Arc<ScoreboardOwned>>,
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("invalid owners config: {error}"))
}

impl Owners {
    pub(crate) fn start(config: Config, feeds: Feeds) -> io::Result<Self> {
        let rows = registry::rows().map_err(invalid)?;
        let registry = match &config.files {
            Some(files) => Registry::from_boot(rows, files).map_err(invalid)?,
            None if config
                .owned
                .as_ref()
                .is_some_and(|owned| owned.status && owned.scoreboard) =>
            {
                let files = rows
                    .iter()
                    .map(|row| {
                        let role = if row.id == "readers-check" {
                            registry::Role::RustService
                        } else {
                            row.owner
                        };
                        (row.id.clone(), role.word().to_owned())
                    })
                    .collect();
                Registry::from_boot(rows, &files).map_err(invalid)?
            }
            None => Registry::defaults(rows),
        };
        let store = config.store.map(StoreHost::new);
        // Status and Scoreboard share one readers-check writer and one in-flight flush.
        let check = Arc::new(Mutex::new(
            aac_status::share::check::ReadersCheckWriter::new(
                config
                    .owned
                    .as_ref()
                    .map(|owned| owned.reports_root.clone())
                    .unwrap_or_default(),
            ),
        ));
        let check_flushing = Arc::new(AtomicBool::new(false));
        // G16b: the answer store moves with the owned Status group.
        let answers_owned = match (&config.owned, &store) {
            (Some(owned), Some(_)) if owned.status => Some(AnswersOwned::new(
                config.ccs_dir.clone(),
                owned.reports_root.clone(),
            )),
            _ => None,
        };
        let status_owned = match (&config.owned, &store) {
            (Some(owned), Some(host)) if owned.status => Some(StatusOwned::new(
                owned.reports_root.clone(),
                owned.registry_dir.clone(),
                Arc::clone(host),
                feeds.clone(),
                registry.clone(),
                Arc::clone(&check),
                Arc::clone(&check_flushing),
            )),
            _ => None,
        };
        // G16c: the producers of the fed Status generations (the owned Status group).
        let scoreboard_feeds = feeds.clone();
        let status_produce = match (&config.owned, &answers_owned) {
            (Some(owned), Some(answers)) if owned.status => {
                Some(Produce::new(feeds.clone(), Arc::clone(answers)))
            }
            _ => None,
        };
        let scoreboard_owned = match (&config.owned, &store) {
            (Some(owned), Some(host)) if owned.scoreboard => Some(ScoreboardOwned::new(
                owned.reports_root.clone(),
                Arc::clone(host),
                scoreboard_feeds,
                Arc::clone(&check),
                registry.clone(),
                Arc::clone(&check_flushing),
            )),
            _ => None,
        };
        Ok(Self {
            #[cfg(not(unix))]
            feeds,
            store,
            status_owned,
            status_produce,
            answers_owned,
            scoreboard_owned,
        })
    }
}
