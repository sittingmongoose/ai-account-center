//! The app frame (PLAN step 4): one chrome around all five tabs, every one drawn here, and every one reading its own
//! data: Status `/api/status`, the Scoreboard its own, Analytics `/api/accounts/analytics` (analytics/feed.rs), and
//! Home and Accounts & Settings the dashboard (dash.rs, `public/frame-dashboard.mjs`: the server calls, timers and
//! guards bridge.js kept, run in this window since G5). The classic Slint app and bridge.js were retired in Leptos
//! finish (step 9): every page address opens this frame.
//!
//! Details (details.rs) is the frame's own overlay over every tab.
//!
//! Sign-in is the frame's (session.rs); until it is done no tab is drawn.

use crate::Page;
use crate::dash::{self, DashCtx};
use crate::home::HomePage;
use crate::refreshing::{self, Refreshing};
use crate::session::{self, SessionView};
use crate::ui::*;
use crate::{accounts, analytics, details, live, scoreboard, status};
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
    /// The server has a newer build than the one running (`watch_build`): the frame offers "New version · Reload".
    /// Holds the offered build id; empty means no notice.
    pub new_build: RwSignal<String>,
    /// The build "Later" dismissed (review L6): the poll offers again only for a different one.
    pub dismissed_build: RwSignal<String>,
    pub toasts: RwSignal<Vec<Toast>>,
    pub username: RwSignal<String>,
    next_toast: RwSignal<u64>,
    /// The dashboard's views for Home, Accounts & Settings and Details, and their way back
    /// (dash.rs).
    pub dash: DashCtx,
    /// The theme setting (auto, light or dark): the header's button and Settings' Appearance show the same one.
    pub theme: RwSignal<String>,
    /// The tabs whose page has drawn its first data (or its first failure), one bit per `Page`: the whole-page splash
    /// waits for the first tab's (`follow_boot`).
    ready: RwSignal<u8>,
    /// The header's Refresh while a refresh it started runs (refreshing.rs).
    refreshing: RwSignal<Refreshing>,
}

fn push_history(path: &str, replace: bool) {
    let Ok(history) = window().history() else {
        return;
    };
    // the classic app's ?view=, ?classic=1 and ?content=1 go: the tab is in the path now. Taking over the address the
    // frame was opened at (replace) keeps its #fragment; a new tab starts without one (recheck L2).
    let location = window().location();
    let hash = if replace {
        location.hash().unwrap_or_default()
    } else {
        String::new()
    };
    let url = crate::frame_url(path, &location.search().unwrap_or_default(), &hash);
    let _ = if replace {
        history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&url))
    } else {
        history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&url))
    };
}

/// A page tells the frame it has drawn its first data (`FrameCtx::mark_ready`) once `drawn` is true. Embedded pages
/// have no frame, and nothing to tell.
pub(crate) fn mark_ready_when(page: Page, drawn: impl Fn() -> bool + 'static) {
    let Some(ctx) = use_context::<FrameCtx>() else {
        return;
    };
    Effect::new(move |done: Option<bool>| {
        if done == Some(true) {
            return true;
        }
        if drawn() {
            ctx.mark_ready(page);
            return true;
        }
        false
    });
}

/// Until the frame knows whether this browser is signed in, and, signed in, until the tab on screen has its first
/// data, index.html's splash stays (ui/splash.rs). Signed out, the sign-in form follows it at once. A tab whose first
/// read takes longer than `BOOT_DATA_WAIT_MS` draws its own splash in its content area instead.
fn follow_boot(ctx: FrameCtx, view: RwSignal<SessionView>) {
    let waited = RwSignal::new(false);
    let _ = set_timeout(
        move || {
            let _ = waited.try_set(true);
        },
        std::time::Duration::from_millis(crate::ui::splash::BOOT_DATA_WAIT_MS),
    );
    Effect::new(move |done: Option<bool>| {
        if done == Some(true) {
            return true;
        }
        let (signed_in, loading) = view.with(|v| (v.signed_in, v.state == "loading"));
        let wait = if signed_in {
            !ctx.is_ready(ctx.route.get()) && !waited.get()
        } else {
            loading
        };
        if wait {
            return false;
        }
        crate::ui::splash::dismiss_boot(crate::ui::motion_on());
        true
    });
}

