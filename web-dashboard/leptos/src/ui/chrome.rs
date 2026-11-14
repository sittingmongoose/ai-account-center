//! The app frame's chrome (PLAN step 4): the desktop header, the phone's top and tab bars, the Duo's status-strip rail,
//! or its status-band head with a tab row. Which one shows is `html[data-chrome]` (device.rs). Inside today's Slint
//! shell (`?embed=1`) the shell draws the chrome and a page is the content only (`PageShell`).

use super::controls::Spinner;
use super::icons::{Icon, Logo};
use super::parts::runs_view;
use crate::Page;
use crate::api::{Run, Strip};
use crate::frame::FrameCtx;
use leptos::prelude::*;

/// What the chrome says about the page on screen: its age and health, and a one-line summary for the status band.
#[derive(Clone, Copy)]
pub struct ChromeInfo {
    /// "1m ago"; empty until the first reading.
    pub updated: Signal<String>,
    /// False while the last read failed (or the readings are cached).
    pub live: Signal<bool>,
    pub summary: Signal<Vec<Run>>,
    /// The page's summary counts, drawn in the status band in place of `summary` when the page has them (Status).
    pub strip: Option<Signal<Option<Strip>>>,
    pub refresh: Callback<()>,
    /// The status line's first word when it isn't "Updated" ("Refreshing", "Account data unavailable").
    pub lead: Option<Signal<String>>,
}

/// The theme setting the frame and the dashboard share (`aac-theme`: auto, light or dark). A device that last chose
/// one in a build before `aac-theme` kept only light or dark under `ccs-slint-theme`, which still counts.
pub fn stored_theme() -> String {
    let storage = window().local_storage().ok().flatten();
    let read = |key: &str| {
        storage
            .as_ref()
            .and_then(|s| s.get_item(key).ok().flatten())
    };
    theme_from(
        read("aac-theme").as_deref(),
        read("ccs-slint-theme").as_deref(),
    )
}

/// The theme from the stored setting, or the older builds' light or dark when there is none.
pub(crate) fn theme_from(current: Option<&str>, legacy: Option<&str>) -> String {
    match current {
        Some(t @ ("light" | "dark")) => t.into(),
        Some(_) => "auto".into(),
        None => legacy
            .filter(|t| matches!(*t, "light" | "dark"))
            .unwrap_or("auto")
            .into(),
    }
}

/// The header's Update apps, from the dashboard's `update` view (view-model.mjs `updateViewModel`): while a job runs
/// it counts the results ("Updating 3 of 21") beside a spinner, it shows a check for 2 s when the job ends, and its tip
/// says what the last run did. Empty (the plain button) until the dashboard controller has loaded.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct UpdatePill {
    pub running: bool,
    pub done: bool,
    pub count: u32,
    pub total: u32,
    pub tip: String,
}

impl UpdatePill {
    fn read(json: &str) -> Self {
        serde_json::from_str(json).unwrap_or_default()
    }

    pub(crate) fn label(&self) -> String {
        if self.running && self.total > 0 {
            format!("Updating {} of {}", self.count, self.total)
        } else if self.running {
            format!("Updating {}", self.count)
        } else if self.done {
            "Apps updated".into()
        } else {
            "Update apps".into()
        }
    }

    pub(crate) fn icon(&self) -> &'static str {
        if self.done && !self.running {
            "check"
        } else {
            "download"
        }
    }

    /// The tip: the view's own sentence, or the label while there is none.
    pub(crate) fn tip(&self) -> String {
        if self.tip.is_empty() {
            self.label()
        } else {
            self.tip.clone()
        }
    }
}

/// The Update apps button's icon: a spinner while a job runs, a check just after it ends, else the arrow.
fn update_icon(pill: Memo<UpdatePill>) -> impl IntoView {
    move || {
        if pill.with(|p| p.running) {
            view! { <Spinner/> }.into_any()
        } else {
            view! { <Icon name=pill.with(|p| p.icon())/> }.into_any()
        }
    }
}

