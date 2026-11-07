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

fn verdict_label(v: &str) -> String {
    match v {
        "adopt" => "Adopt".into(),
        "adapt" => "Adapt".into(),
        "keep" | "keep-ours" | "keep ours" => "Keep ours".into(),
        "reject" => "Reject".into(),
        other => {
            let mut c = other.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        }
    }
}

fn stance_label(s: &str) -> Option<&'static str> {
    match s {
        "keep" => Some("Keep ours"),
        "borrow" => Some("Borrow"),
        "own" => Some("Our own"),
        _ => None,
    }
}

/// Phones read the decision first: sections fold under the options.
fn narrow() -> bool {
    window()
        .inner_width()
        .ok()
        .and_then(|v| v.as_f64())
        .is_some_and(|w| w < 600.0)
}

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
    // While answers are on their way the deck stays on its review screen: no second send, no moving away. Both live
    // here, above the deck, so a read that empties the cards mid-send can't take the result with it.
    let sending = RwSignal::new(false);
    let problem = RwSignal::new(None::<String>);
    // a send's failure belongs to the review it came from
    Effect::new(move |_| {
        if deck_view.get() != DeckView::Review && problem.with_untracked(|p| p.is_some()) {
            problem.set(None);
        }
    });
    // An idle review whose cards all closed has given way to "No questions waiting"; a question arriving later opens
    // on the deck, not on that stale review.
    Effect::new(move |_| {
        let idle_review =
            deck_view.get() == DeckView::Review && !sending.get() && problem.with(|p| p.is_none());
        if idle_review && open.with(|o| o.is_empty()) {
            deck_view.set(DeckView::Cards);
        }
    });

    // Keep drafts on this device; drop drafts of cards that closed (see the prune below). Each save merges this tab's
    // changes since its last read of storage into what storage holds now, card by card, so another tab's work on
    // other cards survives even when both write before either hears the other.
    let base = StoredValue::new(drafts.get_untracked());
    Effect::new(move |_| {
        let current = drafts.get();
        // a refused read or write stores nothing: the base and this tab's drafts stay as they are, so the next save
        // still knows what is this tab's own (nothing is mistaken for another tab's deletion)
        if let Some(merged) = drafts::save_merged(&base.get_value(), &current) {
            base.set_value(merged.clone());
            if merged != current {
                drafts.set(merged);
            }
        }
    });
    // Another tab (or the installed app beside a browser tab) changed the drafts: take storage's copy as the new base.
    // Storage events reach only the other tabs, and an unchanged value fires none, so this can't echo.
    let other_tab = window_event_listener(leptos::ev::storage, move |e| {
        if e.key().as_deref() == Some(drafts::STORAGE_KEY) {
            // a refused read is not "no drafts": leave this tab alone until storage answers
            let Some(theirs) = drafts::load_checked() else {
                return;
            };
            let stored = drafts::merge(&theirs, &base.get_value(), &drafts.get_untracked());
            base.set_value(theirs);
            if drafts.with_untracked(|d| *d != stored) {
                drafts.set(stored);
            }
        }
    });
    on_cleanup(move || other_tab.remove());
    Effect::new(move |_| {
        let cards = open.get();
        // Only a card list the server calls complete proves a card closed: after a restart while the share fails,
        // or with one writer folder unreadable, the list is short and pruning would lose unsent work.
        let complete = live
            .view
            .with_untracked(|v| v.as_ref().is_some_and(|v| v.cards.proves_closed()));
        if complete {
            drafts.update(|d| drafts::prune(d, &cards));
        }
    });

    view! {
        <Panel title="Questions for you" source=source id="cards">
            {move || live.view.with(|v| v.as_ref().and_then(|v| v.cards.answer_history_problem.clone()))
                .map(|message| view! { <p class="warn" role="status">{message}</p> })}
            <Show
                // A send's result outlives the cards: while a send runs, after it fails and on the sent screen the deck
                // stays even when a read empties the cards under it. An idle review with no cards gives way.
                when=move || {
                    !open.with(|o| o.is_empty())
                        || matches!(deck_view.get(), DeckView::Sent(_))
                        || sending.get()
                        || problem.with(|p| p.is_some())
                }
                fallback=|| view! { <p class="empty">"No questions waiting."</p> }
            >
                <Deck open=open drafts=drafts at=at deck_view=deck_view sending=sending problem=problem/>
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
    sending: RwSignal<bool>,
    problem: RwSignal<Option<String>>,
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
    // A verdict filter (Scout's adopt / adapt / keep ours / reject) narrows the deck; the review always lists every card.
    let verdict = RwSignal::new(None::<String>);
    let verdicts = Memo::new(move |_| {
        let mut v: Vec<String> =
            open.with(|cards| cards.iter().filter_map(|c| c.verdict.clone()).collect());
        v.sort();
        v.dedup();
        v
    });
    // a filter whose last card closed shows every card again, never an empty "1 of 0" deck
    Effect::new(move |_| {
        let gone = verdict.with(|v| {
            v.as_ref()
                .is_some_and(|v| !verdicts.with(|all| all.contains(v)))
        });
        if gone {
            verdict.set(None);
        }
    });
    let shown = Memo::new(move |_| {
        let pick = verdict.get();
        open.with(|cards| {
            cards
                .iter()
                .filter(|c| pick.is_none() || c.verdict == pick)
                .cloned()
                .collect::<Vec<_>>()
        })
    });
    // The deck follows a card, not a position: when cards close or a filter changes, the owner stays on the same
    // card if it is still shown, else the nearest one.
    let current_id = RwSignal::new(None::<String>);
    Effect::new(move |_| {
        let ids: Vec<String> = shown.with(|cards| cards.iter().map(|c| c.id.clone()).collect());
        let keep = current_id
            .get_untracked()
            .and_then(|id| ids.iter().position(|x| *x == id));
        let i = keep.unwrap_or_else(|| at.get_untracked().min(ids.len().saturating_sub(1)));
        if at.get_untracked() != i {
            at.set(i);
        }
        current_id.set(ids.get(i).cloned());
    });
    let go_card = move |i: usize| {
        if sending.get_untracked() {
            return;
        }
        at.set(i);
        current_id.set(shown.with_untracked(|cards| cards.get(i).map(|c| c.id.clone())));
        deck_view.set(DeckView::Cards);
        to_top();
    };
    // From the review, a row is a card in the whole deck: drop the filter first.
    let go_open = move |i: usize| {
        if sending.get_untracked() {
            return;
        }
        verdict.set(None);
        let id = open.with_untracked(|cards| cards.get(i).map(|c| c.id.clone()));
        current_id.set(id.clone());
        let at_shown =
            id.and_then(|id| shown.with_untracked(|cards| cards.iter().position(|c| c.id == id)));
        go_card(at_shown.unwrap_or(i));
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
                    .filter(|c| c.can_answer() && drafts::standing(d, c) == Standing::Ready)
                    .count()
            })
        })
    };
    let current = Memo::new(move |_| shown.with(|cards| cards.get(at.get()).cloned()));
    let next = move || {
        let last = at.get_untracked() + 1 >= shown.with_untracked(|o| o.len());
        if last {
            go_review()
        } else {
            go_card(at.get_untracked() + 1)
        }
    };
    let back = move || {
        if at.get_untracked() > 0 {
            go_card(at.get_untracked() - 1)
        }
    };
    // Keyboard on a desktop: left and right move through the deck, 1-9 pick an option on the card shown. Typing in
    // the note field is left alone.
    let keys = window_event_listener(leptos::ev::keydown, move |e| {
        if e.ctrl_key()
            || e.meta_key()
            || e.alt_key()
            || deck_view.get_untracked() != DeckView::Cards
        {
            return;
        }
        let typing = document().active_element().is_some_and(|el| {
            matches!(el.tag_name().as_str(), "TEXTAREA" | "INPUT" | "SELECT")
                || el.closest(".viewer").ok().flatten().is_some()
        });
        if typing
            || document()
                .query_selector(".viewer")
                .ok()
                .flatten()
                .is_some()
        {
            return;
        }
        match e.key().as_str() {
            "ArrowRight" => next(),
            "ArrowLeft" => back(),
            k if k.len() == 1 && k.as_bytes()[0].is_ascii_digit() && k != "0" => {
                let index = (k.as_bytes()[0] - b'1') as usize;
                if let Some(card) = current.get_untracked() {
                    if card.can_answer()
                        && matches!(card.kind.as_str(), "" | "question" | "judgement")
                    {
                        if let Some(option) = card.options.get(index) {
                            let key = option.key.clone();
                            drafts.update(|d| drafts::toggle(d, &card, &key));
                        }
                    }
                }
            }
            _ => {}
        }
    });
    // The deck can be unmounted (no questions left); its key listener goes with it.
    on_cleanup(move || keys.remove());
    view! {
        <div class="deck">
            <div class="deck-sub">
                <span>{move || format!("{} open · {} ready to send", open.with(|o| o.len()), ready())}</span>
                <Show when=move || deck_view.get() == DeckView::Cards && open.with(|cards| cards.iter().any(|c| c.recommendation.is_some() && c.can_answer() && c.is_question()))>
                    <button class="link" on:click=move |_| { open.with_untracked(|cards| drafts.update(|d| { drafts::use_recommendations(d, cards); })); }>
                        "Use recommendations"
                    </button>
                </Show>
            </div>
            <Show when=move || deck_view.get() == DeckView::Cards && !verdicts.with(|v| v.is_empty())>
                <div class="verdicts" role="group" aria-label="Show cards by verdict">
                    <button class="chip-btn" aria-pressed=move || verdict.with(|v| v.is_none()).to_string()
                        on:click=move |_| { current_id.set(None); at.set(0); verdict.set(None); }>"All"</button>
                    {move || verdicts.get().into_iter().map(|v| {
                        let label = verdict_label(&v);
                        let pick = v.clone();
                        let counted = v.clone();
                        let count = move || open.with(|cards| cards.iter().filter(|c| c.verdict.as_deref() == Some(counted.as_str())).count());
                        view! {
                            <button class="chip-btn" data-verdict=v.clone() aria-pressed=move || verdict.with(|x| x.as_deref() == Some(pick.as_str())).to_string()
                                on:click=move |_| { current_id.set(None); at.set(0); verdict.set(Some(v.clone())); }>{move || format!("{label} ({})", count())}</button>
                        }
                    }).collect_view()}
                </div>
            </Show>
            <Show when=move || !matches!(deck_view.get(), DeckView::Sent(_))>
                <div class="jump" role="tablist" aria-label="Questions">
                    {move || shown.get().into_iter().enumerate().map(|(i, card)| {
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
                    {move || current.get().map(|card| view! { <CardBody card=card drafts=drafts open=open/> })}
                    <Show when=move || { ready() > 0 && at.get() + 1 < shown.with(|o| o.len()) }>
                        <button class="link early" on:click=move |_| go_review()>{move || format!("Send current selections ({})", ready())}</button>
                    </Show>
                    <p class="fine">"Nothing is sent until you review and send."</p>
                    <div class="deck-nav">
                        <button class="secondary" disabled=move || at.get() == 0 on:click=move |_| back()>"‹ Back"</button>
                        <span class="count">{move || format!("{} of {}", at.get() + 1, shown.with(|o| o.len()))}</span>
                        <button class="primary" on:click=move |_| next()>
                            {move || if at.get() + 1 >= shown.with(|o| o.len()) { "Review" } else { "Next ›" }}
                        </button>
                    </div>
                }.into_any(),
                DeckView::Review => view! { <Review open=open drafts=drafts deck_view=deck_view go_card=go_open sending=sending problem=problem/> }.into_any(),
                DeckView::Sent(summary) => view! { <Sent summary=summary back=move || go_card(0)/> }.into_any(),
            }}
        </div>
    }
}

