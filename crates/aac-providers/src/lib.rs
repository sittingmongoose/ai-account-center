//! The shared provider, tool and host tables (`config/tables/*.json`), as typed constants.
//!
//! `build.rs` generates [`ProviderId`], [`PROVIDERS`], [`ANALYTICS_TOOLS`] and the other statics
//! from the same JSON files TypeScript reads (`src/shared/tables.ts`), so Rust and TS cannot
//! drift. Consumers derive their lists from these rows and hold no list literals of their own
//! (`tests/drift.rs` checks that). An id the table does not list is `None` here: a consumer
//! declines it, never treats it as a retired provider. Computers stay in
//! `web-dashboard/public/computers.json`.

include!(concat!(env!("OUT_DIR"), "/tables.rs"));

/// The JSON the constants were generated from (tests compare the two).
pub const PROVIDERS_JSON: &str = include_str!("../../../config/tables/providers.json");
/// See [`PROVIDERS_JSON`].
pub const TOOLS_JSON: &str = include_str!("../../../config/tables/tools.json");
/// See [`PROVIDERS_JSON`].
pub const HOSTS_JSON: &str = include_str!("../../../config/tables/hosts.json");
/// See [`PROVIDERS_JSON`].
pub const ANTIGRAVITY_JSON: &str = include_str!("../../../config/tables/antigravity-columns.json");

/// One provider row of `providers.json`.
#[derive(Debug, PartialEq, Eq)]
pub struct Provider {
    /// The row's id.
    pub id: ProviderId,
    /// The row's position in the table.
    pub index: usize,
    /// The dashboard label (`label`).
    pub label: &'static str,
    /// The long label (`longLabel`).
    pub long_label: &'static str,
    /// A dashboard provider (`DASHBOARD_PROVIDER_IDS`); false for failover-only rows.
    pub dashboard: bool,
    /// Home shows it as a provider card (view-model.mjs `CARD_PROVIDERS`).
    pub card: bool,
    /// Its API keys live in AAC's key store (Python `KEY_PROVIDERS`).
    pub key_store: bool,
    /// Analytics' provider order and label (`USAGE_PROVIDER_ORDER`, `ADDITIONAL_PROVIDERS`).
    pub analytics: Option<Analytics>,
    /// The page's own registry row (view-model.mjs `PROVIDER_REGISTRY`).
    pub page: Option<Page>,
    /// Failover inventory: its label and identity kinds (`inventoryKind`).
    pub failover: Failover,
    /// A plan provider the quota reader answers (Python `plan_common.PROVIDERS`).
    pub quota: bool,
}

/// `analytics` of a provider row.
#[derive(Debug, PartialEq, Eq)]
pub struct Analytics {
    /// Position in Analytics' provider order ("other" follows the last).
    pub order: u32,
    /// Analytics' label for the provider.
    pub label: &'static str,
}

/// `page` of a provider row: the page's fallback registry facts.
#[derive(Debug, PartialEq, Eq)]
pub struct Page {
    /// The sign-in words.
    pub sign_in: &'static str,
    /// Whether the page lists several accounts.
    pub multi: bool,
}

/// `failover` of a provider row.
#[derive(Debug, PartialEq, Eq)]
pub struct Failover {
    /// The failover label (Python `failover_usage.LABELS`).
    pub label: &'static str,
    /// The identity kinds an inventory candidate may carry.
    pub kinds: &'static [&'static str],
}

/// `planRetryAfter`: Retry-After seconds are clamped to `floor_s..=ceiling_s`; an unreadable
/// header counts as `default_s`.
#[derive(Debug, PartialEq, Eq)]
pub struct RetryAfter {
    /// Lower clamp.
    pub floor_s: u64,
    /// Upper clamp.
    pub ceiling_s: u64,
    /// The value of an absent or unreadable header.
    pub default_s: u64,
}

/// One row of `tools.json` `analyticsTools`.
#[derive(Debug, PartialEq, Eq)]
pub struct AnalyticsTool {
    /// The tool id (Analytics kind and scanner kind).
    pub id: &'static str,
    /// The tool's label.
    pub label: &'static str,
    /// It has experiment usage roots.
    pub experiment: bool,
    /// The scanner accepts extra roots for it.
    pub extra_roots: bool,
}

/// An id with a label (`scannerOnlyKinds`, `usageLogTools`).
#[derive(Debug, PartialEq, Eq)]
pub struct Labelled {
    /// The id.
    pub id: &'static str,
    /// The label.
    pub label: &'static str,
}

/// One key of `usageLogFieldMapping`.
#[derive(Debug, PartialEq, Eq)]
pub struct MappingKey {
    /// The key name.
    pub name: &'static str,
    /// A mapping must name it.
    pub required: bool,
}

impl ProviderId {
    /// This provider's row.
    pub fn row(self) -> &'static Provider {
        &PROVIDERS[self as usize]
    }
}

/// The row of a provider id, or `None` when the table does not list it.
pub fn provider(id: &str) -> Option<&'static Provider> {
    ProviderId::parse(id).map(ProviderId::row)
}

