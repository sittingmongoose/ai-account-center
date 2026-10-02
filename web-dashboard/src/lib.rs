use serde::Deserialize;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::prelude::*;
slint::include_modules!();
mod analytics;

thread_local! { static UI: RefCell<Option<Dashboard>> = const { RefCell::new(None) }; }
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = window, js_name = ccsDashboardAction)]
    fn dispatch_action(action: &str, value: &str);
}
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct UsageDto {
    label: String,
    amount: String,
    percent: f32,
    has_percent: bool,
    reset: String,
    reset_compact: String,
    expiration: String,
    meta: String,
}
impl From<UsageDto> for UsageView {
    fn from(v: UsageDto) -> Self {
        Self {
            label: v.label.into(),
            amount: v.amount.into(),
            percent: v.percent,
            has_percent: v.has_percent,
            reset: v.reset.into(),
            reset_compact: v.reset_compact.into(),
            expiration: v.expiration.into(),
            meta: v.meta.into(),
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct AccountDto {
    id: String,
    profile: String,
    email: String,
    plan: String,
    status: String,
    active: bool,
    can_mac: bool,
    can_windows: bool,
    five: UsageDto,
    weekly: UsageDto,
    fable: UsageDto,
    show_fable: bool,
    windows: Vec<UsageDto>,
    note: String,
}
impl From<AccountDto> for AccountView {
    fn from(v: AccountDto) -> Self {
        Self {
            id: v.id.into(),
            profile: v.profile.into(),
            email: v.email.into(),
            plan: v.plan.into(),
            status: v.status.into(),
            active: v.active,
            can_mac: v.can_mac,
            can_windows: v.can_windows,
            five: v.five.into(),
            weekly: v.weekly.into(),
            fable: v.fable.into(),
            show_fable: v.show_fable,
            windows: model(v.windows.into_iter().map(UsageView::from).collect()),
            note: v.note.into(),
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct AntigravityDto {
    id: String,
    profile: String,
    email: String,
    plan: String,
    status: String,
    selected: bool,
    runtime_verified: bool,
    can_activate: bool,
    five: UsageDto,
    weekly: UsageDto,
    note: String,
}
impl From<AntigravityDto> for AntigravityRowView {
    fn from(v: AntigravityDto) -> Self {
        Self {
            id: v.id.into(),
            profile: v.profile.into(),
            email: v.email.into(),
            plan: v.plan.into(),
            status: v.status.into(),
            selected: v.selected,
            runtime_verified: v.runtime_verified,
            can_activate: v.can_activate,
            five: v.five.into(),
            weekly: v.weekly.into(),
            note: v.note.into(),
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ProviderDto {
    id: String,
    name: String,
    glyph: String,
    identity: String,
    plan: String,
    status: String,
    note: String,
    source: String,
    windows: Vec<UsageDto>,
}
impl From<ProviderDto> for ProviderView {
    fn from(v: ProviderDto) -> Self {
        Self {
            id: v.id.into(),
            name: v.name.into(),
            glyph: v.glyph.into(),
            identity: v.identity.into(),
            plan: v.plan.into(),
            status: v.status.into(),
            note: v.note.into(),
            source: v.source.into(),
            windows: model(v.windows.into_iter().map(UsageView::from).collect()),
        }
    }
}
fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    Rc::new(VecModel::from(items)).into()
}
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Snapshot {
    claude: Vec<AccountDto>,
    codex: Vec<AccountDto>,
    providers: Vec<ProviderDto>,
    antigravity_accounts: Vec<AntigravityDto>,
    antigravity_auto_known: bool,
    antigravity_auto_enabled: bool,
    antigravity_auto_available: bool,
    antigravity_settings_available: bool,
    antigravity_pool_available: bool,
    antigravity_threshold_label: String,
    antigravity_pool_options: Vec<String>,
    antigravity_pool_label: String,
    antigravity_preview: String,
    antigravity_auto_setting: String,
    antigravity_auto_message: String,
    auto_enabled: bool,
    auto_available: bool,
    auto_setting: String,
    auto_message: String,
    updated: String,
    threshold_used: i32,
    threshold_label: String,
    active_codex_email: String,
}

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
    ui.on_action(|action, value| dispatch_action(action.as_str(), value.as_str()));
    UI.with(|slot| *slot.borrow_mut() = Some(ui.clone_strong()));
    ui.run().map_err(|e| JsValue::from_str(&e.to_string()))
}
#[wasm_bindgen]
pub fn resize_dashboard(width: f32, height: f32) {
    UI.with(|slot| {
        if let Some(ui) = slot.borrow().as_ref() {
            ui.window()
                .set_size(slint::LogicalSize::new(width.max(320.), height.max(320.)));
        }
    });
}
#[wasm_bindgen]
pub fn set_dashboard(json: &str) -> Result<(), JsValue> {
    let v: Snapshot =
        serde_json::from_str(json).map_err(|_| JsValue::from_str("Invalid dashboard view data"))?;
    UI.with(|slot| {
        if let Some(ui) = slot.borrow().as_ref() {
            ui.set_claude(model(v.claude.into_iter().map(AccountView::from).collect()));
            ui.set_codex_has_five_hour(v.codex.iter().any(|account| account.five.has_percent));
            ui.set_codex(model(v.codex.into_iter().map(AccountView::from).collect()));
            ui.set_providers(model(
                v.providers.into_iter().map(ProviderView::from).collect(),
            ));
            ui.set_antigravity_accounts(model(
                v.antigravity_accounts
                    .into_iter()
                    .map(AntigravityRowView::from)
                    .collect(),
            ));
            ui.set_antigravity_auto_known(v.antigravity_auto_known);
            ui.set_antigravity_auto_enabled(v.antigravity_auto_enabled);
            ui.set_antigravity_auto_available(v.antigravity_auto_available);
            ui.set_antigravity_settings_available(v.antigravity_settings_available);
            ui.set_antigravity_pool_available(v.antigravity_pool_available);
            ui.set_antigravity_threshold_label(v.antigravity_threshold_label.into());
            // ComboBox models reset their selected values: apply choices before confirmed label.
            ui.set_antigravity_pool_options(model(
                v.antigravity_pool_options
                    .into_iter()
                    .map(SharedString::from)
                    .collect(),
            ));
            ui.set_antigravity_pool_label(v.antigravity_pool_label.into());
            ui.set_antigravity_preview(v.antigravity_preview.into());
            ui.set_antigravity_auto_setting(v.antigravity_auto_setting.into());
            ui.set_antigravity_auto_message(v.antigravity_auto_message.into());
            ui.set_active_codex_email(v.active_codex_email.into());
            ui.set_auto_enabled(v.auto_enabled);
            ui.set_auto_available(v.auto_available);
            ui.set_threshold_used(v.threshold_used);
            ui.set_threshold_label(v.threshold_label.into());
            ui.set_auto_setting(v.auto_setting.into());
            ui.set_auto_message(v.auto_message.into());
            ui.set_updated(v.updated.into());
        }
    });
    Ok(())
}
#[wasm_bindgen]
pub fn set_session(authenticated: bool, loading: bool, busy: bool, message: &str) {
    UI.with(|slot| {
        if let Some(ui) = slot.borrow().as_ref() {
            ui.set_authenticated(authenticated);
            ui.set_auth_loading(loading);
            ui.set_busy(busy);
            ui.set_message(message.into());
            if authenticated {
                ui.set_password(SharedString::default());
            }
        }
    });
}
#[wasm_bindgen]
pub fn set_message(message: &str, busy: bool) {
    UI.with(|slot| {
        if let Some(ui) = slot.borrow().as_ref() {
            ui.set_message(message.into());
            ui.set_busy(busy);
        }
    });
}
#[wasm_bindgen]
pub fn set_theme(dark: bool) {
    UI.with(|slot| {
        if let Some(ui) = slot.borrow().as_ref() {
            ui.set_dark(dark);
        }
    });
}
#[wasm_bindgen]
pub fn show_details(title: &str, note: &str, windows_json: &str) -> Result<(), JsValue> {
    let windows: Vec<UsageDto> = serde_json::from_str(windows_json)
        .map_err(|_| JsValue::from_str("Invalid usage details"))?;
    UI.with(|slot| {
        if let Some(ui) = slot.borrow().as_ref() {
            ui.set_details_title(title.into());
            ui.set_details_note(note.into());
            ui.set_details_windows(model(windows.into_iter().map(UsageView::from).collect()));
            ui.set_details_open(true);
        }
    });
    Ok(())
}
#[wasm_bindgen]
pub fn set_update_status(message: &str, running: bool) {
    UI.with(|slot| {
        if let Some(ui) = slot.borrow().as_ref() {
            ui.set_update_message(message.into());
            ui.set_update_running(running);
        }
    });
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
#[wasm_bindgen]
pub fn show_activation_confirmation(json: &str) -> Result<(), JsValue> {
    let v: ConfirmationDto = serde_json::from_str(json)
        .map_err(|_| JsValue::from_str("Invalid activation confirmation"))?;
    UI.with(|slot| {
        if let Some(ui) = slot.borrow().as_ref() {
            ui.set_confirmation_product(
                if v.product == "Antigravity" {
                    "Antigravity"
                } else {
                    "Codex"
                }
                .into(),
            );
            ui.set_details_open(false);
            ui.set_confirmation_target(v.target_profile.into());
            ui.set_confirmation_warning(v.warning.into());
            ui.set_confirmation_expires(v.expires_at.into());
            ui.set_confirmation_error(v.error.into());
            ui.set_confirmation_valid(v.can_confirm);
            ui.set_confirmation_busy(v.in_progress);
            ui.set_confirmation_processes(model(
                v.processes
                    .into_iter()
                    .map(|p| ProcessView {
                        label: p.label.into(),
                        pid: p.pid.to_string().into(),
                        role: p.role.into(),
                    })
                    .collect(),
            ));
            ui.set_confirmation_open(true);
        }
    });
    Ok(())
}
#[wasm_bindgen]
pub fn close_activation_confirmation() {
    UI.with(|slot| {
        if let Some(ui) = slot.borrow().as_ref() {
            ui.set_confirmation_open(false);
        }
    });
}

#[wasm_bindgen]
pub fn set_analytics(json: &str) -> Result<(), JsValue> {
    UI.with(|slot| {
        if let Some(ui) = slot.borrow().as_ref() {
            analytics::set_analytics(ui, json)
        } else {
            Err(JsValue::from_str("Dashboard not initialized"))
        }
    })
}
#[wasm_bindgen]
pub fn set_analytics_loading(loading: bool, error: &str) {
    UI.with(|slot| {
        if let Some(ui) = slot.borrow().as_ref() {
            ui.set_analytics_loading(loading);
            ui.set_analytics_error(error.into());
        }
    });
}

#[wasm_bindgen]
pub fn set_current_page(page: &str) {
    UI.with(|slot| {
        if let Some(ui) = slot.borrow().as_ref() {
            ui.set_current_page(
                if page == "analytics" {
                    "analytics"
                } else {
                    "dashboard"
                }
                .into(),
            );
        }
    });
}
#[wasm_bindgen]
pub fn set_refresh_interval(seconds: i32, known: bool) {
    if (30..=3600).contains(&seconds) {
        UI.with(|slot| {
            if let Some(ui) = slot.borrow().as_ref() {
                ui.set_refresh_interval_known(known);
                ui.set_refresh_interval_label(
                    if known {
                        format!("{seconds}s")
                    } else {
                        "Unavailable".to_string()
                    }
                    .into(),
                );
            }
        });
    }
}
