//! Home view model v3: the JSON `web-dashboard/public/view-model.mjs` `dashboardViewModel` builds, as types.
//!
//! The Rust server (`aac-views::home`) serializes these and the Leptos Home page (`web-dashboard/leptos`) reads them,
//! so a shape drift between the two is a compile error. Field order is the JS object literals' order and numbers are
//! written as JavaScript writes them, so a Rust-built view is byte-equal to `JSON.stringify` of the JS one
//! (tests/spec/views/home). Reading is lenient the way the page always read the JS view: a missing field or `null`
//! text or list reads as empty, never as a guessed value.
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The view model version Home draws (view-model.mjs `VIEW_MODEL_VERSION`).
pub const HOME_VIEW_VERSION: u32 = 3;

/// Text and lists sent as `null` read as empty; numbers are left strict.
pub fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// Writes an optional number as `JSON.stringify` does: integers as integers, others in JS notation, non-finite as null.
/// The text is `aac_num::ecma::to_string`, whose shortest digits break a tie between two equally close strings to the
/// even one as ECMAScript does (2251799813685247.25 writes `.2`; Rust's own formatter wrote `.3`); it is proven against
/// Node over all of binary64 (`tests/spec/num/ecma.json`).
pub fn js_number<S: Serializer>(value: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match value {
        None => s.serialize_none(),
        Some(x) if !x.is_finite() => s.serialize_none(),
        Some(x) if *x == x.trunc() && x.abs() < 9_007_199_254_740_992.0 => {
            s.serialize_i64(*x as i64)
        }
        Some(x) => serde_json::value::RawValue::from_string(aac_num::ecma::to_string(*x))
            .map_err(serde::ser::Error::custom)?
            .serialize(s),
    }
}

/// A piece of a line with its own weight and tone ("**3** accounts"). Tone is `""`, `"good"` or `"warn"` on Home.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Run {
    /// The words.
    #[serde(deserialize_with = "nullable")]
    pub text: String,
    /// Drawn bold.
    pub strong: bool,
    /// Colour word; Home always sends one (`""` for none).
    pub tone: Option<String>,
}

/// One window's reading as `meterView` words it. `key` empty is an intentionally empty slot; `has_value` false is a
/// real window with no reading (never a zero bar).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MeterView {
    /// `account id|window key`, or empty for an empty slot.
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    /// Column or short label.
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    /// Long label for Details and titles.
    #[serde(deserialize_with = "nullable")]
    pub full_label: String,
    /// A real reading to draw.
    pub has_value: bool,
    /// Used percent; 0 when there is no reading (`has_value` says which).
    #[serde(serialize_with = "js_number")]
    pub value: Option<f64>,
    /// The percent in words.
    #[serde(deserialize_with = "nullable")]
    pub value_text: String,
    /// Percent past 100, in words.
    #[serde(deserialize_with = "nullable")]
    pub over_text: String,
    /// "resets in 2h 5m".
    #[serde(deserialize_with = "nullable")]
    pub reset: String,
    /// "Resets Nov 14, 3:05 PM".
    #[serde(deserialize_with = "nullable")]
    pub reset_exact: String,
    /// Resets within two hours.
    pub reset_soon: bool,
    /// Words for a window without a reading.
    #[serde(deserialize_with = "nullable")]
    pub na_text: String,
    /// Second line for a window without a reading.
    #[serde(deserialize_with = "nullable")]
    pub na_sub: String,
    /// "1.2K of 5K".
    #[serde(deserialize_with = "nullable")]
    pub amount: String,
    /// "3 credits left".
    #[serde(deserialize_with = "nullable")]
    pub left: String,
    /// When the reading was sampled.
    #[serde(deserialize_with = "nullable")]
    pub sampled: String,
    /// Source, computer and status.
    #[serde(deserialize_with = "nullable")]
    pub source: String,
    /// Caption (unused on Home).
    #[serde(deserialize_with = "nullable")]
    pub caption: String,
    /// Caption tip (unused on Home).
    #[serde(deserialize_with = "nullable")]
    pub caption_tip: String,
}

/// Packs, balances, credits and spend: amounts, never fake meters.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AmountView {
    /// `account id|window key`.
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    /// Label.
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    /// Main figure.
    #[serde(deserialize_with = "nullable")]
    pub value: String,
    /// Words after the figure ("of 5 left").
    #[serde(deserialize_with = "nullable")]
    pub unit: String,
    /// Second line.
    #[serde(deserialize_with = "nullable")]
    pub sub: String,
    /// Icon name.
    #[serde(deserialize_with = "nullable")]
    pub icon: String,
    /// A pack that is used up.
    pub spent: bool,
}

/// A section's footer line (the Antigravity runtime's warnings).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FootView {
    /// Drawn at all.
    pub shown: bool,
    /// Drawn as a warning.
    pub warn: bool,
    /// The line.
    #[serde(deserialize_with = "nullable")]
    pub runs: Vec<Run>,
    /// The line as wrap units.
    #[serde(deserialize_with = "nullable")]
    pub words: Vec<Run>,
    /// A time beside it.
    #[serde(deserialize_with = "nullable")]
    pub when: String,
}