/// The dashboard providers (`DASHBOARD_PROVIDER_IDS`), in table order.
pub fn dashboard_providers() -> impl Iterator<Item = &'static Provider> {
    PROVIDERS.iter().filter(|p| p.dashboard)
}

/// Analytics' provider table (`defaultAnalyticsProviderTable`): every provider row with an
/// `analytics` block, in its declared order, as `(id, label, order)`.
pub fn analytics_provider_table() -> Vec<(&'static str, &'static str, u32)> {
    let mut rows: Vec<(&'static str, &'static str, u32)> = PROVIDERS
        .iter()
        .filter_map(|row| {
            row.analytics
                .as_ref()
                .map(|analytics| (row.id.as_str(), analytics.label, analytics.order))
        })
        .collect();
    rows.sort_by_key(|row| row.2);
    rows
}

/// TS `isDashboardProviderId`.
pub fn is_dashboard_provider(id: &str) -> bool {
    provider(id).is_some_and(|p| p.dashboard)
}

/// Home's card providers, in table order.
pub fn card_providers() -> impl Iterator<Item = &'static Provider> {
    PROVIDERS.iter().filter(|p| p.card)
}

/// Every scanner kind: the analytics tools, then the scanner-only kinds.
pub fn scanner_kinds() -> impl Iterator<Item = &'static str> {
    ANALYTICS_TOOLS
        .iter()
        .map(|t| t.id)
        .chain(SCANNER_ONLY_KINDS.iter().map(|k| k.id))
}

/// A tool an extra usage-log source may name.
pub fn is_usage_log_tool(id: &str) -> bool {
    USAGE_LOG_TOOLS.iter().any(|t| t.id == id)
}

// ------------------------------------------------------- consumer views

/// The providers whose accounts are standalone usage sources (`additionalProviders`), in their
/// declared order: the dashboard providers whose usage is not read from a tool's logs.
pub fn additional_providers() -> impl Iterator<Item = &'static Provider> {
    ADDITIONAL_PROVIDERS.iter().map(|id| id.row())
}

/// The provider order the agent usage answer publishes (`snapshotOrder`).
pub fn snapshot_providers() -> impl Iterator<Item = &'static Provider> {
    SNAPSHOT_PROVIDERS.iter().map(|id| id.row())
}

/// The providers the interactive sign-in session verb admits (`signInSessions`).
pub fn sign_in_session_providers() -> impl Iterator<Item = &'static Provider> {
    SIGN_IN_SESSIONS.iter().map(|id| id.row())
}

/// The dashboard provider a usage route word names (`usageRoutes`).
pub fn usage_route_provider(route: &str) -> Option<&'static str> {
    USAGE_ROUTES
        .iter()
        .find(|(word, _)| *word == route)
        .map(|(_, provider)| *provider)
}

/// The models.dev provider a pricing provider word names (`pricingAliases`); a word the table
/// does not alias is its own models.dev id.
pub fn pricing_alias(provider: &str) -> Option<&'static str> {
    PRICING_ALIASES
        .iter()
        .find(|(word, _)| *word == provider)
        .map(|(_, models_dev)| *models_dev)
}

/// The tools the activity answer's range reads (`activitySources`), in its order.
pub fn activity_sources() -> impl Iterator<Item = &'static str> {
    ACTIVITY_SOURCES.iter().copied()
}

/// Every tool the activity collector reads: [`activity_sources`], then the scanner-only kinds.
pub fn activity_tools() -> impl Iterator<Item = &'static str> {
    activity_sources().chain(SCANNER_ONLY_KINDS.iter().map(|kind| kind.id))
}

/// A session target the activity answer accepts (`cliTargets`).
pub fn is_cli_target(target: &str) -> bool {
    CLI_TARGETS.contains(&target)
}

/// The provider whose usage a tool's logs are, when they are one provider's by construction
/// (`toolProviders`).
pub fn tool_provider(tool: &str) -> Option<&'static str> {
    TOOL_PROVIDERS
        .iter()
        .find(|(name, _)| *name == tool)
        .map(|(_, provider)| *provider)
}

/// The applications native app discovery reports (`appInventory`), in its order.
pub fn app_inventory() -> impl Iterator<Item = &'static str> {
    APP_INVENTORY.iter().copied()
}

/// The last legacy cache version whose rows for a tool must be rebuilt
/// (`legacyRebuildThrough`); `None` for a tool no legacy cache holds.
pub fn legacy_rebuild_through(tool: &str) -> Option<u32> {
    LEGACY_REBUILD_THROUGH
        .iter()
        .find(|(name, _)| *name == tool)
        .map(|(_, version)| *version)
}

