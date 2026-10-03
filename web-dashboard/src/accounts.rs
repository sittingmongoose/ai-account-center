//! Accounts & Settings view model (version 2, public/accounts-view.mjs `accountsViewModel`): the provider
//! sections with their account rows, fixed action slots, the line under a row (remove, restore, refusals), the
//! inline flows (add, sign in again, keys, guided sign-ins) and the Claude trash; the Antigravity policy box;
//! the settings column (Sign-in & connection with the trusted local network, password change and paired trays,
//! refresh interval, auto-switch policies, Update apps results, Connection, About).
//! Everything lands in the `AcData` global (ui/pages/accounts/ac-data.slint).
//!
//! Sections, rows and their actions are persistent models updated in place by id, so an account switch moves
//! the selected-row highlight and cross-fades the Activate slot instead of re-mounting the list.
use crate::sync::{Nested, sync_rows};
use crate::{
    AcAction, AcAgPolicy, AcData, AcDevice, AcFlow, AcLine, AcNetwork, AcPolicy, AcProvider, AcRow,
    AcSignin, AcTrashRow, AcUpdHost, AcUpdItem, Dashboard, FactView, RunView, SegItem,
    StrengthView,
};
use serde_json::Value;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::rc::Rc;
use wasm_bindgen::JsValue;

/// The view-model version this build understands (public/accounts-view.mjs ACCOUNTS_VIEW_VERSION).
pub const ACCOUNTS_VIEW_VERSION: u64 = 2;

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
        refused: b(v, "refused"),
        busy: b(v, "busy"),
        tip: s(v, "tip"),
        probe: s(v, "probe"),
    }
}
fn action_key(a: &AcAction) -> SharedString {
    SharedString::from(format!("{}|{}|{}", a.kind, a.act, a.value))
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
    line_actions: Nested<AcAction>,
    foot: Nested<AcAction>,
    flow_actions: Nested<AcAction>,
    steps: Nested<SharedString>,
    trash: Nested<AcTrashRow>,
    trash_actions: Nested<AcAction>,
    devices: Rc<VecModel<AcDevice>>,
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
    ac.set_devices(ModelRc::from(m.devices.clone()));
}

/// The line under a row or a trash entry; its actions live in `line_actions` under `owner`.
fn line(m: &mut AccountsModels, v: &Value, owner: &str, live: &mut Vec<String>) -> AcLine {
    live.push(owner.to_string());
    let actions = m.line_actions.sync(
        owner,
        arr(v, "actions").iter().map(action).collect(),
        action_key,
    );
    AcLine {
        shown: b(v, "shown"),
        kind: s(v, "kind"),
        icon: s(v, "icon"),
        lead: s(v, "lead"),
        text: s(v, "text"),
        actions,
    }
}

fn flow(m: &mut AccountsModels, v: &Value, owner: &str) -> AcFlow {
    let steps = m.steps.sync(
        owner,
        arr(v, "steps")
            .iter()
            .map(|x| SharedString::from(x.as_str().unwrap_or("")))
            .collect(),
        |x: &SharedString| x.clone(),
    );
    let actions = m.flow_actions.sync(
        owner,
        arr(v, "actions").iter().map(action).collect(),
        action_key,
    );
    AcFlow {
        open: b(v, "open"),
        key: s(v, "key"),
        title: s(v, "title"),
        body: s(v, "body"),
        steps,
        cur: i(v, "cur", 0),
        input_kind: s(v, "inputKind"),
        input_label: s(v, "inputLabel"),
        input_placeholder: s(v, "inputPlaceholder"),
        input_seed: s(v, "inputSeed"),
        input_password: b(v, "inputPassword"),
        label_field: b(v, "labelField"),
        code_shown: b(v, "codeShown"),
        code_url: s(v, "codeUrl"),
        code_text: s(v, "codeText"),
        code_expires: s(v, "codeExpires"),
        code_input: b(v, "codeInput"),
        waiting: s(v, "waiting"),
        done: s(v, "done"),
        error: s(v, "error"),
        error_body: s(v, "errorBody"),
        note: s(v, "note"),
        actions,
    }
}

/// The new password's strength while the change-password form is typed in (public/auth-view.mjs strength).
pub fn set_strength(ui: &Dashboard, json: &str) -> Result<(), JsValue> {
    let v: Value =
        serde_json::from_str(json).map_err(|_| JsValue::from_str("Invalid strength hint"))?;
    let pct = g(&v, "pct")
        .as_f64()
        .filter(|x| x.is_finite())
        .unwrap_or(0.0);
    ui.global::<AcData>().set_pw_strength(StrengthView {
        lv: i(&v, "lv", 0).clamp(0, 5),
        pct: pct.clamp(0.0, 100.0) as f32,
        word: s(&v, "word"),
        hint: s(&v, "hint"),
        matches: b(&v, "matches"),
    });
    Ok(())
}

/// Owners still on the page, per nested family, so models of vanished rows are dropped.
#[derive(Default)]
struct Live {
    rows: Vec<String>,
    runs: Vec<String>,
    lines: Vec<String>,
    trash: Vec<String>,
}

