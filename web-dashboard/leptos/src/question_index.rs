//! Bounded navigation for a large question deck; filtering never changes drafts or the review set. Cards are
//! grouped by their batch (or, outside a batch, by the thread that asked), and each shows its own label, the short
//! name agents use for it ("WL2"). A card without a label shows none: nothing is made up from its id.
use crate::api::{Batch, Card};
use leptos::prelude::*;

const PAGE_SIZE: usize = 8;

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A batch id in words: "work-lanes-2026-11-11" reads "Work lanes · Nov 11"; an id without a date keeps its words.
pub fn batch_words(id: &str) -> String {
    let id = id.trim();
    let (head, date) = match id.len().checked_sub(10).map(|at| id.split_at(at)) {
        Some((head, tail))
            if tail.len() == 10
                && tail.as_bytes()[4] == b'-'
                && tail.as_bytes()[7] == b'-'
                && tail.chars().filter(|c| c.is_ascii_digit()).count() == 8
                && (head.is_empty() || head.ends_with(['-', '_', ' '])) =>
        {
            let month: usize = tail[5..7].parse().unwrap_or(0);
            let day: u32 = tail[8..10].parse().unwrap_or(0);
            if (1..=12).contains(&month) && (1..=31).contains(&day) {
                (
                    head.trim_end_matches(['-', '_', ' ']),
                    Some(format!("{} {day}", MONTHS[month - 1])),
                )
            } else {
                (id, None)
            }
        }
        _ => (id, None),
    };
    let words = head.replace(['-', '_'], " ");
    let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = words.chars();
    let words = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
        None => String::new(),
    };
    match (words.is_empty(), date) {
        (false, Some(date)) => format!("{words} · {date}"),
        (true, Some(date)) => date,
        _ => words,
    }
}

/// What groups a card: its batch, else the thread that asked it.
pub fn group_key(c: &Card) -> String {
    match c.batch.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        Some(batch) => format!("batch:{batch}"),
        None => format!(
            "thread:{}",
            if c.writer.is_empty() {
                &c.asked_by
            } else {
                &c.writer
            }
        ),
    }
}

/// The cards with each group together, groups in the order their first card came, cards in their own order.
pub fn grouped(cards: &[Card]) -> Vec<Card> {
    let mut keys: Vec<String> = Vec::new();
    for c in cards {
        let key = group_key(c);
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys.iter()
        .flat_map(|key| cards.iter().filter(move |c| group_key(c) == *key).cloned())
        .collect()
}

/// A group's heading: the batch in words (its own label when the server has one) and the threads that asked;
/// outside a batch, the thread alone.
pub fn group_heading(card: &Card, cards: &[Card], batches: &[Batch]) -> String {
    let key = group_key(card);
    let mut threads: Vec<String> = Vec::new();
    for c in cards.iter().filter(|c| group_key(c) == key) {
        let who = if c.asked_by.is_empty() {
            c.writer.clone()
        } else {
            c.asked_by.clone()
        };
        if !who.is_empty() && !threads.contains(&who) {
            threads.push(who);
        }
    }
    let from = (!threads.is_empty()).then(|| format!("from {}", threads.join(", ")));
    match card
        .batch
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        Some(batch) => {
            let named = batches
                .iter()
                .find(|b| b.id == batch)
                .map(|b| b.label.trim())
                .filter(|l| !l.is_empty() && *l != batch)
                .map(str::to_string)
                .unwrap_or_else(|| batch_words(batch));
            match from {
                Some(from) => format!("{named} · {from}"),
                None => named,
            }
        }
        None => from
            .map(|f| {
                let mut c = f.chars();
                c.next()
                    .map(|x| x.to_uppercase().chain(c).collect())
                    .unwrap_or_default()
            })
            .unwrap_or_else(|| "Not in a batch".to_string()),
    }
}

/// The card whose label is exactly what was typed (any case): typing "WL2" goes straight to WL2.
pub fn label_match(cards: &[Card], query: &str) -> Option<usize> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    cards
        .iter()
        .position(|c| !c.label.trim().is_empty() && c.label.trim().eq_ignore_ascii_case(q))
}

