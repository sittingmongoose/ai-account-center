//! Browser entry points for the Slint dashboard. bridge.js owns the network, sessions and every
//! truthfulness rule (public/*.mjs); it hands this module version 2 view-model JSON, which is
//! deserialized here and written into persistent models in place (see `sync`). The UI reports user
//! intent through `action(kind, value)`, forwarded unchanged to `window.ccsDashboardAction`.
use serde::Deserialize;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::prelude::*;
slint::include_modules!();
mod analytics;
mod sync;
use sync::{Nested, sync_rows};

/// The view-model version this build understands (public/view-model.mjs VIEW_MODEL_VERSION).
pub const VIEW_MODEL_VERSION: u32 = 2;

thread_local! {
    static UI: RefCell<Option<Dashboard>> = const { RefCell::new(None) };
    static MODELS: RefCell<Option<Models>> = const { RefCell::new(None) };
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = window, js_name = ccsDashboardAction)]
    fn dispatch_action(action: &str, value: &str);
}

/// Every persistent model the window binds to.
struct Models {
    sections: Rc<VecModel<SectionView>>,
    section_rows: Nested<AccountRowView>,
    section_columns: Nested<ColumnView>,
    row_cells: Nested<MeterView>,
    runs: Nested<RunView>,
    cards: Rc<VecModel<ProviderCardView>>,
    card_meters: Nested<MeterView>,
    card_amounts: Nested<AmountView>,
    registry: Rc<VecModel<RegistryView>>,
    toasts: Rc<VecModel<ToastView>>,
    next_toast: i32,
    details_meters: Rc<VecModel<MeterView>>,
    details_amounts: Rc<VecModel<AmountView>>,
    details_facts: Rc<VecModel<FactView>>,
    processes: Rc<VecModel<ProcessView>>,
    analytics: analytics::AnalyticsModels,
}

impl Models {
    fn new() -> Self {
        Self {
            sections: Rc::new(VecModel::default()),
            section_rows: Nested::default(),
            section_columns: Nested::default(),
            row_cells: Nested::default(),
            runs: Nested::default(),
            cards: Rc::new(VecModel::default()),
            card_meters: Nested::default(),
            card_amounts: Nested::default(),
            registry: Rc::new(VecModel::default()),
            toasts: Rc::new(VecModel::default()),
            next_toast: 1,
            details_meters: Rc::new(VecModel::default()),
            details_amounts: Rc::new(VecModel::default()),
            details_facts: Rc::new(VecModel::default()),
            processes: Rc::new(VecModel::default()),
            analytics: analytics::AnalyticsModels::default(),
        }
    }
}

fn with_ui(f: impl FnOnce(&Dashboard)) {
    UI.with(|slot| {
        if let Some(ui) = slot.borrow().as_ref() {
            f(ui);
        }
    });
}

fn with_models<R>(f: impl FnOnce(&mut Models) -> R) -> Option<R> {
    MODELS.with(|slot| slot.borrow_mut().as_mut().map(f))
}