impl FrameCtx {
    fn new(initial: Page) -> Self {
        FrameCtx {
            route: RwSignal::new(initial),
            infos: RwSignal::new(Vec::new()),
            next_info: RwSignal::new(0),
            menu_open: RwSignal::new(false),
            new_build: RwSignal::new(String::new()),
            dismissed_build: RwSignal::new(String::new()),
            toasts: RwSignal::new(Vec::new()),
            username: RwSignal::new(String::new()),
            next_toast: RwSignal::new(0),
            dash: DashCtx::new(),
            theme: RwSignal::new("auto".into()),
            ready: RwSignal::new(0),
            refreshing: RwSignal::new(Refreshing::default()),
        }
    }

    /// The page has drawn its first data, or its first failure (it then says so itself).
    pub fn mark_ready(&self, page: Page) {
        let bit = 1u8 << page as u8;
        if self.ready.try_with_untracked(|r| r & bit == 0) == Some(true) {
            self.ready.update(|r| *r |= bit);
        }
    }

    /// Whether the page has drawn its first data (reactive).
    pub fn is_ready(&self, page: Page) -> bool {
        self.ready.with(|r| r & (1u8 << page as u8) != 0)
    }

    /// Shows a tab: the address follows, and the page reads at once.
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
        window().scroll_to_with_x_and_y(0.0, 0.0);
        remember_page(page);
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

    /// The header's Refresh: one refresh of the tab on screen, the button spinning until it has finished. A press
    /// while one runs starts nothing.
    pub fn refresh(&self) {
        let Some(done) = refreshing::press(self.refreshing) else {
            return;
        };
        if self.route.get_untracked().reads_dashboard() {
            self.dash.command_then("refresh", Some(done));
        } else if let Some(info) = self.infos.with_untracked(|all| {
            let page = self.route.get_untracked();
            all.iter()
                .find(|(p, _, _)| *p == page)
                .map(|(_, _, info)| *info)
        }) {
            info.refresh.run(done);
        } else {
            // no page has registered yet: nothing ran
            done.finished();
        }
    }

    /// Whether the header's Refresh spins (reactive).
    pub fn is_refreshing(&self) -> bool {
        self.refreshing.with(|r| r.on())
    }

    /// Update apps runs in the dashboard controller, and its results are listed under Accounts & Settings. The command
    /// waits while the controller loads, so the first click counts.
    pub fn update_apps(&self) {
        self.go(Page::Accounts);
        self.dash.command("update-apps");
    }

    pub fn set_theme(&self, mode: &str) {
        apply_theme(mode);
        self.theme.set(mode.to_string());
    }