fn matching(cards: &[Card], query: &str, writer: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    cards
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let text = format!(
                "{} {} {} {} {} {}",
                c.title,
                c.label,
                c.project,
                c.asked_by,
                c.stream.as_deref().unwrap_or_default(),
                c.batch.as_deref().map(batch_words).unwrap_or_default()
            )
            .to_lowercase();
            ((writer.is_empty() || c.writer == writer) && text.contains(&query)).then_some(i)
        })
        .collect()
}

#[component]
pub fn QuestionIndex(
    cards: Memo<Vec<Card>>,
    selected: RwSignal<usize>,
    on_select: Callback<usize>,
    meta: impl Fn(&Card) -> AnyView + Copy + Send + Sync + 'static,
    ready: impl Fn(&Card) -> bool + Copy + Send + Sync + 'static,
    /// The heading of the group the card at this position starts.
    heading: impl Fn(usize) -> String + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let query = RwSignal::new(String::new());
    let writer = RwSignal::new(String::new());
    let page = RwSignal::new(0usize);
    let matches = Memo::new(move |_| matching(&cards.get(), &query.get(), &writer.get()));
    let groups = Memo::new(move |_| {
        let mut groups = std::collections::BTreeMap::<String, (String, usize)>::new();
        cards.with(|cards| {
            for c in cards {
                let entry = groups
                    .entry(c.writer.clone())
                    .or_insert_with(|| (c.asked_by.clone(), 0));
                entry.1 += 1;
            }
        });
        groups
    });
    Effect::new(move |_| {
        let pick = writer.get();
        if !pick.is_empty() && !groups.with(|g| g.contains_key(&pick)) {
            writer.set(String::new());
        }
    });
    Effect::new(move |_| {
        let ids = matches.get();
        let active = selected.get();
        let last = ids.len().saturating_sub(1) / PAGE_SIZE;
        let next = ids
            .iter()
            .position(|i| *i == active)
            .map(|i| i / PAGE_SIZE)
            .unwrap_or_else(|| page.get_untracked().min(last));
        page.set(next);
    });
    view! {
        <div class="q-index-tools">
            <input type="search" aria-label="Search questions" placeholder="Search questions"
                prop:value=move || query.get()
                on:input=move |e| {
                    let typed = event_target_value(&e);
                    // An exact label goes straight to its card.
                    if let Some(i) = cards.with_untracked(|c| label_match(c, &typed)) {
                        on_select.run(i);
                    }
                    query.set(typed);
                    page.set(0);
                }/>
            <select aria-label="Questions from thread" prop:value=move || writer.get()
                on:change=move |e| { writer.set(event_target_value(&e)); page.set(0); }>
                <option value="">"All threads"</option>
                {move || groups.get().into_iter().map(|(id, (name, n))| view! {
                    <option value=id>{format!("{name} ({n})")}</option>
                }).collect_view()}
            </select>
        </div>
        <div class="q-index-pages" aria-live="polite">
            <button class="btn sm" disabled=move || page.get() == 0
                on:click=move |_| page.update(|p| *p = p.saturating_sub(1))>"Previous"</button>
            <span>{move || {
                let n = matches.with(|m| m.len());
                if n == 0 { "No matches".to_string() } else {
                    format!("{}–{} of {n}", page.get() * PAGE_SIZE + 1, ((page.get() + 1) * PAGE_SIZE).min(n))
                }
            }}</span>
            <button class="btn sm" disabled=move || { (page.get() + 1) * PAGE_SIZE >= matches.with(|m| m.len()) }
                on:click=move |_| page.update(|p| *p += 1)>"More"</button>
        </div>
        <ol class="q-index-items">
            {move || {
                let rows: Vec<usize> = matches.get().into_iter().skip(page.get() * PAGE_SIZE).take(PAGE_SIZE).collect();
                let mut last: Option<String> = None;
                rows.into_iter().map(|i| {
                    let card = cards.with(|c| c[i].clone());
                    let key = group_key(&card);
                    let head = (last.as_ref() != Some(&key)).then(|| heading(i));
                    last = Some(key);
                    let detail = meta(&card);
                    let answered = ready(&card);
                    let label = card.label.trim().to_string();
                    view! {
                        {head.map(|h| view! { <li class="q-group" role="presentation">{h}</li> })}
                        <li><button class="q-item" class:sel=move || selected.get() == i
                            class:done=answered data-state=answered.then_some("ready")
                            data-label=card.short() on:click=move |_| on_select.run(i)>
                            <span class="q-n">{(i + 1).to_string()}</span>
                            <span class="q-t">
                                {(!label.is_empty()).then(|| view! { <span class="q-label">{label.clone()}</span>" · " })}
                                {card.title.clone()}
                            </span>
                            <span class="q-m">{detail}</span>
                        </button></li>
                    }
                }).collect_view()
            }}
        </ol>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn large_inbox_keeps_original_card_positions_when_searching_and_filtering() {
        let cards: Vec<Card> = (0..72)
            .map(|i| Card {
                title: format!("Question {i}"),
                writer: format!("thread-{}", i % 3),
                ..Default::default()
            })
            .collect();
        let all = matching(&cards, "", "");
        assert_eq!(all.len(), 72);
        assert_eq!(all.chunks(PAGE_SIZE).count(), 9);
        assert!(all.chunks(PAGE_SIZE).all(|page| page.len() <= 8));
        assert_eq!(matching(&cards, "QUESTION 71", "thread-2"), vec![71]);
        assert!(matching(&cards, "Question 71", "thread-0").is_empty());
        assert_eq!(matching(&cards, "", "thread-0").len(), 24);
    }

    fn card(id: &str, label: &str, batch: Option<&str>, writer: &str, asked_by: &str) -> Card {
        Card {
            id: id.into(),
            label: label.into(),
            batch: batch.map(str::to_string),
            writer: writer.into(),
            asked_by: asked_by.into(),
            title: format!("Title {id}"),
            ..Default::default()
        }
    }

    #[test]
    fn batch_ids_read_as_words_with_their_date() {
        assert_eq!(batch_words("work-lanes-2026-11-11"), "Work lanes · Nov 11");
        assert_eq!(batch_words("F-questions-2026-11-05"), "F questions · Nov 5");
        assert_eq!(batch_words("C-judgements"), "C judgements");
        assert_eq!(batch_words("2026-11-11"), "Nov 11");
        assert_eq!(batch_words("release-2026-13-40"), "Release 2026 13 40");
    }

    #[test]
    fn cards_group_by_batch_then_thread_and_keep_their_order() {
        let cards = vec![
            card(
                "a",
                "WL1",
                Some("work-lanes-2026-11-11"),
                "vm.lanes",
                "Lanes lead",
            ),
            card("b", "T1", None, "vm.tools", "Tools lead"),
            card(
                "c",
                "WL2",
                Some("work-lanes-2026-11-11"),
                "vm.lanes",
                "Lanes lead",
            ),
            card("d", "", None, "vm.tools", "Tools lead"),
        ];
        let g = grouped(&cards);
        assert_eq!(
            g.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["a", "c", "b", "d"]
        );
        assert_eq!(
            group_heading(&g[0], &g, &[]),
            "Work lanes · Nov 11 · from Lanes lead"
        );
        assert_eq!(group_heading(&g[2], &g, &[]), "From Tools lead");
        // the server's own batch label wins over the id in words
        let named = vec![Batch {
            id: "work-lanes-2026-11-11".into(),
            label: "Work lanes, part 2".into(),
            card_ids: vec![],
        }];
        assert_eq!(
            group_heading(&g[0], &g, &named),
            "Work lanes, part 2 · from Lanes lead"
        );
    }

    #[test]
    fn typing_a_label_finds_that_card_and_a_missing_label_is_never_made_up() {
        let cards = vec![
            card("vm.lanes--WL1", "WL1", None, "vm.lanes", "Lanes lead"),
            card("vm.lanes--WL2", "WL2", None, "vm.lanes", "Lanes lead"),
            card("vm.lanes--Q9", "", None, "vm.lanes", "Lanes lead"),
        ];
        assert_eq!(label_match(&cards, "wl2"), Some(1));
        assert_eq!(label_match(&cards, " WL2 "), Some(1));
        assert_eq!(label_match(&cards, "WL"), None);
        // the id's last part is not a label: "Q9" finds nothing by label
        assert_eq!(label_match(&cards, "Q9"), None);
        assert_eq!(matching(&cards, "WL2", "").first(), Some(&1));
    }
}