fn update_pill(ctx: FrameCtx) -> Memo<UpdatePill> {
    Memo::new(move |_| ctx.dash.update.with(|j| UpdatePill::read(j)))
}

/// Saves and applies a theme; the frame tells the dashboard's iframe too.
pub fn apply_theme(mode: &str) {
    if let Some(s) = window().local_storage().ok().flatten() {
        let _ = s.set_item("aac-theme", mode);
    }
    crate::set_theme(mode);
}

#[component]
fn IconButton(icon: &'static str, label: &'static str, on_click: Callback<()>) -> impl IntoView {
    view! {
        <button class="icon-btn" aria-label=label title=label on:click=move |_| on_click.run(())><Icon name=icon/></button>
    }
}

fn status_words(ctx: FrameCtx) -> impl IntoView {
    view! {
        <span class="status-line">
            {move || {
                let Some(info) = ctx.info() else { return view! { "Reading..." }.into_any() };
                let updated = info.updated.get();
                let lead = info.lead.map(|l| l.get()).unwrap_or_else(|| "Updated".into());
                if updated.is_empty() {
                    view! { {if lead == "Updated" { "Reading...".to_string() } else { lead }} }.into_any()
                } else {
                    view! { {lead}" "<b>{updated}</b>{if info.live.get() { " · live" } else { " · old reading" }} }.into_any()
                }
            }}
        </span>
    }
}

/// The five tabs as links: a plain click changes the tab in place; a modified click opens the page's own address.
fn tab_links(ctx: FrameCtx) -> impl IntoView {
    Page::ALL
        .into_iter()
        .map(|page| {
            view! {
                <a href=page.path() class:on=move || ctx.route.get() == page
                    aria-current=move || (ctx.route.get() == page).then_some("page")
                    on:click=move |e: web_sys::MouseEvent| {
                        if e.button() == 0 && !e.meta_key() && !e.ctrl_key() && !e.shift_key() && !e.alt_key() {
                            e.prevent_default();
                            ctx.go(page);
                        }
                    }>
                    <Icon name=page.icon()/>{page.label()}
                </a>
            }
        })
        .collect_view()
}

/// Refresh, Update apps and the account menu, as the phone bar, rail and band show them.
fn tools(ctx: FrameCtx) -> impl IntoView {
    let pill = update_pill(ctx);
    view! {
        <IconButton icon="refresh" label="Refresh" on_click=Callback::new(move |_| ctx.refresh())/>
        <button class="icon-btn" class:upd-run=move || pill.with(|p| p.running)
            aria-label=move || pill.with(|p| p.label()) title=move || pill.with(|p| p.tip())
            on:click=move |_| ctx.update_apps()>{update_icon(pill)}</button>
        <button class="icon-btn" aria-label="Account" title="Account" aria-haspopup="menu"
            aria-expanded=move || ctx.menu_open.get().to_string()
            on:click=move |e: web_sys::MouseEvent| { e.stop_propagation(); ctx.menu_open.update(|o| *o = !*o); }>
            <Icon name="user"/>
        </button>
    }
}

fn theme_icon(mode: &str) -> &'static str {
    match mode {
        "light" => "sun",
        "dark" => "moon",
        _ => "monitor",
    }
}

/// Whether Escape closes the account menu: only for the Escape key while the menu is open (review N1).
pub(crate) fn menu_escapes(key: &str, open: bool) -> bool {
    key == "Escape" && open
}