// ---------------------------------------------------------------- DTOs (camelCase JSON)

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct MeterDto {
    key: String,
    label: String,
    full_label: String,
    has_value: bool,
    value: f32,
    value_text: String,
    over_text: String,
    reset: String,
    reset_exact: String,
    reset_soon: bool,
    na_text: String,
    na_sub: String,
    amount: String,
    left: String,
    sampled: String,
    source: String,
    caption: String,
    caption_tip: String,
    notch: Option<f32>,
    notch_faint: bool,
    notch_off: bool,
}
impl From<MeterDto> for MeterView {
    fn from(v: MeterDto) -> Self {
        Self {
            key: v.key.into(),
            label: v.label.into(),
            full_label: v.full_label.into(),
            has_value: v.has_value && v.value.is_finite(),
            value: if v.value.is_finite() { v.value } else { 0. },
            value_text: v.value_text.into(),
            over_text: v.over_text.into(),
            reset: v.reset.into(),
            reset_exact: v.reset_exact.into(),
            reset_soon: v.reset_soon,
            na_text: v.na_text.into(),
            na_sub: v.na_sub.into(),
            amount: v.amount.into(),
            left: v.left.into(),
            sampled: v.sampled.into(),
            source: v.source.into(),
            caption: v.caption.into(),
            caption_tip: v.caption_tip.into(),
            notch: v.notch.filter(|n| n.is_finite()).unwrap_or(-1.),
            notch_faint: v.notch_faint,
            notch_off: v.notch_off,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct AmountDto {
    key: String,
    label: String,
    value: String,
    unit: String,
    sub: String,
    icon: String,
    spent: bool,
}
impl From<AmountDto> for AmountView {
    fn from(v: AmountDto) -> Self {
        Self {
            key: v.key.into(),
            label: v.label.into(),
            value: v.value.into(),
            unit: v.unit.into(),
            sub: v.sub.into(),
            icon: v.icon.into(),
            spent: v.spent,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RunDto {
    text: String,
    strong: bool,
    tone: String,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct FootDto {
    shown: bool,
    warn: bool,
    runs: Vec<RunDto>,
    when: String,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ColumnDto {
    key: String,
    label: String,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RowDto {
    id: String,
    provider: String,
    profile: String,
    email: String,
    plan: String,
    meta: String,
    status: String,
    note: String,
    platform: String,
    active: bool,
    active_label: String,
    setup: bool,
    can_activate: bool,
    activate_kind: String,
    activate_hint: String,
    can_mac: bool,
    can_windows: bool,
    amounts_line: String,
    amounts_runs: Vec<RunDto>,
    confirm: bool,
    confirm_runs: Vec<RunDto>,
    cells: Vec<MeterDto>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct AutoDto {
    known: bool,
    shown: bool,
    enabled: bool,
    available: bool,
    can_enable: bool,
    threshold_used: Option<i32>,
    threshold_label: String,
    min: Option<i32>,
    max: Option<i32>,
    pool: String,
    off_runs: Vec<RunDto>,
    setting: String,
    message: String,
    example: bool,
}
impl AutoDto {
    fn into_view(self, off_runs: ModelRc<RunView>) -> AutoSwitchView {
        AutoSwitchView {
            known: self.known,
            shown: self.shown,
            enabled: self.enabled,
            available: self.available,
            can_enable: self.can_enable,
            threshold_used: self.threshold_used.unwrap_or(-1),
            threshold_label: self.threshold_label.into(),
            min: self.min.unwrap_or(50),
            max: self.max.unwrap_or(99),
            pool: self.pool.into(),
            off_runs,
            setting: self.setting.into(),
            message: self.message.into(),
            example: self.example,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct SectionDto {
    id: String,
    kind: String,
    label: String,
    long_label: String,
    meta: String,
    meta_runs: Vec<RunDto>,
    switchable: bool,
    can_switch: bool,
    active_id: String,
    active_label: String,
    empty: String,
    auto: AutoDto,
    foot: FootDto,
    columns: Vec<ColumnDto>,
    rows: Vec<RowDto>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct CardDto {
    id: String,
    provider: String,
    account_id: String,
    label: String,
    identity: String,
    plan: String,
    status: String,
    source: String,
    flag: String,
    sampled: String,
    platform: String,
    plan_note: String,
    packs_note: String,
    note: String,
    meters: Vec<MeterDto>,
    amounts: Vec<AmountDto>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RegistryDto {
    id: String,
    label: String,
    long_label: String,
    sign_in: String,
    multi: bool,
    switchable: bool,
    visible: bool,
    count: i32,
}
impl From<RegistryDto> for RegistryView {
    fn from(v: RegistryDto) -> Self {
        Self {
            id: v.id.into(),
            label: v.label.into(),
            long_label: v.long_label.into(),
            sign_in: v.sign_in.into(),
            multi: v.multi,
            switchable: v.switchable,
            visible: v.visible,
            count: v.count,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ChromeDto {
    status_lead: String,
    status_strong: String,
    status_more: String,
    status_tip: String,
    refreshing: bool,
    username: String,
    host: String,
}
impl From<ChromeDto> for ChromeView {
    fn from(v: ChromeDto) -> Self {
        Self {
            status_lead: v.status_lead.into(),
            status_strong: v.status_strong.into(),
            status_more: v.status_more.into(),
            status_tip: v.status_tip.into(),
            refreshing: v.refreshing,
            username: v.username.into(),
            host: v.host.into(),
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct DashboardDto {
    version: u32,
    sections: Vec<SectionDto>,
    cards: Vec<CardDto>,
    registry: Vec<RegistryDto>,
    chrome: Option<ChromeDto>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct FactDto {
    label: String,
    value: String,
    mono: bool,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct DetailsDto {
    id: String,
    provider: String,
    title: String,
    sub: String,
    sub_lead: String,
    state: String,
    active: bool,
    switchable: bool,
    can_activate: bool,
    activate_kind: String,
    can_switch: bool,
    active_label: String,
    activate_hint: String,
    platform: String,
    confirm: bool,
    confirm_runs: Vec<RunDto>,
    profile: String,
    can_mac: bool,
    can_windows: bool,
    note: String,
    meters: Vec<MeterDto>,
    amounts: Vec<AmountDto>,
    facts: Vec<FactDto>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct UpdateDto {
    running: bool,
    done: bool,
    count: i32,
    total: i32,
    tip: String,
    summary: String,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct AuthDto {
    state: String,
    message: String,
    username: String,
    host: String,
    retry: String,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ProcessDto {
    label: String,
    pid: i32,
    role: String,
}
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ConfirmationDto {
    product: String,
    target_profile: String,
    warning: String,
    expires_at: String,
    error: String,
    can_confirm: bool,
    in_progress: bool,
    processes: Vec<ProcessDto>,
}

// ---------------------------------------------------------------- conversions with nested models

fn meter_key(m: &MeterView) -> SharedString {
    m.key.clone()
}

/// A persistent nested model of text runs, so an unchanged line keeps its elements.
fn sync_runs(
    m: &mut Models,
    owner: &str,
    runs: Vec<RunDto>,
    live: &mut Vec<String>,
) -> ModelRc<RunView> {
    live.push(owner.to_string());
    m.runs.sync(
        owner,
        runs.into_iter()
            .map(|r| RunView {
                text: r.text.into(),
                strong: r.strong,
                tone: r.tone.into(),
            })
            .collect(),
        |r: &RunView| r.text.clone(),
    )
}

fn apply_dashboard(ui: &Dashboard, m: &mut Models, v: DashboardDto) {
    let mut live_sections = Vec::new();
    let mut live_rows = Vec::new();
    let mut live_runs = Vec::new();
    let mut sections = Vec::with_capacity(v.sections.len());
    for section in v.sections {
        live_sections.push(section.id.clone());
        let mut rows = Vec::with_capacity(section.rows.len());
        for row in section.rows {
            let owner = format!("{}|{}", section.id, row.id);
            live_rows.push(owner.clone());
            let cells = m.row_cells.sync(
                &owner,
                row.cells.into_iter().map(MeterView::from).collect(),
                meter_key,
            );
            let amounts_runs = sync_runs(
                m,
                &format!("amounts|{owner}"),
                row.amounts_runs,
                &mut live_runs,
            );
            let confirm_runs = sync_runs(
                m,
                &format!("confirm|{owner}"),
                row.confirm_runs,
                &mut live_runs,
            );
            rows.push(AccountRowView {
                id: row.id.into(),
                provider: row.provider.into(),
                profile: row.profile.into(),
                email: row.email.into(),
                plan: row.plan.into(),
                meta: row.meta.into(),
                status: row.status.into(),
                note: row.note.into(),
                platform: row.platform.into(),
                active: row.active,
                active_label: row.active_label.into(),
                setup: row.setup,
                can_activate: row.can_activate,
                activate_kind: row.activate_kind.into(),
                activate_hint: row.activate_hint.into(),
                can_mac: row.can_mac,
                can_windows: row.can_windows,
                amounts_line: row.amounts_line.into(),
                amounts_runs,
                confirm: row.confirm,
                confirm_runs,
                cells,
            });
        }
        let count = rows.len() as i32;
        let rows = m
            .section_rows
            .sync(&section.id, rows, |r: &AccountRowView| r.id.clone());
        let columns = m.section_columns.sync(
            &section.id,
            section
                .columns
                .into_iter()
                .map(|c| ColumnView {
                    key: c.key.into(),
                    label: c.label.into(),
                })
                .collect(),
            |c: &ColumnView| c.key.clone(),
        );
        let meta_runs = sync_runs(
            m,
            &format!("meta|{}", section.id),
            section.meta_runs,
            &mut live_runs,
        );
        let foot_runs = sync_runs(
            m,
            &format!("foot|{}", section.id),
            section.foot.runs,
            &mut live_runs,
        );
        let mut auto = section.auto;
        let off_runs = sync_runs(
            m,
            &format!("off|{}", section.id),
            std::mem::take(&mut auto.off_runs),
            &mut live_runs,
        );
        sections.push(SectionView {
            id: section.id.into(),
            kind: section.kind.into(),
            label: section.label.into(),
            long_label: section.long_label.into(),
            meta: section.meta.into(),
            meta_runs,
            count,
            switchable: section.switchable,
            can_switch: section.can_switch,
            active_id: section.active_id.into(),
            active_label: section.active_label.into(),
            empty: section.empty.into(),
            auto: auto.into_view(off_runs),
            foot: FootView {
                shown: section.foot.shown,
                warn: section.foot.warn,
                runs: foot_runs,
                when: section.foot.when.into(),
            },
            columns,
            rows,
        });
    }
    sync_rows(&m.sections, sections, |sv: &SectionView| sv.id.clone());
    m.section_rows.retain(&live_sections);
    m.section_columns.retain(&live_sections);
    m.row_cells.retain(&live_rows);
    m.runs.retain(&live_runs);

    let mut live_cards = Vec::new();
    let mut cards = Vec::with_capacity(v.cards.len());
    for card in v.cards {
        live_cards.push(card.id.clone());
        let meters = m.card_meters.sync(
            &card.id,
            card.meters.into_iter().map(MeterView::from).collect(),
            meter_key,
        );
        let amounts = m.card_amounts.sync(
            &card.id,
            card.amounts.into_iter().map(AmountView::from).collect(),
            |a: &AmountView| a.key.clone(),
        );
        cards.push(ProviderCardView {
            id: card.id.into(),
            provider: card.provider.into(),
            account_id: card.account_id.into(),
            label: card.label.into(),
            identity: card.identity.into(),
            plan: card.plan.into(),
            status: card.status.into(),
            source: card.source.into(),
            flag: card.flag.into(),
            sampled: card.sampled.into(),
            platform: card.platform.into(),
            plan_note: card.plan_note.into(),
            packs_note: card.packs_note.into(),
            note: card.note.into(),
            meters,
            amounts,
        });
    }
    sync_rows(&m.cards, cards, |c: &ProviderCardView| c.id.clone());
    m.card_meters.retain(&live_cards);
    m.card_amounts.retain(&live_cards);

    sync_rows(
        &m.registry,
        v.registry.into_iter().map(RegistryView::from).collect(),
        |r: &RegistryView| r.id.clone(),
    );
    if let Some(chrome) = v.chrome {
        ui.set_chrome(chrome.into());
    }
    // The first data starts the load-in (sections rise, meters sweep, numbers roll).
    ui.set_data_ready(true);
}

// ---------------------------------------------------------------- entry points

#[wasm_bindgen]
pub fn start_dashboard(width: f32, height: f32, scale_factor: f32) -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    let ui = Dashboard::new().map_err(|e| JsValue::from_str(&e.to_string()))?;
    // Seed the browser's logical size and scale before the first show/render.
    // winit reports the initial browser size asynchronously; the scene and
    // renderer must already agree on physical dimensions on Retina displays.
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0. {
        scale_factor
    } else {
        1.
    };
    ui.window()
        .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor });
    ui.window()
        .set_size(slint::LogicalSize::new(width.max(320.), height.max(320.)));
    let models = Models::new();
    ui.set_sections(ModelRc::from(models.sections.clone()));
    ui.set_cards(ModelRc::from(models.cards.clone()));
    ui.set_registry(ModelRc::from(models.registry.clone()));
    ui.set_toasts(ModelRc::from(models.toasts.clone()));
    ui.set_confirmation_processes(ModelRc::from(models.processes.clone()));
    analytics::bind(&ui, &models.analytics);
    ui.on_action(|action, value| dispatch_action(action.as_str(), value.as_str()));
    let ax = ui.global::<AxData>();
    ax.on_action(|action, value| dispatch_action(action.as_str(), value.as_str()));
    // the chart boxes report their size; bridge.js lays the charts out in those pixels
    ax.on_layout(|kind, width, height| {
        dispatch_action(
            "analytics-layout",
            &format!("{},{},{}", kind, width.round(), height.round()),
        )
    });
    ui.on_toast_dismissed(|id| {
        with_models(|m| {
            if let Some(index) = (0..m.toasts.row_count())
                .find(|&i| m.toasts.row_data(i).is_some_and(|t| t.id == id))
            {
                m.toasts.remove(index);
            }
        });
    });
    MODELS.with(|slot| *slot.borrow_mut() = Some(models));
    UI.with(|slot| *slot.borrow_mut() = Some(ui.clone_strong()));
    ui.run().map_err(|e| JsValue::from_str(&e.to_string()))
}

#[wasm_bindgen]
pub fn resize_dashboard(width: f32, height: f32) {
    with_ui(|ui| {
        ui.window()
            .set_size(slint::LogicalSize::new(width.max(320.), height.max(320.)));
    });
}

/// The dashboard view model (version 2, public/view-model.mjs).
#[wasm_bindgen]
pub fn set_dashboard(json: &str) -> Result<(), JsValue> {
    let v: DashboardDto =
        serde_json::from_str(json).map_err(|_| JsValue::from_str("Invalid dashboard view data"))?;
    if v.version != VIEW_MODEL_VERSION {
        return Err(JsValue::from_str(
            "Unsupported dashboard view-model version",
        ));
    }
    UI.with(|slot| {
        if let Some(ui) = slot.borrow().as_ref() {
            with_models(|m| apply_dashboard(ui, m, v));
        }
    });
    Ok(())
}

/// The header status line alone (it ticks between refreshes: "Updated 2m ago").
#[wasm_bindgen]
pub fn set_chrome(json: &str) -> Result<(), JsValue> {
    let v: ChromeDto =
        serde_json::from_str(json).map_err(|_| JsValue::from_str("Invalid header status"))?;
    with_ui(|ui| ui.set_chrome(v.into()));
    Ok(())
}

/// Sign-in state: `authenticated` hides the sign-in layer; `json` is the AuthView
/// ({ state, message, username, host, retry }).
#[wasm_bindgen]
pub fn set_auth(authenticated: bool, json: &str) -> Result<(), JsValue> {
    let v: AuthDto =
        serde_json::from_str(json).map_err(|_| JsValue::from_str("Invalid sign-in state"))?;
    with_ui(|ui| {
        ui.set_authenticated(authenticated);
        ui.set_auth(AuthView {
            state: v.state.into(),
            message: v.message.into(),
            username: v.username.into(),
            host: v.host.into(),
            retry: v.retry.into(),
        });
        if authenticated {
            ui.set_password(SharedString::default());
        } else {
            ui.set_details_open(false);
            ui.set_data_ready(false);
        }
    });
    Ok(())
}

#[wasm_bindgen]
pub fn set_busy(busy: bool) {
    with_ui(|ui| ui.set_busy(busy));
}

/// 0 = Auto (follows the browser), 1 = Light, 2 = Dark.
#[wasm_bindgen]
pub fn set_theme_mode(mode: i32) {
    with_ui(|ui| ui.set_theme_mode(mode.clamp(0, 2)));
}

/// The browser's prefers-color-scheme, kept live by bridge.js.
#[wasm_bindgen]
pub fn set_system_dark(dark: bool) {
    with_ui(|ui| ui.set_system_dark(dark));
}

/// prefers-reduced-motion, and the headless screenshot guard: every duration collapses.
#[wasm_bindgen]
pub fn set_reduced_motion(reduced: bool) {
    with_ui(|ui| ui.set_reduced_motion(reduced));
}

/// kind: "ok" | "err" | "info". At most three toasts stay live.
#[wasm_bindgen]
pub fn push_toast(kind: &str, title: &str, body: &str, ms: i32) {
    with_models(|m| {
        while m.toasts.row_count() >= 3 {
            m.toasts.remove(0);
        }
        let id = m.next_toast;
        m.next_toast += 1;
        let kind = if ["ok", "err", "info"].contains(&kind) {
            kind
        } else {
            "info"
        };
        m.toasts.push(ToastView {
            id,
            kind: kind.into(),
            title: title.into(),
            body: body.into(),
            ms: if ms > 0 { ms } else { 4800 },
        });
    });
}

#[wasm_bindgen]
pub fn show_details(json: &str) -> Result<(), JsValue> {
    let v: DetailsDto =
        serde_json::from_str(json).map_err(|_| JsValue::from_str("Invalid usage details"))?;
    UI.with(|slot| {
        if let Some(ui) = slot.borrow().as_ref() {
            with_models(|m| {
                let meter_count = v.meters.len() as i32;
                let amount_count = v.amounts.len() as i32;
                sync_rows(
                    &m.details_meters,
                    v.meters.into_iter().map(MeterView::from).collect(),
                    meter_key,
                );
                sync_rows(
                    &m.details_amounts,
                    v.amounts.into_iter().map(AmountView::from).collect(),
                    |a: &AmountView| a.key.clone(),
                );
                sync_rows(
                    &m.details_facts,
                    v.facts
                        .into_iter()
                        .map(|f| FactView {
                            label: f.label.into(),
                            value: f.value.into(),
                            mono: f.mono,
                        })
                        .collect(),
                    |f: &FactView| f.label.clone(),
                );
                ui.set_details(DetailsView {
                    id: v.id.clone().into(),
                    provider: v.provider.into(),
                    title: v.title.into(),
                    sub: v.sub.into(),
                    sub_lead: v.sub_lead.into(),
                    state: v.state.into(),
                    active: v.active,
                    switchable: v.switchable,
                    can_activate: v.can_activate,
                    activate_kind: v.activate_kind.into(),
                    can_switch: v.can_switch,
                    active_label: v.active_label.into(),
                    activate_hint: v.activate_hint.into(),
                    platform: v.platform.into(),
                    confirm: v.confirm,
                    confirm_runs: ModelRc::new(VecModel::from(
                        v.confirm_runs
                            .into_iter()
                            .map(|r| RunView {
                                text: r.text.into(),
                                strong: r.strong,
                                tone: r.tone.into(),
                            })
                            .collect::<Vec<_>>(),
                    )),
                    profile: v.profile.into(),
                    can_mac: v.can_mac,
                    can_windows: v.can_windows,
                    note: v.note.into(),
                    meter_count,
                    amount_count,
                    meters: m.details_meters.clone().into(),
                    amounts: m.details_amounts.clone().into(),
                    facts: m.details_facts.clone().into(),
                });
                ui.set_selected_id(v.id.into());
                ui.set_details_open(true);
            });
        }
    });
    Ok(())
}

#[wasm_bindgen]
pub fn close_details() {
    with_ui(|ui| {
        ui.set_details_open(false);
        ui.set_selected_id(SharedString::default());
    });
}

#[wasm_bindgen]
pub fn set_update_status(json: &str) -> Result<(), JsValue> {
    let v: UpdateDto =
        serde_json::from_str(json).map_err(|_| JsValue::from_str("Invalid update status"))?;
    with_ui(|ui| {
        ui.set_update(UpdateView {
            running: v.running,
            done: v.done,
            count: v.count.max(0),
            total: if v.total > 0 { v.total } else { 21 },
            tip: v.tip.into(),
            summary: v.summary.into(),
        })
    });
    Ok(())
}

#[wasm_bindgen]
pub fn show_activation_confirmation(json: &str) -> Result<(), JsValue> {
    let v: ConfirmationDto = serde_json::from_str(json)
        .map_err(|_| JsValue::from_str("Invalid activation confirmation"))?;
    UI.with(|slot| {
        if let Some(ui) = slot.borrow().as_ref() {
            with_models(|m| {
                sync_rows(
                    &m.processes,
                    v.processes
                        .into_iter()
                        .map(|p| ProcessView {
                            label: p.label.into(),
                            pid: p.pid.to_string().into(),
                            role: p.role.into(),
                        })
                        .collect(),
                    |p: &ProcessView| p.pid.clone(),
                );
            });
            ui.set_details_open(false);
            ui.set_confirmation(ConfirmationView {
                open: true,
                product: if v.product == "Antigravity" {
                    "Antigravity"
                } else {
                    "Codex"
                }
                .into(),
                target: v.target_profile.into(),
                warning: v.warning.into(),
                expires: v.expires_at.into(),
                error: v.error.into(),
                valid: v.can_confirm,
                busy: v.in_progress,
            });
        }
    });
    Ok(())
}

#[wasm_bindgen]
pub fn close_activation_confirmation() {
    with_ui(|ui| {
        let mut confirmation = ui.get_confirmation();
        confirmation.open = false;
        confirmation.busy = false;
        ui.set_confirmation(confirmation);
    });
}

/// The analytics view model (version 3, public/analytics-data.mjs).
#[wasm_bindgen]
pub fn set_analytics(json: &str) -> Result<(), JsValue> {
    UI.with(|slot| {
        if let Some(ui) = slot.borrow().as_ref() {
            with_models(|m| analytics::set_analytics(ui, &mut m.analytics, json))
                .unwrap_or_else(|| Err(JsValue::from_str("Dashboard not initialized")))
        } else {
            Err(JsValue::from_str("Dashboard not initialized"))
        }
    })
}

/// The analytics header alone ("read 2m ago" ticks between refreshes).
#[wasm_bindgen]
pub fn set_analytics_head(json: &str) -> Result<(), JsValue> {
    let mut result = Ok(());
    with_ui(|ui| result = analytics::set_head(ui, json));
    result
}

/// One frame of the usage-trend morph (bridge.js interpolates the point arrays).
#[wasm_bindgen]
pub fn set_analytics_trend_paths(json: &str) -> Result<(), JsValue> {
    let mut result = Ok(());
    with_ui(|ui| result = analytics::set_trend_paths(ui, json));
    result
}

#[wasm_bindgen]
pub fn set_analytics_loading(loading: bool, error: &str) {
    with_ui(|ui| analytics::set_loading(ui, loading, error));
}

/// "home" | "analytics" | "accounts"; the shell animates the change.
#[wasm_bindgen]
pub fn set_current_page(page: &str) {
    with_ui(|ui| {
        ui.set_current_page(
            match page {
                "analytics" => "analytics",
                "accounts" => "accounts",
                _ => "home",
            }
            .into(),
        );
    });
}

#[wasm_bindgen]
pub fn set_refresh_interval(seconds: i32, known: bool) {
    if (30..=3600).contains(&seconds) {
        with_ui(|ui| {
            ui.set_refresh_interval_known(known);
            ui.set_refresh_interval_label(interval_label(seconds).into());
        });
    }
}

/// "30 s", "1 min", "1 min 30 s": the label the settings Select shows (public/view-model.mjs
/// intervalLabel produces the same strings).
pub fn interval_label(seconds: i32) -> String {
    if seconds < 60 {
        format!("{seconds} s")
    } else if seconds % 60 == 0 {
        format!("{} min", seconds / 60)
    } else {
        format!("{} min {} s", seconds / 60, seconds % 60)
    }
}
