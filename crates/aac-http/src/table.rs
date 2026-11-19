//! L1: the one route table (`src/web-server/rust-service-routes.json`). TS gates from it and Rust
//! builds its dispatch from it: each `rust` row names its group, the principal roles it admits,
//! its budget and its handler. Startup refuses a row without a known handler.
use aac_api::{PrincipalRole, VerifiedPrincipal};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::io;

/// The table, embedded at build time so Rust and TS cannot disagree about it.
pub(crate) const ROUTES: &str = include_str!("../../../src/web-server/rust-service-routes.json");

/// The handlers the service implements. A row names one of these words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Handler {
    Health,
    Diag,
    Settings,
    Home,
    Accounts,
    Profiles,
    Files,
    Status,
    Scoreboard,
    StoreStatus,
    StatusInputs,
    StatusAnswers,
    StatusUsage,
    ScoreboardInputs,
    Feed,
    FeedStatus,
    Events,
    Agent,
    AgentControl,
    Store,
}

impl Handler {
    fn named(word: &str) -> Option<Self> {
        Some(match word {
            "health" => Handler::Health,
            "diag" => Handler::Diag,
            "settings" => Handler::Settings,
            "home" => Handler::Home,
            "accounts" => Handler::Accounts,
            "profiles" => Handler::Profiles,
            "files" => Handler::Files,
            "status" => Handler::Status,
            "scoreboard" => Handler::Scoreboard,
            "store-status" => Handler::StoreStatus,
            "status-inputs" => Handler::StatusInputs,
            "status-answers" => Handler::StatusAnswers,
            "status-usage" => Handler::StatusUsage,
            "scoreboard-inputs" => Handler::ScoreboardInputs,
            "feed" => Handler::Feed,
            "feed-status" => Handler::FeedStatus,
            "events" => Handler::Events,
            "agent" => Handler::Agent,
            "agent-control" => Handler::AgentControl,
            "store" => Handler::Store,
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
                    Handler::Events | Handler::Agent | Handler::Store | Handler::StatusAnswers
                ))
            || internal != (row.group == "internal")
            || internal != roles.iter().all(|r| *r == Role::Service)
            || (!internal && roles.contains(&Role::Service))
            || (!internal && !row.path.starts_with("/api/"))
            || (method == axum::http::Method::POST
                && !matches!(
                    handler,
                    Handler::Agent | Handler::AgentControl | Handler::Store
                ))
            // G16b: the forwarded answer send is the one other internal write.
            || (method == axum::http::Method::PUT
                && !matches!(
                    handler,
                    Handler::Feed | Handler::StatusAnswers | Handler::StatusUsage
                ))
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
                Handler::Events | Handler::Agent | Handler::Store | Handler::StatusAnswers
            )));
        assert!(rows.iter().any(|r| r.handler == Handler::Store));
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