/// The account menu: who is signed in, the theme, the way back to the classic app, and Sign out.
#[component]
fn AccountMenu(ctx: FrameCtx) -> impl IntoView {
    let theme = ctx.theme;
    let pick = move |mode: &'static str| {
        ctx.set_theme(mode);
    };
    // Escape closes the menu, like the other overlays (review N1).
    let esc = window_event_listener(leptos::ev::keydown, move |e| {
        if menu_escapes(&e.key(), ctx.menu_open.get_untracked()) {
            ctx.menu_open.set(false);
        }
    });
    on_cleanup(move || esc.remove());
    view! {
        <Show when=move || ctx.menu_open.get()>
            <div class="menu-scrim" on:click=move |_| ctx.menu_open.set(false)></div>
            <div class="acct-menu" role="menu" on:click=|e: web_sys::MouseEvent| e.stop_propagation()>
                <p class="menu-head">{move || {
                    let name = ctx.username.get();
                    if name.is_empty() { "Signed in".to_string() } else { format!("Signed in as {name}") }
                }}</p>
                <div class="menu-row">
                    <span class="q3">"Theme"</span>
                    <div class="seg">
                        {["auto", "light", "dark"].into_iter().map(|mode| view! {
                            <button class:on=move || theme.get() == mode aria-pressed=move || (theme.get() == mode).to_string() on:click=move |_| pick(mode)>
                                <Icon name=theme_icon(mode)/>{match mode { "light" => "Light", "dark" => "Dark", _ => "Auto" }}
                            </button>
                        }).collect_view()}
                    </div>
                </div>
                <button class="menu-item" role="menuitem" on:click=move |_| { ctx.menu_open.set(false); ctx.go(Page::Accounts); }>"Accounts & Settings"</button>
                <button class="menu-item" role="menuitem" on:click=move |_| ctx.classic()>"Classic app"<span class="q3">" · the previous look"</span></button>
                <button class="menu-item" role="menuitem" on:click=move |_| { let _ = window().location().reload(); }>"Reload app"<span class="q3">" · get the newest version"</span></button>
                <button class="menu-item" role="menuitem" on:click=move |_| {
                    ctx.menu_open.set(false);
                    let _ = window().alert_with_message(&super::device::device_report());
                }>"Device info"<span class="q3">" · for layout reports"</span></button>
                <button class="menu-item" role="menuitem" on:click=move |_| { ctx.menu_open.set(false); ctx.sign_out(); }>"Sign out"</button>
            </div>
        </Show>
    }
}

/// The frame: chrome around the current tab's content. `children` holds every tab's content; only the current one
/// shows (frame.rs).
#[component]
pub fn FrameShell(ctx: FrameCtx, children: Children) -> impl IntoView {
    let theme = ctx.theme;
    let cycle = move |_| {
        let next = match theme.get_untracked().as_str() {
            "auto" => "light",
            "light" => "dark",
            _ => "auto",
        };
        ctx.set_theme(next);
    };
    let pill = update_pill(ctx);
    view! {
        <div class="shell">
            <header class="chrome">
                <div class="chrome-in">
                    <a class="brand" href=Page::Home.path() on:click=move |e: web_sys::MouseEvent| { e.prevent_default(); ctx.go(Page::Home); }>
                        <Logo/><span class="brand-name">"AI Account Center"</span>
                    </a>
                    <nav class="nav">{tab_links(ctx)}</nav>
                    <div class="chrome-right">
                        {status_words(ctx)}
                        <IconButton icon="refresh" label="Refresh" on_click=Callback::new(move |_| ctx.refresh())/>
                        <button class="upd-btn" class:upd-run=move || pill.with(|p| p.running) title=move || pill.with(|p| p.tip())
                            on:click=move |_| ctx.update_apps()>{update_icon(pill)}<span class="tnum">{move || pill.with(|p| p.label())}</span></button>
                        <span class="chrome-sep"></span>
                        <button class="select-btn" aria-label="Theme" on:click=cycle>
                            {move || view! { <Icon name=theme_icon(&theme.get())/> }}<Icon name="chev-down" class="chev"/>
                        </button>
                        <button class="select-btn" aria-label="Account" aria-haspopup="menu"
                            on:click=move |e: web_sys::MouseEvent| { e.stop_propagation(); ctx.menu_open.update(|o| *o = !*o); }>
                            <Icon name="user"/><Icon name="chev-down" class="chev"/>
                        </button>
                    </div>
                </div>
            </header>
            <header class="topbar">
                <div class="topbar-in">
                    <Logo/>
                    <div><div class="t-title">{move || ctx.route.get().label()}</div><div class="t-sub">{status_words(ctx)}</div></div>
                    <div class="tools">{tools(ctx)}</div>
                </div>
            </header>
            <header class="band-head">
                // The band takes no taps (DUO-MEASURED.md): it shows the page line and a summary only.
                <div class="band-info">
                    <div class="bi1"><b>{move || ctx.route.get().title()}</b>" · "{move || {
                        let Some(info) = ctx.info() else { return "reading".to_string() };
                        let updated = info.updated.get();
                        if updated.is_empty() { "reading".to_string() } else { format!("updated {updated}{}", if info.live.get() { " · live" } else { " · old reading" }) }
                    }}</div>
                    {move || {
                        let info = ctx.info()?;
                        Some(match info.strip.and_then(|s| s.get()) {
                            Some(strip) => view! { <div class="bi2 bs">{crate::summary::band_line(&strip)}</div> }.into_any(),
                            None => view! { <div class="bi2">{runs_view(info.summary.get())}</div> }.into_any(),
                        })
                    }}
                </div>
                <div class="band-tabs">
                    <nav class="band-nav">{tab_links(ctx)}</nav>
                    <div class="band-tools">{tools(ctx)}</div>
                </div>
            </header>
            <main class="content paper-grid">{children()}</main>
            <nav class="tabbar">{tab_links(ctx)}</nav>
            <nav class="rail">
                {tab_links(ctx)}
                <div class="rail-tools">{tools(ctx)}</div>
            </nav>
            <AccountMenu ctx=ctx/>
        </div>
    }
}

