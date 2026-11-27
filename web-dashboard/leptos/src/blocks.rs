//! A card's optional sections. A card carries only what its question needs; each type here draws one kind of
//! section. A type this page doesn't know is shown as such, never dropped silently.

use crate::api::{Block, Media};
use crate::media::MediaList;
use crate::ui::Icon;
use leptos::prelude::*;
use serde_json::Value;

fn text_of(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn strings(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

fn default_title(kind: &str) -> Option<&'static str> {
    match kind {
        "compare" => Some("What others do"),
        "notCopying" => Some("What we are not copying"),
        "unknowns" => Some("Unknowns"),
        "terms" => Some("Terms"),
        "links" => Some("Links"),
        "quote" => Some("In their words"),
        "planChange" => Some("Plan change"),
        _ => None,
    }
}

fn paragraphs(body: &str) -> impl IntoView + use<> {
    body.split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| view! { <p>{p.to_string()}</p> })
        .collect_view()
}

/// One quote exactly as the card wrote it, with its origin and page link.
fn quote_figure(q: &Value) -> AnyView {
    let where_ = text_of(q, "where");
    let source = text_of(q, "source");
    let version = text_of(q, "version");
    let context = q
        .get("context")
        .and_then(Value::as_array)
        .map(|a| strings(a))
        .unwrap_or_default();
    let link = q
        .get("link")
        .filter(|l| !l.is_null())
        .map(|l| (text_of(l, "label"), text_of(l, "url")));
    let origin = [source, where_, version]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    view! {
        <figure class="quote">
            <blockquote>{paragraphs(&text_of(q, "text"))}</blockquote>
            {(!context.is_empty()).then(|| view! { <p class="quote-context">{context.join(" · ")}</p> })}
            {(!origin.is_empty() || link.is_some()).then(|| view! {
                <figcaption>{origin.clone()}{link.clone().map(|(label, url)| view! { " " {link_view(if label.is_empty() { url.clone() } else { label }, url)} })}</figcaption>
            })}
        </figure>
    }
    .into_any()
}

