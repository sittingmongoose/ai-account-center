//! L1: the one route table (`config/rust-service-routes.json`). TS gates from it and Rust
//! builds its dispatch from it: each `rust` row names its group, the principal roles it admits,
//! its budget and its handler. Startup refuses a row without a known handler.
use aac_api::{PrincipalRole, VerifiedPrincipal};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::io;

/// The table, embedded at build time so Rust and TS cannot disagree about it.
pub(crate) const ROUTES: &str = include_str!("../../../config/rust-service-routes.json");

/// The handlers the service implements. A row names one of these words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Handler {
    Health,
    Diag,
    Settings,
    WriteSettings,
    WriteVisibility,
    WritePreferences,
    Home,
    Accounts,
    Dashboard,
    Profiles,
    Files,
    Status,
    Scoreboard,
    StoreStatus,
    StatusInputs,
    StatusAnswers,
    /// GET `/api/status/answers` (the owner's history, the route view).
    StatusAnswersRead,
    /// POST `/api/status/answers` (one answer set).
    StatusAnswersPost,
    /// GET `/api/usage/history`, `/api/usage/at` and `/api/usage/burn` (the hosted store).
    UsageHistory,
    UsageAt,
    UsageBurn,
    StatusUsage,
    ScoreboardInputs,
    Feed,
    FeedStatus,
    HomeInputs,
    Events,
    Agent,
    Collect,
    CollectionTick,
    CollectionRecheck,
    CollectionSources,
    ProviderCache,
    CursorAccount,
    CollectionCache,
    AgentControl,
    Approvals,
    Lifecycle,
    AppUpdates,
    /// M5 lane K: the reset claims file (ledger and ticketed source reads).
    ResetClaims,
    SignInHealth,
    SignInPoll,
    Store,
    UsageSample,
    /// R5/A2: the native Analytics read (store history, activity, pricing, display zone).
    Analytics,
    ClaudeOpen,
    ClaudeHistoryCleanup,
}

impl Handler {
    fn named(word: &str) -> Option<Self> {
        Some(match word {
            "health" => Handler::Health,
            "diag" => Handler::Diag,
            "settings" => Handler::Settings,
            "write-settings" => Handler::WriteSettings,
            "write-visibility" => Handler::WriteVisibility,
            "write-preferences" => Handler::WritePreferences,
            "home" => Handler::Home,
            "accounts" => Handler::Accounts,
            "dashboard" => Handler::Dashboard,
            "profiles" => Handler::Profiles,
            "files" => Handler::Files,
            "status" => Handler::Status,
            "scoreboard" => Handler::Scoreboard,
            "store-status" => Handler::StoreStatus,
            "status-inputs" => Handler::StatusInputs,
            "status-answers" => Handler::StatusAnswers,
            "status-answers-read" => Handler::StatusAnswersRead,
            "status-answers-post" => Handler::StatusAnswersPost,
            "usage-history" => Handler::UsageHistory,
            "usage-at" => Handler::UsageAt,
            "usage-burn" => Handler::UsageBurn,
            "status-usage" => Handler::StatusUsage,
            "scoreboard-inputs" => Handler::ScoreboardInputs,
            "feed" => Handler::Feed,
            "feed-status" => Handler::FeedStatus,
            "home-inputs" => Handler::HomeInputs,
            "events" => Handler::Events,
            "collection-cache" => Handler::CollectionCache,
            "collect" => Handler::Collect,
            "collection-provider-cache" => Handler::ProviderCache,
            "collection-cursor" => Handler::CursorAccount,
            "collection-sources" => Handler::CollectionSources,
            "collection-tick" => Handler::CollectionTick,
            "collection-recheck" => Handler::CollectionRecheck,
            "agent" => Handler::Agent,
            "agent-control" => Handler::AgentControl,
            "account-approvals" => Handler::Approvals,
            "account-lifecycle" => Handler::Lifecycle,
            "app-updates" => Handler::AppUpdates,
            "reset-claims" => Handler::ResetClaims,
            "collection-signins" => Handler::SignInHealth,
            "collection-signin-poll" => Handler::SignInPoll,
            "store" => Handler::Store,
            "usage-sample" => Handler::UsageSample,
            "analytics" => Handler::Analytics,
            "claude-open" => Handler::ClaudeOpen,
            "claude-history-cleanup" => Handler::ClaudeHistoryCleanup,
            _ => return None,
        })
    }
}

