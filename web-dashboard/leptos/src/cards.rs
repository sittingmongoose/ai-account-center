//! Questions for the owner, answered as a deck: one card at a time with Back and Next, a row of card labels to jump,
//! and nothing sent until the review screen ("Send current selections" jumps there early). Only the owner answers;
//! the server refuses an answer to a card that changed after it was shown.

use crate::api::*;
use crate::blocks::Blocks;
use crate::drafts::{self, Drafts, Standing};
use crate::fmt;
use crate::live::Live;
use crate::media::MediaList;
use crate::ui::*;
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

fn confidence_words(c: &str) -> Option<&'static str> {
    match c {
        "high" => Some("Very sure."),
        "medium" => Some("Fairly sure."),
        "low" => Some("Not sure."),
        _ => None,
    }
}

fn sentence_case(text: &str) -> String {
    let mut c = text.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// The card guide's last sentence, when a card has it ("You won't see any change in the app either way."): it is
/// reassurance, so it sits with the facts, not in the problem.
fn split_calm(text: &str) -> (String, Option<String>) {
    match text.trim().rsplit_once("\n\n") {
        Some((body, last)) if last.trim_start().starts_with("You won't see") => {
            (body.to_string(), Some(last.trim().to_string()))
        }
        _ => (text.to_string(), None),
    }
}

fn paragraphs(text: &str) -> impl IntoView + use<> {
    text.split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| view! { <p class="q-now">{p.to_string()}</p> })
        .collect_view()
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

    let answered_open = RwSignal::new(false);
    let history_problem = move || {
        live.view.with(|v| {
            v.as_ref()
                .and_then(|v| v.cards.answer_history_problem.clone())
        })
    };
    view! {
        <section class="plate sec-questions" id="cards">
            <Show
                // A send's result outlives the cards: while a send runs, after it fails and on the sent screen the deck
                // stays even when a read empties the cards under it. An idle review with no cards gives way.
                when=move || {
                    !open.with(|o| o.is_empty())
                        || matches!(deck_view.get(), DeckView::Sent(_))
                        || sending.get()
                        || problem.with(|p| p.is_some())
                }
                fallback=move || view! {
                    <div class="plate-head"><h2>"Questions for you"</h2><span class="plate-meta">"None waiting"</span></div>
                    {source_note(source.get(), "No reading from the agent reports yet.")}
                    {history_problem().map(|m| view! { <p class="plate-note problem" role="status">{m}</p> })}
                    <p class="q-empty">"No questions waiting."</p>
                }
            >
                <Deck open=open drafts=drafts at=at deck_view=deck_view sending=sending problem=problem source=source history_problem=Signal::derive(history_problem)/>
            </Show>
            <Show when=move || !answered().is_empty()>
                <div class="q-foot-actions">
                    <Disclosure open=answered_open label=format!("Answered ({})", answered().len())/>
                </div>
                <Show when=move || answered_open.get()>
                    <ul class="answered">
                        {move || answered().into_iter().map(|a| view! { <AnsweredView answered=a/> }).collect_view()}
                    </ul>
                </Show>
            </Show>
        </section>
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
    source: Signal<Option<Source>>,
    history_problem: Signal<Option<String>>,
) -> impl IntoView {
    let live = expect_context::<Live>();
    let list_open = RwSignal::new(false);
    // After the new card or the review screen is in place, bring the deck's top back into view when the owner had
    // scrolled past it (a long card). A deck already in view does not move. A plain browser timer on purpose:
    // Leptos 0.9 cancels its own `set_timeout` when the owner that started it is cleaned up, and this click redraws
    // the very branch that owns it (see NOTES.md).
    let to_top = move || {
        let callback = wasm_bindgen::closure::Closure::once_into_js(move || {
            if let Some(el) = document().query_selector(".sec-questions").ok().flatten() {
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
    // One line for a question in the list: what it holds up (or that nothing waits), then how the owner left it.
    let item_meta = move |card: &Card| -> AnyView {
        let draft = drafts.with(|d| drafts::standing(d, card));
        let first = if card.kind == "action" {
            view! { <span>"Only you can do this"</span> }.into_any()
        } else if let Some(b) = card.blocks.clone().filter(|b| !b.is_empty()) {
            view! { <span data-tone="warn">{format!("Holds up {b}")}</span> }.into_any()
        } else {
            view! { <span>{sentence_case(card.stream.as_deref().unwrap_or("Nothing waits on it"))}</span> }.into_any()
        };
        let second = match draft {
            Standing::Ready if card.kind == "action" => {
                view! { <span data-tone="good">"marked done"</span> }.into_any()
            }
            Standing::Ready => {
                view! { <span data-tone="good">"answered, not sent"</span> }.into_any()
            }
            Standing::Changed | Standing::NeedsWords => {
                view! { <span data-tone="warn">"look again"</span> }.into_any()
            }
            Standing::Empty
                if card.blocks.as_deref().unwrap_or("").is_empty() && card.stream.is_some() =>
            {
                view! { <span>"nothing waits on it"</span> }.into_any()
            }
            Standing::Empty => {
                view! { <span>{fmt::age_words_since(&card.asked_at, live.now_ms.get())}</span> }
                    .into_any()
            }
        };
        view! { {first}" · "{second} }.into_any()
    };
    let list_items =
        move || {
            shown.get().into_iter().enumerate().map(|(i, card)| {
            let ready = drafts.with(|d| drafts::standing(d, &card) == Standing::Ready);
            let meta = item_meta(&card);
            view! {
                <li>
                    <button
                        class="q-item"
                        class:sel=move || deck_view.get() == DeckView::Cards && at.get() == i
                        class:done=ready
                        data-label=card.short()
                        data-state=ready.then_some("ready")
                        on:click=move |_| { list_open.set(false); go_card(i) }
                    >
                        <span class="q-n">{(i + 1).to_string()}</span>
                        <span class="q-t">{card.title.clone()}</span>
                        <span class="q-m">{meta}</span>
                    </button>
                </li>
            }
        }).collect_view()
        };
    let review_label = move || format!("Review and send ({})", ready());
    let at_end = move || at.get() + 1 >= shown.with(|o| o.len());
    let next_label = move || if at_end() { "Review" } else { "Next" };
    let back_disabled = move || at.get() == 0 || deck_view.get() != DeckView::Cards;
    let can_use_rec = move || {
        deck_view.get() == DeckView::Cards
            && open.with(|cards| {
                cards
                    .iter()
                    .any(|c| c.recommendation.is_some() && c.can_answer() && c.is_question())
            })
    };
    let use_rec = move |_| {
        open.with_untracked(|cards| {
            drafts.update(|d| {
                drafts::use_recommendations(d, cards);
            })
        });
    };
    view! {
        <div class="plate-head">
            <h2>"Questions for you"</h2>
            <span class="plate-meta"><b>{move || open.with(|o| o.len()).to_string()}</b>" open · "<b>{move || ready().to_string()}</b>" answered, not sent yet"</span>
            <Show when=can_use_rec>
                <div class="plate-tools only-d"><button class="btn sm" on:click=use_rec>"Use my recommendations"</button></div>
            </Show>
            // The deck's navigation in the head; the review screen has its own Send and Back.
            <Show when=move || deck_view.get() == DeckView::Cards>
                <div class="plate-tools only-land">
                    <button class="btn sm" disabled=back_disabled on:click=move |_| back()>"Back"</button>
                    <button class="btn sm" on:click=move |_| next()>{next_label}</button>
                    <button class="btn sm primary" on:click=move |_| go_review()>{review_label}</button>
                </div>
            </Show>
        </div>
        {move || source_note(source.get(), "No reading from the agent reports yet.")}
        {move || history_problem.get().map(|m| view! { <p class="plate-note problem" role="status">{m}</p> })}
        <Show when=move || !verdicts.with(|v| v.is_empty())>
            <div class="q-seg">
                <Segmented
                    options={std::iter::once(("all".to_string(), "All".to_string())).chain(verdicts.get().into_iter().map(|v| {
                        let n = open.with(|cards| cards.iter().filter(|c| c.verdict.as_deref() == Some(v.as_str())).count());
                        (v.clone(), format!("{} ({n})", verdict_label(&v)))
                    })).collect::<Vec<_>>()}
                    selected={Signal::derive(move || verdict.get().unwrap_or_else(|| "all".to_string()))}
                    on_select={Callback::new(move |v: String| { current_id.set(None); at.set(0); verdict.set((v != "all").then_some(v)); })}
                    label="Show cards by verdict"
                />
            </div>
        </Show>
        <Show when=move || !matches!(deck_view.get(), DeckView::Sent(_))>
            <div class="q-step">
                <span>"Question "<b>{move || (at.get() + 1).min(shown.with(|o| o.len())).to_string()}</b>{move || format!(" of {}", shown.with(|o| o.len()))}</span>
                <Disclosure open=list_open label="All questions".to_string()/>
            </div>
            <Show when=move || list_open.get()>
                <ol class="q-list-phone">{list_items}</ol>
            </Show>
        </Show>
        <div class="q-wrap">
            <Show when=move || !matches!(deck_view.get(), DeckView::Sent(_))>
                <ol class="q-list">{list_items}</ol>
            </Show>
            {move || match deck_view.get() {
                DeckView::Cards => view! {
                    {move || current.get().map(|card| view! { <CardBody card=card drafts=drafts open=open/> })}
                }.into_any(),
                DeckView::Review => view! { <Review open=open drafts=drafts deck_view=deck_view go_card=go_open sending=sending problem=problem/> }.into_any(),
                DeckView::Sent(summary) => view! { <Sent summary=summary back=move || go_card(0)/> }.into_any(),
            }}
        </div>
        <Show when=move || deck_view.get() == DeckView::Cards>
            <div class="q-send q-deck-nav">
                <button class="btn" disabled=back_disabled on:click=move |_| back()>"Back"</button>
                <span class="count">{move || format!("{} of {}", at.get() + 1, shown.with(|o| o.len()))}</span>
                <button class="btn" on:click=move |_| next()>{next_label}</button>
                <div class="right"><button class="btn primary" on:click=move |_| go_review()>{review_label}</button></div>
            </div>
        </Show>
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
        // A short stream id reads as "stream C"; a named one stands on its own ("upkeep").
        c.stream.clone().map(|s| {
            if s.len() <= 5 {
                format!("stream {s}")
            } else {
                s
            }
        }),
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
    let live = expect_context::<Live>();
    // The facts line under the question, in the card guide's terms.
    let mut facts: Vec<(&'static str, String)> = Vec::new();
    facts.push((
        "Waiting on it",
        c.blocks
            .clone()
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| "nothing".into()),
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
            (true, Some(how)) if !how.is_empty() => how,
            (true, _) => "yes".to_string(),
            (false, Some(how)) if !how.is_empty() => format!("no: {how}"),
            (false, _) => "no".to_string(),
        };
        facts.push(("Undo", words));
    }
    facts.push((
        "Rules loosened",
        c.exception
            .clone()
            .map(|e| e.rule)
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| "none".into()),
    ));
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
        _ => "Add a note (optional). It goes to the agent word for word.",
    };

    view! {
        <article class="q-card" data-urgency=c.urgency.clone() data-kind=c.kind.clone()>
            <div class="q-kicker">
                <span class="cap">{if place.is_empty() { c.short() } else { place.clone() }}</span>
                <span class="when" title=fmt::clock(&c.asked_at)>
                    <span class="nw">{let asked_at = c.asked_at.clone(); move || format!("{} ·", fmt::age_words_since(&asked_at, live.now_ms.get()))}</span>
                    " "<span class="nw">{format!("from {asker}")}</span>
                </span>
            </div>
            <h3 class="q-title">{c.title.clone()}</h3>
            <div class="q-why">
                {(c.kind == "sample").then(|| view! {
                    <p class="sample-banner">"Format sample · no answer wanted. Nothing on this card is sent."</p>
                })}
                {(c.kind != "sample" && (locked || c.problem.is_some())).then(|| view! {
                    <p class="q-problem">
                        {if locked { "Can't be answered right now. " } else { "" }}
                        {c.problem.clone().unwrap_or_else(|| "The card could not be checked.".into())}
                        {locked.then_some(" The last good version is shown; it can be answered once the agent rewrites it.")}
                    </p>
                })}
                {c.exception.clone().map(|e| view! {
                    <p class="q-rule">{format!("This bends one of your rules: {}.{}", e.rule, if e.text.is_empty() { String::new() } else { format!(" {}", e.text) })}</p>
                })}
                {kind_word.map(|w| view! { <p class="q-kind">{w}</p> })}
                {(c.verdict.is_some() || group.is_some()).then(|| view! {
                    <p class="q-kind">
                        {c.verdict.clone().map(|v| view! { <span data-tone="acc">{verdict_label(&v)}</span> })}
                        {group.clone().map(|g| view! { <span class="q3">{format!(" · group {g}")}</span> })}
                    </p>
                })}
                {(!c.now.is_empty()).then(|| view! {
                    <p class="cap q-sub">"What is wrong today"</p>
                    {paragraphs(&split_calm(&c.now).0)}
                    {c.now_as_of.as_deref().map(|t| view! { <p class="q-calm">{format!("As of {}.", fmt::clock(t))}</p> })}
                })}
                {(c.old_answer.is_some() || c.what_changed.is_some()).then(|| view! {
                    <div class="q-old">
                        {c.old_answer.clone().map(|o| view! {
                            <p><b>"Your earlier answer: "</b>{o.text}{o.at.as_deref().map(|t| format!(" ({})", fmt::clock(t)))}</p>
                        })}
                        {c.what_changed.clone().map(|w| view! { <p><b>"What changed: "</b>{w}</p> })}
                    </div>
                })}
                {c.agreement.clone().and_then(|a| match a.same {
                    Some(false) => Some(view! { <p class="q-problem"><b>"The judges split. "</b>{a.note.unwrap_or_default()}</p> }.into_any()),
                    Some(true) => Some(view! { <p class="q-old">{format!("The judges agreed.{}", a.note.map(|n| format!(" {n}")).unwrap_or_default())}</p> }.into_any()),
                    None => a.note.map(|n| view! { <p class="q-old">{n}</p> }.into_any()),
                })}
                {c.doubt.clone().filter(|d| !d.is_empty()).map(|d| view! { <p class="q-old"><b>"Unsure: "</b>{d}</p> })}
                {(!fold_sections).then(|| view! { <Blocks blocks=c.sections.clone()/> })}
                {fold_sections.then(|| view! { <Blocks blocks=c.sections.clone() fold_all=true/> })}
            </div>
            <div class="q-meta">
                {split_calm(&c.now).1.map(|calm| view! { <p class="q-calm">{calm}</p> })}
                <div class="q-facts">
                    {facts.into_iter().map(|(k, v)| view! { <span>{format!("{k}: ")}<b>{v}</b></span> }).collect_view()}
                </div>
            </div>
            {match c.kind.as_str() {
                "action" => view! {
                    <div class="q-actions">
                        <button class="btn" disabled=locked aria-pressed=move || chosen("done").to_string()
                            on:click=move |_| drafts.update(|d| drafts::toggle(d, &card.get_value(), "done"))>
                            {move || if chosen("done") { "Marked done (sent when you review)" } else { "Mark done" }}
                        </button>
                    </div>
                }.into_any(),
                "objection" => view! {
                    <div class="q-actions">
                        <button class="btn" data-tone="warn" disabled=locked aria-pressed=move || chosen("object").to_string()
                            on:click=move |_| drafts.update(|d| drafts::toggle(d, &card.get_value(), "object"))>
                            {move || if chosen("object") { "You object: say why below" } else { "Object" }}
                        </button>
                        <Show when=move || standing() == Standing::NeedsWords>
                            <span class="q-problem">"An objection needs a reason before it can be sent."</span>
                        </Show>
                    </div>
                }.into_any(),
                _ => view! {
                    <div class="opts" role=if c.multiple { "group" } else { "radiogroup" }>
                        {c.options.clone().into_iter().map(|o| {
                            let key = o.key.clone();
                            let key_state = key.clone();
                            let key_sel = key.clone();
                            let key_radio = key.clone();
                            let key_click = key.clone();
                            let is_rec = key == recommended;
                            let has_extra = !o.media.is_empty() || !o.sections.is_empty();
                            view! {
                                <div class="opt" class:sel=move || chosen(&key_sel) data-checked=move || chosen(&key_state).then_some("1")>
                                    <button
                                        class="opt-pick"
                                        disabled=locked
                                        role=if c.multiple { "checkbox" } else { "radio" }
                                        aria-checked={let k = key.clone(); move || chosen(&k).to_string()}
                                        on:click=move |_| drafts.update(|d| drafts::toggle(d, &card.get_value(), &key_click))
                                    >
                                        <span class="radio" class:on=move || chosen(&key_radio)></span>
                                        <span class="opt-head">
                                            <span class="opt-key">{key.clone()}</span>
                                            <span class="opt-label">{o.label.clone()}
                                                {o.stance.as_deref().and_then(stance_label).map(|w| view! { <span class="q3">{format!(" · {w}")}</span> })}
                                                {is_rec.then(|| view! { <span class="opt-rec" data-tone="acc">" Recommended"</span> })}
                                            </span>
                                        </span>
                                        <dl class="opt-gc">
                                            {(!o.gets.is_empty()).then(|| view! { <dt>"You get"</dt><dd>{o.gets.clone()}</dd> })}
                                            {(!o.costs.is_empty()).then(|| view! { <dt>"It costs"</dt><dd>{o.costs.clone()}</dd> })}
                                        </dl>
                                    </button>
                                    {has_extra.then(|| view! {
                                        <div class="opt-extra">
                                            <MediaList items=o.media.clone()/>
                                            <Blocks blocks=o.sections.clone()/>
                                        </div>
                                    })}
                                </div>
                            }
                        }).collect_view()}
                    </div>
                }.into_any(),
            }}
            <div class="q-after">
                {recommendation.clone().map(|r| {
                    let sure = r.confidence.as_deref().and_then(confidence_words);
                    let why = r.why.clone();
                    let key = r.key.clone();
                    view! {
                        <p class="rec"><span data-tone="acc">{format!("We recommend {key}.")}</span>" "{why}{sure.map(|w| view! { <span class="q3">{format!(" {w}")}</span> })}</p>
                        {r.flip.filter(|x| !x.is_empty()).map(|f| view! { <p class="q-calm">{format!("What would change it: {f}")}</p> })}
                    }
                })}
                {(group_size > 1 && c.can_answer() && recommendation.is_some()).then(|| view! {
                    <button class="link" on:click=use_group.clone()>{format!("Use the recommendations for this group ({group_size})")}</button>
                })}
                <Show when=move || standing() == Standing::Changed>
                    <p class="q-problem">{move || match drafts.with(|d| drafts::stale_parts(d, &card.get_value())) {
                        (true, true) => "This question changed after you answered. Nothing is sent until you tap an option again and edit your note.",
                        (true, false) => "This question changed after you chose. Nothing is sent until you tap an option again.",
                        _ => "This question changed after you wrote your note. Nothing is sent until you edit the note.",
                    }}</p>
                </Show>
                {(!c.notes.is_empty() || !c.replies.is_empty()).then(|| view! {
                    <ul class="talk">
                        {c.notes.iter().map(|n| view! {
                            <li data-kind=n.kind.clone() data-delivery=n.delivery.state.clone()>
                                <span class="who">{format!(
                                    "{} · {}{}",
                                    if n.kind == "object" { "You objected" } else { "You" },
                                    n.at.as_deref().map(fmt::clock).unwrap_or_default(),
                                    n.delivery.not_told().map(|t| format!(" · {t}")).unwrap_or_default(),
                                )}</span>
                                {n.note.clone()}
                                <DeliveryWarnings delivery=n.delivery.clone()/>
                            </li>
                        }).collect_view()}
                        {c.replies.iter().map(|r| view! {
                            <li><span class="who">{format!("{} · {}", asker, r.at.as_deref().map(fmt::clock).unwrap_or_default())}</span>{r.text.clone()}</li>
                        }).collect_view()}
                    </ul>
                })}
                <div class="q-more">
                    {(!c.evidence.is_empty()).then(|| {
                        let tech = RwSignal::new(false);
                        let items = c.evidence.clone();
                        view! {
                            <Disclosure open=tech label="Technical detail for agents".to_string()/>
                            <Show when=move || tech.get()>
                                <ul class="q-tech">{items.iter().map(|e| view! { <li><span class="q3">{format!("{}: ", e.label)}</span><code>{e.path.clone()}</code></li> }).collect_view()}</ul>
                            </Show>
                        }
                    })}
                    <label class="q-note-label">
                        <span class="sr">"Note to the agent"</span>
                        <textarea
                            class="note"
                            rows="2"
                            maxlength="4000"
                            disabled=locked
                            placeholder=note_hint
                            prop:value=note_text
                            on:input=move |ev| { let text = event_target_value(&ev); drafts.update(|d| drafts::set_note(d, &card.get_value(), text)); }
                        ></textarea>
                    </label>
                </div>
            </div>
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
                            _ if c.kind == "sample" => ("Format sample · no answer wanted".to_string(), ""),
                            _ if !c.can_answer() => ("Can't be answered right now · nothing is sent".to_string(), "warn"),
                            Standing::Changed => ("Changed since you chose · look again".to_string(), "warn"),
                            Standing::NeedsWords => ("Objection without a reason · add one to send it".to_string(), "warn"),
                            Standing::Empty if c.kind == "objection" => ("No objection · nothing is sent".to_string(), ""),
                            Standing::Empty if c.kind == "action" => ("Not done yet · stays open".to_string(), ""),
                            Standing::Empty => ("Not answered · stays open for later".to_string(), ""),
                            Standing::Ready if draft.choice.is_empty() => ("Note only · the question stays open".to_string(), ""),
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
                                <span class="review-title">{c.title.clone()}</span>
                                <span class="review-what" data-tone=(!tone.is_empty()).then_some(tone)>{what}</span>
                                {(!note.is_empty()).then(|| view! { <span class="review-note">{format!("\u{201c}{note}\u{201d}")}</span> })}
                            </button>
                        </li>
                    }
                }).collect_view()}
            </ul>
            <div class="q-send">
                <button class="btn" disabled=move || sending.get() on:click=move |_| deck_view.set(DeckView::Cards)>"Back to the questions"</button>
                <div class="right">
                    <button class="btn primary" disabled=move || count() == 0 || sending.get() on:click=move |_| send()>
                        {move || match (sending.get(), count()) {
                            (true, _) => "Sending...".to_string(),
                            (false, 0) => "Nothing to send yet".to_string(),
                            (false, 1) => "Send 1 answer".to_string(),
                            (false, n) => format!("Send {n} answers"),
                        }}
                    </button>
                </div>
            </div>
            // once the drafts the failure speaks of are gone (their cards closed), say so instead of offering a resend
            {move || problem.get().map(|p| {
                let words = if count() == 0 {
                    "The send didn't confirm, and these questions have since closed, so nothing is left to send. Reload to see what was recorded.".to_string()
                } else {
                    p
                };
                view! { <p class="plate-note crit" role="alert">{words}</p> }
            })}
            <p class="review-fine">
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
                <p class="plate-note problem">{format!("Not sent, the question changed after you opened it: {}. Your choice is kept; look again.", summary.changed.join(", "))}</p>
            })}
            {(!summary.refused.is_empty()).then(|| view! {
                <ul class="plate-note crit">{summary.refused.iter().map(|r| view! { <li>{r.clone()}</li> }).collect_view()}</ul>
            })}
            <button class="btn" on:click=move |_| back()>"Back to the questions"</button>
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
    let delivery = a.delivery.told();
    view! {
        <li data-delivery=a.delivery.state.clone()>
            <div><b>{c.title.clone()}</b></div>
            <div>{a.label(&labels)}{(a.via == "recommended").then_some(" (recommended)")}</div>
            {a.note.clone().filter(|n| !n.is_empty()).map(|n| view! { <blockquote>{n}</blockquote> })}
            <div class="q3">{format!("{} · {} · {delivery}", c.asked_by, fmt::clock(&a.answered_at))}</div>
            <DeliveryWarnings delivery=a.delivery.clone()/>
        </li>
    }
}

/// A record's delivery warnings (`Delivery::warnings`), shared by answered rows and an open card's notes.
#[component]
fn DeliveryWarnings(delivery: Delivery) -> impl IntoView {
    delivery
        .warnings()
        .into_iter()
        .map(|w| view! { <div data-tone="warn">{w}</div> })
        .collect_view()
}
