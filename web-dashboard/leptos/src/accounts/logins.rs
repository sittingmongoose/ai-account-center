//! Logins (Central Logins step 1): one read-only list of every account x computer x tool with its kind, end,
//! renewer and health, from `GET /api/accounts/logins` (`aac_views::logins`). The server decides every word; this
//! only lays it out. Nothing here signs in, renews or changes anything, and the view's type has no field for a secret.
use crate::api::{FetchError, answer_error, send};
use aac_api::logins::{LOGINS_VIEW_VERSION, LoginRow, LoginsView};
use leptos::prelude::*;

pub(crate) const PATH: &str = "/api/accounts/logins";

/// Read the view once.
pub(crate) async fn fetch() -> Result<LoginsView, FetchError> {
    let (status, text) = send("GET", PATH, None).await?;
    if status == 401 {
        return Err(FetchError::SignedOut);
    }
    if status != 200 {
        return Err(answer_error(status, &text));
    }
    parse(&text)
}

pub(crate) fn parse(text: &str) -> Result<LoginsView, FetchError> {
    let view: LoginsView = serde_json::from_str(text).map_err(|_| FetchError::Malformed)?;
    if view.version != LOGINS_VIEW_VERSION {
        return Err(FetchError::Version(view.version));
    }
    Ok(view)
}

/// The head's meta line: "31 logins · 2 need you · 1 ends soon".
pub(crate) fn meta(view: &LoginsView) -> String {
    let c = &view.counts;
    let mut parts = vec![format!(
        "{} login{}",
        c.total,
        if c.total == 1 { "" } else { "s" }
    )];
    if c.needs_you > 0 {
        parts.push(format!(
            "{} need{} you",
            c.needs_you,
            if c.needs_you == 1 { "s" } else { "" }
        ));
    }
    if c.expiring > 0 {
        parts.push(format!(
            "{} end{} soon",
            c.expiring,
            if c.expiring == 1 { "s" } else { "" }
        ));
    }
    parts.join(" · ")
}

/// The health word's class: state is colour and weight on the words, never a tinted box.
fn health_class(health: &str) -> &'static str {
    match health {
        "needs_you" | "ended" => "lg-h crit",
        "expiring" => "lg-h warn",
        "ok" => "lg-h good",
        _ => "lg-h",
    }
}

fn row_view(row: LoginRow) -> impl IntoView {
    let ends_tip = row.ends_at.clone().unwrap_or_default();
    let last = (!row.last_renewal.is_empty()).then(|| format!("Last renewed {}", row.last_renewal));
    view! {
        <div class="lg-row" role="row" data-provider=row.provider.clone()>
            <span class="lg-acct" role="cell" title=row.account.clone()>{row.account.clone()}</span>
            <span class="lg-comp" role="cell">{row.computer.clone()}</span>
            <span class="lg-tool" role="cell" title=row.tool.clone()>{row.tool.clone()}</span>
            <span class="lg-kind" role="cell">{row.kind_label.clone()}</span>
            <span class="lg-ends" role="cell" title=ends_tip>{row.ends_text.clone()}</span>
            <span class="lg-renew" role="cell" title=last.unwrap_or_default()>{row.renewer.clone()}</span>
            <span class=health_class(&row.health) role="cell" title=row.health_text.clone()>{row.health_text.clone()}</span>
        </div>
    }
}

/// The Logins plate, full width under the Accounts & Settings cards. It reads when its tab is shown.
#[component]
pub(crate) fn LoginsPlate(ctx: crate::frame::FrameCtx) -> impl IntoView {
    let state = RwSignal::new(None::<Result<LoginsView, FetchError>>);
    let busy = RwSignal::new(false);
    let read = move || {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        leptos::task::spawn_local(async move {
            let result = fetch().await;
            let _ = busy.try_set(false);
            let _ = state.try_set(Some(result));
        });
    };
    Effect::new(move |_| {
        if ctx.route.get() == crate::Page::Accounts {
            read();
        }
    });
    let meta = Signal::derive(move || {
        state.with(|s| match s {
            Some(Ok(view)) => meta(view),
            _ => String::new(),
        })
    });
    view! {
        <section class="plate lg-plate" aria-label="Logins" data-card="logins">
            <super::CardHead title="Logins" meta=meta/>
            <div class="plate-body">
                {move || state.with(|s| match s {
                    None => view! { <p class="ac-note">"Reading logins…"</p> }.into_any(),
                    Some(Err(e)) => view! { <p class="ac-note crit">{format!("Logins could not be read. {}", e.sentence())}</p> }.into_any(),
                    Some(Ok(view)) => {
                        let notes = view.notes.clone();
                        let rows = view.rows.clone();
                        let empty = rows.is_empty();
                        view! {
                            <p class="ac-note lg-lead">"Where each sign-in and key lives, when it ends and who renews it. Read-only; no secret is shown or sent."</p>
                            {notes.into_iter().map(|n| view! { <p class="ac-note lg-note">{n}</p> }).collect_view()}
                            {if empty {
                                view! { <p class="ac-note">"No logins are known yet."</p> }.into_any()
                            } else {
                                view! {
                                    <div class="lg-rows" role="table" aria-label="Logins">
                                        <div class="lg-row lg-headrow" role="row">
                                            <span role="columnheader">"Account"</span>
                                            <span role="columnheader">"Computer"</span>
                                            <span role="columnheader">"Tool"</span>
                                            <span role="columnheader">"Kind"</span>
                                            <span role="columnheader">"Ends"</span>
                                            <span role="columnheader">"Renewed by"</span>
                                            <span role="columnheader">"Health"</span>
                                        </div>
                                        {rows.into_iter().map(row_view).collect_view()}
                                    </div>
                                }.into_any()
                            }}
                        }.into_any()
                    }
                })}
            </div>
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_meta_line_counts_only_what_is_there() {
        let mut view = LoginsView {
            version: 1,
            ..Default::default()
        };
        view.counts.total = 1;
        assert_eq!(meta(&view), "1 login");
        view.counts.total = 31;
        view.counts.needs_you = 2;
        view.counts.expiring = 1;
        assert_eq!(meta(&view), "31 logins · 2 need you · 1 ends soon");
    }

    #[test]
    fn another_version_is_refused_not_half_drawn() {
        assert!(matches!(
            parse(r#"{"version":2,"rows":[]}"#),
            Err(FetchError::Version(2))
        ));
        assert!(parse(r#"{"version":1,"rows":[{"id":"a","account":null}]}"#).is_ok());
    }

    #[test]
    fn the_device_fixture_reads() {
        let view = parse(include_str!("../../fixtures/logins-view.json")).expect("test value");
        assert_eq!(view.rows.len() as u32, view.counts.total);
        assert_eq!(meta(&view), "15 logins · 3 need you · 1 ends soon");
    }
}
