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
}
