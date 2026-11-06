//! Questions for the owner, answered as a deck: one card at a time with Back and Next, a row of card labels to jump,
//! and nothing sent until the review screen ("Send current selections" jumps there early). Only the owner answers;
//! the server refuses an answer to a card that changed after it was shown.

use crate::api::*;
use crate::blocks::Blocks;
use crate::drafts::{self, Drafts, Standing};
use crate::fmt;
use crate::live::Live;
use crate::media::MediaList;
use crate::status::Panel;
use leptos::prelude::*;
use leptos::task::spawn_local;

fn urgency_label(u: &str) -> &'static str {
    match u {
        "now" => "Now",
        "today" => "Today",
        "this-week" => "This week",
        _ => "Whenever",
    }
}

#[derive(Clone, Debug, PartialEq)]
struct SentSummary {
    at: String,
    recorded: usize,
    askers: Vec<String>,
    changed: Vec<String>,
    refused: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
enum DeckView {
    Cards,
    Review,
    Sent(SentSummary),
}

/// The open cards without their ages, so a refresh that changes nothing else does not redraw the card being read.
fn steady(cards: &[Card]) -> Vec<Card> {
    cards
        .iter()
        .cloned()
        .map(|mut c| {
            c.age_seconds = None;
            c.asker_state = None;
            c
        })
        .collect()
}

#[component]
pub fn CardsPanel() -> impl IntoView {
    let live = expect_context::<Live>();
    let source = Signal::derive(move || {
        let view = live.view.get()?;
        view.sources
            .iter()
            .find(|s| s.id == view.cards.source)
            .cloned()
    });
    let open = Memo::new(move |_| {
        live.view.with(|v| {
            v.as_ref()
                .map(|v| steady(&v.cards.open))
                .unwrap_or_default()
        })
    });
    let answered = move || {
        live.view.with(|v| {
            v.as_ref()
                .map(|v| v.cards.answered.clone())
                .unwrap_or_default()
        })
    };
    let drafts = RwSignal::new(drafts::load());
    let at = RwSignal::new(0usize);
    let deck_view = RwSignal::new(DeckView::Cards);
    let show_answered = RwSignal::new(false);

    // Keep drafts on this device; drop drafts of cards that closed (only once a real view is in).
    Effect::new(move |_| drafts.with(drafts::save));
    Effect::new(move |_| {
        let cards = open.get();
        if live.view.with_untracked(|v| v.is_some()) {
            drafts.update(|d| drafts::prune(d, &cards));
        }
        if at.get_untracked() >= cards.len() {
            at.set(cards.len().saturating_sub(1));
        }
    });

    view! {
        <Panel title="Questions for you" source=source id="cards">
            <Show
                when=move || !open.with(|o| o.is_empty()) || matches!(deck_view.get(), DeckView::Sent(_))
                fallback=|| view! { <p class="empty">"No questions waiting."</p> }
            >
                <Deck open=open drafts=drafts at=at deck_view=deck_view/>
            </Show>
            <Show when=move || !answered().is_empty()>
                <button class="link" aria-expanded=move || show_answered.get().to_string() on:click=move |_| show_answered.update(|s| *s = !*s)>
                    {move || format!("{} answered ({})", if show_answered.get() { "Hide" } else { "Show" }, answered().len())}
                </button>
                <Show when=move || show_answered.get()>
                    <ul class="answered">
                        {move || answered().into_iter().map(|a| view! { <AnsweredView answered=a/> }).collect_view()}
                    </ul>
                </Show>
            </Show>
        </Panel>
    }
}

#[component]
fn Deck(
    open: Memo<Vec<Card>>,
    drafts: RwSignal<Drafts>,
    at: RwSignal<usize>,
    deck_view: RwSignal<DeckView>,
) -> impl IntoView {
    // After the new card or the review screen is in place, bring the deck's top back into view when the owner had
    // scrolled past it (a long card). A deck already in view does not move. A plain browser timer on purpose:
    // Leptos 0.9 cancels its own `set_timeout` when the owner that started it is cleaned up, and this click redraws
    // the very branch that owns it (see NOTES.md).
    let to_top = move || {
        let callback = wasm_bindgen::closure::Closure::once_into_js(move || {
            if let Some(el) = document().query_selector(".deck").ok().flatten() {
                if el.get_bounding_client_rect().top() < 0.0 {
                    el.scroll_into_view();
                }
            }
        });
        let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(
            wasm_bindgen::JsCast::unchecked_ref(&callback),
            30,
        );
    };
    let go_card = move |i: usize| {
        at.set(i);
        deck_view.set(DeckView::Cards);
        to_top();
    };
    let go_review = move || {
        deck_view.set(DeckView::Review);
        to_top();
    };
    let ready = move || {
        open.with(|cards| {
            drafts.with(|d| {
                cards
                    .iter()
                    .filter(|c| drafts::standing(d, c) == Standing::Ready)
                    .count()
            })
        })
    };
    let current = Memo::new(move |_| open.with(|cards| cards.get(at.get()).cloned()));

    view! {
        <div class="deck">
            <div class="deck-sub">
                <span>{move || format!("{} open · {} answered", open.with(|o| o.len()), ready())}</span>
                <Show when=move || deck_view.get() == DeckView::Cards>
                    <button class="link" on:click=move |_| { open.with_untracked(|cards| drafts.update(|d| { drafts::use_recommendations(d, cards); })); }>
                        "Use recommendations"
                    </button>
                </Show>
            </div>
            <Show when=move || !matches!(deck_view.get(), DeckView::Sent(_))>
                <div class="jump" role="tablist" aria-label="Questions">
                    {move || open.get().into_iter().enumerate().map(|(i, card)| {
                        let label = card.short();
                        let state = move || drafts.with(|d| match drafts::standing(d, &card) {
                            Standing::Ready => "done",
                            Standing::Changed | Standing::NeedsWords => "changed",
                            Standing::Empty => "",
                        });
                        view! {
                            <button
                                role="tab"
                                data-state=state
                                aria-selected=move || (deck_view.get() == DeckView::Cards && at.get() == i).to_string()
                                on:click=move |_| go_card(i)
                            >{label}</button>
                        }
                    }).collect_view()}
                    <button role="tab" aria-selected=move || (deck_view.get() == DeckView::Review).to_string() on:click=move |_| go_review()>"Review"</button>
                </div>
            </Show>
            {move || match deck_view.get() {
                DeckView::Cards => view! {
                    {move || current.get().map(|card| view! { <CardBody card=card drafts=drafts/> })}
                    <Show when=move || { ready() > 0 && at.get() + 1 < open.with(|o| o.len()) }>
                        <button class="link early" on:click=move |_| go_review()>{move || format!("Send current selections ({})", ready())}</button>
                    </Show>
                    <p class="fine">"Nothing is sent until you review and send."</p>
                    <div class="deck-nav">
                        <button class="secondary" disabled=move || at.get() == 0 on:click=move |_| go_card(at.get_untracked().saturating_sub(1))>"‹ Back"</button>
                        <span class="count">{move || format!("{} of {}", at.get() + 1, open.with(|o| o.len()))}</span>
                        <button class="primary" on:click=move |_| {
                            let last = at.get_untracked() + 1 >= open.with_untracked(|o| o.len());
                            if last { go_review() } else { go_card(at.get_untracked() + 1) }
                        }>
                            {move || if at.get() + 1 >= open.with(|o| o.len()) { "Review" } else { "Next ›" }}
                        </button>
                    </div>
                }.into_any(),
                DeckView::Review => view! { <Review open=open drafts=drafts deck_view=deck_view go_card=go_card/> }.into_any(),
                DeckView::Sent(summary) => view! { <Sent summary=summary deck_view=deck_view at=at/> }.into_any(),
            }}
        </div>
    }
}

#[component]
fn CardBody(card: Card, drafts: RwSignal<Drafts>) -> impl IntoView {
    let card = StoredValue::new(card);
    let c = card.get_value();
    let recommended = c
        .recommendation
        .as_ref()
        .map(|r| r.key.clone())
        .unwrap_or_default();
    let initial_note =
        drafts.with_untracked(|d| d.get(&c.id).map(|x| x.note.clone()).unwrap_or_default());
    let place = [
        Some(c.project.clone()),
        c.stream.clone().map(|s| format!("stream {s}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let standing = move || drafts.with(|d| drafts::standing(d, &card.get_value()));
    let chosen = move |key: &str| {
        drafts.with(|d| {
            d.get(&card.get_value().id)
                .is_some_and(|x| x.choice.iter().any(|k| k == key))
        })
    };
    let kind_word = match c.kind.as_str() {
        "action" => Some("Only you can do this"),
        "objection" => Some("Already decided · object if you disagree"),
        _ => None,
    };
    let mut facts: Vec<(&'static str, String)> = Vec::new();
    facts.push((
        "Holding up",
        c.blocks
            .clone()
            .unwrap_or_else(|| "Nothing waits on this".into()),
    ));
    if let Some(n) = c.needed_by.clone() {
        let when = n.at.as_deref().map(fmt::clock).unwrap_or_default();
        let what = n.if_no_answer.unwrap_or_default();
        let words = [when, what]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        if !words.is_empty() {
            facts.push(("Needed by", words));
        }
    }
    if let Some(r) = c.reversible.clone() {
        let words = match (r.yes, r.how) {
            (true, Some(how)) if !how.is_empty() => format!("Yes: {how}"),
            (true, _) => "Yes".to_string(),
            (false, Some(how)) if !how.is_empty() => format!("No: {how}"),
            (false, _) => "No".to_string(),
        };
        facts.push(("Reversible", words));
    }
    let note_hint = match c.kind.as_str() {
        "objection" => "Say why you object, or leave a note. It goes to the agent word for word.",
        "action" => "Note for the agent (optional). It goes word for word.",
        _ => "Note, a different answer or a question. It goes to the agent word for word.",
    };

    view! {
        <article class="card" data-urgency=c.urgency.clone() data-kind=c.kind.clone()>
            {c.exception.clone().map(|e| view! {
                <p class="exception"><strong>{format!("Exception to your rule: {}", e.rule)}</strong>{(!e.text.is_empty()).then(|| format!(". {}", e.text))}</p>
            })}
            <div class="card-meta">
                <span class="card-label">{c.short()}</span>
                <span class="chip" data-urgency=c.urgency.clone()>{urgency_label(&c.urgency)}</span>
                <span>{place}</span>
                <span class="asked">{format!("{} · {}", c.asked_by, fmt::clock(&c.asked_at))}</span>
            </div>
            {kind_word.map(|w| view! { <p class="kind-word">{w}</p> })}
            <h3 class="card-title">{c.title.clone()}</h3>
            <p class="now">
                {c.now.clone()}
                {c.now_as_of.as_deref().map(|t| view! { <span class="asof">{format!(" As of {}.", fmt::clock(t))}</span> })}
            </p>
            {(c.old_answer.is_some() || c.what_changed.is_some()).then(|| view! {
                <div class="old">
                    {c.old_answer.clone().map(|o| view! {
                        <p><strong>"Your earlier answer: "</strong>{o.text}{o.at.as_deref().map(|t| format!(" ({})", fmt::clock(t)))}</p>
                    })}
                    {c.what_changed.clone().map(|w| view! { <p><strong>"What changed: "</strong>{w}</p> })}
                </div>
            })}
            <dl class="card-facts">
                {facts.into_iter().map(|(k, v)| view! { <div><dt>{k}</dt><dd>{v}</dd></div> }).collect_view()}
            </dl>
            <Blocks blocks=c.sections.clone()/>
            <Show when=move || standing() == Standing::Changed>
                <p class="item-note" data-tone="warn">"This question changed after you chose. Look again; your choice is kept but not sent until you touch it."</p>
            </Show>

            {match c.kind.as_str() {
                "action" => view! {
                    <button class="toggle" aria-pressed=move || chosen("done").to_string()
                        on:click=move |_| drafts.update(|d| drafts::toggle(d, &card.get_value(), "done"))>
                        {move || if chosen("done") { "Marked done (sent when you review)" } else { "Mark done" }}
                    </button>
                }.into_any(),
                "objection" => view! {
                    <button class="toggle" data-tone="warn" aria-pressed=move || chosen("object").to_string()
                        on:click=move |_| drafts.update(|d| drafts::toggle(d, &card.get_value(), "object"))>
                        {move || if chosen("object") { "You object: say why below" } else { "Object" }}
                    </button>
                    <Show when=move || standing() == Standing::NeedsWords>
                        <p class="item-note" data-tone="warn">"An objection needs a reason before it can be sent."</p>
                    </Show>
                }.into_any(),
                _ => view! {
                    <div class="options" role=if c.multiple { "group" } else { "radiogroup" }>
                        {c.options.clone().into_iter().map(|o| {
                            let key = o.key.clone();
                            let key_state = key.clone();
                            let key_click = key.clone();
                            let is_rec = key == recommended;
                            view! {
                                <div class="option" data-checked=move || chosen(&key_state).then_some("1")>
                                    <button
                                        class="opt-pick"
                                        role=if c.multiple { "checkbox" } else { "radio" }
                                        aria-checked={let k = key.clone(); move || chosen(&k).to_string()}
                                        on:click=move |_| drafts.update(|d| drafts::toggle(d, &card.get_value(), &key_click))
                                    >
                                        <span class="opt-top">
                                            <span class="opt-key">{key.clone()}</span>
                                            {is_rec.then(|| view! { <span class="rec-tag">"Recommended"</span> })}
                                        </span>
                                        <span class="opt-label">{o.label.clone()}</span>
                                        {(!o.gets.is_empty()).then(|| view! { <span class="gc"><strong>"Gets "</strong>{o.gets.clone()}</span> })}
                                        {(!o.costs.is_empty()).then(|| view! { <span class="gc"><strong>"Costs "</strong>{o.costs.clone()}</span> })}
                                    </button>
                                    <MediaList items=o.media.clone()/>
                                    <Blocks blocks=o.sections.clone()/>
                                </div>
                            }
                        }).collect_view()}
                    </div>
                    {c.recommendation.clone().map(|r| view! {
                        <div class="why">
                            <p><strong>{format!("Why {}: ", r.key)}</strong>{r.why}
                                {r.confidence.filter(|x| !x.is_empty()).map(|x| view! { <span class="muted">{format!(" Confidence: {x}.")}</span> })}
                            </p>
                            {r.flip.filter(|x| !x.is_empty()).map(|f| view! { <p class="muted"><strong>"What would change it: "</strong>{f}</p> })}
                        </div>
                    })}
                }.into_any(),
            }}

            {(!c.evidence.is_empty()).then(|| view! {
                <details class="evidence">
                    <summary>"Evidence"</summary>
                    <ul>{c.evidence.iter().map(|e| view! { <li><span class="muted">{format!("{}: ", e.label)}</span><code>{e.path.clone()}</code></li> }).collect_view()}</ul>
                </details>
            })}
            {(!c.notes.is_empty() || !c.replies.is_empty()).then(|| view! {
                <ul class="talk">
                    {c.notes.iter().map(|n| view! {
                        <li class="mine"><span class="who">{format!("You · {}", n.at.as_deref().map(fmt::clock).unwrap_or_default())}</span>{n.note.clone()}</li>
                    }).collect_view()}
                    {c.replies.iter().map(|r| view! {
                        <li class="theirs"><span class="who">{format!("{} · {}", c.asked_by, r.at.as_deref().map(fmt::clock).unwrap_or_default())}</span>{r.text.clone()}</li>
                    }).collect_view()}
                </ul>
            })}
            <label class="note">
                <span class="sr">"Note to the agent"</span>
                <textarea
                    rows="2"
                    maxlength="4000"
                    placeholder=note_hint
                    prop:value=initial_note
                    on:input=move |ev| { let text = event_target_value(&ev); drafts.update(|d| drafts::set_note(d, &card.get_value(), text)); }
                ></textarea>
            </label>
        </article>
    }
}

#[component]
fn Review(
    open: Memo<Vec<Card>>,
    drafts: RwSignal<Drafts>,
    deck_view: RwSignal<DeckView>,
    go_card: impl Fn(usize) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let live = expect_context::<Live>();
    let sending = RwSignal::new(false);
    let problem = RwSignal::new(None::<String>);
    let count = move || open.with(|cards| drafts.with(|d| drafts::entries(d, cards).len()));

    let send = move || {
        let cards = open.get_untracked();
        let entries = drafts.with_untracked(|d| drafts::entries(d, &cards));
        if entries.is_empty() {
            return;
        }
        sending.set(true);
        problem.set(None);
        spawn_local(async move {
            match post_answers(&entries).await {
                SendOutcome::Results(results) => {
                    let label_of = |id: &str| {
                        cards
                            .iter()
                            .find(|c| c.id == id)
                            .map(Card::short)
                            .unwrap_or_else(|| id.to_string())
                    };
                    let mut summary = SentSummary {
                        at: fmt::clock(&String::from(js_sys::Date::new_0().to_iso_string())),
                        recorded: 0,
                        askers: vec![],
                        changed: vec![],
                        refused: vec![],
                    };
                    for r in &results {
                        match r.outcome.as_str() {
                            "recorded" => {
                                summary.recorded += 1;
                                if let Some(c) = cards.iter().find(|c| c.id == r.card_id) {
                                    if !summary.askers.contains(&c.asked_by) {
                                        summary.askers.push(c.asked_by.clone());
                                    }
                                }
                                drafts.update(|d| {
                                    d.remove(&r.card_id);
                                });
                            }
                            "card_changed" => summary.changed.push(label_of(&r.card_id)),
                            _ => summary.refused.push(format!(
                                "{}: {}",
                                label_of(&r.card_id),
                                if r.error.is_empty() {
                                    "refused"
                                } else {
                                    &r.error
                                }
                            )),
                        }
                    }
                    sending.set(false);
                    deck_view.set(DeckView::Sent(summary));
                    live.reload();
                }
                SendOutcome::Failed(words) => {
                    sending.set(false);
                    problem.set(Some(words));
                }
            }
        });
    };

    view! {
        <div class="review">
            <ul class="review-rows">
                {move || open.get().into_iter().enumerate().map(|(i, c)| {
                    let (what, tone) = drafts.with(|d| {
                        let draft = d.get(&c.id).cloned().unwrap_or_default();
                        match drafts::standing(d, &c) {
                            Standing::Changed => ("Changed since you chose · look again".to_string(), "warn"),
                            Standing::NeedsWords => ("Objection without a reason · add one to send it".to_string(), "warn"),
                            Standing::Empty if c.kind == "objection" => ("No objection · nothing is sent".to_string(), "muted"),
                            Standing::Empty if c.kind == "action" => ("Not done yet · stays open".to_string(), "muted"),
                            Standing::Empty => ("Not answered · stays open for later".to_string(), "muted"),
                            Standing::Ready if draft.choice.is_empty() => ("Note only · the question stays open".to_string(), "note"),
                            Standing::Ready if c.kind == "action" => ("Done".to_string(), "good"),
                            Standing::Ready if c.kind == "objection" => ("You object".to_string(), "warn"),
                            Standing::Ready => {
                                let labels = draft.choice.iter().map(|k| {
                                    let label = c.options.iter().find(|o| &o.key == k).map(|o| o.label.clone()).unwrap_or_default();
                                    format!("{k} · {label}")
                                }).collect::<Vec<_>>().join(", ");
                                (if draft.via == "recommended" { format!("{labels} (recommended)") } else { labels }, "good")
                            }
                        }
                    });
                    let note = drafts.with(|d| d.get(&c.id).map(|x| x.note.trim().to_string()).unwrap_or_default());
                    view! {
                        <li>
                            <button class="review-row" on:click=move |_| go_card(i)>
                                <span class="card-label">{c.short()}</span>
                                <span class="review-title">{c.title.clone()}</span>
                                <span class="review-what" data-tone=tone>{what}</span>
                                {(!note.is_empty()).then(|| view! { <span class="review-note">{format!("\u{201c}{note}\u{201d}")}</span> })}
                            </button>
                        </li>
                    }
                }).collect_view()}
            </ul>
            <div class="review-actions">
                <button class="primary" disabled=move || count() == 0 || sending.get() on:click=move |_| send()>
                    {move || match (sending.get(), count()) {
                        (true, _) => "Sending...".to_string(),
                        (false, 0) => "Nothing to send yet".to_string(),
                        (false, 1) => "Send 1 answer".to_string(),
                        (false, n) => format!("Send {n} answers"),
                    }}
                </button>
                <button class="secondary" on:click=move |_| deck_view.set(DeckView::Cards)>"Back to the cards"</button>
            </div>
            {move || problem.get().map(|p| view! { <p class="item-note" data-tone="crit" role="alert">{p}</p> })}
            <p class="fine">
                {move || {
                    let left = open.with(|o| o.len()).saturating_sub(count());
                    let stays = match left { 0 => String::new(), 1 => "1 question stays open for later. ".into(), n => format!("{n} questions stay open for later. ") };
                    format!("{stays}Tap a row to change it. When you send, AAC records your exact choices, your notes word for word and the time, signs them, and tells each agent that asked.")
                }}
            </p>
        </div>
    }
}

#[component]
fn Sent(summary: SentSummary, deck_view: RwSignal<DeckView>, at: RwSignal<usize>) -> impl IntoView {
    let told = match summary.askers.len() {
        0 => String::new(),
        1 => format!(" {} will be told.", summary.askers[0]),
        n => format!(
            " {} and {} will be told.",
            summary.askers[..n - 1].join(", "),
            summary.askers[n - 1]
        ),
    };
    view! {
        <div class="sent">
            <p class="ok" data-tone="good">{format!("Sent {} at {}.{told}", summary.recorded, summary.at)}</p>
            {(!summary.changed.is_empty()).then(|| view! {
                <p class="item-note" data-tone="warn">{format!("Not sent, the question changed after you opened it: {}. Your choice is kept; look again.", summary.changed.join(", "))}</p>
            })}
            {(!summary.refused.is_empty()).then(|| view! {
                <ul class="item-note" data-tone="crit">{summary.refused.iter().map(|r| view! { <li>{r.clone()}</li> }).collect_view()}</ul>
            })}
            <button class="secondary" on:click=move |_| { at.set(0); deck_view.set(DeckView::Cards); }>"Back to the questions"</button>
        </div>
    }
}

#[component]
fn AnsweredView(answered: AnsweredCard) -> impl IntoView {
    let a = answered.answer;
    let c = answered.card;
    let choice = a.choice.clone().unwrap_or_default();
    let labels = choice
        .iter()
        .map(|k| {
            let label = c
                .options
                .iter()
                .find(|o| &o.key == k)
                .map(|o| o.label.clone())
                .unwrap_or_default();
            format!("{k} · {label}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let delivery = match a.delivery.state.as_str() {
        "rang" => "the agent was told",
        "failed" => "could not tell the agent",
        _ => "telling the agent",
    };
    view! {
        <li class="answered-row" data-delivery=a.delivery.state.clone()>
            <div class="card-title-sm"><span class="card-label">{c.short()}</span>" "{c.title.clone()}</div>
            <div>{if labels.is_empty() { "Note only".to_string() } else { labels }}{(a.via == "recommended").then_some(" (recommended)")}</div>
            {a.note.clone().filter(|n| !n.is_empty()).map(|n| view! { <blockquote>{n}</blockquote> })}
            <div class="muted">{format!("{} · {} · {delivery}", c.asked_by, fmt::clock(&a.answered_at))}</div>
            {(a.delivery.state == "failed").then(|| view! { <div class="item-note" data-tone="warn">{a.delivery.detail.clone()}</div> })}
        </li>
    }
}