/// The usage-log tools that bring their own built-in roots: every one but the generic JSONL
/// format ([`GENERIC_USAGE_LOG_TOOL`]), whose roots an extra source supplies.
pub fn builtin_root_tools() -> impl Iterator<Item = &'static str> {
    USAGE_LOG_TOOLS
        .iter()
        .map(|tool| tool.id)
        .filter(|id| *id != GENERIC_USAGE_LOG_TOOL)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rows_are_indexed_and_parse_round_trips() {
        for (index, row) in PROVIDERS.iter().enumerate() {
            assert_eq!(row.index, index);
            assert_eq!(ProviderId::ALL[index], row.id);
            assert_eq!(ProviderId::parse(row.id.as_str()), Some(row.id));
            assert_eq!(provider(row.id.as_str()), Some(row));
        }
        for unknown in [
            "",
            "retired-provider",
            "Claude",
            "constructor",
            "__proto__",
            "other",
        ] {
            assert_eq!(provider(unknown), None, "{unknown}");
        }
    }

    /// The consumer views equal the hand lists the crates held before they were derived, value
    /// for value and, where the consumers read them in order, order for order.
    #[test]
    fn consumer_views_keep_the_old_lists() {
        let providers = |rows: Vec<&'static Provider>| -> Vec<&'static str> {
            rows.into_iter().map(|row| row.id.as_str()).collect()
        };
        assert_eq!(
            providers(additional_providers().collect()),
            [
                "antigravity",
                "muse",
                "cursor",
                "kimi-code",
                "qwen",
                "zai",
                "opencode-go"
            ]
        );
        assert_eq!(
            providers(snapshot_providers().collect()),
            [
                "codex",
                "claude",
                "antigravity",
                "muse",
                "cursor",
                "kimi-code",
                "qwen",
                "zai",
                "opencode-go"
            ]
        );
        assert_eq!(
            providers(sign_in_session_providers().collect()),
            ["codex", "muse", "antigravity", "claude"]
        );
        assert_eq!(
            USAGE_ROUTES.to_vec(),
            [
                ("anthropic", "claude"),
                ("claude", "claude"),
                ("openai", "codex"),
                ("codex", "codex"),
                ("alibaba-token-plan", "qwen"),
                ("zai", "zai"),
                ("zai-coding-plan", "zai"),
                ("kimi-code", "kimi-code"),
                ("opencode-go", "opencode-go"),
                ("opencode-zen", "opencode-go"),
                ("cursor", "cursor"),
                ("muse-code", "muse"),
                ("google-antigravity", "antigravity")
            ]
        );
        assert_eq!(usage_route_provider("opencode-zen"), Some("opencode-go"));
        assert_eq!(usage_route_provider("gemini"), None);
        assert_eq!(
            PRICING_ALIASES.to_vec(),
            [
                ("agy", "google"),
                ("antigravity", "google"),
                ("claude", "anthropic"),
                ("codex", "openai"),
                ("copilot", "github-copilot"),
                ("gemini", "google"),
                ("ghcp", "github-copilot"),
                ("github", "github-copilot"),
                ("kimi", "moonshotai"),
                ("moonshot", "moonshotai"),
                ("qwen", "alibaba")
            ]
        );
        assert_eq!(pricing_alias("ghcp"), Some("github-copilot"));
        assert_eq!(pricing_alias("google"), None);
        assert_eq!(
            activity_sources().collect::<Vec<_>>(),
            [
                "claude",
                "codex",
                "omp",
                "muse",
                "zcode",
                "jsonl",
                "antigravity"
            ]
        );
        assert_eq!(
            activity_tools().collect::<Vec<_>>(),
            [
                "claude",
                "codex",
                "omp",
                "muse",
                "zcode",
                "jsonl",
                "antigravity",
                "cursor",
                "devin"
            ]
        );
        assert_eq!(
            CLI_TARGETS.to_vec(),
            [
                "claude",
                "codex",
                "droid",
                "omp",
                "muse",
                "zcode",
                "antigravity",
                "cursor",
                "devin"
            ]
        );
        assert!(is_cli_target("droid") && !is_cli_target("codex-cli"));
        assert_eq!(
            TOOL_PROVIDERS.to_vec(),
            [
                ("claude", "claude"),
                ("codex", "codex"),
                ("muse", "muse"),
                ("antigravity", "antigravity"),
                ("cursor", "cursor")
            ]
        );
        assert_eq!(tool_provider("omp"), None);
        assert_eq!(tool_provider("cursor"), Some("cursor"));
        assert_eq!(
            APP_INVENTORY.to_vec(),
            [
                "antigravity-cli",
                "muse-code",
                "omp",
                "codex-cli",
                "codex-desktop",
                "claude-code",
                "claude-desktop",
                "zcode",
                "t3-acp-adapters",
                "t3-code",
                "cursor-cli",
                "antigravity-acp"
            ]
        );
        assert_eq!(
            LEGACY_REBUILD_THROUGH.to_vec(),
            [("claude", 5), ("codex", 6), ("muse", 6)]
        );
        assert_eq!(legacy_rebuild_through("claude"), Some(5));
        assert_eq!(legacy_rebuild_through("zcode"), None);
        assert_eq!(GENERIC_USAGE_LOG_TOOL, "jsonl");
        let mut roots: Vec<&str> = builtin_root_tools().collect();
        roots.sort_unstable();
        assert_eq!(roots, ["claude-code", "codex", "muse", "omp", "zcode"]);
    }
}
