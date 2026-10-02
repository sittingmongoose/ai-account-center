//! Accounts & Settings view model (version 1, public/accounts-view.mjs `accountsViewModel`): the provider
//! sections with their account rows and fixed action slots, the Antigravity policy box, the settings column
//! (Dashboard sign-in, refresh interval, auto-switch policies, Update apps results, Connection, About).
//! Everything lands in the `AcData` global (ui/pages/accounts/ac-data.slint).
//!
//! Sections, rows and their actions are persistent models updated in place by id, so an account switch moves
//! the selected-row highlight and cross-fades the Activate slot instead of re-mounting the list.
use crate::sync::{Nested, sync_rows};
use crate::{
    AcAction, AcAgPolicy, AcData, AcPolicy, AcProvider, AcRow, AcSignin, AcUpdHost, AcUpdItem,
    Dashboard, FactView, RunView, SegItem,
};
use serde_json::Value;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::rc::Rc;
use wasm_bindgen::JsValue;

/// The view-model version this build understands (public/accounts-view.mjs ACCOUNTS_VIEW_VERSION).
pub const ACCOUNTS_VIEW_VERSION: u64 = 1;

static NULL: Value = Value::Null;
fn g<'a>(v: &'a Value, k: &str) -> &'a Value {
    v.get(k).unwrap_or(&NULL)
}
fn s(v: &Value, k: &str) -> SharedString {
    g(v, k).as_str().unwrap_or("").into()
}
fn b(v: &Value, k: &str) -> bool {
    g(v, k).as_bool().unwrap_or(false)
}
fn i(v: &Value, k: &str, fallback: i32) -> i32 {
    g(v, k)
        .as_f64()
        .filter(|x| x.is_finite())
        .map(|x| x.round().clamp(i32::MIN as f64, i32::MAX as f64) as i32)
        .unwrap_or(fallback)
}
fn arr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    g(v, k).as_array().map(|a| a.as_slice()).unwrap_or(&[])
}
fn run(v: &Value) -> RunView {
    RunView {
        text: s(v, "text"),
        strong: b(v, "strong"),
        tone: s(v, "tone"),
    }
}
fn seg(v: &Value) -> SegItem {
    SegItem {
        value: s(v, "value"),
        label: s(v, "label"),
        icon: s(v, "icon"),
    }
}
fn action(v: &Value) -> AcAction {
    AcAction {
        kind: s(v, "kind"),
        act: s(v, "act"),
        value: s(v, "value"),
        label: s(v, "label"),
        icon: s(v, "icon"),
        platform: s(v, "platform"),
        style: s(v, "style"),
        enabled: b(v, "enabled"),
        coming: b(v, "coming"),
        tip: s(v, "tip"),
    }
}
fn fact(v: &Value) -> FactView {
    FactView {
        label: s(v, "label"),
        value: s(v, "value"),
        mono: b(v, "mono"),
    }
}

/// Every persistent model the page binds to.
#[derive(Default)]
pub struct AccountsModels {
    col_a: Rc<VecModel<AcProvider>>,
    col_b: Rc<VecModel<AcProvider>>,
    rows: Nested<AcRow>,
    actions: Nested<AcAction>,
    foot: Nested<AcAction>,
    runs: Nested<RunView>,
    segs: Nested<SegItem>,
    policies: Rc<VecModel<AcPolicy>>,
    hosts: Rc<VecModel<AcUpdHost>>,
    host_items: Nested<AcUpdItem>,
    update_head: Rc<VecModel<RunView>>,
    pairing_note: Rc<VecModel<RunView>>,
    connection: Rc<VecModel<FactView>>,
}

pub fn bind(ui: &Dashboard, m: &AccountsModels) {
    let ac = ui.global::<AcData>();
    ac.set_col_a(ModelRc::from(m.col_a.clone()));
    ac.set_col_b(ModelRc::from(m.col_b.clone()));
    ac.set_policies(ModelRc::from(m.policies.clone()));
    ac.set_update_hosts(ModelRc::from(m.hosts.clone()));
    ac.set_update_head(ModelRc::from(m.update_head.clone()));
    ac.set_pairing_note(ModelRc::from(m.pairing_note.clone()));
    ac.set_connection(ModelRc::from(m.connection.clone()));
}

