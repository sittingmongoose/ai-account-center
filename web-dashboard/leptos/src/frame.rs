//! The app frame (PLAN step 4): one chrome around all five tabs. Home, Accounts & Settings, Status and Scoreboard are
//! drawn here; Analytics is the Slint dashboard shown content-only in one same-origin iframe (`/?content=1`), kept
//! loaded while the tabs change. The two talk by postMessage:
//!
//! - frame to dashboard: `{type: "aac-frame", action, value}` with action navigate, refresh, update-apps, theme,
//!   logout or auth-recheck (bridge.js runs them as its own actions);
//! - dashboard to frame: `{type: "aac-content", kind, ...}` with kind ready, page (its own links moved it), toast,
//!   chrome (the header's status line) or auth (it signed out, or its session ended).
//!
//! Home and Accounts & Settings draw the dashboard's views (dash.rs): the iframe still loads for them and keeps the
//! network, session and every server call, but stays hidden while a native tab shows. Details (details.rs) and the
//! switch confirmation (activation.rs) are the frame's own overlays over every tab.
//!
//! Sign-in is the frame's (session.rs); until it is done no tab is drawn.

use crate::Page;
use crate::activation::ActivationDialog;
use crate::dash::{self, DashCtx};
use crate::home::HomePage;
use crate::session::{self, SessionView};
use crate::ui::*;
use crate::{accounts, details, live, scoreboard, status};
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
    /// The dashboard's page has started and listens (its `ready`).
    booted: RwSignal<bool>,
    /// The dashboard has signed in (its `auth`): only then does it run refresh, update-apps and logout.
    authed: RwSignal<bool>,
    /// Commands for the dashboard, held until it can run them and kept until it says it ran them (`done`).
    outbox: RwSignal<Vec<Command>>,
    next_command: RwSignal<u64>,
    /// Changes each time the frame signs in or out: a sign-out fallback made in one session never acts in the next.
    epoch: RwSignal<u64>,
    /// The dashboard's views for the native pages, and their way back (dash.rs).
    pub dash: DashCtx,
    /// The theme setting (auto, light or dark): the header's button and Settings' Appearance show the same one.
    pub theme: RwSignal<String>,
}

/// One of the dashboard's own actions, waiting for it or on its way.
#[derive(Clone, Debug, PartialEq)]
struct Command {
    id: u64,
    action: String,
    value: String,
    sent: bool,
}

/// What the dashboard needs before it runs an action: navigate and theme work while it starts; the rest wait for
/// its sign-in, which it would otherwise ignore (bridge.js `ccsDashboardAction`).
fn needs_sign_in(action: &str) -> bool {
    !matches!(action, "navigate" | "theme")
}

fn origin() -> String {
    window().location().origin().unwrap_or_default()
}

pub(crate) fn slint_window() -> Option<web_sys::Window> {
    document()
        .query_selector("iframe.slint-frame")
        .ok()
        .flatten()
        .and_then(|el| el.dyn_into::<web_sys::HtmlIFrameElement>().ok())
        .and_then(|frame| frame.content_window())
}

/// Posts one command to the dashboard's window; the dashboard answers `done` with its id once it has run it.
fn post(target: &web_sys::Window, command: &Command) {
    let message = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&message, &"type".into(), &"aac-frame".into());
    let _ = js_sys::Reflect::set(&message, &"action".into(), &command.action.as_str().into());
    let _ = js_sys::Reflect::set(&message, &"value".into(), &command.value.as_str().into());
    let _ = js_sys::Reflect::set(&message, &"id".into(), &(command.id as f64).into());
    let _ = target.post_message(&message, &origin());
}

