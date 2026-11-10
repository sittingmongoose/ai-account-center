//! The Accounts & Settings view, version 3 (`public/accounts-view.mjs` `accountsViewModel`). Every word and every
//! enabled, refused or "coming" decision is made there; these types only read it. Text and lists read through
//! `nullable` (a `null` becomes empty), numbers stay strict.

use crate::api::Run;
use crate::dash::nullable;
use serde::Deserialize;

/// The version this page draws; any other is refused, not half-drawn (src/accounts.rs does the same).
pub const ACCOUNTS_VIEW_VERSION: u64 = 3;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct AccountsView {
    pub version: u64,
    #[serde(deserialize_with = "nullable")]
    pub access_message: String,
    #[serde(deserialize_with = "nullable")]
    pub col_a: Vec<Provider>,
    #[serde(deserialize_with = "nullable")]
    pub col_b: Vec<Provider>,
    pub ag: AgPolicy,
    #[serde(deserialize_with = "nullable")]
    pub policies: Vec<Policy>,
    #[serde(deserialize_with = "nullable")]
    pub install: String,
    pub refresh: Refresh,
    pub timezone: TimeZone,
    pub cleanup: Cleanup,
    pub log_sources: LogSources,
    pub update: Update,
    pub signin: Signin,
    #[serde(deserialize_with = "nullable")]
    pub connection: Vec<Fact>,
    pub about: About,
}

/// One fixed action slot (`action()` in accounts-view.mjs). kind: switch | button | icon | quiet | empty.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Action {
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    #[serde(deserialize_with = "nullable")]
    pub act: String,
    #[serde(deserialize_with = "nullable")]
    pub value: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub icon: String,
    #[serde(deserialize_with = "nullable")]
    pub platform: String,
    #[serde(deserialize_with = "nullable")]
    pub style: String,
    pub enabled: bool,
    pub coming: bool,
    pub refused: bool,
    pub busy: bool,
    #[serde(deserialize_with = "nullable")]
    pub tip: String,
    #[serde(deserialize_with = "nullable")]
    pub probe: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Provider {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub kind_label: String,
    #[serde(deserialize_with = "nullable")]
    pub kind_icon: String,
    pub count: u32,
    #[serde(deserialize_with = "nullable")]
    pub count_text: String,
    pub visible: bool,
    #[serde(deserialize_with = "nullable")]
    pub hidden_note: String,
    pub toggle_enabled: bool,
    #[serde(deserialize_with = "nullable")]
    pub toggle_tip: String,
    pub tray_visible: bool,
    pub tray_enabled: bool,
    pub tray_coming: bool,
    #[serde(deserialize_with = "nullable")]
    pub tray_tip: String,
    pub switchable: bool,
    pub can_switch: bool,
    pub slots: u32,
    #[serde(deserialize_with = "nullable")]
    pub how: String,
    pub needs: bool,
    #[serde(deserialize_with = "nullable")]
    pub foot: Vec<Action>,
    #[serde(deserialize_with = "nullable")]
    pub empty: String,
    pub ag: bool,
    pub flow: Flow,
    #[serde(deserialize_with = "nullable")]
    pub trash: Vec<TrashRow>,
    #[serde(deserialize_with = "nullable")]
    pub rows: Vec<Row>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Row {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub provider: String,
    #[serde(deserialize_with = "nullable")]
    pub email: String,
    #[serde(deserialize_with = "nullable")]
    pub meta: String,
    #[serde(deserialize_with = "nullable")]
    pub status: String,
    #[serde(deserialize_with = "nullable")]
    pub sampled: String,
    #[serde(deserialize_with = "nullable")]
    pub sampled_tip: String,
    #[serde(deserialize_with = "nullable")]
    pub src: String,
    #[serde(deserialize_with = "nullable")]
    pub src_sub: String,
    pub active: bool,
    #[serde(deserialize_with = "nullable")]
    pub active_label: String,
    pub can_activate: bool,
    #[serde(deserialize_with = "nullable")]
    pub activate_kind: String,
    #[serde(deserialize_with = "nullable")]
    pub profile: String,
    #[serde(deserialize_with = "nullable")]
    pub activate_hint: String,
    pub confirm: bool,
    #[serde(deserialize_with = "nullable")]
    pub confirm_runs: Vec<Run>,
    #[serde(deserialize_with = "nullable")]
    pub actions: Vec<Action>,
    pub line: Line,
    pub gone: bool,
    pub shown_dash: bool,
    pub shown_tray: bool,
    pub dash_enabled: bool,
    pub tray_acct_enabled: bool,
    #[serde(deserialize_with = "nullable")]
    pub dash_tip: String,
    #[serde(deserialize_with = "nullable")]
    pub tray_acct_tip: String,
}

/// The line under a row: a remove or restore confirmation, a refusal, or a request in flight (`lineView`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Line {
    pub shown: bool,
    /// confirm | refuse | busy.
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    #[serde(deserialize_with = "nullable")]
    pub icon: String,
    #[serde(deserialize_with = "nullable")]
    pub lead: String,
    #[serde(deserialize_with = "nullable")]
    pub text: String,
    #[serde(deserialize_with = "nullable")]
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct TrashRow {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub sub: String,
    #[serde(deserialize_with = "nullable")]
    pub actions: Vec<Action>,
    pub line: Line,
}

/// The flow panel under a provider (`flowView`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Flow {
    pub open: bool,
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    #[serde(deserialize_with = "nullable")]
    pub title: String,
    #[serde(deserialize_with = "nullable")]
    pub body: String,
    #[serde(deserialize_with = "nullable")]
    pub steps: Vec<String>,
    pub cur: u32,
    #[serde(deserialize_with = "nullable")]
    pub input_kind: String,
    #[serde(deserialize_with = "nullable")]
    pub input_label: String,
    #[serde(deserialize_with = "nullable")]
    pub input_placeholder: String,
    #[serde(deserialize_with = "nullable")]
    pub input_seed: String,
    pub input_password: bool,
    pub label_field: bool,
    pub code_shown: bool,
    #[serde(deserialize_with = "nullable")]
    pub code_url: String,
    #[serde(deserialize_with = "nullable")]
    pub code_text: String,
    #[serde(deserialize_with = "nullable")]
    pub code_expires: String,
    pub code_input: bool,
    #[serde(deserialize_with = "nullable")]
    pub waiting: String,
    #[serde(deserialize_with = "nullable")]
    pub done: String,
    #[serde(deserialize_with = "nullable")]
    pub error: String,
    #[serde(deserialize_with = "nullable")]
    pub error_body: String,
    #[serde(deserialize_with = "nullable")]
    pub note: String,
    #[serde(deserialize_with = "nullable")]
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Choice {
    #[serde(deserialize_with = "nullable")]
    pub value: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
}

/// The Antigravity policy box (`antigravityPolicy`). Numbers are -1 when the server did not report them.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct AgPolicy {
    pub shown: bool,
    pub live: bool,
    pub known: bool,
    #[serde(deserialize_with = "nullable")]
    pub note_strong: String,
    #[serde(deserialize_with = "nullable")]
    pub note: String,
    pub enabled: bool,
    pub toggle_enabled: bool,
    #[serde(deserialize_with = "nullable")]
    pub toggle_tip: String,
    pub threshold: i32,
    pub min: i32,
    pub max: i32,
    pub step_enabled: bool,
    #[serde(deserialize_with = "nullable")]
    pub pools: Vec<Choice>,
    #[serde(deserialize_with = "nullable")]
    pub pool: String,
    pub pool_enabled: bool,
    #[serde(deserialize_with = "nullable")]
    pub cooldowns: Vec<Choice>,
    #[serde(deserialize_with = "nullable")]
    pub cooldown: String,
    pub cooldown_enabled: bool,
}