/// One window column of a section.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ColumnView {
    /// Window key, or `five` / `weekly`.
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    /// Column head.
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    /// Single-line head (Antigravity only; it is never empty there, and the other sections send none).
    #[serde(
        deserialize_with = "nullable",
        skip_serializing_if = "String::is_empty"
    )]
    pub short_label: String,
}

/// One account row of a section.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RowView {
    /// Account id.
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    /// Provider id.
    #[serde(deserialize_with = "nullable")]
    pub provider: String,
    /// Profile id, or empty.
    #[serde(deserialize_with = "nullable")]
    pub profile: String,
    /// Email or label.
    #[serde(deserialize_with = "nullable")]
    pub email: String,
    /// Reported plan.
    #[serde(deserialize_with = "nullable")]
    pub plan: String,
    /// Plan, computer, sample age.
    #[serde(deserialize_with = "nullable")]
    pub meta: String,
    /// Status words.
    #[serde(deserialize_with = "nullable")]
    pub status: String,
    /// The reading's own message.
    #[serde(deserialize_with = "nullable")]
    pub note: String,
    /// Computer label.
    #[serde(deserialize_with = "nullable")]
    pub platform: String,
    /// Retired; always false.
    pub setup: bool,
    /// Claude can open on the Mac.
    pub can_mac: bool,
    /// Claude can open on Windows.
    pub can_windows: bool,
    /// Amounts line as text.
    #[serde(deserialize_with = "nullable")]
    pub amounts_line: String,
    /// Amounts line as runs.
    #[serde(deserialize_with = "nullable")]
    pub amounts_runs: Vec<Run>,
    /// Amounts line as wrap units.
    #[serde(deserialize_with = "nullable")]
    pub amounts_words: Vec<Run>,
    /// One meter per column.
    #[serde(deserialize_with = "nullable")]
    pub cells: Vec<MeterView>,
}

/// The Claude, Codex or Antigravity section.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SectionView {
    /// Provider id.
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    /// `claude` or `accounts`.
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    /// Short name.
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    /// Long name.
    #[serde(deserialize_with = "nullable")]
    pub long_label: String,
    /// Meta line as text.
    #[serde(deserialize_with = "nullable")]
    pub meta: String,
    /// Meta line as runs.
    #[serde(deserialize_with = "nullable")]
    pub meta_runs: Vec<Run>,
    /// Meta line as wrap units.
    #[serde(deserialize_with = "nullable")]
    pub meta_words: Vec<Run>,
    /// Footer line.
    pub foot: FootView,
    /// Words for no accounts.
    #[serde(deserialize_with = "nullable")]
    pub empty: String,
    /// Window columns.
    #[serde(deserialize_with = "nullable")]
    pub columns: Vec<ColumnView>,
    /// Account rows.
    #[serde(deserialize_with = "nullable")]
    pub rows: Vec<RowView>,
}

/// One single-account provider card (Cursor, Muse, Kimi, Qwen, Z.ai, OpenCode Go).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CardView {
    /// Account id.
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    /// Provider id.
    #[serde(deserialize_with = "nullable")]
    pub provider: String,
    /// Account id again (Details target).
    #[serde(deserialize_with = "nullable")]
    pub account_id: String,
    /// Provider name.
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    /// Email or label.
    #[serde(deserialize_with = "nullable")]
    pub identity: String,
    /// Plan without the provider's own name.
    #[serde(deserialize_with = "nullable")]
    pub plan: String,
    /// Status words.
    #[serde(deserialize_with = "nullable")]
    pub status: String,
    /// Status and source.
    #[serde(deserialize_with = "nullable")]
    pub source: String,
    /// "Stale", a status, or empty.
    #[serde(deserialize_with = "nullable")]
    pub flag: String,
    /// "sampled 5m ago".
    #[serde(deserialize_with = "nullable")]
    pub sampled: String,
    /// Computer label.
    #[serde(deserialize_with = "nullable")]
    pub platform: String,
    /// Plan note.
    #[serde(deserialize_with = "nullable")]
    pub plan_note: String,
    /// Shared pack expiry.
    #[serde(deserialize_with = "nullable")]
    pub packs_note: String,
    /// The reading's message, or "Usage unavailable".
    #[serde(deserialize_with = "nullable")]
    pub note: String,
    /// Up to three meters.
    #[serde(deserialize_with = "nullable")]
    pub meters: Vec<MeterView>,
    /// Amounts.
    #[serde(deserialize_with = "nullable")]
    pub amounts: Vec<AmountView>,
}