fn provider(m: &mut AccountsModels, v: &Value, live: &mut Live) -> AcProvider {
    let id = s(v, "id");
    let mut rows = Vec::new();
    for r in arr(v, "rows") {
        let owner = format!("{}|{}", id, s(r, "id"));
        live.rows.push(owner.clone());
        let actions = m.actions.sync(
            &owner,
            arr(r, "actions").iter().map(action).collect(),
            action_key,
        );
        let row_line = line(m, g(r, "line"), &format!("line|{}", owner), &mut live.lines);
        live.runs.push(owner.clone());
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
            line: row_line,
            gone: b(r, "gone"),
            shown_dash: b(r, "shownDash"),
            shown_tray: b(r, "shownTray"),
            dash_enabled: b(r, "dashEnabled"),
            tray_acct_enabled: b(r, "trayAcctEnabled"),
            dash_tip: s(r, "dashTip"),
            tray_acct_tip: s(r, "trayAcctTip"),
        });
    }
    let mut trash = Vec::new();
    for t in arr(v, "trash") {
        let tid = s(t, "id");
        live.trash.push(tid.to_string());
        let actions = m.trash_actions.sync(
            tid.as_str(),
            arr(t, "actions").iter().map(action).collect(),
            action_key,
        );
        let trash_line = line(m, g(t, "line"), &format!("trash|{}", tid), &mut live.lines);
        trash.push(AcTrashRow {
            id: tid,
            label: s(t, "label"),
            sub: s(t, "sub"),
            actions,
            line: trash_line,
        });
    }
    let trash = m
        .trash
        .sync(id.as_str(), trash, |t: &AcTrashRow| t.id.clone());
    let flow = flow(m, g(v, "flow"), id.as_str());
    let rows = m.rows.sync(id.as_str(), rows, |r: &AcRow| r.id.clone());
    let foot = m.foot.sync(
        id.as_str(),
        arr(v, "foot").iter().map(action).collect(),
        |a: &AcAction| a.act.clone(),
    );
    let foot_coming = arr(v, "foot").iter().any(|a| b(a, "coming"));
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
        tray_visible: b(v, "trayVisible"),
        tray_enabled: b(v, "trayEnabled"),
        tray_coming: b(v, "trayComing"),
        tray_tip: s(v, "trayTip"),
        switchable: b(v, "switchable"),
        can_switch: b(v, "canSwitch"),
        slots: i(v, "slots", 1),
        acts_min: i(v, "actsMin", 150) as f32,
        how: s(v, "how"),
        needs: b(v, "needs"),
        empty: s(v, "empty"),
        ag: b(v, "ag"),
        foot,
        foot_coming,
        rows,
        flow,
        trash,
    }
}

pub fn set_accounts(ui: &Dashboard, m: &mut AccountsModels, json: &str) -> Result<(), JsValue> {
    let v: Value =
        serde_json::from_str(json).map_err(|_| JsValue::from_str("Invalid accounts view data"))?;
    if g(&v, "version").as_u64() != Some(ACCOUNTS_VIEW_VERSION) {
        return Err(JsValue::from_str("Unsupported accounts view-model version"));
    }
    let ac = ui.global::<AcData>();
    let mut live = Live::default();
    let mut live_providers = Vec::new();
    for (key, model) in [("colA", m.col_a.clone()), ("colB", m.col_b.clone())] {
        let list: Vec<AcProvider> = arr(&v, key)
            .iter()
            .map(|p| {
                live_providers.push(s(p, "id").to_string());
                provider(m, p, &mut live)
            })
            .collect();
        sync_rows(&model, list, |p: &AcProvider| p.id.clone());
    }
    m.rows.retain(&live_providers);
    m.foot.retain(&live_providers);
    m.flow_actions.retain(&live_providers);
    m.steps.retain(&live_providers);
    m.trash.retain(&live_providers);
    m.actions.retain(&live.rows);
    m.runs.retain(&live.runs);
    m.line_actions.retain(&live.lines);
    m.trash_actions.retain(&live.trash);

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
    ac.set_update_cancelling(b(update, "cancelling"));
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
    let net = g(si, "network");
    ac.set_signin(AcSignin {
        username: s(si, "username"),
        connection: s(si, "connection"),
        session: s(si, "session"),
        session_sub: s(si, "sessionSub"),
        session_tip: s(si, "sessionTip"),
        others_text: s(si, "othersText"),
        others_enabled: b(si, "othersEnabled"),
        others_busy: b(si, "othersBusy"),
        others_tip: s(si, "othersTip"),
        lifetime_value: s(g(si, "lifetime"), "value"),
        lifetime_enabled: b(g(si, "lifetime"), "enabled"),
        lifetime_busy: b(g(si, "lifetime"), "busy"),
        lifetime_tip: s(g(si, "lifetime"), "tip"),
        password_when: s(si, "passwordWhen"),
        password_can: b(si, "passwordCan"),
        password_note: s(si, "passwordNote"),
        password_open: b(si, "passwordOpen"),
        password_busy: b(si, "passwordBusy"),
        password_done: b(si, "passwordDone"),
        password_field: s(si, "passwordField"),
        password_error: s(si, "passwordError"),
        password_nonce: i(si, "passwordNonce", 0),
        password_others: s(si, "passwordOthers"),
        devices_note: s(si, "devicesNote"),
        devices_known: b(si, "devicesKnown"),
        revoke_busy: s(si, "revokeBusy"),
        revoke_all_enabled: b(si, "revokeAllEnabled"),
        revoke_all_busy: b(si, "revokeAllBusy"),
        sign_out_busy: b(si, "signOutBusy"),
        network: AcNetwork {
            known: b(net, "known"),
            on: b(net, "on"),
            line: s(net, "line"),
            note: s(net, "note"),
            act: s(net, "act"),
            act_label: s(net, "actLabel"),
            act_enabled: b(net, "actEnabled"),
            tip: s(net, "tip"),
            busy: b(net, "busy"),
        },
    });
    sync_rows(
        &m.devices,
        arr(si, "devices")
            .iter()
            .map(|d| AcDevice {
                id: s(d, "id"),
                name: s(d, "name"),
                platform: s(d, "platform"),
                sub: s(d, "sub"),
                tip: s(d, "tip"),
            })
            .collect(),
        |d: &AcDevice| d.id.clone(),
    );
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