fn provider(
    m: &mut AccountsModels,
    v: &Value,
    live_rows: &mut Vec<String>,
    live_runs: &mut Vec<String>,
) -> AcProvider {
    let id = s(v, "id");
    let mut rows = Vec::new();
    for r in arr(v, "rows") {
        let owner = format!("{}|{}", id, s(r, "id"));
        live_rows.push(owner.clone());
        let actions = m.actions.sync(
            &owner,
            arr(r, "actions").iter().map(action).collect(),
            |a: &AcAction| SharedString::from(format!("{}|{}|{}", a.kind, a.act, a.value)),
        );
        live_runs.push(owner.clone());
        let confirm_runs = m.runs.sync(
            &owner,
            arr(r, "confirmRuns").iter().map(run).collect(),
            |x: &RunView| x.text.clone(),
        );
        rows.push(AcRow {
            id: s(r, "id"),
            provider: s(r, "provider"),
            email: s(r, "email"),
            meta: s(r, "meta"),
            src_inline: s(r, "srcInline"),
            status: s(r, "status"),
            sampled: s(r, "sampled"),
            sampled_tip: s(r, "sampledTip"),
            src: s(r, "src"),
            src_sub: s(r, "srcSub"),
            active: b(r, "active"),
            active_label: s(r, "activeLabel"),
            can_activate: b(r, "canActivate"),
            activate_kind: s(r, "activateKind"),
            profile: s(r, "profile"),
            activate_hint: s(r, "activateHint"),
            confirm: b(r, "confirm"),
            confirm_runs,
            actions,
        });
    }
    let rows = m.rows.sync(id.as_str(), rows, |r: &AcRow| r.id.clone());
    let foot = m.foot.sync(
        id.as_str(),
        arr(v, "foot").iter().map(action).collect(),
        |a: &AcAction| a.act.clone(),
    );
    AcProvider {
        id: id.clone(),
        label: s(v, "label"),
        kind_label: s(v, "kindLabel"),
        kind_icon: s(v, "kindIcon"),
        count: i(v, "count", 0),
        count_text: s(v, "countText"),
        visible: b(v, "visible"),
        hidden_note: s(v, "hiddenNote"),
        toggle_enabled: b(v, "toggleEnabled"),
        toggle_tip: s(v, "toggleTip"),
        switchable: b(v, "switchable"),
        can_switch: b(v, "canSwitch"),
        slots: i(v, "slots", 1),
        acts_min: i(v, "actsMin", 150) as f32,
        how: s(v, "how"),
        empty: s(v, "empty"),
        ag: b(v, "ag"),
        foot,
        rows,
    }
}