/// A role word of the table. `admin` is the verified dashboard session (session or bar).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Public,
    Admin,
    Device,
    Hub,
    Service,
}

impl Role {
    fn named(word: &str) -> Option<Self> {
        Some(match word {
            "public" => Role::Public,
            "admin" => Role::Admin,
            "device" => Role::Device,
            "hub" => Role::Hub,
            "service" => Role::Service,
            _ => return None,
        })
    }

    fn admits(self, principal: &VerifiedPrincipal) -> bool {
        match self {
            Role::Admin => principal.dashboard_session(),
            Role::Public => principal.role == PrincipalRole::Public,
            Role::Device => principal.role == PrincipalRole::Device,
            Role::Hub => principal.role == PrincipalRole::Hub,
            Role::Service => principal.role == PrincipalRole::Service,
        }
    }
}

/// The blocking pool a row's work runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pool {
    Reads,
    Store,
    /// Fed Status. Not a route-table group: the fed arm chooses it, and the envelope read stays
    /// on [`Pool::Reads`].
    Status,
    /// The reset claims file: its own slots, so a seconds-long source read never holds a read
    /// permit.
    Reset,
}

/// One `rust` row, checked.
#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub method: axum::http::Method,
    pub path: String,
    pub handler: Handler,
    pub pool: Pool,
    /// 0: no deadline (the event stream lives as long as its connection).
    pub budget_ms: u64,
    roles: Vec<Role>,
}

