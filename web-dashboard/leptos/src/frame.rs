//! The app frame (PLAN step 4): one chrome around all five tabs. Status and Scoreboard are drawn here; Home, Analytics
//! and Accounts are the Slint dashboard shown content-only in one same-origin iframe (`/?content=1`), kept loaded while
//! the tabs change. The two talk by postMessage:
//!
//! - frame to dashboard: `{type: "aac-frame", action, value}` with action navigate, refresh, update-apps, theme,
//!   logout or auth-recheck (bridge.js runs them as its own actions);
//! - dashboard to frame: `{type: "aac-content", kind, ...}` with kind ready, page (its own links moved it), toast,
//!   chrome (the header's status line) or auth (it signed out, or its session ended).
//!
//! Sign-in is the frame's (session.rs); until it is done no tab is drawn.

use crate::Page;
use crate::session::{self, SessionView};
use crate::ui::*;
use crate::{live, scoreboard, status};
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;

/// A message the dashboard or the frame raised, shown over every tab.
#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    pub id: u64,
    /// ok, err or info.
    pub kind: String,
    pub title: String,
    pub body: String,
}

#[derive(Clone, Copy)]
pub struct FrameCtx {
    pub route: RwSignal<Page>,
    /// What each tab's page says in the chrome, with the id of its registration.
    infos: RwSignal<Vec<(Page, u64, ChromeInfo)>>,
    next_info: RwSignal<u64>,
    pub menu_open: RwSignal<bool>,
    pub toasts: RwSignal<Vec<Toast>>,
    pub username: RwSignal<String>,
    next_toast: RwSignal<u64>,
}

fn origin() -> String {
    window().location().origin().unwrap_or_default()
}

fn slint_window() -> Option<web_sys::Window> {
    document()
        .query_selector("iframe.slint-frame")
        .ok()
        .flatten()
        .and_then(|el| el.dyn_into::<web_sys::HtmlIFrameElement>().ok())
        .and_then(|frame| frame.content_window())
}

/// Sends the dashboard one of its own actions; nothing happens before it has loaded.
pub fn post_slint(action: &str, value: &str) {
    let Some(target) = slint_window() else { return };
    let message = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&message, &"type".into(), &"aac-frame".into());
    let _ = js_sys::Reflect::set(&message, &"action".into(), &action.into());
    let _ = js_sys::Reflect::set(&message, &"value".into(), &value.into());
    let _ = target.post_message(&message, &origin());
}

fn push_history(path: &str, replace: bool) {
    let Ok(history) = window().history() else { return };
    let search = window().location().search().unwrap_or_default();
    let url = format!("{path}{search}");
    let _ = if replace {
        history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&url))
    } else {
        history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&url))
    };
}

impl FrameCtx {
    fn new(initial: Page) -> Self {
        FrameCtx {
            route: RwSignal::new(initial),
            infos: RwSignal::new(Vec::new()),
            next_info: RwSignal::new(0),
            menu_open: RwSignal::new(false),
            toasts: RwSignal::new(Vec::new()),
            username: RwSignal::new(String::new()),
            next_toast: RwSignal::new(0),
        }
    }

    /// Shows a tab: the address follows, the dashboard follows for its own pages, and the page reads at once.
    pub fn go(&self, page: Page) {
        self.menu_open.set(false);
        if self.route.get_untracked() == page {
            return;
        }
        push_history(page.path(), false);
        self.show(page);
    }

    fn show(&self, page: Page) {
        self.route.set(page);
        crate::set_active(page);
        if page.slint() {
            post_slint("navigate", page.id());
        } else {
            window().scroll_to_with_x_and_y(0.0, 0.0);
        }
    }

    /// A page tells the chrome what to say for its tab; the id lets it take that back when it is unmounted.
    pub fn register(&self, page: Page, info: ChromeInfo) -> u64 {
        let id = self.next_info.get_untracked() + 1;
        self.next_info.set(id);
        self.infos.update(|all| {
            all.retain(|(p, _, _)| *p != page);
            all.push((page, id, info));
        });
        id
    }

    /// The page was unmounted (Status and the Scoreboard at sign-out): its words go, as its signals have. Untracked,
    /// because the chrome that read them is being unmounted with it.
    pub fn unregister(&self, page: Page, id: u64) {
        self.infos.try_update_untracked(|all| all.retain(|(p, i, _)| !(*p == page && *i == id)));
    }

