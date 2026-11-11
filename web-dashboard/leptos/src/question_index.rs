//! Bounded navigation for a large question deck; filtering never changes drafts or the review set.
use crate::api::Card;
use leptos::prelude::*;

const PAGE_SIZE: usize = 8;

fn matching(cards: &[Card], query: &str, writer: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    cards
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let text = format!(
                "{} {} {} {} {}",
                c.title,
                c.label,
                c.project,
                c.asked_by,
                c.stream.as_deref().unwrap_or_default()
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
                on:input=move |e| { query.set(event_target_value(&e)); page.set(0); }/>
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
            <button class="btn sm" disabled=move || (page.get() + 1) * PAGE_SIZE >= matches.with(|m| m.len())
                on:click=move |_| page.update(|p| *p += 1)>"More"</button>
        </div>
        <ol class="q-index-items">
            {move || matches.get().into_iter().skip(page.get() * PAGE_SIZE).take(PAGE_SIZE).map(|i| {
                let card = cards.with(|c| c[i].clone());
                let detail = meta(&card);
                let answered = ready(&card);
                view! {
                    <li><button class="q-item" class:sel=move || selected.get() == i
                        class:done=answered data-state=answered.then_some("ready")
                        data-label=card.short() on:click=move |_| on_select.run(i)>
                        <span class="q-n">{(i + 1).to_string()}</span>
                        <span class="q-t">{card.title.clone()}</span>
                        <span class="q-m">{detail}</span>
                    </button></li>
                }
            }).collect_view()}
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
}