fn push_history(path: &str, replace: bool) {
    let Ok(history) = window().history() else {
        return;
    };
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
            booted: RwSignal::new(false),
            authed: RwSignal::new(false),
            outbox: RwSignal::new(Vec::new()),
            next_command: RwSignal::new(0),
            epoch: RwSignal::new(0),
            dash: DashCtx::new(),
            theme: RwSignal::new("auto".into()),
        }
    }

    /// Asks the dashboard to run one of its actions. The command waits while the dashboard's page is created, starts
    /// or signs in, and a newer one of the same kind replaces one not sent yet (the latest page, theme; one refresh).
    pub fn send(&self, action: &str, value: &str) -> u64 {
        let id = self.next_command.get_untracked() + 1;
        self.next_command.set(id);
        self.outbox.update(|all| {
            all.retain(|c| c.sent || c.action != action);
            all.push(Command {
                id,
                action: action.into(),
                value: value.into(),
                sent: false,
            });
        });
        self.flush();
        id
    }

    /// Sends what the dashboard can run now.
    fn flush(&self) {
        let (booted, authed) = (self.booted.get_untracked(), self.authed.get_untracked());
        if !booted {
            return;
        }
        let Some(target) = slint_window() else { return };
        self.outbox.update(|all| {
            for command in all
                .iter_mut()
                .filter(|c| !c.sent && (authed || !needs_sign_in(&c.action)))
            {
                post(&target, command);
                command.sent = true;
            }
        });
    }

    /// The dashboard's page started (again). Of what went to an earlier page and was never answered, only navigate
    /// and theme go again: a refresh or an update may already have run, and an update must never start twice. An
    /// unanswered update is dropped with a word to check its results; an unanswered logout stays for Sign out's
    /// own fallback, which asks the server first.
    fn dashboard_started(&self) {
        self.booted.set(true);
        self.authed.set(false);
        self.dash.restarted();
        let mut lost_update = false;
        self.outbox.update(|all| {
            all.retain_mut(|c| {
                if !c.sent {
                    return true;
                }
                match c.action.as_str() {
                    "navigate" | "theme" => {
                        c.sent = false;
                        true
                    }
                    "logout" => true,
                    "update-apps" => {
                        lost_update = true;
                        false
                    }
                    _ => false,
                }
            })
        });
        if lost_update {
            self.toast(
                "info",
                "Update apps may not have started.",
                "Check the Update apps results.",
                8000,
            );
        }
        self.flush();
    }

    fn dashboard_signed_in(&self, signed_in: bool) {
        self.authed.set(signed_in);
        self.dash.live.set(signed_in);
        if signed_in {
            self.flush();
        }
    }

    fn command_done(&self, id: u64) {
        self.outbox.update(|all| all.retain(|c| c.id != id));
    }

    fn command_pending(&self, id: u64) -> bool {
        self.outbox
            .with_untracked(|all| all.iter().any(|c| c.id == id))
    }

    /// The dashboard's page is gone (the frame signed out): nothing waits for it any more.
    fn dashboard_gone(&self) {
        let _ = self.booted.try_set(false);
        let _ = self.authed.try_set(false);
        let _ = self.outbox.try_update(|all| all.clear());
        self.dash.clear();
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
        if page.bridged() {
            self.send("navigate", page.id());
        }
        if !page.slint() {
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
        self.infos
            .try_update_untracked(|all| all.retain(|(p, i, _)| !(*p == page && *i == id)));
    }

    /// What the chrome says for the tab on screen (reactive).
    pub fn info(&self) -> Option<ChromeInfo> {
        let page = self.route.get();
        self.infos.with(|all| {
            all.iter()
                .find(|(p, _, _)| *p == page)
                .map(|(_, _, info)| *info)
        })
    }

    pub fn refresh(&self) {
        if self.route.get_untracked().bridged() {
            self.send("refresh", "");
        } else if let Some(info) = self.infos.with_untracked(|all| {
            let page = self.route.get_untracked();
            all.iter()
                .find(|(p, _, _)| *p == page)
                .map(|(_, _, info)| *info)
        }) {
            info.refresh.run(());
        }
    }

    /// Update apps runs in the dashboard, and its results are listed under Accounts & Settings. The command waits
    /// while the dashboard is created and signs in, so the first click counts.
    pub fn update_apps(&self) {
        self.go(Page::Accounts);
        self.send("update-apps", "");
    }

    pub fn set_theme(&self, mode: &str) {
        apply_theme(mode);
        self.theme.set(mode.to_string());
        self.send("theme", mode);
    }

    /// The way back: this device opens the classic app again until the frame is turned on.
    pub fn classic(&self) {
        if let Some(s) = window().local_storage().ok().flatten() {
            let _ = s.set_item("aac-frame", "classic");
        }
        let _ = window().location().set_href("/");
    }

    /// Sign out. With the dashboard signed in, its own Sign out runs: it waits while an account switch is waiting on
    /// screen, resets its state and tells the frame. Otherwise (not created, still starting, not signed in yet, so
    /// no switch can be waiting) the frame ends the session itself, and the dashboard's page goes with the
    /// signed-in frame. If a signed-in dashboard has not answered after 10 s, the frame asks the server: a session
    /// already ended is only followed here, one still signed in gets the frame's one logout. The fallback is dropped
    /// when the frame signs out or in meanwhile, so it never acts on a later session.
    pub fn sign_out(&self) {
        if !(self.booted.get_untracked() && self.authed.get_untracked() && slint_window().is_some())
        {
            self.outbox.update(|all| all.clear());
            session::sign_out(*self);
            return;
        }
        let id = self.send("logout", "");
        let ctx = *self;
        let epoch = self.epoch.get_untracked();
        let fallback = Closure::once_into_js(move || {
            if ctx.epoch.try_get_untracked() != Some(epoch) || !ctx.command_pending(id) {
                return;
            }
            ctx.command_done(id);
            session::finish_sign_out(ctx);
        });
        let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(
            fallback.unchecked_ref(),
            10_000,
        );
    }

    pub fn toast(&self, kind: &str, title: &str, body: &str, ms: i32) {
        let id = self.next_toast.get_untracked() + 1;
        self.next_toast.set(id);
        self.toasts.update(|all| {
            all.push(Toast {
                id,
                kind: kind.into(),
                title: title.into(),
                body: body.into(),
            });
            if all.len() > 4 {
                all.remove(0);
            }
        });
        // A plain browser timer: Leptos cancels its own when the owner that started it is cleaned up (NOTES.md).
        let toasts = self.toasts;
        let dismiss =
            Closure::once_into_js(move || toasts.update(|all| all.retain(|t| t.id != id)));
        let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(
            dismiss.unchecked_ref(),
            ms.max(2000),
        );
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
    let handler =
        Closure::<dyn Fn(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
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
                    ctx.dashboard_started();
                    // The native pages draw its views: ask for them (it listens from now on).
                    if let Some(target) = slint_window() {
                        dash::subscribe(&target);
                    }
                    ctx.send("theme", &stored_theme());
                    let page = ctx.route.get_untracked();
                    if page.bridged() {
                        ctx.send("navigate", page.id());
                    }
                }
                "view" => {
                    let json = read_str(&data, "json");
                    ctx.dash.receive(&read_str(&data, "key"), json);
                }
                "done" => {
                    if let Some(id) = js_sys::Reflect::get(&data, &"id".into())
                        .ok()
                        .and_then(|v| v.as_f64())
                    {
                        ctx.command_done(id as u64);
                    }
                }
                "page" => {
                    let Some(page) = Page::from_id(&read_str(&data, "page")) else {
                        return;
                    };
                    if page == ctx.route.get_untracked() {
                        return;
                    }
                    if page.bridged() {
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
                    ctx.toast(
                        &read_str(&data, "toastKind"),
                        &read_str(&data, "title"),
                        &read_str(&data, "body"),
                        ms,
                    );
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
                    ctx.dashboard_signed_in(signed_in);
                    if !signed_in && matches!(state.as_str(), "default" | "expired") {
                        let reason = read_str(&data, "reason");
                        session::dashboard_signed_out(
                            &state,
                            (!reason.is_empty()).then_some(reason.as_str()),
                        );
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
    let workers = js_sys::Reflect::get(&navigator, &"serviceWorker".into())
        .unwrap_or(wasm_bindgen::JsValue::UNDEFINED);
    if workers.is_undefined() {
        return;
    }
    if let Ok(register) = js_sys::Reflect::get(&workers, &"register".into()) {
        if let Some(register) = register.dyn_ref::<js_sys::Function>() {
            // A refusal (no worker on this server) only costs the offline copy.
            if let Some(promise) = register
                .call1(&workers, &"/sw.js".into())
                .ok()
                .and_then(|p| p.dyn_into::<js_sys::Promise>().ok())
            {
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
    let _ =
        window().set_timeout_with_callback_and_timeout_and_arguments_0(warm.unchecked_ref(), 4000);
}

#[component]
pub fn Frame(initial: Page) -> impl IntoView {
    let _ = js_sys::Reflect::set(&window(), &"__aacFrame".into(), &true.into());
    crate::set_theme(&stored_theme());
    let ctx = FrameCtx::new(initial);
    ctx.theme.set(stored_theme());
    provide_context(ctx);
    provide_context(ctx.dash);
    push_history(initial.path(), true);
    let view_signal = session::start();
    Effect::new(move |_| {
        let name = view_signal.with(|v| v.username.clone());
        ctx.username.set(name);
    });
    // The dashboard's chrome line stands for its three tabs.
    let chrome = SlintChrome {
        lead: RwSignal::new(String::new()),
        strong: RwSignal::new(String::new()),
        more: RwSignal::new(String::new()),
    };
    let slint_info = ChromeInfo {
        updated: Signal::derive(move || chrome.strong.get()),
        live: Signal::derive(move || chrome.more.get() == "live"),
        summary: Signal::derive(Vec::new),
        refresh: Callback::new(move |_| {
            ctx.send("refresh", "");
        }),
        lead: Some(Signal::derive(move || {
            let lead = chrome.lead.get();
            if lead.is_empty() {
                "Updated".into()
            } else {
                lead
            }
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
    // Each sign-in and sign-out starts a new epoch (see `sign_out`).
    Effect::new(move |_| {
        signed_in.track();
        ctx.epoch.update(|e| *e += 1);
    });
    view! {
        <Show when=move || signed_in.get() fallback=move || view! { <session::SignIn view=view_signal/> }>
            <FrameShell ctx=ctx>
                <HomeRoute ctx=ctx/>
                <StatusRoute ctx=ctx/>
                <ScoreboardRoute ctx=ctx/>
                <AccountsRoute ctx=ctx/>
                <SlintHost ctx=ctx/>
                <details::DetailsOverlay ctx=ctx/>
            </FrameShell>
            <ActivationDialog/>
        </Show>
        <Toasts ctx=ctx/>
    }
}

/// Home, drawn here from the dashboard's view (PLAN step 5). Built the first time its tab is shown, then kept.
#[component]
fn HomeRoute(ctx: FrameCtx) -> impl IntoView {
    let seen = Memo::new(move |prev: Option<&bool>| {
        prev.copied().unwrap_or(false) || ctx.route.get() == Page::Home
    });
    view! {
        <div class="route" hidden=move || ctx.route.get() != Page::Home>
            {move || seen.get().then(|| view! { <HomePage/> })}
        </div>
    }
}

/// Built the first time its tab is shown, then kept, so a tab change keeps a question's draft and the sheet's place.
#[component]
fn StatusRoute(ctx: FrameCtx) -> impl IntoView {
    let seen = Memo::new(move |prev: Option<&bool>| {
        prev.copied().unwrap_or(false) || ctx.route.get() == Page::Status
    });
    view! {
        <div class="route" hidden=move || ctx.route.get() != Page::Status>
            {move || seen.get().then(|| {
                let live = live::start();
                provide_context(live);
                // The session ended under this page: the frame's sign-in says why.
                Effect::new(move |_| {
                    if live.error.with(|e| matches!(e, Some(crate::api::FetchError::SignedOut))) {
                        session::dashboard_signed_out("expired", None);
                    }
                });
                view! { <status::StatusPage embedded=false/> }
            })}
        </div>
    }
}

#[component]
fn ScoreboardRoute(ctx: FrameCtx) -> impl IntoView {
    let seen = Memo::new(move |prev: Option<&bool>| {
        prev.copied().unwrap_or(false) || ctx.route.get() == Page::Scoreboard
    });
    view! {
        <div class="route" hidden=move || ctx.route.get() != Page::Scoreboard>
            {move || seen.get().then(|| view! { <scoreboard::ScoreboardPage embedded=false/> })}
        </div>
    }
}

/// Accounts & Settings, drawn from the dashboard's views: built the first time its tab is shown, then kept, so a flow's
/// typed name and an open form survive a tab change.
#[component]
fn AccountsRoute(ctx: FrameCtx) -> impl IntoView {
    let seen = Memo::new(move |prev: Option<&bool>| {
        prev.copied().unwrap_or(false) || ctx.route.get() == Page::Accounts
    });
    view! {
        <div class="route" hidden=move || ctx.route.get() != Page::Accounts>
            {move || seen.get().then(|| view! { <accounts::AccountsPage ctx=ctx/> })}
        </div>
    }
}

/// The Slint dashboard, content-only: loaded the first time a tab that takes its data is shown (Home, Analytics,
/// Accounts), shown only while a tab it draws is current (Analytics, Accounts).
#[component]
fn SlintHost(ctx: FrameCtx) -> impl IntoView {
    let seen = Memo::new(move |prev: Option<&bool>| {
        prev.copied().unwrap_or(false) || ctx.route.get().bridged()
    });
    // A switch confirmation the dashboard draws itself (the frame doesn't draw that dialog yet) shows the dashboard
    // over a native tab while it is open, so it is never answered unseen.
    let raised = Memo::new(move |_| {
        !dash::owns("activation")
            && !ctx.route.get().slint()
            && ctx.dash.activation.with(|a| !a.is_empty())
    });
    view! {
        <div class="route slint-host" class:raised=move || raised.get() hidden=move || !ctx.route.get().slint() && !raised.get()>
            {move || seen.get().then(|| {
                // The page goes with the signed-in frame; commands for it go too.
                on_cleanup(move || ctx.dashboard_gone());
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
    fn commands_wait_for_the_dashboard_and_the_newest_of_a_kind_wins() {
        let owner = Owner::new();
        owner.set();
        let ctx = FrameCtx::new(Page::Status);
        // Nothing has started yet: every command waits, unsent.
        ctx.send("navigate", "home");
        let update = ctx.send("update-apps", "");
        ctx.send("navigate", "accounts");
        let waiting: Vec<(String, String, bool)> = ctx
            .outbox
            .get_untracked()
            .into_iter()
            .map(|c| (c.action, c.value, c.sent))
            .collect();
        assert_eq!(
            waiting,
            vec![
                ("update-apps".into(), String::new(), false),
                ("navigate".into(), "accounts".into(), false)
            ]
        );
        // The dashboard said it ran it: it leaves the outbox.
        assert!(ctx.command_pending(update));
        ctx.command_done(update);
        assert!(!ctx.command_pending(update));
        // Only navigate and theme go before the dashboard has signed in.
        assert!(
            ["logout", "update-apps", "refresh", "auth-recheck"]
                .iter()
                .all(|a| needs_sign_in(a))
        );
        assert!(!needs_sign_in("navigate") && !needs_sign_in("theme"));
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