    /// What the chrome says for the tab on screen (reactive).
    pub fn info(&self) -> Option<ChromeInfo> {
        let page = self.route.get();
        self.infos
            .with(|all| all.iter().find(|(p, _, _)| *p == page).map(|(_, _, info)| *info))
    }

    pub fn refresh(&self) {
        if self.route.get_untracked().slint() {
            post_slint("refresh", "");
        } else if let Some(info) = self.infos.with_untracked(|all| {
            let page = self.route.get_untracked();
            all.iter().find(|(p, _, _)| *p == page).map(|(_, _, info)| *info)
        }) {
            info.refresh.run(());
        }
    }

    /// Update apps runs in the dashboard, and its results are listed under Accounts & Settings.
    pub fn update_apps(&self) {
        self.go(Page::Accounts);
        post_slint("update-apps", "");
    }

    pub fn set_theme(&self, mode: &str) {
        apply_theme(mode);
        post_slint("theme", mode);
    }

    /// The way back: this device opens the classic app again until the frame is turned on.
    pub fn classic(&self) {
        if let Some(s) = window().local_storage().ok().flatten() {
            let _ = s.set_item("aac-frame", "classic");
        }
        let _ = window().location().set_href("/");
    }

    /// Sign out. With the dashboard loaded its own Sign out runs (it waits for a switch on screen, and resets its
    /// state); it then tells the frame. Without it, the frame makes the same call.
    pub fn sign_out(&self) {
        if slint_window().is_some() {
            post_slint("logout", "");
        } else {
            session::sign_out(*self);
        }
    }

    pub fn toast(&self, kind: &str, title: &str, body: &str, ms: i32) {
        let id = self.next_toast.get_untracked() + 1;
        self.next_toast.set(id);
        self.toasts.update(|all| {
            all.push(Toast { id, kind: kind.into(), title: title.into(), body: body.into() });
            if all.len() > 4 {
                all.remove(0);
            }
        });
        // A plain browser timer: Leptos cancels its own when the owner that started it is cleaned up (NOTES.md).
        let toasts = self.toasts;
        let dismiss = Closure::once_into_js(move || toasts.update(|all| all.retain(|t| t.id != id)));
        let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(dismiss.unchecked_ref(), ms.max(2000));
    }
}

fn read_str(data: &wasm_bindgen::JsValue, key: &str) -> String {
    js_sys::Reflect::get(data, &key.into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default()
}

/// The dashboard's header line, as its own chrome view words it (view-model.mjs chromeView).
#[derive(Clone, Copy)]
struct SlintChrome {
    lead: RwSignal<String>,
    strong: RwSignal<String>,
    more: RwSignal<String>,
}

fn listen_to_dashboard(ctx: FrameCtx, chrome: SlintChrome) {
    let handler = Closure::<dyn Fn(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
        if event.origin() != origin() {
            return;
        }
        let from_dashboard = match (event.source(), slint_window()) {
            (Some(source), Some(frame)) => {
                let frame: &wasm_bindgen::JsValue = frame.as_ref();
                js_sys::Object::is(&source, frame)
            }
            _ => false,
        };
        if !from_dashboard {
            return;
        }
        let data = event.data();
        if read_str(&data, "type") != "aac-content" {
            return;
        }
        match read_str(&data, "kind").as_str() {
            "ready" => {
                post_slint("theme", &stored_theme());
                let page = ctx.route.get_untracked();
                if page.slint() {
                    post_slint("navigate", page.id());
                }
            }
            "page" => {
                let Some(page) = Page::from_id(&read_str(&data, "page")) else { return };
                if page == ctx.route.get_untracked() {
                    return;
                }
                if page.slint() {
                    // The dashboard moved itself (a Home link to Analytics): follow without telling it back.
                    push_history(page.path(), true);
                    ctx.route.set(page);
                    crate::set_active(page);
                } else {
                    ctx.go(page);
                }
            }
            "toast" => {
                let ms = js_sys::Reflect::get(&data, &"ms".into())
                    .ok()
                    .and_then(|v| v.as_f64())
                    .unwrap_or(4800.0) as i32;
                ctx.toast(&read_str(&data, "toastKind"), &read_str(&data, "title"), &read_str(&data, "body"), ms);
            }
            "chrome" => {
                chrome.lead.set(read_str(&data, "statusLead"));
                chrome.strong.set(read_str(&data, "statusStrong"));
                chrome.more.set(read_str(&data, "statusMore"));
            }
            "auth" => {
                let signed_in = js_sys::Reflect::get(&data, &"signedIn".into())
                    .ok()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let state = read_str(&data, "state");
                if !signed_in && matches!(state.as_str(), "default" | "expired") {
                    session::dashboard_signed_out(&state);
                }
            }
            _ => {}
        }
    });
    let _ = window().add_event_listener_with_callback("message", handler.as_ref().unchecked_ref());
    handler.forget();
}