/// A page's own wrapper. Inside today's Slint shell (`embedded`) it is the content box; in the app frame the page
/// tells the frame what its chrome should say and draws its content only.
#[component]
pub fn PageShell(
    page: Page,
    embedded: bool,
    info: ChromeInfo,
    children: Children,
) -> impl IntoView {
    if embedded {
        return view! { <div class="shell"><main class="content">{children()}</main></div> }
            .into_any();
    }
    if let Some(ctx) = use_context::<FrameCtx>() {
        let id = ctx.register(page, info);
        on_cleanup(move || ctx.unregister(page, id));
    }
    children().into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_theme_reads_the_older_builds_key_only_without_a_current_one() {
        assert_eq!(theme_from(Some("dark"), Some("light")), "dark");
        assert_eq!(theme_from(Some("auto"), Some("dark")), "auto");
        assert_eq!(theme_from(Some("bogus"), Some("dark")), "auto");
        assert_eq!(theme_from(None, Some("dark")), "dark");
        assert_eq!(theme_from(None, Some("light")), "light");
        assert_eq!(theme_from(None, Some("auto")), "auto");
        assert_eq!(theme_from(None, None), "auto");
    }

    #[test]
    fn update_apps_says_how_far_a_job_is_and_when_it_ended() {
        let idle = UpdatePill::read("");
        assert_eq!(
            (idle.label().as_str(), idle.icon(), idle.tip().as_str()),
            ("Update apps", "download", "Update apps")
        );
        let running = UpdatePill::read(
            r#"{"running":true,"done":false,"count":3,"total":21,"tip":"Updating apps on Mac"}"#,
        );
        assert_eq!(running.label(), "Updating 3 of 21");
        assert_eq!(running.tip(), "Updating apps on Mac");
        let no_total = UpdatePill::read(r#"{"running":true,"count":4,"total":0}"#);
        assert_eq!(no_total.label(), "Updating 4");
        let done = UpdatePill::read(
            r#"{"running":false,"done":true,"count":21,"total":21,"tip":"Last run done"}"#,
        );
        assert_eq!(
            (done.label().as_str(), done.icon()),
            ("Apps updated", "check")
        );
        assert_eq!(UpdatePill::read("not json"), UpdatePill::default());
    }

    #[test]
    fn escape_closes_an_open_menu() {
        assert!(menu_escapes("Escape", true));
        assert!(!menu_escapes("Escape", false));
        assert!(!menu_escapes("Enter", true));
    }
}
