//! The page's own chrome, drawn only when it is opened standalone (`/app/status`, no `?embed=1`): a header, the phone's
//! top and tab bars, the Duo's status-strip rail, or its status-band head. Inside the Slint shell's iframe the shell
//! draws the chrome and this draws just the content. Which one shows is `html[data-chrome]` (device.rs).

use super::icons::{Icon, Logo};
use super::parts::runs_view;
use crate::api::Run;
use leptos::prelude::*;

/// What the chrome says about the page: its age and health, and a one-line summary for the status band.
#[derive(Clone, Copy)]
pub struct ChromeInfo {
    /// "1m ago"; empty until the first reading.
    pub updated: Signal<String>,
    /// False while the last read failed.
    pub live: Signal<bool>,
    pub summary: Signal<Vec<Run>>,
    pub refresh: Callback<()>,
}

const NAV: [(&str, &str, &str); 5] = [
    ("home", "Home", "/"),
    ("chart", "Analytics", "/analytics"),
    ("users", "Accounts", "/accounts"),
    ("activity", "Status", "/app/status"),
    ("trophy", "Scoreboard", "/app/scoreboard"),
];

fn nav_links(current: &'static str, with_icon_labels: bool) -> impl IntoView + use<> {
    let _ = with_icon_labels;
    NAV.into_iter()
        .map(|(icon, label, href)| {
            view! {
                <a href=href class:on=label == current aria-current=(label == current).then_some("page")>
                    <Icon name=icon/>{label}
                </a>
            }
        })
        .collect_view()
}

/// The theme switch for a page opened on its own (the shell drives the theme inside its iframe). Shares the shell's
/// `aac-theme` setting.
fn cycle_theme() -> &'static str {
    let storage = window().local_storage().ok().flatten();
    let now = storage
        .as_ref()
        .and_then(|s| s.get_item("aac-theme").ok().flatten())
        .unwrap_or_else(|| "auto".into());
    let next = match now.as_str() {
        "auto" => "light",
        "light" => "dark",
        _ => "auto",
    };
    if let Some(s) = storage.as_ref() {
        let _ = s.set_item("aac-theme", next);
    }
    if let Some(root) = document().document_element() {
        if next == "auto" {
            let _ = root.remove_attribute("data-theme");
        } else {
            let _ = root.set_attribute("data-theme", next);
        }
    }
    next
}

/// Applies the saved theme to a page opened on its own.
pub fn restore_theme() {
    let saved = window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|s| s.get_item("aac-theme").ok().flatten());
    if let (Some(theme), Some(root)) = (
        saved.as_deref().filter(|t| matches!(*t, "light" | "dark")),
        document().document_element(),
    ) {
        let _ = root.set_attribute("data-theme", theme);
    }
}

fn status_words(info: ChromeInfo) -> impl IntoView {
    view! {
        <span class="status-line">
            {move || {
                let updated = info.updated.get();
                if updated.is_empty() {
                    view! { "Reading..." }.into_any()
                } else {
                    view! { "Updated "<b>{updated}</b>{if info.live.get() { " · live" } else { " · old reading" }} }.into_any()
                }
            }}
        </span>
    }
}

#[component]
fn IconButton(icon: &'static str, label: &'static str, on_click: Callback<()>) -> impl IntoView {
    view! {
        <button class="icon-btn" aria-label=label title=label on:click=move |_| on_click.run(())><Icon name=icon/></button>
    }
}

/// Refresh, Update apps and account, as the phone bar, rail and band show them.
fn tools(info: ChromeInfo) -> impl IntoView {
    let refresh = info.refresh;
    let open_home = Callback::new(|_| {
        let _ = window().location().set_href("/");
    });
    let open_accounts = Callback::new(|_| {
        let _ = window().location().set_href("/accounts");
    });
    view! {
        <IconButton icon="refresh" label="Refresh" on_click=refresh/>
        <IconButton icon="download" label="Update apps" on_click=open_home/>
        <IconButton icon="user" label="Account" on_click=open_accounts/>
    }
}

/// `page` is the nav label ("Status", "Scoreboard"); `title` is the page's name in the band ("Model Scoreboard").
#[component]
pub fn Shell(
    embedded: bool,
    page: &'static str,
    title: &'static str,
    info: ChromeInfo,
    children: Children,
) -> impl IntoView {
    if embedded {
        return view! { <div class="shell"><main class="content">{children()}</main></div> }
            .into_any();
    }
    restore_theme();
    let theme = RwSignal::new("monitor");
    let refresh = info.refresh;
    view! {
        <div class="shell">
            <header class="chrome">
                <div class="chrome-in">
                    <a class="brand" href="/"><Logo/><span class="brand-name">"AI Account Center"</span></a>
                    <nav class="nav">{nav_links(page, false)}</nav>
                    <div class="chrome-right">
                        {status_words(info)}
                        <IconButton icon="refresh" label="Refresh" on_click=refresh/>
                        <a class="upd-btn" href="/"><Icon name="download"/>"Update apps"</a>
                        <span class="chrome-sep"></span>
                        <button class="select-btn" aria-label="Theme"
                            on:click=move |_| theme.set(match cycle_theme() { "light" => "sun", "dark" => "moon", _ => "monitor" })>
                            {move || view! { <Icon name=theme.get()/> }}<Icon name="chev-down" class="chev"/>
                        </button>
                        <a class="select-btn" aria-label="Account" href="/accounts"><Icon name="user"/><Icon name="chev-down" class="chev"/></a>
                    </div>
                </div>
            </header>
            <header class="topbar">
                <div class="topbar-in">
                    <Logo/>
                    <div><div class="t-title">{page}</div><div class="t-sub">{status_words(info)}</div></div>
                    <div class="tools">{tools(info)}</div>
                </div>
            </header>
            <header class="band-head">
                <div class="band-info">
                    <div class="bi1"><b>{title}</b>" · "{move || {
                        let updated = info.updated.get();
                        if updated.is_empty() { "reading".to_string() } else { format!("updated {updated}{}", if info.live.get() { " · live" } else { " · old reading" }) }
                    }}</div>
                    <div class="bi2">{move || runs_view(info.summary.get())}</div>
                </div>
                <div class="band-tabs">
                    <nav class="band-nav">{nav_links(page, true)}</nav>
                    <div class="band-tools">{tools(info)}</div>
                </div>
            </header>
            <main class="content paper-grid">{children()}</main>
            <nav class="tabbar">{nav_links(page, true)}</nav>
            <nav class="rail">
                {nav_links(page, true)}
                <div class="rail-tools">{tools(info)}</div>
            </nav>
        </div>
    }
    .into_any()
}