/// The browser's Back and Forward move between tabs.
fn follow_history(ctx: FrameCtx) {
    let handler = Closure::<dyn Fn()>::new(move || {
        let page = crate::page_from_path(&window().location().pathname().unwrap_or_default());
        if page != ctx.route.get_untracked() {
            ctx.show(page);
        }
    });
    let _ = window().add_event_listener_with_callback("popstate", handler.as_ref().unchecked_ref());
    handler.forget();
}

/// The installed app opens offline too: the service worker serves each tab's address once it has stored it, so the
/// frame asks for every one while online (sw-route.js keeps an /app page's own copy only).
fn warm_offline_copies() {
    let navigator = window().navigator();
    let workers = js_sys::Reflect::get(&navigator, &"serviceWorker".into()).unwrap_or(wasm_bindgen::JsValue::UNDEFINED);
    if workers.is_undefined() {
        return;
    }
    if let Ok(register) = js_sys::Reflect::get(&workers, &"register".into()) {
        if let Some(register) = register.dyn_ref::<js_sys::Function>() {
            // A refusal (no worker on this server) only costs the offline copy.
            if let Some(promise) = register.call1(&workers, &"/sw.js".into()).ok().and_then(|p| p.dyn_into::<js_sys::Promise>().ok()) {
                leptos::task::spawn_local(async move {
                    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
                });
            }
        }
    }
    let warm = Closure::once_into_js(move || {
        if !window().navigator().on_line() {
            return;
        }
        // The start page too: the installed app opens at `/`, which sends this device on to its tab.
        let _ = window().fetch_with_str("/");
        for page in Page::ALL {
            let _ = window().fetch_with_str(page.path());
        }
    });
    let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(warm.unchecked_ref(), 4000);
}

#[component]
pub fn Frame(initial: Page) -> impl IntoView {
    let _ = js_sys::Reflect::set(&window(), &"__aacFrame".into(), &true.into());
    crate::set_theme(&stored_theme());
    let ctx = FrameCtx::new(initial);
    provide_context(ctx);
    push_history(initial.path(), true);
    let view_signal = session::start();
    Effect::new(move |_| {
        let name = view_signal.with(|v| v.username.clone());
        ctx.username.set(name);
    });
    // The dashboard's chrome line stands for its three tabs.
    let chrome = SlintChrome { lead: RwSignal::new(String::new()), strong: RwSignal::new(String::new()), more: RwSignal::new(String::new()) };
    let slint_info = ChromeInfo {
        updated: Signal::derive(move || chrome.strong.get()),
        live: Signal::derive(move || chrome.more.get() == "live"),
        summary: Signal::derive(Vec::new),
        refresh: Callback::new(move |_| post_slint("refresh", "")),
        lead: Some(Signal::derive(move || {
            let lead = chrome.lead.get();
            if lead.is_empty() { "Updated".into() } else { lead }
        })),
    };
    for page in [Page::Home, Page::Analytics, Page::Accounts] {
        ctx.register(page, slint_info);
    }
    listen_to_dashboard(ctx, chrome);
    follow_history(ctx);
    warm_offline_copies();
    let close_menu = window_event_listener(leptos::ev::click, move |_| ctx.menu_open.set(false));
    on_cleanup(move || close_menu.remove());
    let signed_in = Memo::new(move |_| view_signal.with(|v: &SessionView| v.signed_in));
    view! {
        <Show when=move || signed_in.get() fallback=move || view! { <session::SignIn view=view_signal/> }>
            <FrameShell ctx=ctx>
                <StatusRoute ctx=ctx/>
                <ScoreboardRoute ctx=ctx/>
                <SlintHost ctx=ctx/>
            </FrameShell>
        </Show>
        <Toasts ctx=ctx/>
    }
}