impl Row {
    pub fn admits(&self, principal: &VerifiedPrincipal) -> bool {
        self.roles.iter().any(|role| role.admits(principal))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fallback {
    method: String,
    path: String,
    owner: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Owned {
    method: String,
    path: String,
    owner: String,
    group: String,
    roles: Vec<String>,
    budget_ms: u64,
    handler: String,
}

const OWNED_KEYS: [&str; 7] = [
    "method", "path", "owner", "group", "roles", "budgetMs", "handler",
];

fn failure() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid route table")
}

fn word(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 32
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The app-update route surface: it stays with the private Unix front door until Windows'
/// worker routes are retired, so only Unix validates the row.
#[cfg(unix)]
fn app_update_route(
    method: &axum::http::Method,
    path: &str,
) -> Option<crate::app_update_routes::Operation> {
    crate::app_update_routes::Operation::from_route(method, path)
}

#[cfg(not(unix))]
fn app_update_route(method: &axum::http::Method, path: &str) -> Option<()> {
    // The same four rows, without the Unix job owner's route parser.
    let expected = match path {
        "/api/app-updates/status" => axum::http::Method::GET,
        "/api/app-updates/start" | "/api/app-updates/retry" | "/api/app-updates/cancel" => {
            axum::http::Method::POST
        }
        _ => return None,
    };
    (*method == expected).then_some(())
}

/// Parse and check the table: every `rust` row has all seven keys, a known handler, known
/// roles, a word group and a budget of at most 10 s; the one `ts-fallback` wildcard row has
/// exactly its three keys and comes last.
pub(crate) fn rows(table: &str) -> io::Result<Vec<Row>> {
    let raw: Vec<Box<RawValue>> = serde_json::from_str(table).map_err(|_| failure())?;
    let mut rows = Vec::new();
    let mut fallback = false;
    for entry in raw {
        let bytes = entry.get().as_bytes();
        if fallback {
            return Err(failure());
        }
        if let Ok(row) =
            crate::json_object::from_slice::<Fallback>(bytes, &["method", "path", "owner"])
        {
            if row.owner != "ts-fallback" || row.method != "*" || row.path != "*" {
                return Err(failure());
            }
            fallback = true;
            continue;
        }
        let row: Owned =
            crate::json_object::from_slice(bytes, &OWNED_KEYS).map_err(|_| failure())?;
        let handler = Handler::named(&row.handler).ok_or_else(failure)?;
        let roles = row
            .roles
            .iter()
            .map(|r| Role::named(r))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(failure)?;
        let method = match row.method.as_str() {
            "GET" => axum::http::Method::GET,
            "PUT" => axum::http::Method::PUT,
            "POST" => axum::http::Method::POST,
            "PATCH" => axum::http::Method::PATCH,
            _ => return Err(failure()),
        };
        // Internal routes are the front door's alone; public routes never are.
        let internal = row.path.starts_with("/internal/");
        if row.owner != "rust"
            || !word(&row.group)
            || roles.is_empty()
            || row.budget_ms > 10_000
            || (row.budget_ms == 0
                && !matches!(
                    handler,
                    Handler::Events
                        | Handler::Agent
                        | Handler::Collect
                        | Handler::CollectionTick
                        | Handler::CollectionRecheck
                        | Handler::ResetClaims
                        | Handler::CursorAccount
                        | Handler::Store
                        | Handler::UsageHistory
                        | Handler::UsageAt
                        | Handler::UsageBurn
                        | Handler::UsageSample
                        | Handler::ClaudeOpen
                        | Handler::ClaudeHistoryCleanup
                        | Handler::StatusAnswers
                        | Handler::StatusAnswersPost
                        | Handler::SignInPoll
                        | Handler::Lifecycle
                ))
            || (row.group == "internal" && !internal)
            || (internal && !matches!(row.group.as_str(), "internal" | "collection"))
            || (row.group == "collection"
                && !matches!(
                    handler,
                    Handler::Dashboard
                        | Handler::CollectionCache
                        | Handler::CollectionRecheck
                        | Handler::CollectionTick
                        | Handler::CollectionSources
                        | Handler::ProviderCache
                        | Handler::CursorAccount
                        | Handler::Collect
                ))
            || (handler != Handler::Lifecycle && internal != roles.iter().all(|r| *r == Role::Service))
            || (!internal && roles.contains(&Role::Service))
            || (!internal && !row.path.starts_with("/api/"))
            || (method == axum::http::Method::POST
                && !matches!(
                    handler,
                    Handler::Agent
                        | Handler::AgentControl
                        | Handler::CollectionRecheck
                        | Handler::Collect
                        | Handler::AppUpdates
                        | Handler::Approvals
                        | Handler::Lifecycle
                        | Handler::ResetClaims
                        | Handler::Store
                        | Handler::UsageSample
                        | Handler::ClaudeOpen
                        | Handler::ClaudeHistoryCleanup
                        | Handler::StatusAnswersPost
                ))
            || (method == axum::http::Method::PUT
                && !matches!(
                    handler,
                    Handler::Feed
                        | Handler::HomeInputs
                        | Handler::CollectionCache
                        | Handler::CollectionTick
                        | Handler::WriteSettings
                        | Handler::WriteVisibility
                        | Handler::WritePreferences
                        | Handler::ProviderCache
                        | Handler::CursorAccount
                        | Handler::StatusAnswers
                        | Handler::StatusUsage
                        | Handler::SignInHealth
                        | Handler::SignInPoll
                ))
            || (handler == Handler::StatusAnswers
                && (method != axum::http::Method::PUT
                    || row.path != "/internal/status/answers"
                    || row.budget_ms != 0))
            || (handler == Handler::StatusUsage
                && (method != axum::http::Method::PUT || row.path != "/internal/status/usage"))
            || (handler == Handler::WriteSettings
                && (method != axum::http::Method::PUT
                    || row.group != "writes"
                    || roles != [Role::Admin]
                    || !matches!(
                        row.path.as_str(),
                        "/api/accounts/settings" | "/api/accounts/analytics/capture"
                    )))
            || (handler == Handler::AppUpdates
                && (row.group != "app-updates"
                    || roles != [Role::Admin]
                    || app_update_route(&method, &row.path).is_none()))
            || (handler == Handler::Approvals
                && (method != axum::http::Method::POST
                    || row.path != "/internal/account-confirmations"))
            || (handler == Handler::Lifecycle
                && (method != axum::http::Method::POST
                    || row.budget_ms != 0
                    || !matches!(
                        (row.path.as_str(), roles.as_slice()),
                        (crate::public_auth::lifecycle_host::ENDPOINT, [Role::Admin])
                            | (crate::public_auth::lifecycle_host::COLLECTION_ENDPOINT, [Role::Admin, Role::Device])
                    )))
            // The TS route owns the deadline (a Claude source read takes seconds); the work is
            // one blocking transaction that cannot stop halfway, so the row's budget is 0.
            || (handler == Handler::ResetClaims
                && (method != axum::http::Method::POST
                    || row.path != "/internal/reset-claims"
                    || row.budget_ms != 0))
            || (handler == Handler::UsageSample
                && (method != axum::http::Method::POST
                    || row.path != "/internal/usage-snapshot/sample"
                    || row.budget_ms != 0))
            || (handler == Handler::CollectionRecheck
                && (method != axum::http::Method::POST
                    || row.path != "/internal/collection/recheck"
                    || row.budget_ms != 0))
            || (method == axum::http::Method::PATCH
                && !matches!(
                    handler,
                    Handler::WriteVisibility | Handler::WritePreferences
                ))
            || (handler == Handler::WritePreferences
                && (!matches!(method, axum::http::Method::PUT | axum::http::Method::PATCH)
                    || row.group != "writes"
                    || roles != [Role::Admin]
                    || row.path != "/api/accounts/preferences"))
            || (handler == Handler::WriteVisibility
                && (!matches!(method, axum::http::Method::PUT | axum::http::Method::PATCH)
                    || row.group != "writes"
                    || roles != [Role::Admin]
                    || row.path != "/api/accounts/visibility"))
            || (handler == Handler::ClaudeOpen
                && (method != axum::http::Method::POST
                    || row.path != "/internal/claude-open"
                    || row.budget_ms != 0))
            || (handler == Handler::ClaudeHistoryCleanup
                && (method != axum::http::Method::POST
                    || row.path != "/internal/claude-history-cleanup"
                    || row.budget_ms != 0))
            || {
                let worker_removal = matches!(
                    handler,
                    Handler::StatusAnswersRead
                        | Handler::StatusAnswersPost
                        | Handler::UsageHistory
                        | Handler::UsageAt
                        | Handler::UsageBurn
                );
                let shape_ok = match handler {
                    Handler::StatusAnswersRead => {
                        method == axum::http::Method::GET
                            && row.path == "/api/status/answers"
                            && row.budget_ms == 400
                            && roles == [Role::Admin]
                    }
                    Handler::StatusAnswersPost => {
                        method == axum::http::Method::POST
                            && row.path == "/api/status/answers"
                            && row.budget_ms == 0
                            && roles == [Role::Admin]
                    }
                    Handler::UsageHistory => {
                        method == axum::http::Method::GET
                            && row.path == "/api/usage/history"
                            && row.budget_ms == 0
                            && roles == [Role::Admin, Role::Device]
                    }
                    Handler::UsageAt => {
                        method == axum::http::Method::GET
                            && row.path == "/api/usage/at"
                            && row.budget_ms == 0
                            && roles == [Role::Admin, Role::Device]
                    }
                    Handler::UsageBurn => {
                        method == axum::http::Method::GET
                            && row.path == "/api/usage/burn"
                            && row.budget_ms == 0
                            && roles == [Role::Admin, Role::Device]
                    }
                    _ => true,
                };
                worker_removal && !shape_ok
            }
            || rows
                .iter()
                .any(|r: &Row| r.path == row.path && r.method == method)
        {
            return Err(failure());
        }
        rows.push(Row {
            method,
            path: row.path,
            handler,
            pool: if row.group == "store" || handler == Handler::Store {
                Pool::Store
            } else if handler == Handler::ResetClaims {
                Pool::Reset
            } else {
                Pool::Reads
            },
            budget_ms: row.budget_ms,
            roles,
        });
    }
    if !fallback {
        return Err(failure());
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_table_parses_with_a_handler_per_row() -> io::Result<()> {
        let rows = rows(ROUTES)?;
        assert!(rows.iter().any(|r| r.handler == Handler::Health));
        assert!(rows.iter().all(|r| r.budget_ms > 0
            || matches!(
                r.handler,
                Handler::Events
                    | Handler::Agent
                    | Handler::Collect
                    | Handler::CollectionTick
                    | Handler::CollectionRecheck
                    | Handler::ResetClaims
                    | Handler::CursorAccount
                    | Handler::Store
                    | Handler::UsageHistory
                    | Handler::UsageAt
                    | Handler::UsageBurn
                    | Handler::UsageSample
                    | Handler::ClaudeOpen
                    | Handler::ClaudeHistoryCleanup
                    | Handler::StatusAnswers
                    | Handler::StatusAnswersPost
                    | Handler::SignInPoll
            )
            || (r.handler == Handler::Lifecycle
                && r.method == axum::http::Method::POST
                && matches!(
                    (r.path.as_str(), r.roles.as_slice()),
                    (crate::public_auth::lifecycle_host::ENDPOINT, [Role::Admin])
                        | (
                            crate::public_auth::lifecycle_host::COLLECTION_ENDPOINT,
                            [Role::Admin, Role::Device]
                        )
                ))));
        assert!(rows.iter().any(|r| r.handler == Handler::Store));
        // M5 lane K: the one reset claims row, internal, on its own pool.
        let reset: Vec<&Row> = rows
            .iter()
            .filter(|r| r.handler == Handler::ResetClaims)
            .collect();
        assert_eq!(reset.len(), 1);
        assert_eq!(reset[0].path, "/internal/reset-claims");
        assert_eq!(reset[0].method, axum::http::Method::POST);
        assert_eq!(reset[0].pool, Pool::Reset);
        assert_eq!(reset[0].budget_ms, 0);
        for (path, handler) in [
            ("/internal/collection/recheck", Handler::CollectionRecheck),
            ("/internal/collection-signin-poll", Handler::SignInPoll),
            ("/internal/status/answers", Handler::StatusAnswers),
            (
                crate::public_auth::lifecycle_host::ENDPOINT,
                Handler::Lifecycle,
            ),
            (
                crate::public_auth::lifecycle_host::COLLECTION_ENDPOINT,
                Handler::Lifecycle,
            ),
        ] {
            assert!(
                rows.iter()
                    .any(|row| row.path == path && row.handler == handler && row.budget_ms == 0),
                "{path} must keep its owner-managed budget",
            );
        }
        Ok(())
    }

    #[test]
    fn startup_refuses_a_reset_claims_row_off_its_one_shape() {
        let tail = r#"{"method":"*","path":"*","owner":"ts-fallback"}"#;
        let good = r#"{"method":"POST","path":"/internal/reset-claims","owner":"rust","group":"internal","roles":["service"],"budgetMs":0,"handler":"reset-claims"}"#;
        assert!(rows(&format!("[{good},{tail}]")).is_ok());
        for bad in [
            // a deadline the TS route owns
            r#"{"method":"POST","path":"/internal/reset-claims","owner":"rust","group":"internal","roles":["service"],"budgetMs":400,"handler":"reset-claims"}"#,
            // another path
            r#"{"method":"POST","path":"/internal/reset","owner":"rust","group":"internal","roles":["service"],"budgetMs":0,"handler":"reset-claims"}"#,
            // a read
            r#"{"method":"GET","path":"/internal/reset-claims","owner":"rust","group":"internal","roles":["service"],"budgetMs":0,"handler":"reset-claims"}"#,
            // a public route
            r#"{"method":"POST","path":"/api/accounts/x/reset-claims","owner":"rust","group":"writes","roles":["admin"],"budgetMs":0,"handler":"reset-claims"}"#,
        ] {
            assert!(rows(&format!("[{bad},{tail}]")).is_err(), "{bad}");
        }
    }

    #[test]
    fn startup_refuses_the_worker_removal_rows_off_their_one_shape() -> io::Result<()> {
        let tail = r#"{"method":"*","path":"*","owner":"ts-fallback"}"#;
        for good in [
            r#"{"method":"GET","path":"/api/status/answers","owner":"rust","group":"status","roles":["admin"],"budgetMs":400,"handler":"status-answers-read"}"#,
            r#"{"method":"POST","path":"/api/status/answers","owner":"rust","group":"status","roles":["admin"],"budgetMs":0,"handler":"status-answers-post"}"#,
            r#"{"method":"GET","path":"/api/usage/history","owner":"rust","group":"store","roles":["admin","device"],"budgetMs":0,"handler":"usage-history"}"#,
            r#"{"method":"GET","path":"/api/usage/at","owner":"rust","group":"store","roles":["admin","device"],"budgetMs":0,"handler":"usage-at"}"#,
            r#"{"method":"GET","path":"/api/usage/burn","owner":"rust","group":"store","roles":["admin","device"],"budgetMs":0,"handler":"usage-burn"}"#,
        ] {
            assert!(rows(&format!("[{good},{tail}]")).is_ok(), "{good}");
        }
        for bad in [
            // the answers write with a deadline
            r#"{"method":"POST","path":"/api/status/answers","owner":"rust","group":"status","roles":["admin"],"budgetMs":400,"handler":"status-answers-post"}"#,
            // the answers read as a write
            r#"{"method":"POST","path":"/api/status/answers","owner":"rust","group":"status","roles":["admin"],"budgetMs":400,"handler":"status-answers-read"}"#,
            // a usage read with the wrong path
            r#"{"method":"GET","path":"/api/usage","owner":"rust","group":"store","roles":["admin","device"],"budgetMs":0,"handler":"usage-history"}"#,
            // a usage read open to the service role
            r#"{"method":"GET","path":"/api/usage/at","owner":"rust","group":"store","roles":["service"],"budgetMs":0,"handler":"usage-at"}"#,
            // a capped timeout bypasses the store's queue for a wide-range read
            r#"{"method":"GET","path":"/api/usage/burn","owner":"rust","group":"store","roles":["admin","device"],"budgetMs":400,"handler":"usage-burn"}"#,
        ] {
            assert!(rows(&format!("[{bad},{tail}]")).is_err(), "{bad}");
        }
        let rows = rows(ROUTES)?;
        for handler in [
            Handler::StatusAnswersRead,
            Handler::StatusAnswersPost,
            Handler::UsageHistory,
            Handler::UsageAt,
            Handler::UsageBurn,
        ] {
            assert_eq!(
                rows.iter().filter(|row| row.handler == handler).count(),
                1,
                "{handler:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn startup_refuses_rows_without_a_known_handler_or_with_bad_fields() {
        let good = r#"{"method":"GET","path":"/api/health","owner":"rust","group":"core","roles":["public"],"budgetMs":400,"handler":"health"}"#;
        let tail = r#"{"method":"*","path":"*","owner":"ts-fallback"}"#;
        assert!(rows(&format!("[{good},{tail}]")).is_ok());
        for bad in [
            // no handler
            r#"{"method":"GET","path":"/api/health","owner":"rust","group":"core","roles":["public"],"budgetMs":400}"#,
            // unknown handler
            r#"{"method":"GET","path":"/api/health","owner":"rust","group":"core","roles":["public"],"budgetMs":400,"handler":"nope"}"#,
            // unknown role
            r#"{"method":"GET","path":"/api/health","owner":"rust","group":"core","roles":["root"],"budgetMs":400,"handler":"health"}"#,
            // no roles
            r#"{"method":"GET","path":"/api/health","owner":"rust","group":"core","roles":[],"budgetMs":400,"handler":"health"}"#,
            // a read with no deadline
            r#"{"method":"GET","path":"/api/health","owner":"rust","group":"core","roles":["public"],"budgetMs":0,"handler":"health"}"#,
            // a public route reachable by the service role
            r#"{"method":"GET","path":"/api/health","owner":"rust","group":"core","roles":["service"],"budgetMs":400,"handler":"health"}"#,
            // an internal route reachable by a session
            r#"{"method":"PUT","path":"/internal/feed/{source}","owner":"rust","group":"internal","roles":["admin"],"budgetMs":400,"handler":"feed"}"#,
            // a write that is not the feed
            r#"{"method":"PUT","path":"/api/health","owner":"rust","group":"core","roles":["public"],"budgetMs":400,"handler":"health"}"#,
            // an extra key
            r#"{"method":"GET","path":"/api/health","owner":"rust","group":"core","roles":["public"],"budgetMs":400,"handler":"health","x":1}"#,
        ] {
            assert!(rows(&format!("[{bad},{tail}]")).is_err(), "{bad}");
        }
        assert!(
            rows(&format!("[{good}]")).is_err(),
            "the wildcard row is required"
        );
        assert!(
            rows(&format!("[{tail},{good}]")).is_err(),
            "the wildcard row comes last"
        );
        assert!(
            rows(&format!("[{good},{good},{tail}]")).is_err(),
            "one row per route"
        );
    }
}