#[component]
fn CardBody(card: Card, drafts: RwSignal<Drafts>, open: Memo<Vec<Card>>) -> impl IntoView {
    let card = StoredValue::new(card);
    let c = card.get_value();
    // A judgement card is the owner's own call: no recommendation is shown, so nothing anchors it.
    let recommendation = c.recommendation.clone().filter(|_| !c.is_judgement());
    let recommended = recommendation
        .as_ref()
        .map(|r| r.key.clone())
        .unwrap_or_default();
    let group = c.group.clone();
    let group_size = group.as_ref().map_or(0, |g| {
        open.with_untracked(|cards| cards.iter().filter(|x| x.group.as_ref() == Some(g)).count())
    });
    let use_group = {
        let group = group.clone();
        move |_| {
            open.with_untracked(|cards| {
                let mine: Vec<Card> = cards.iter().filter(|x| x.group == group).cloned().collect();
                drafts.update(|d| {
                    drafts::use_recommendations(d, &mine);
                });
            })
        }
    };
    let related: Vec<String> = c
        .related
        .iter()
        .map(|id| {
            open.with_untracked(|cards| {
                cards
                    .iter()
                    .find(|x| &x.id == id)
                    .map(Card::short)
                    .unwrap_or_else(|| id.rsplit("--").next().unwrap_or(id).to_string())
            })
        })
        .collect();
    let fold_sections = narrow();
    // The box follows the draft, so a change from another tab (or a send there) shows; typing sets the same text back,
    // which leaves the cursor where it is.
    let note_text = {
        let id = c.id.clone();
        move || drafts.with(|d| d.get(&id).map(|x| x.note.clone()).unwrap_or_default())
    };
    let place = [
        Some(c.project.clone()).filter(|p| !p.is_empty()),
        c.stream.clone().map(|s| format!("stream {s}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let asker = if c.asked_by.is_empty() {
        c.writer.clone()
    } else {
        c.asked_by.clone()
    };
    // A card the server can't vouch for right now is shown but can't be answered.
    let locked = !c.can_answer();
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
        "judgement" => Some(
            "Your judgement · no recommendation is shown, and \u{201c}Can't tell\u{201d} is a fine answer",
        ),
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
    if let Some(a) = c.applies_to.clone() {
        let words = match a.count {
            Some(n) => format!("{n} cases · {}", a.description),
            None => a.description,
        };
        facts.push(("Applies to", words));
    }
    if !c.changes_canon.is_empty() {
        facts.push((
            "Changes the plan",
            c.changes_canon
                .iter()
                .map(|x| format!("{}: {}", x.reference, x.what))
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    if !related.is_empty() {
        facts.push(("Related", related.join(", ")));
    }
    let note_hint = match c.kind.as_str() {
        "objection" => "Say why you object, or leave a note. It goes to the agent word for word.",
        "action" => "Note for the agent (optional). It goes word for word.",
        _ => "Note, a different answer or a question. It goes to the agent word for word.",
    };

    view! {
        <article class="card" data-urgency=c.urgency.clone() data-kind=c.kind.clone()>
            {(c.kind == "sample").then(|| view! {
                <p class="sample-banner">"Format sample · no answer wanted. Nothing on this card is sent."</p>
            })}
            {(c.kind != "sample" && (locked || c.problem.is_some())).then(|| view! {
                <p class="item-note" data-tone="warn">
                    {if locked { "Can't be answered right now. " } else { "" }}
                    {c.problem.clone().unwrap_or_else(|| "The card could not be checked.".into())}
                    {locked.then_some(" The last good version is shown; it can be answered once the agent rewrites it.")}
                </p>
            })}
            {c.exception.clone().map(|e| view! {
                <p class="exception"><strong>{format!("Exception to your rule: {}", e.rule)}</strong>{(!e.text.is_empty()).then(|| format!(". {}", e.text))}</p>
            })}
            <div class="card-meta">
                <span class="card-label">{c.short()}</span>
                <span class="chip" data-urgency=c.urgency.clone()>{urgency_label(&c.urgency)}</span>
                {c.verdict.clone().map(|v| view! { <span class="chip" data-verdict=v.clone()>{verdict_label(&v)}</span> })}
                {group.clone().map(|g| view! { <span class="muted">{format!("group {g}")}</span> })}
                <span>{place}</span>
                <span class="asked">{[asker.clone(), fmt::clock(&c.asked_at)].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")}</span>
            </div>
            {kind_word.map(|w| view! { <p class="kind-word">{w}</p> })}
            <h3 class="card-title">{c.title.clone()}</h3>
            {(!c.now.is_empty()).then(|| view! {
                <p class="now">
                    {c.now.clone()}
                    {c.now_as_of.as_deref().map(|t| view! { <span class="asof">{format!(" As of {}.", fmt::clock(t))}</span> })}
                </p>
            })}
            {(c.old_answer.is_some() || c.what_changed.is_some()).then(|| view! {
                <div class="old">
                    {c.old_answer.clone().map(|o| view! {
                        <p><strong>"Your earlier answer: "</strong>{o.text}{o.at.as_deref().map(|t| format!(" ({})", fmt::clock(t)))}</p>
                    })}
                    {c.what_changed.clone().map(|w| view! { <p><strong>"What changed: "</strong>{w}</p> })}
                </div>
            })}
            {c.agreement.clone().and_then(|a| match a.same {
                Some(false) => Some(view! { <p class="item-note" data-tone="warn"><strong>"The judges split. "</strong>{a.note.unwrap_or_default()}</p> }.into_any()),
                Some(true) => Some(view! { <p class="item-note muted">{format!("The judges agreed.{}", a.note.map(|n| format!(" {n}")).unwrap_or_default())}</p> }.into_any()),
                None => a.note.map(|n| view! { <p class="item-note muted">{n}</p> }.into_any()),
            })}
            {c.doubt.clone().filter(|d| !d.is_empty()).map(|d| view! { <p class="item-note muted"><strong>"Unsure: "</strong>{d}</p> })}
            <dl class="card-facts">
                {facts.into_iter().map(|(k, v)| view! { <div><dt>{k}</dt><dd>{v}</dd></div> }).collect_view()}
            </dl>
            {(!fold_sections).then(|| view! { <Blocks blocks=c.sections.clone()/> })}
            <Show when=move || standing() == Standing::Changed>
                <p class="item-note" data-tone="warn">{move || match drafts.with(|d| drafts::stale_parts(d, &card.get_value())) {
                    (true, true) => "This question changed after you answered. Nothing is sent until you tap an option again and edit your note.",
                    (true, false) => "This question changed after you chose. Nothing is sent until you tap an option again.",
                    _ => "This question changed after you wrote your note. Nothing is sent until you edit the note.",
                }}</p>
            </Show>

            {match c.kind.as_str() {
                "action" => view! {
                    <button class="toggle" disabled=locked aria-pressed=move || chosen("done").to_string()
                        on:click=move |_| drafts.update(|d| drafts::toggle(d, &card.get_value(), "done"))>
                        {move || if chosen("done") { "Marked done (sent when you review)" } else { "Mark done" }}
                    </button>
                }.into_any(),
                "objection" => view! {
                    <button class="toggle" data-tone="warn" disabled=locked aria-pressed=move || chosen("object").to_string()
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
                                        disabled=locked
                                        role=if c.multiple { "checkbox" } else { "radio" }
                                        aria-checked={let k = key.clone(); move || chosen(&k).to_string()}
                                        on:click=move |_| drafts.update(|d| drafts::toggle(d, &card.get_value(), &key_click))
                                    >
                                        <span class="opt-top">
                                            <span class="opt-key">{key.clone()}</span>
                                            {o.stance.as_deref().and_then(stance_label).map(|w| view! { <span class="stance-tag">{w}</span> })}
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
                    {(group_size > 1 && c.can_answer() && recommendation.is_some()).then(|| view! {
                        <button class="link" on:click=use_group.clone()>{format!("Use the recommendations for this group ({group_size})")}</button>
                    })}
                    {recommendation.clone().map(|r| view! {
                        <div class="why">
                            <p><strong>{format!("Why {}: ", r.key)}</strong>{r.why}
                                {r.confidence.filter(|x| !x.is_empty()).map(|x| view! { <span class="muted">{format!(" Confidence: {x}.")}</span> })}
                            </p>
                            {r.flip.filter(|x| !x.is_empty()).map(|f| view! { <p class="muted"><strong>"What would change it: "</strong>{f}</p> })}
                        </div>
                    })}
                }.into_any(),
            }}

            {fold_sections.then(|| view! { <Blocks blocks=c.sections.clone() fold_all=true/> })}
            {(!c.evidence.is_empty()).then(|| view! {
                <details class="evidence">
                    <summary>"Evidence"</summary>
                    <ul>{c.evidence.iter().map(|e| view! { <li><span class="muted">{format!("{}: ", e.label)}</span><code>{e.path.clone()}</code></li> }).collect_view()}</ul>
                </details>
            })}
            {(!c.notes.is_empty() || !c.replies.is_empty()).then(|| view! {
                <ul class="talk">
                    {c.notes.iter().map(|n| view! {
                        <li class="mine" data-kind=n.kind.clone()><span class="who">{format!("{} · {}", if n.kind == "object" { "You objected" } else { "You" }, n.at.as_deref().map(fmt::clock).unwrap_or_default())}</span>{n.note.clone()}{n.delivery.summary_warning.clone().map(|w| view! { <div class="item-note" data-tone="warn">{w}</div> })}</li>
                    }).collect_view()}
                    {c.replies.iter().map(|r| view! {
                        <li class="theirs"><span class="who">{format!("{} · {}", asker, r.at.as_deref().map(fmt::clock).unwrap_or_default())}</span>{r.text.clone()}</li>
                    }).collect_view()}
                </ul>
            })}
            <label class="note">
                <span class="sr">"Note to the agent"</span>
                <textarea
                    rows="2"
                    maxlength="4000"
                    disabled=locked
                    placeholder=note_hint
                    prop:value=note_text
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
    sending: RwSignal<bool>,
    problem: RwSignal<Option<String>>,
) -> impl IntoView {
    let live = expect_context::<Live>();
    let count = move || open.with(|cards| drafts.with(|d| drafts::entries(d, cards).len()));

    let send = move || {
        let cards = open.get_untracked();
        drafts.update(|d| drafts::refresh_recorded(d, &cards));
        let entries = drafts.with_untracked(|d| drafts::entries(d, &cards));
        if entries.is_empty() || sending.get_untracked() {
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
                                // Drop the draft only if it is still the one sent: another tab may have
                                // edited it while this send was on its way, and that newer draft stays.
                                if let Some(sent) = entries.iter().find(|e| e.card_id == r.card_id)
                                {
                                    drafts.update(|d| drafts::forget_sent(d, &cards, sent));
                                }
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
                            _ if c.kind == "sample" => ("Format sample · no answer wanted".to_string(), "muted"),
                            _ if !c.can_answer() => ("Can't be answered right now · nothing is sent".to_string(), "warn"),
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
                <button class="secondary" disabled=move || sending.get() on:click=move |_| deck_view.set(DeckView::Cards)>"Back to the cards"</button>
            </div>
            // once the drafts the failure speaks of are gone (their cards closed), say so instead of offering a resend
            {move || problem.get().map(|p| {
                let words = if count() == 0 {
                    "The send didn't confirm, and these questions have since closed, so nothing is left to send. Reload to see what was recorded.".to_string()
                } else {
                    p
                };
                view! { <p class="item-note" data-tone="crit" role="alert">{words}</p> }
            })}
            <p class="fine">
                {move || {
                    // Answers and "done" close their cards; notes, objections and samples stay open.
                    let closing = open.with(|cards| drafts.with(|d| {
                        drafts::entries(d, cards).iter().filter(|e| e.kind == "answer" || e.kind == "done").count()
                    }));
                    let left = open.with(|o| o.len()).saturating_sub(closing);
                    let stays = match left { 0 => String::new(), 1 => "1 question stays open for later. ".into(), n => format!("{n} questions stay open for later. ") };
                    format!("{stays}Tap a row to change it. When you send, AAC records your exact choices, your notes word for word and the time, signs them, and tells each agent that asked.")
                }}
            </p>
        </div>
    }
}

#[component]
fn Sent(summary: SentSummary, back: impl Fn() + Copy + Send + Sync + 'static) -> impl IntoView {
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
            <button class="secondary" on:click=move |_| back()>"Back to the questions"</button>
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
            {a.delivery.summary_warning.clone().map(|w| view! { <div class="item-note" data-tone="warn">{w}</div> })}
        </li>
    }
}