/// Built the first time its tab is shown, then kept, so a tab change keeps a question's draft and the sheet's place.
#[component]
fn StatusRoute(ctx: FrameCtx) -> impl IntoView {
    let seen = Memo::new(move |prev: Option<&bool>| prev.copied().unwrap_or(false) || ctx.route.get() == Page::Status);
    view! {
        <div class="route" hidden=move || ctx.route.get() != Page::Status>
            {move || seen.get().then(|| {
                let live = live::start();
                provide_context(live);
                // The session ended under this page: the frame's sign-in says why.
                Effect::new(move |_| {
                    if live.error.with(|e| matches!(e, Some(crate::api::FetchError::SignedOut))) {
                        session::dashboard_signed_out("expired");
                    }
                });
                view! { <status::StatusPage embedded=false/> }
            })}
        </div>
    }
}

#[component]
fn ScoreboardRoute(ctx: FrameCtx) -> impl IntoView {
    let seen = Memo::new(move |prev: Option<&bool>| prev.copied().unwrap_or(false) || ctx.route.get() == Page::Scoreboard);
    view! {
        <div class="route" hidden=move || ctx.route.get() != Page::Scoreboard>
            {move || seen.get().then(|| view! { <scoreboard::ScoreboardPage embedded=false/> })}
        </div>
    }
}

/// The Slint dashboard, content-only, for Home, Analytics and Accounts: loaded the first time one of them is shown.
#[component]
fn SlintHost(ctx: FrameCtx) -> impl IntoView {
    let seen = Memo::new(move |prev: Option<&bool>| prev.copied().unwrap_or(false) || ctx.route.get().slint());
    view! {
        <div class="route slint-host" hidden=move || !ctx.route.get().slint()>
            {move || seen.get().then(|| {
                let page = ctx.route.get_untracked();
                // The browser kit's probes (`?e2e`) reach the dashboard too.
                let e2e = window().location().search().unwrap_or_default().split(['?', '&']).any(|p| p == "e2e");
                let src = format!("/?content=1&view={}{}", page.id(), if e2e { "&e2e" } else { "" });
                view! { <iframe class="slint-frame" title="AI Account Center dashboard" src=src></iframe> }
            })}
        </div>
    }
}

#[component]
fn Toasts(ctx: FrameCtx) -> impl IntoView {
    view! {
        <div class="toasts" role="status" aria-live="polite">
            {move || ctx.toasts.get().into_iter().map(|t| {
                let id = t.id;
                view! {
                    <div class="toast" data-kind=t.kind.clone()>
                        <div class="toast-text">
                            <b data-tone=match t.kind.as_str() { "err" => Some("crit"), "ok" => Some("good"), _ => None }>{t.title}</b>
                            {(!t.body.is_empty()).then(|| view! { <span>{t.body}</span> })}
                        </div>
                        <button class="icon-btn bare" aria-label="Dismiss" on:click=move |_| ctx.toasts.update(|all| all.retain(|x| x.id != id))><Icon name="close"/></button>
                    </div>
                }
            }).collect_view()}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> ChromeInfo {
        ChromeInfo {
            updated: Signal::derive(String::new),
            live: Signal::derive(|| true),
            summary: Signal::derive(Vec::new),
            refresh: Callback::new(|_| {}),
            lead: None,
        }
    }

    #[test]
    fn an_unmounted_page_takes_back_its_words_but_not_a_newer_ones() {
        let owner = Owner::new();
        owner.set();
        let ctx = FrameCtx::new(Page::Status);
        let old = ctx.register(Page::Status, info());
        let new = ctx.register(Page::Status, info());
        // The page signed out earlier is cleaned up after the new one registered: the new words stay.
        ctx.unregister(Page::Status, old);
        assert!(ctx.info().is_some());
        ctx.unregister(Page::Status, new);
        assert!(ctx.info().is_none());
    }
}