/// One provider of the registry.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RegistryRow {
    /// Provider id.
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    /// Short name.
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    /// Long name.
    #[serde(deserialize_with = "nullable")]
    pub long_label: String,
    /// How it signs in.
    #[serde(deserialize_with = "nullable")]
    pub sign_in: String,
    /// Takes more than one account.
    pub multi: bool,
    /// Shown on Home.
    pub visible: bool,
    /// Accounts reported.
    pub count: u64,
}

/// The header's words.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChromeView {
    /// "Updated", "Refreshing" or "Account data unavailable".
    #[serde(deserialize_with = "nullable")]
    pub status_lead: String,
    /// "5m ago".
    #[serde(deserialize_with = "nullable")]
    pub status_strong: String,
    /// "live" or "cached readings".
    #[serde(deserialize_with = "nullable")]
    pub status_more: String,
    /// Tooltip.
    #[serde(deserialize_with = "nullable")]
    pub status_tip: String,
    /// A refresh runs.
    pub refreshing: bool,
    /// Signed-in user.
    #[serde(deserialize_with = "nullable")]
    pub username: String,
    /// The page's host.
    #[serde(deserialize_with = "nullable")]
    pub host: String,
}

/// The complete Home view model.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HomeView {
    /// [`HOME_VIEW_VERSION`].
    pub version: u32,
    /// Claude, Codex, Antigravity.
    #[serde(deserialize_with = "nullable")]
    pub sections: Vec<SectionView>,
    /// The other providers.
    #[serde(deserialize_with = "nullable")]
    pub cards: Vec<CardView>,
    /// Every provider with its visibility and count.
    #[serde(deserialize_with = "nullable")]
    pub registry: Vec<RegistryRow>,
    /// The header's words.
    pub chrome: ChromeView,
}

/// What the browser alone knows when it asks for the finished view (sent by the TS front door to Rust).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HomeContext {
    /// The browser's clock, UTC ms.
    pub now: i64,
    /// The display time zone the page formats in.
    pub time_zone: String,
    /// `mac` or `windows`.
    pub platform: String,
    /// The refresh interval the page runs (30..=3600).
    pub interval_seconds: u32,
    /// Signed-in user, as the page shows it.
    pub username: String,
    /// The page's host.
    pub host: String,
    /// The page's own formatter outputs for fixed reference values (view-model.mjs `homeFormatProbe`).
    pub format_probe: Vec<String>,
    /// The page's display-zone UTC offsets (view-model.mjs `homeZoneFacts`); absent reads as no proof.
    #[serde(default)]
    pub zone: HomeZone,
}

/// The display zone's UTC offsets as the page's own `Intl` found them, over `from..=to` (UTC ms): `offsets` holds
/// `[start, minutes]` segments, the first starting at `from`, each holding until the next starts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HomeZone {
    /// The first instant the facts cover.
    pub from: i64,
    /// The last instant the facts cover.
    pub to: i64,
    /// `[start ms, offset minutes]`, in time order.
    pub offsets: Vec<[i64; 2]>,
}

#[cfg(test)]
mod tests {
    // Exact binary ties and neighbours are written out in full on purpose.
    #![allow(clippy::excessive_precision)]
    use super::*;

    #[test]
    fn numbers_are_written_as_javascript_writes_them() -> serde_json::Result<()> {
        for (x, js) in [
            (0.0, "0"),
            (-0.0, "0"),
            (100.0, "100"),
            (37.5, "37.5"),
            (33.333333333333336, "33.333333333333336"),
            (0.000001, "0.000001"),
            (1e-7, "1e-7"),
            (1.5e-7, "1.5e-7"),
            (1e21, "1e+21"),
            (1.5e20, "150000000000000000000"),
            (123456789012345680000.0, "123456789012345680000"),
            (-2.5, "-2.5"),
            (9_007_199_254_740_992.0, "9007199254740992"),
            (2251799813685247.25, "2251799813685247.2"),
            (140737488355327.125, "140737488355327.12"),
        ] {
            assert_eq!(aac_num::ecma::to_string(x), js, "{x}");
            let v = MeterView {
                value: Some(x),
                ..Default::default()
            };
            let text = serde_json::to_string(&v)?;
            assert!(text.contains(&format!("\"value\":{js}")), "{text}");
        }
        Ok(())
    }

    #[test]
    fn the_view_reads_with_nulls_and_missing_values() -> serde_json::Result<()> {
        let v: HomeView = serde_json::from_str(
            r#"{"version":3,"sections":[{"id":"claude","metaRuns":[{"text":"3","strong":true,"tone":""}],
            "foot":{"shown":false,"runs":null,"when":null},"columns":[{"key":"five","label":null}],
            "rows":[{"id":"a","email":null,"cells":[{"key":"a|five_hour","hasValue":false,"value":null}]}]}],
            "cards":null,"chrome":{"statusLead":"Updated"}}"#,
        )?;
        assert_eq!(v.sections[0].rows[0].cells[0].value, None);
        assert!(v.cards.is_empty());
        assert_eq!(v.sections[0].columns[0].label, "");
        Ok(())
    }
}