    /// Sign out. With the dashboard controller started, its own Sign out runs (bridge.js signOut): it words a failure
    /// and ends the frame's session when it has signed out. Otherwise (not loaded yet) the frame's session signs out
    /// itself.
    pub fn sign_out(&self) {
        if self.dash.sign_out().is_none() {
            session::sign_out(*self);
        }
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

/// The tab this device showed last (a page id, never data): the classic app's key, so a device keeps its place.
const LAST_PAGE_KEY: &str = "aac-last-page";

/// Where a fresh sign-in goes (DESIGN-MOBILE.md 4.5.4, bridge.js `signedIn`): signed in through the form while the
/// frame shows Home, the tab this device showed last opens instead; a stored id that names no tab, or Home, changes
/// nothing. An address that names another tab is kept.
pub(crate) fn reopen_target(fresh: bool, on: Page, stored: Option<&str>) -> Option<Page> {
    if !fresh || on != Page::Home {
        return None;
    }
    Page::from_id(stored?).filter(|p| *p != Page::Home)
}

/// The tab a signed-in session lands on: the last tab after a fresh sign-in on Home (`reopen_target`), else the tab
/// the address opened. It is what the frame shows, so it is what the device remembers (review L2: a directly opened
/// tab counts as shown, as the classic controller's first `page()` did).
pub(crate) fn landing_page(fresh: bool, on: Page, stored: Option<&str>) -> Page {
    reopen_target(fresh, on, stored).unwrap_or(on)
}

/// The tab this device showed last, for the next fresh sign-in (`reopen_last_page`). Only a signed-in frame writes it:
/// while the sign-in form waits, the stored tab is kept for it.
fn remember_page(page: Page) {
    if let Some(s) = window().local_storage().ok().flatten() {
        let _ = s.set_item(LAST_PAGE_KEY, page.id());
    }
}

/// Follows the session: once per sign-in, the frame lands on `landing_page` and remembers it; after a fresh sign-in
/// on Home that is the last tab.
fn reopen_last_page(ctx: FrameCtx, view: RwSignal<SessionView>) {
    Effect::new(move |done: Option<bool>| {
        let (signed_in, fresh) = view.with(|v| (v.signed_in, v.fresh));
        // one reopen per sign-in: armed again once signed out
        if !signed_in {
            return false;
        }
        if done == Some(true) {
            return true;
        }
        let stored = window()
            .local_storage()
            .ok()
            .flatten()
            .and_then(|s| s.get_item(LAST_PAGE_KEY).ok().flatten());
        let on = ctx.route.get_untracked();
        match landing_page(fresh, on, stored.as_deref()) {
            page if page != on => ctx.go(page),
            page => remember_page(page),
        }
        true
    });
}

/// The browser's Back and Forward move between tabs.
fn follow_history(ctx: FrameCtx) {
    let handler = Closure::<dyn Fn()>::new(move || {
        let location = window().location();
        let page = crate::page_from_location(
            &location.pathname().unwrap_or_default(),
            &location.search().unwrap_or_default(),
        );
        if page != ctx.route.get_untracked() {
            ctx.show(page);
        }
    });
    let _ = window().add_event_listener_with_callback("popstate", handler.as_ref().unchecked_ref());
    handler.forget();
}

/// index.html's page-module loaders, warmed for the offline copy (`warm_offline_copies`).
const WARMED_MODULES: &[&str] = &["aacLoadDashboard", "aacLoadAnalytics"];

/// The installed app opens offline too: the service worker serves each tab's address once it has stored it, so the
/// frame asks for every one while online (sw-route.js keeps an /app page's own copy only).
fn warm_offline_copies() {
    let navigator = window().navigator();
    let workers = js_sys::Reflect::get(&navigator, &"serviceWorker".into())
        .unwrap_or(wasm_bindgen::JsValue::UNDEFINED);
    if workers.is_undefined() {
        return;
    }
    if let Ok(register) = js_sys::Reflect::get(&workers, &"register".into())
        && let Some(register) = register.dyn_ref::<js_sys::Function>()
    {
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
    let warm = Closure::once_into_js(move || {
        if !window().navigator().on_line() {
            return;
        }
        // The start page too: the installed app opens at `/`, which sends this device on to its tab.
        let _ = window().fetch_with_str("/");
        for page in Page::ALL {
            let _ = window().fetch_with_str(page.path());
        }
        // The page modules that tabs load on first use (Home and Accounts, Analytics) with everything they import:
        // the worker stores them as they arrive, so an offline start on any tab finds its module. Loading one only
        // evaluates it; nothing is installed or read until its tab is shown (index.html's loader keeps the one load).
        // A failure only costs the offline copy: the loader handles it and the tab asks again when it is shown.
        for loader in WARMED_MODULES {
            if let Some(f) = js_sys::Reflect::get(&window(), &(*loader).into())
                .ok()
                .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
            {
                let _ = f.call0(&wasm_bindgen::JsValue::NULL);
            }
        }
    });
    let _ =
        window().set_timeout_with_callback_and_timeout_and_arguments_0(warm.unchecked_ref(), 4000);
}

/// The build a page is: the id in its runtime's address (`/pkg/<id>/aac_web.js`, index.html's {{PKG}}).
pub fn build_id(html_or_href: &str) -> Option<String> {
    let at = html_or_href.find("/pkg/")? + 5;
    let rest = &html_or_href[at..];
    let end = rest.find('/')?;
    let id = &rest[..end];
    (!id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    .then(|| id.to_string())
}

/// The runtime files a served build needs to start: the worker caches them cache-first, so fetching them warms the
/// cache beside the page the check just stored (review M1: a cached page without its runtime shows "This page could
/// not start").
pub(crate) fn runtime_paths(served: &str) -> [String; 2] {
    [
        format!("/pkg/{served}/aac_web.js"),
        format!("/pkg/{served}/aac_web_bg.wasm"),
    ]
}

/// Fetches a served build's runtime so the worker stores it behind the cached page.
fn warm_runtime(served: &str) {
    for path in runtime_paths(served) {
        let _ = window().fetch_with_str(&path);
    }
}

/// Whether the check may offer a served build: only when it differs from the one "Later" dismissed
/// (review L6: the 10-minute poll must not bring the same notice back).
pub(crate) fn should_offer(served: &str, dismissed: &str) -> bool {
    served != dismissed
}

/// What the service worker can do for a newer build the server has (`worker_state`). The installed app's worker
/// (public/sw.js) stays in charge while any page is open and answers a plain reload with its own build, so a reload
/// reaches a newer build only through a worker that finished installing it (2026-11-19: Reload kept bringing the
/// notice back).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkerState {
    /// No worker serves this page (none registered, or no worker support): a reload reaches the server.
    Unmanaged,
    /// A new worker finished installing its whole copy and waits: Reload hands over to it (`hand_over`).
    Waiting,
    /// The worker in charge changed while this page ran (another window handed over): a reload reaches it.
    Replaced,
    /// A new worker is still downloading its copy.
    Installing,
    /// Only the worker that started this page: nothing newer is installed yet.
    Current,
}

impl WorkerState {
    /// Whether a reload now reaches a newer build, so the frame may offer "New version · Reload".
    pub(crate) fn ready(self) -> bool {
        matches!(
            self,
            WorkerState::Unmanaged | WorkerState::Waiting | WorkerState::Replaced
        )
    }
}

/// The worker state from what the registration shows: a waiting worker first (handing over to it is what Reload
/// does), then a worker already replaced while this page ran, then one still installing.
pub(crate) fn worker_state(
    registered: bool,
    waiting: bool,
    replaced: bool,
    installing: bool,
) -> WorkerState {
    if !registered {
        WorkerState::Unmanaged
    } else if waiting {
        WorkerState::Waiting
    } else if replaced {
        WorkerState::Replaced
    } else if installing {
        WorkerState::Installing
    } else {
        WorkerState::Current
    }
}

/// How long the frame waits for a new worker to finish installing (Reload, or a check that found a newer build).
const INSTALL_WAIT_MS: u32 = 30_000;
/// How long Reload waits for the waiting worker to take over before reloading anyway.
const TAKEOVER_WAIT_MS: u32 = 10_000;

thread_local! {
    /// `controllerchange` events seen since this page started (`watch_workers`): Reload waits for the next one.
    static CONTROLLER_CHANGES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// The worker in charge was replaced by a newer one while this page ran (`watch_workers`).
    static REPLACED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// A Reload's hand-over is under way: a second press, or a resumed check, waits for it.
    static HANDING_OVER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn workers() -> Option<web_sys::ServiceWorkerContainer> {
    js_sys::Reflect::get(&window().navigator(), &"serviceWorker".into())
        .ok()
        .and_then(|w| w.dyn_into::<web_sys::ServiceWorkerContainer>().ok())
}

async fn registration() -> Option<web_sys::ServiceWorkerRegistration> {
    let container = workers()?;
    wasm_bindgen_futures::JsFuture::from(container.get_registration())
        .await
        .ok()
        .and_then(|r| r.dyn_into::<web_sys::ServiceWorkerRegistration>().ok())
}

fn state_of(registration: Option<&web_sys::ServiceWorkerRegistration>) -> WorkerState {
    match registration {
        None => worker_state(false, false, false, false),
        Some(r) => worker_state(
            true,
            r.waiting().is_some(),
            REPLACED.with(|c| c.get()),
            r.installing().is_some(),
        ),
    }
}

async fn sleep(ms: u32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// The worker state once a newer build had its chance to install: with nothing newer installing, the frame asks the
/// browser to look for a new worker (an installed app that is only resumed may not look by itself), then waits up to
/// `wait_ms` for one to finish installing. Never activates anything.
async fn prepare(wait_ms: u32) -> (WorkerState, Option<web_sys::ServiceWorkerRegistration>) {
    let registration = registration().await;
    let state = state_of(registration.as_ref());
    let Some(r) = registration else {
        return (state, None);
    };
    if state.ready() {
        return (state, Some(r));
    }
    if state == WorkerState::Current
        && let Ok(promise) = r.update()
    {
        // A failed look (offline, a server error) leaves the state as it is.
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    }
    let mut waited = 0;
    loop {
        let state = state_of(Some(&r));
        if state.ready() || waited >= wait_ms {
            return (state, Some(r));
        }
        sleep(250).await;
        waited += 250;
    }
}

/// Reload (the notice's button, and a resumed app with nothing being typed): moves to the server's build without
/// breaking the worker's promises. A waiting worker has its whole copy installed: it is asked to take over (sw.js
/// `activate-now`; its activation drops the older caches and claims this page), and the page reloads once it has.
/// With none waiting yet the frame lets a new one install first (`prepare`). If none installs, the page still reloads
/// as asked; the next check offers the notice again only once a worker is waiting.
async fn hand_over() {
    if HANDING_OVER.with(|h| h.replace(true)) {
        return;
    }
    let (state, registration) = prepare(INSTALL_WAIT_MS).await;
    let waiting = registration
        .as_ref()
        .and_then(|r| r.waiting())
        .filter(|_| state == WorkerState::Waiting);
    if let Some(waiting) = waiting {
        let seen = CONTROLLER_CHANGES.with(|c| c.get());
        let message = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&message, &"type".into(), &"activate-now".into());
        if waiting.post_message(&message).is_ok() {
            let mut waited = 0;
            while CONTROLLER_CHANGES.with(|c| c.get()) == seen && waited < TAKEOVER_WAIT_MS {
                sleep(100).await;
                waited += 100;
            }
        }
    }
    let _ = window().location().reload();
}

/// Counts `controllerchange` (`WorkerState::Replaced`). A worker that another window handed over to serves this
/// page's next reload, so the frame checks again and offers the notice here too. A page that started without a
/// worker (its first visit) is claimed once by the worker that installed this same build: that first change is not
/// a newer build.
fn watch_workers(on_change: std::rc::Rc<dyn Fn(bool)>) {
    let Some(container) = workers() else { return };
    let first_claim = std::cell::Cell::new(container.controller().is_none());
    let changed = Closure::<dyn Fn()>::new(move || {
        CONTROLLER_CHANGES.with(|c| c.set(c.get() + 1));
        if first_claim.replace(false) {
            return;
        }
        REPLACED.with(|r| r.set(true));
        if !HANDING_OVER.with(|h| h.get()) {
            on_change(false);
        }
    });
    let _ = container
        .add_event_listener_with_callback("controllerchange", changed.as_ref().unchecked_ref());
    changed.forget();
}

/// An installed app that iOS resumes keeps the build it started with, for days. So whenever the app comes back on
/// screen, and every 10 minutes while it is open, the frame asks the server for its page (no cache) and compares the
/// build. A newer build is acted on only once a reload can reach it (`WorkerState::ready`: its worker installed);
/// until then the frame lets the worker install and says nothing. Coming back on screen with nothing being typed, it
/// hands over at once (`hand_over`); otherwise it offers "New version · Reload", so a note being written is never lost.
fn watch_build(ctx: FrameCtx) {
    let running = document()
        .query_selector("link[rel=\"modulepreload\"]")
        .ok()
        .flatten()
        .and_then(|l| l.get_attribute("href"))
        .and_then(|href| build_id(&href));
    let Some(running) = running else { return };
    let check = std::rc::Rc::new(move |resumed: bool| {
        if !window().navigator().on_line() {
            return;
        }
        let running = running.clone();
        leptos::task::spawn_local(async move {
            let init = web_sys::RequestInit::new();
            init.set_cache(web_sys::RequestCache::NoStore);
            init.set_credentials(web_sys::RequestCredentials::SameOrigin);
            let path = window()
                .location()
                .pathname()
                .unwrap_or_else(|_| "/app/home".into());
            let Ok(response) = wasm_bindgen_futures::JsFuture::from(
                window().fetch_with_str_and_init(&path, &init),
            )
            .await
            .and_then(|r| r.dyn_into::<web_sys::Response>()) else {
                return;
            };
            if !response.ok() {
                return;
            }
            let Some(text) = response
                .text()
                .ok()
                .map(wasm_bindgen_futures::JsFuture::from)
            else {
                return;
            };
            let Some(html) = text.await.ok().and_then(|t| t.as_string()) else {
                return;
            };
            match build_id(&html) {
                Some(served) if served != running => {
                    let dismissed = ctx.dismissed_build.with_untracked(|d| d.clone());
                    if !should_offer(&served, &dismissed) {
                        return;
                    }
                    // Only a build a reload can reach (2026-11-19): a plain reload under the old worker answers
                    // with the old build, so the notice would come back after every Reload.
                    let (state, _) = prepare(INSTALL_WAIT_MS).await;
                    if !state.ready() || HANDING_OVER.with(|h| h.get()) {
                        return;
                    }
                    if resumed && !crate::ui::device::typing() {
                        hand_over().await;
                    } else {
                        // Store the runtime behind the page the worker just stored, so an
                        // offline start finds it too (review M1).
                        warm_runtime(&served);
                        ctx.new_build.set(served);
                    }
                }
                _ => {}
            }
        });
    });
    {
        let check = check.clone();
        let on_show = Closure::<dyn Fn()>::new(move || {
            if document().visibility_state() == web_sys::VisibilityState::Visible {
                check(true);
            }
        });
        let _ = document()
            .add_event_listener_with_callback("visibilitychange", on_show.as_ref().unchecked_ref());
        on_show.forget();
    }
    watch_workers(check.clone());
    let poll_check = check.clone();
    let poll = Closure::<dyn Fn()>::new(move || {
        if document().visibility_state() == web_sys::VisibilityState::Visible {
            poll_check(false);
        }
    });
    let _ = window().set_interval_with_callback_and_timeout_and_arguments_0(
        poll.as_ref().unchecked_ref(),
        600_000,
    );
    poll.forget();
    {
        // Once shortly after start (review M1): a slow start served from the old build still
        // holds its HTML in the cache, so check it now and warm the served runtime too.
        let check = check.clone();
        let start = Closure::once_into_js(move || check(false));
        let _ = window()
            .set_timeout_with_callback_and_timeout_and_arguments_0(start.unchecked_ref(), 4000);
    }
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
    // The dashboard's chrome line stands for its two tabs (Analytics words its own, `AnalyticsRoute`).
    let line = Memo::new(move |_| {
        ctx.dash
            .chrome
            .with(|j| serde_json::from_str::<analytics::feed::ChromeLine>(j).unwrap_or_default())
    });
    let dash_info = ChromeInfo {
        updated: Signal::derive(move || line.with(|l| l.status_strong.clone())),
        live: Signal::derive(move || line.with(|l| l.status_more == "live")),
        summary: Signal::derive(Vec::new),
        strip: None,
        refresh: Callback::new(move |done| ctx.dash.command_then("refresh", Some(done))),
        lead: Some(Signal::derive(move || {
            let lead = line.with(|l| l.status_lead.clone());
            if lead.is_empty() {
                "Updated".into()
            } else {
                lead
            }
        })),
    };
    for page in [Page::Home, Page::Accounts] {
        ctx.register(page, dash_info);
    }
    reopen_last_page(ctx, view_signal);
    follow_boot(ctx, view_signal);
    follow_history(ctx);
    warm_offline_copies();
    watch_build(ctx);
    let close_menu = window_event_listener(leptos::ev::click, move |_| ctx.menu_open.set(false));
    on_cleanup(move || close_menu.remove());
    let signed_in = Memo::new(move |_| view_signal.with(|v: &SessionView| v.signed_in));
    // Signed out, a refresh still running is not this session's: the next sign-in starts with Refresh at rest.
    Effect::new(move |_| {
        if !signed_in.get() {
            ctx.refreshing.update(|r| r.cancel());
        }
    });
    // While the session is still asking the server, the splash is drawn, never the sign-in form (owner report
    // 2026-11-20). index.html's own splash covers that time; this one only stands in for a page that has none.
    let checking = Memo::new(move |_| view_signal.with(|v: &SessionView| v.state == "loading"));
    let signed_out = move || {
        if checking.get() {
            (!crate::ui::splash::boot_shown())
                .then(|| view! { <Splash/> })
                .into_any()
        } else {
            view! { <session::SignIn view=view_signal/> }.into_any()
        }
    };
    view! {
        <Show when=move || signed_in.get() fallback=move || view! { {signed_out} }>
            <FrameShell ctx=ctx>
                <HomeRoute ctx=ctx/>
                <StatusRoute ctx=ctx/>
                <ScoreboardRoute ctx=ctx/>
                <AnalyticsRoute ctx=ctx/>
                <AccountsRoute ctx=ctx/>
                <DashHost ctx=ctx/>
                <details::DetailsOverlay ctx=ctx/>
            </FrameShell>
            <crate::media::ViewerOverlay/>
        </Show>
        <Toasts ctx=ctx/>
        <Show when=move || !ctx.new_build.get().is_empty()>
            <div class="new-build" role="status">
                <span>"New version"</span>
                <button class="btn primary sm" on:click=move |_| leptos::task::spawn_local(hand_over())>"Reload"</button>
                <button class="icon-btn bare" aria-label="Later" on:click=move |_| {
                    ctx.dismissed_build.set(ctx.new_build.get_untracked());
                    ctx.new_build.set(String::new());
                }><Icon name="close"/></button>
            </div>
        </Show>
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

/// Analytics, drawn here from its own reads (analytics/feed.rs, public/frame-analytics.mjs): built the first time its
/// tab is shown, then kept. It never loads the dashboard; its reads and timers run only while its tab is shown.
#[component]
fn AnalyticsRoute(ctx: FrameCtx) -> impl IntoView {
    let seen = Memo::new(move |prev: Option<&bool>| {
        prev.copied().unwrap_or(false) || ctx.route.get() == Page::Analytics
    });
    view! {
        <div class="route" hidden=move || ctx.route.get() != Page::Analytics>
            {move || seen.get().then(|| {
                let shown = Signal::derive(move || ctx.route.get() == Page::Analytics);
                // A read found the session gone: the frame's sign-in says why (revoked, or worked out from when the
                // session was last seen, as for Status).
                let feed = analytics::feed::start(shown, |reason: String| {
                    session::dashboard_signed_out("expired", (!reason.is_empty()).then_some(reason.as_str()));
                });
                provide_context(feed);
                let line = feed.chrome_line();
                let id = ctx.register(Page::Analytics, ChromeInfo {
                    updated: Signal::derive(move || line.with(|l| l.status_strong.clone())),
                    live: Signal::derive(move || line.with(|l| l.status_more == "live")),
                    summary: Signal::derive(Vec::new),
                    strip: None,
                    refresh: Callback::new(move |done| {
                        feed.refresh_then(done);
                    }),
                    lead: Some(Signal::derive(move || {
                        let lead = line.with(|l| l.status_lead.clone());
                        if lead.is_empty() { "Updated".into() } else { lead }
                    })),
                });
                on_cleanup(move || ctx.unregister(Page::Analytics, id));
                view! { <analytics::AnalyticsPage/> }
            })}
        </div>
    }
}

/// The dashboard controller (dash.rs, `public/frame-dashboard.mjs`): loaded the first time a tab that takes its data is
/// shown (Home, Accounts) and kept for the signed-in frame. Nothing is drawn here.
#[component]
fn DashHost(ctx: FrameCtx) -> impl IntoView {
    let seen = Memo::new(move |prev: Option<&bool>| {
        prev.copied().unwrap_or(false) || ctx.route.get().reads_dashboard()
    });
    move || {
        seen.get().then(|| {
            let route = Signal::derive(move || ctx.route.get());
            dash::start(ctx.dash, route, move |note| match note {
                dash::Note::Toast(kind, title, body, ms) => ctx.toast(&kind, &title, &body, ms),
                // The session ended under the page, or Sign out finished: the frame's sign-in says so.
                dash::Note::SignedOut(state, reason) => {
                    session::dashboard_signed_out(&state, reason.as_deref())
                }
            });
        })
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
            strip: None,
            refresh: Callback::new(|_| {}),
            lead: None,
        }
    }

    #[test]
    fn a_fresh_sign_in_on_home_reopens_the_last_tab() {
        assert_eq!(
            reopen_target(true, Page::Home, Some("analytics")),
            Some(Page::Analytics)
        );
        assert_eq!(
            reopen_target(true, Page::Home, Some("scoreboard")),
            Some(Page::Scoreboard)
        );
        // nothing stored, Home itself, or something that is not a tab: Home stays
        assert_eq!(reopen_target(true, Page::Home, None), None);
        assert_eq!(reopen_target(true, Page::Home, Some("home")), None);
        assert_eq!(reopen_target(true, Page::Home, Some("settings")), None);
        // a session the browser already had, or an address that named another tab, is kept
        assert_eq!(reopen_target(false, Page::Home, Some("analytics")), None);
        assert_eq!(reopen_target(true, Page::Status, Some("analytics")), None);
    }

    #[test]
    fn a_signed_in_frame_remembers_the_tab_it_lands_on() {
        // a tab opened directly is remembered, as a clicked one is (review L2)
        assert_eq!(
            landing_page(false, Page::Accounts, Some("home")),
            Page::Accounts
        );
        assert_eq!(landing_page(false, Page::Status, None), Page::Status);
        // a fresh sign-in on Home lands on the remembered tab, which stays remembered
        assert_eq!(
            landing_page(true, Page::Home, Some("accounts")),
            Page::Accounts
        );
        // a session the browser already had shows the address's tab, Home included
        assert_eq!(
            landing_page(false, Page::Home, Some("accounts")),
            Page::Home
        );
        assert_eq!(landing_page(true, Page::Home, None), Page::Home);
    }

    #[test]
    fn later_remembers_the_dismissed_build() {
        // The same build never offers again; only a different one does (review L6).
        assert!(should_offer("newbuild01", ""));
        assert!(!should_offer("newbuild01", "newbuild01"));
        assert!(should_offer("newer00001", "newbuild01"));
    }

    #[test]
    fn the_notice_waits_for_a_worker_that_can_serve_the_new_build() {
        // 2026-11-19: Reload under the old worker got the old build back, so the notice returned forever.
        // A reload reaches a newer build only with no worker, a waiting (fully installed) one, or one that
        // already took over while this page ran.
        assert_eq!(
            worker_state(false, false, false, false),
            WorkerState::Unmanaged
        );
        assert_eq!(worker_state(true, true, false, false), WorkerState::Waiting);
        assert_eq!(worker_state(true, true, true, true), WorkerState::Waiting);
        assert_eq!(worker_state(true, false, true, true), WorkerState::Replaced);
        assert_eq!(
            worker_state(true, false, false, true),
            WorkerState::Installing
        );
        assert_eq!(
            worker_state(true, false, false, false),
            WorkerState::Current
        );
        assert!(WorkerState::Unmanaged.ready());
        assert!(WorkerState::Waiting.ready());
        assert!(WorkerState::Replaced.ready());
        // still downloading, or nothing newer installed: no notice, no automatic reload
        assert!(!WorkerState::Installing.ready());
        assert!(!WorkerState::Current.ready());
    }

    #[test]
    fn warming_a_new_build_caches_its_runtime() {
        assert_eq!(
            runtime_paths("deadbeefcafe"),
            [
                "/pkg/deadbeefcafe/aac_web.js".to_string(),
                "/pkg/deadbeefcafe/aac_web_bg.wasm".to_string()
            ]
        );
    }

    #[test]
    fn the_build_is_read_from_the_runtime_address() {
        assert_eq!(
            build_id("/pkg/5b71f6820ff9/aac_web.js").as_deref(),
            Some("5b71f6820ff9")
        );
        let page = r#"<link rel="modulepreload" href="/pkg/c965f1a909c2/aac_web.js" />"#;
        assert_eq!(build_id(page).as_deref(), Some("c965f1a909c2"));
        assert_eq!(build_id("/app/home"), None);
        assert_eq!(build_id("/pkg//aac_web.js"), None);
        assert_eq!(build_id("/pkg/a\"b/x"), None);
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
