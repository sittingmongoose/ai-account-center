//! A card's optional sections. A card carries only what its question needs; each type here draws one kind of
//! section. A type this page doesn't know is shown as such, never dropped silently.

use crate::api::{Block, Media};
use crate::media::MediaList;
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

fn body_of(block: &Block) -> AnyView {
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

#[component]
pub fn Blocks(blocks: Vec<Block>) -> impl IntoView {
    blocks
        .into_iter()
        .map(|block| {
            let kind = block.kind.clone();
            let title = block
                .title
                .clone()
                .or_else(|| default_title(&kind).map(str::to_string));
            let body = body_of(&block);
            if block.fold {
                view! {
                    <details class="block" data-type=kind>
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