pub fn set_accounts(ui: &Dashboard, m: &mut AccountsModels, json: &str) -> Result<(), JsValue> {
    let v: Value =
        serde_json::from_str(json).map_err(|_| JsValue::from_str("Invalid accounts view data"))?;
    if g(&v, "version").as_u64() != Some(ACCOUNTS_VIEW_VERSION) {
        return Err(JsValue::from_str("Unsupported accounts view-model version"));
    }
    let ac = ui.global::<AcData>();
    let mut live_rows = Vec::new();
    let mut live_runs = Vec::new();
    let mut live_providers = Vec::new();
    for (key, model) in [("colA", m.col_a.clone()), ("colB", m.col_b.clone())] {
        let list: Vec<AcProvider> = arr(&v, key)
            .iter()
            .map(|p| {
                live_providers.push(s(p, "id").to_string());
                provider(m, p, &mut live_rows, &mut live_runs)
            })
            .collect();
        sync_rows(&model, list, |p: &AcProvider| p.id.clone());
    }
    m.rows.retain(&live_providers);
    m.foot.retain(&live_providers);
    m.actions.retain(&live_rows);
    m.runs.retain(&live_runs);

    let ag = g(&v, "ag");
    let pools = m.segs.sync(
        "pools",
        arr(ag, "pools").iter().map(seg).collect(),
        |x: &SegItem| x.value.clone(),
    );
    let cooldowns = m.segs.sync(
        "cooldowns",
        arr(ag, "cooldowns").iter().map(seg).collect(),
        |x: &SegItem| x.value.clone(),
    );
    ac.set_ag(AcAgPolicy {
        shown: b(ag, "shown"),
        live: b(ag, "live"),
        known: b(ag, "known"),
        note_strong: s(ag, "noteStrong"),
        note: s(ag, "note"),
        enabled: b(ag, "enabled"),
        toggle_enabled: b(ag, "toggleEnabled"),
        toggle_tip: s(ag, "toggleTip"),
        threshold: i(ag, "threshold", -1),
        min: i(ag, "min", 50),
        max: i(ag, "max", 99),
        step_enabled: b(ag, "stepEnabled"),
        pools,
        pool: s(ag, "pool"),
        pool_enabled: b(ag, "poolEnabled"),
        cooldowns,
        cooldown: s(ag, "cooldown"),
        cooldown_enabled: b(ag, "cooldownEnabled"),
    });

    sync_rows(
        &m.policies,
        arr(&v, "policies")
            .iter()
            .map(|p| AcPolicy {
                provider: s(p, "provider"),
                name: s(p, "name"),
                known: b(p, "known"),
                enabled: b(p, "enabled"),
                toggle_enabled: b(p, "toggleEnabled"),
                threshold: i(p, "threshold", -1),
                min: i(p, "min", 50),
                max: i(p, "max", 99),
                step_enabled: b(p, "stepEnabled"),
                wait: b(p, "wait"),
                sub: s(p, "sub"),
                tip: s(p, "tip"),
            })
            .collect(),
        |p: &AcPolicy| p.provider.clone(),
    );

    let refresh = g(&v, "refresh");
    ac.set_refresh_seconds(i(refresh, "seconds", 60).clamp(30, 3600));
    ac.set_refresh_known(b(refresh, "known"));

    let update = g(&v, "update");
    ac.set_update_shown(b(update, "shown"));
    ac.set_update_running(b(update, "running"));
    sync_rows(
        &m.update_head,
        arr(update, "headRuns").iter().map(run).collect(),
        |x: &RunView| x.text.clone(),
    );
    let mut live_hosts = Vec::new();
    let mut hosts = Vec::new();
    for h in arr(update, "hosts") {
        let id = s(h, "id");
        live_hosts.push(id.to_string());
        let items = m.host_items.sync(
            id.as_str(),
            arr(h, "items")
                .iter()
                .map(|x| AcUpdItem {
                    key: s(x, "key"),
                    app: s(x, "app"),
                    result: s(x, "result"),
                    tone: s(x, "tone"),
                    running: b(x, "running"),
                    tip: s(x, "tip"),
                })
                .collect(),
            |x: &AcUpdItem| x.key.clone(),
        );
        hosts.push(AcUpdHost {
            id,
            label: s(h, "label"),
            platform: s(h, "platform"),
            items,
        });
    }
    sync_rows(&m.hosts, hosts, |h: &AcUpdHost| h.id.clone());
    m.host_items.retain(&live_hosts);

    let si = g(&v, "signin");
    ac.set_signin(AcSignin {
        username: s(si, "username"),
        connection: s(si, "connection"),
        session: s(si, "session"),
        session_sub: s(si, "sessionSub"),
        session_tip: s(si, "sessionTip"),
        other_tip: s(si, "otherTip"),
        password_tip: s(si, "passwordTip"),
        devices_note: s(si, "devicesNote"),
        devices_tip: s(si, "devicesTip"),
        revoke_all_tip: s(si, "revokeAllTip"),
    });
    sync_rows(
        &m.pairing_note,
        arr(si, "pairingNote").iter().map(run).collect(),
        |x: &RunView| x.text.clone(),
    );
    sync_rows(
        &m.connection,
        arr(&v, "connection").iter().map(fact).collect(),
        |x: &FactView| x.label.clone(),
    );
    ac.set_about_version(s(g(&v, "about"), "version"));
    ac.set_ready(true);
    Ok(())
}