fn quote_item_id(q: &Value) -> Option<&str> {
    q.get("item")
        .or_else(|| q.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
}

#[derive(Clone, Debug, PartialEq)]
struct QuotePartition {
    shown: Vec<usize>,
    hidden: Vec<usize>,
    products: Vec<String>,
    more: bool,
}

/// Product names of the hidden quotes, most quoted first and alphabetically on a tie (the Scout fold line's rule).
fn quote_products(items: &[Value], indexes: &[usize]) -> Vec<String> {
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for i in indexes {
        let source = text_of(&items[*i], "source");
        if !source.is_empty() {
            *counts.entry(source).or_default() += 1;
        }
    }
    let mut products = counts.into_iter().collect::<Vec<_>>();
    products.sort_by(|(a_name, a_count), (b_name, b_count)| {
        b_count.cmp(a_count).then_with(|| a_name.cmp(b_name))
    });
    products.into_iter().map(|(name, _)| name).collect()
}

/// Which quotes face the card and which fold. Without deciding-quote metadata every quote folds, as the follow-up asks;
/// with it the deciding quotes stay visible in card order and only the rest fold.
fn partition_quotes(block: &Block) -> QuotePartition {
    let mut keyed = std::collections::BTreeSet::new();
    for wanted in &block.key {
        for (i, q) in block.items.iter().enumerate() {
            if quote_item_id(q) == Some(wanted.as_str()) {
                keyed.insert(i);
            }
        }
    }
    for (i, q) in block.items.iter().enumerate() {
        if q.get("key").and_then(Value::as_bool) == Some(true) {
            keyed.insert(i);
        }
    }
    let more = !keyed.is_empty();
    let shown = keyed.iter().copied().collect::<Vec<_>>();
    let hidden = (0..block.items.len())
        .filter(|i| !keyed.contains(i))
        .collect::<Vec<_>>();
    let products = quote_products(&block.items, &hidden);
    QuotePartition {
        shown,
        hidden,
        products,
        more,
    }
}

fn quote_fold_label(count: usize, products: &[String], more: bool) -> String {
    let noun = if count == 1 { "quote" } else { "quotes" };
    let lead = if more {
        format!("{count} more {noun}")
    } else {
        format!("{count} {noun}")
    };
    if products.is_empty() {
        lead
    } else {
        format!("{lead} from {}", products.join(", "))
    }
}

/// An option's quotes: deciding quotes face the card when the card names them; the rest stay behind a button with
/// aria-expanded. Without deciding metadata the whole list folds behind the same kind of button.
#[component]
fn OptionQuotes(block: Block) -> impl IntoView {
    let partition = partition_quotes(&block);
    let shown = partition
        .shown
        .iter()
        .map(|i| block.items[*i].clone())
        .collect::<Vec<_>>();
    let hidden = partition
        .hidden
        .iter()
        .map(|i| block.items[*i].clone())
        .collect::<Vec<_>>();
    let label = quote_fold_label(hidden.len(), &partition.products, partition.more);
    let expanded = RwSignal::new(false);
    view! {
        <div class="quotes option-quotes">
            {shown.iter().map(quote_figure).collect_view()}
            {(!hidden.is_empty()).then(|| view! {
                <button type="button" class="quote-fold" class:open=move || expanded.get()
                    aria-expanded=move || expanded.get().to_string()
                    on:click=move |_| expanded.update(|open| *open = !*open)>
                    <Icon name="chev-right"/><span class="quote-fold-line">{label.clone()}</span>
                </button>
                <Show when=move || expanded.get()>
                    <div class="quote-fold-list">
                        {hidden.iter().map(quote_figure).collect_view()}
                    </div>
                </Show>
            })}
        </div>
    }
}

fn quote_body(block: &Block, fold_for_option: bool) -> AnyView {
    if fold_for_option {
        return view! { <OptionQuotes block=block.clone()/> }.into_any();
    }
    view! {
        <div class="quotes">
            {block.items.iter().map(quote_figure).collect_view()}
        </div>
    }
    .into_any()
}

fn body_of(block: &Block, fold_quotes_for_option: bool) -> AnyView {
    match block.kind.as_str() {
        "text" => paragraphs(block.body.as_deref().unwrap_or_default()).into_any(),
        "list" | "unknowns" => {
            let items = strings(&block.items).into_iter().map(|s| view! { <li>{s}</li> }).collect_view();
            if block.ordered { view! { <ol>{items}</ol> }.into_any() } else { view! { <ul>{items}</ul> }.into_any() }
        }
        "facts" => view! {
            <dl class="facts">
                {block.rows.iter().map(|r| view! { <dt>{text_of(r, "label")}</dt><dd>{text_of(r, "value")}</dd> }).collect_view()}
            </dl>
        }
        .into_any(),
        "terms" => view! {
            <dl class="terms">
                {block.items.iter().map(|r| view! { <dt>{text_of(r, "term")}</dt><dd>{text_of(r, "meaning")}</dd> }).collect_view()}
            </dl>
        }
        .into_any(),
        "table" => view! {
            <div class="table-scroll">
                <table>
                    <thead><tr>{block.columns.iter().map(|c| view! { <th scope="col">{c.clone()}</th> }).collect_view()}</tr></thead>
                    <tbody>
                        {block.rows.iter().map(|row| {
                            let cells = row.as_array().cloned().unwrap_or_default();
                            view! { <tr>{cells.iter().map(|c| view! { <td>{c.as_str().map(str::to_string).unwrap_or_else(|| c.to_string())}</td> }).collect_view()}</tr> }
                        }).collect_view()}
                    </tbody>
                </table>
            </div>
        }
        .into_any(),
        "compare" => view! {
            <ul class="compare">
                {block.items.iter().map(|i| {
                    let version = text_of(i, "version");
                    let link = i.get("link").filter(|l| !l.is_null()).map(|l| (text_of(l, "label"), text_of(l, "url")));
                    let does = text_of(i, "does");
                    let problem = text_of(i, "problem");
                    view! {
                        <li>
                            <div class="compare-name">
                                <strong>{text_of(i, "name")}</strong>
                                {(!version.is_empty()).then(|| view! { <span class="muted">{version.clone()}</span> })}
                                {link.map(|(label, url)| link_view(if label.is_empty() { url.clone() } else { label }, url))}
                            </div>
                            {(!does.is_empty()).then(|| view! { <div><strong>"Does: "</strong>{does.clone()}</div> })}
                            {(!problem.is_empty()).then(|| view! { <div><strong>"Problem: "</strong>{problem.clone()}</div> })}
                        </li>
                    }
                }).collect_view()}
            </ul>
        }
        .into_any(),
        "notCopying" => view! {
            <ul>{block.items.iter().map(|i| view! { <li><strong>{text_of(i, "what")}</strong>{format!(": {}", text_of(i, "why"))}</li> }).collect_view()}</ul>
        }
        .into_any(),
        "callout" => view! {
            <div class="callout" data-tone=block.tone.clone().unwrap_or_else(|| "info".into())>{paragraphs(block.body.as_deref().unwrap_or_default())}</div>
        }
        .into_any(),
        "links" => view! {
            <ul class="links">{block.items.iter().map(|i| view! { <li>{link_view(text_of(i, "label"), text_of(i, "url"))}</li> }).collect_view()}</ul>
        }
        .into_any(),
        // Verbatim words copied by code: shown exactly as written, with where they came from. In an option the list
        // folds behind its count and products; key quotes the asker names stay on the card.
        "quote" => quote_body(block, fold_quotes_for_option),
        "planChange" => view! {
            <div class="plan-change">
                <p class="muted">{[block.doc.clone().unwrap_or_default(), block.unit.clone().unwrap_or_default()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" › ")}</p>
                <div class="pc-now"><span class="pc-label">"Now"</span>{paragraphs(block.now.as_deref().unwrap_or_default())}</div>
                <div class="pc-new"><span class="pc-label">"Proposed"</span>{paragraphs(block.proposed.as_deref().unwrap_or_default())}</div>
            </div>
        }
        .into_any(),
        "media" => {
            let items: Vec<Media> = block.items.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect();
            view! { <MediaList items=items/> }.into_any()
        }
        _ => view! {
            <p class="muted">{format!("This card has a section this page can't show yet ({}).", block.kind)}</p>
        }
        .into_any(),
    }
}

/// Web links open in a new tab; anything else (a share path) is shown to copy.
fn link_view(label: String, url: String) -> AnyView {
    if url.starts_with("https://") || url.starts_with("http://") {
        view! { <a href=url target="_blank" rel="noopener noreferrer">{label}</a> }.into_any()
    } else {
        view! { <span><span class="muted">{format!("{label} ")}</span><code>{url}</code></span> }
            .into_any()
    }
}

/// Open sections for the current card, identified by the option (if any), type, title and occurrence.
type BlockKey = (Option<String>, String, Option<String>, usize);

#[derive(Clone, Copy)]
pub struct BlockFolds(RwSignal<std::collections::BTreeSet<BlockKey>>);

impl BlockFolds {
    pub fn new() -> Self {
        Self(RwSignal::new(Default::default()))
    }
}

/// `fold_all`: every section starts folded under its title (phones, where the decision comes first).
#[component]
pub fn Blocks(
    blocks: Vec<Block>,
    #[prop(optional)] fold_all: bool,
    #[prop(optional)] option: Option<String>,
) -> impl IntoView {
    let BlockFolds(open) = expect_context::<BlockFolds>();
    let mut occurrences = std::collections::BTreeMap::new();
    blocks
        .into_iter()
        .map(|block| {
            let kind = block.kind.clone();
            let title = block
                .title
                .clone()
                .or_else(|| default_title(&kind).map(str::to_string));
            let body = body_of(&block, option.is_some());
            if block.fold || fold_all {
                let n = occurrences.entry((kind.clone(), title.clone())).or_insert(0);
                let key = StoredValue::new((option.clone(), kind.clone(), title.clone(), *n));
                *n += 1;
                view! {
                    <details class="block" data-type=kind
                        open=move || open.with(|keys| key.with_value(|key| keys.contains(key)))
                        on:toggle=move |event| {
                            let expanded = event_target::<web_sys::HtmlElement>(&event).has_attribute("open");
                            open.update(|keys| {
                                if expanded {
                                    keys.insert(key.get_value());
                                } else {
                                    key.with_value(|key| keys.remove(key));
                                }
                            });
                        }>
                        <summary>{title.unwrap_or_else(|| "More".into())}</summary>
                        {body}
                    </details>
                }
                .into_any()
            } else {
                view! {
                    <section class="block" data-type=kind>
                        {title.map(|t| view! { <h4>{t}</h4> })}
                        {body}
                    </section>
                }
                .into_any()
            }
        })
        .collect_view()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn quote(id: &str, source: &str) -> Value {
        json!({
            "item": id,
            "text": format!("quote {id}"),
            "source": source,
            "link": {"label": "page", "url": "https://example.com/page"}
        })
    }

    fn quote_block(items: Vec<Value>, key: Vec<&str>) -> Block {
        Block {
            kind: "quote".into(),
            items,
            key: key.into_iter().map(str::to_string).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn an_option_quote_list_is_folded_by_default_with_its_count_and_products() {
        let block = quote_block(
            vec![
                quote("q1", "Beta"),
                quote("q2", "Alpha"),
                quote("q3", "Alpha"),
                quote("q4", "Beta"),
            ],
            vec![],
        );
        let partition = partition_quotes(&block);
        assert!(
            partition.shown.is_empty(),
            "nothing faces the card without key metadata"
        );
        assert_eq!(partition.hidden, vec![0, 1, 2, 3]);
        assert_eq!(
            partition.products,
            vec!["Alpha".to_string(), "Beta".to_string()]
        );
        assert_eq!(
            quote_fold_label(partition.hidden.len(), &partition.products, partition.more),
            "4 quotes from Alpha, Beta"
        );
    }

    #[test]
    fn product_ties_read_alphabetically_and_one_quote_reads_singular() {
        let block = quote_block(vec![quote("q1", "Zep"), quote("q2", "Mem0")], vec![]);
        let partition = partition_quotes(&block);
        assert_eq!(
            partition.products,
            vec!["Mem0".to_string(), "Zep".to_string()]
        );
        assert_eq!(
            quote_fold_label(1, &partition.products[..1], false),
            "1 quote from Mem0"
        );
    }

    #[test]
    fn deciding_quotes_stay_visible_in_card_order_and_only_the_rest_fold() {
        let mut second = quote("q2", "Mem0");
        second["key"] = json!(true);
        let block = quote_block(
            vec![
                quote("q1", "Mem0"),
                second,
                quote("q3", "Claude"),
                quote("q4", "Zep"),
            ],
            vec!["q4"],
        );
        let partition = partition_quotes(&block);
        assert_eq!(
            partition.shown,
            vec![1, 3],
            "card order, not key-list order"
        );
        assert_eq!(partition.hidden, vec![0, 2]);
        assert_eq!(
            partition.products,
            vec!["Claude".to_string(), "Mem0".to_string()]
        );
        assert!(partition.more);
        assert_eq!(
            quote_fold_label(partition.hidden.len(), &partition.products, partition.more),
            "2 more quotes from Claude, Mem0"
        );
    }

    #[test]
    fn a_tap_expands_the_fold_and_a_second_tap_collapses_it() {
        let owner = Owner::new();
        owner.set();
        let expanded = RwSignal::new(false);
        assert!(!expanded.get(), "folded by default");
        expanded.update(|open| *open = !*open);
        assert!(expanded.get(), "expanded on tap");
        expanded.update(|open| *open = !*open);
        assert!(!expanded.get(), "collapsed on the next tap");
    }
}