/// One auto-switch policy row in Settings (`policies`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Policy {
    #[serde(deserialize_with = "nullable")]
    pub provider: String,
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    pub known: bool,
    pub enabled: bool,
    pub toggle_enabled: bool,
    pub threshold: i32,
    pub min: i32,
    pub max: i32,
    pub step_enabled: bool,
    pub wait: bool,
    #[serde(deserialize_with = "nullable")]
    pub sub: String,
    pub warn: bool,
    #[serde(deserialize_with = "nullable")]
    pub tip: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Refresh {
    pub seconds: u32,
    pub known: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct TimeZone {
    #[serde(deserialize_with = "nullable")]
    pub value: String,
    #[serde(deserialize_with = "nullable")]
    pub options: Vec<String>,
    pub enabled: bool,
    pub busy: bool,
    #[serde(deserialize_with = "nullable")]
    pub tip: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Cleanup {
    pub auto: bool,
    pub enabled: bool,
    pub busy: bool,
    #[serde(deserialize_with = "nullable")]
    pub note: String,
    #[serde(deserialize_with = "nullable")]
    pub tip: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct LogSource {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub tool: String,
    #[serde(deserialize_with = "nullable")]
    pub tool_label: String,
    #[serde(deserialize_with = "nullable")]
    pub host: String,
    #[serde(deserialize_with = "nullable")]
    pub host_label: String,
    #[serde(deserialize_with = "nullable")]
    pub path: String,
    #[serde(deserialize_with = "nullable")]
    pub mapping: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Tool {
    #[serde(deserialize_with = "nullable")]
    pub tool: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct LogSources {
    pub nonce: u64,
    #[serde(deserialize_with = "nullable")]
    pub sources: Vec<LogSource>,
    #[serde(deserialize_with = "nullable")]
    pub tools: Vec<Tool>,
    #[serde(deserialize_with = "nullable")]
    pub hosts: std::collections::BTreeMap<String, Vec<String>>,
    #[serde(deserialize_with = "nullable")]
    pub host_labels: std::collections::BTreeMap<String, Vec<String>>,
    #[serde(deserialize_with = "nullable")]
    pub hints: std::collections::BTreeMap<String, String>,
    pub enabled: bool,
    pub busy: bool,
    #[serde(deserialize_with = "nullable")]
    pub error: String,
    #[serde(deserialize_with = "nullable")]
    pub tip: String,
}

/// One Update apps line (`updateResultsView` host items).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct UpdItem {
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    #[serde(deserialize_with = "nullable")]
    pub app: String,
    #[serde(deserialize_with = "nullable")]
    pub result: String,
    /// good | crit | warn | run | "".
    #[serde(deserialize_with = "nullable")]
    pub tone: String,
    pub running: bool,
    #[serde(deserialize_with = "nullable")]
    pub tip: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct UpdHost {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub platform: String,
    #[serde(deserialize_with = "nullable")]
    pub items: Vec<UpdItem>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Update {
    pub shown: bool,
    pub running: bool,
    pub cancelling: bool,
    #[serde(deserialize_with = "nullable")]
    pub head_runs: Vec<Run>,
    #[serde(deserialize_with = "nullable")]
    pub hosts: Vec<UpdHost>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Lifetime {
    #[serde(deserialize_with = "nullable")]
    pub value: String,
    pub enabled: bool,
    pub busy: bool,
    #[serde(deserialize_with = "nullable")]
    pub tip: String,
}

/// The password strength (auth-view.mjs `strength`, plus `matches`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Strength {
    pub lv: i32,
    pub pct: f64,
    #[serde(deserialize_with = "nullable")]
    pub word: String,
    #[serde(deserialize_with = "nullable")]
    pub hint: String,
    pub matches: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Device {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    #[serde(deserialize_with = "nullable")]
    pub platform: String,
    #[serde(deserialize_with = "nullable")]
    pub sub: String,
    #[serde(deserialize_with = "nullable")]
    pub tip: String,
    #[serde(deserialize_with = "nullable")]
    pub probe: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Network {
    pub known: bool,
    pub on: bool,
    #[serde(deserialize_with = "nullable")]
    pub line: String,
    #[serde(deserialize_with = "nullable")]
    pub note: String,
    #[serde(deserialize_with = "nullable")]
    pub act: String,
    #[serde(deserialize_with = "nullable")]
    pub act_label: String,
    pub act_enabled: bool,
    #[serde(deserialize_with = "nullable")]
    pub tip: String,
    pub busy: bool,
}

/// Sign-in & connection (`signinFacts`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Signin {
    #[serde(deserialize_with = "nullable")]
    pub username: String,
    #[serde(deserialize_with = "nullable")]
    pub connection: String,
    #[serde(deserialize_with = "nullable")]
    pub session: String,
    #[serde(deserialize_with = "nullable")]
    pub session_sub: String,
    #[serde(deserialize_with = "nullable")]
    pub session_tip: String,
    pub lifetime: Lifetime,
    #[serde(deserialize_with = "nullable")]
    pub others_text: String,
    pub others_enabled: bool,
    pub others_busy: bool,
    #[serde(deserialize_with = "nullable")]
    pub others_tip: String,
    #[serde(deserialize_with = "nullable")]
    pub password_when: String,
    pub password_can: bool,
    #[serde(deserialize_with = "nullable")]
    pub password_note: String,
    pub password_open: bool,
    pub password_busy: bool,
    pub password_done: bool,
    #[serde(deserialize_with = "nullable")]
    pub password_field: String,
    #[serde(deserialize_with = "nullable")]
    pub password_error: String,
    pub password_nonce: u64,
    #[serde(deserialize_with = "nullable")]
    pub password_others: String,
    pub strength: Strength,
    #[serde(deserialize_with = "nullable")]
    pub devices: Vec<Device>,
    #[serde(deserialize_with = "nullable")]
    pub devices_note: String,
    pub devices_known: bool,
    #[serde(deserialize_with = "nullable")]
    pub revoke_busy: String,
    pub revoke_all_enabled: bool,
    pub revoke_all_busy: bool,
    pub sign_out_busy: bool,
    pub network: Network,
    #[serde(deserialize_with = "nullable")]
    pub pairing_note: Vec<Run>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Fact {
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub value: String,
    pub mono: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct About {
    #[serde(deserialize_with = "nullable")]
    pub version: String,
}

/// The view, or why it can't be drawn: empty (nothing pushed yet), unreadable, or another version.
pub fn parse(json: &str) -> Result<Option<AccountsView>, &'static str> {
    if json.is_empty() {
        return Ok(None);
    }
    let view: AccountsView = serde_json::from_str(json)
        .map_err(|_| "The Accounts & Settings view could not be read.")?;
    if view.version != ACCOUNTS_VIEW_VERSION {
        return Err("This dashboard sent an Accounts & Settings view this page does not know.");
    }
    Ok(Some(view))
}
