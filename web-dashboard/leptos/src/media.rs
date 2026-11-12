//! Screenshots and videos on cards. Tapping a screenshot, or Full screen on a video, opens the page's own full-screen
//! viewer: wide media on an upright phone opens turned sideways so it fills the screen (Rotate switches), a tap on a
//! screenshot shows it at full size to pan around, and a video plays there with the system player one tap away.
//! The viewer works the same inside the T3 app, the installed AAC app and a browser, whatever the host's own
//! rotation lock.

use crate::api::Media;
use crate::ui::{OverlayPortal, hold_order};
use leptos::prelude::*;
use wasm_bindgen::JsCast;

#[derive(Clone, Copy)]
pub struct Viewer {
    pub open: RwSignal<Option<Media>>,
}

pub fn provide_viewer() {
    provide_context(Viewer {
        open: RwSignal::new(None),
    });
}

fn label_words(media: &Media) -> Option<&'static str> {
    match media.label.as_deref() {
        Some("built") => Some("Built"),
        Some("mockup") => Some("Mockup"),
        _ => None,
    }
}

fn caption_line(media: &Media) -> String {
    [
        label_words(media).map(str::to_string),
        media.caption.clone().filter(|c| !c.is_empty()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

#[component]
pub fn MediaList(items: Vec<Media>) -> impl IntoView {
    (!items.is_empty()).then(|| {
        view! { <div class="media-list">{items.into_iter().map(|m| view! { <MediaItem media=m/> }).collect_view()}</div> }
    })
}

/// Live reports keep media elements and native playback while captions or sibling data change.
#[component]
pub fn LiveMediaList(
    items: Memo<Vec<Media>>,
    #[prop(optional)] limit: Option<Memo<usize>>,
) -> impl IntoView {
    let available = Memo::new(move |_| {
        items.with(|items| {
            let mut seen = std::collections::BTreeMap::new();
            items
                .iter()
                .map(|m| {
                    let key = (m.kind.clone(), m.url.clone());
                    let occurrence = seen.entry(key.clone()).or_insert(0usize);
                    let value = (key.0, key.1, *occurrence);
                    *occurrence += 1;
                    value
                })
                .collect::<Vec<_>>()
        })
    });
    let (keys, order_id) = hold_order(available, limit);
    view! {
        <div class="media-list" data-focus-list=order_id>
            <For each=move || keys.get() key=|key| key.clone() children=move |(kind, url, occurrence)| {
                let identity = format!("media:{}", serde_json::to_string(&(kind.clone(), url.clone(), occurrence)).unwrap_or_default());
                let media = Memo::new(move |_| items.with(|items| items.iter().filter(|m| m.kind == kind && m.url == url).nth(occurrence).cloned().unwrap_or_default()));
                view! { <LiveMediaItem media=media identity=identity/> }
            }/>
        </div>
    }
}

#[component]
fn LiveMediaItem(media: Memo<Media>, identity: String) -> impl IntoView {
    let viewer = expect_context::<Viewer>();
    // Kind and URL are the list key; assigning src once prevents a metadata poll from reloading the player.
    let initial = media.get_untracked();
    let caption = Memo::new(move |_| media.with(caption_line));
    if initial.kind == "video" {
        view! {
            <figure class="media" data-kind="video" data-focus-key=identity>
                <video data-focus-role="media-player" src=initial.url poster=move || media.with(|m| m.poster_url.clone().unwrap_or_default())
                    controls=true playsinline=true preload="metadata"></video>
                <figcaption><span>{move || caption.get()}</span>
                    <button class="link" data-focus-role="media-enlarge" on:click=move |_| viewer.open.set(Some(media.get_untracked()))>"Full screen"</button>
                </figcaption>
            </figure>
        }.into_any()
    } else {
        let alt = Memo::new(move |_| {
            let caption = caption.get();
            if caption.is_empty() {
                "Screenshot".into()
            } else {
                caption
            }
        });
        view! {
            <figure class="media" data-kind="image" data-focus-key=identity>
                <button class="media-open" data-focus-role="media-enlarge" aria-label=move || format!("Enlarge: {}", alt.get()) on:click=move |_| viewer.open.set(Some(media.get_untracked()))>
                    <img src=initial.url alt=move || alt.get() loading="lazy"/>
                    <span class="enlarge" aria-hidden="true">"Tap to enlarge"</span>
                </button>
                <Show when=move || !caption.with(String::is_empty)><figcaption><span>{move || caption.get()}</span></figcaption></Show>
            </figure>
        }.into_any()
    }
}

/// The iPhone only offers its own player for a video (`webkitEnterFullscreen`); elsewhere the standard API.
fn system_full_screen(video: &web_sys::HtmlVideoElement) {
    let target: &wasm_bindgen::JsValue = video.as_ref();
    if let Ok(f) = js_sys::Reflect::get(target, &"webkitEnterFullscreen".into()) {
        if let Some(f) = f.dyn_ref::<js_sys::Function>() {
            if f.call0(target).is_ok() {
                return;
            }
        }
    }
    let _ = video.request_fullscreen();
}

#[component]
fn MediaItem(media: Media) -> impl IntoView {
    let viewer = expect_context::<Viewer>();
    let caption = caption_line(&media);
    let open = media.clone();
    if media.kind == "video" {
        view! {
            <figure class="media" data-kind="video">
                <video src=media.url.clone() poster=media.poster_url.clone().unwrap_or_default()
                    controls=true playsinline=true preload="metadata"></video>
                <figcaption>
                    <span>{caption}</span>
                    <button class="link" on:click=move |_| viewer.open.set(Some(open.clone()))>"Full screen"</button>
                </figcaption>
            </figure>
        }
        .into_any()
    } else {
        let alt = if caption.is_empty() {
            "Screenshot".to_string()
        } else {
            caption.clone()
        };
        let label = format!("Enlarge: {alt}");
        view! {
            <figure class="media" data-kind="image">
                <button class="media-open" aria-label=label on:click=move |_| viewer.open.set(Some(open.clone()))>
                    <img src=media.url.clone() alt=alt loading="lazy"/>
                    <span class="enlarge" aria-hidden="true">"Tap to enlarge"</span>
                </button>
                {(!caption.is_empty()).then(|| view! { <figcaption><span>{caption}</span></figcaption> })}
            </figure>
        }
        .into_any()
    }
}

fn upright_screen() -> bool {
    let w = window()
        .inner_width()
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let h = window()
        .inner_height()
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    h > w
}

/// The full-screen viewer, once per page. Its video is muted through the property as well as the attribute: the
/// attribute only mutes an element that is created with it, and the page sets attributes after creating the element,
/// so without the property the video starts with sound and phones refuse to autoplay it.
#[component]
pub fn ViewerOverlay() -> impl IntoView {
    let viewer = expect_context::<Viewer>();
    // The viewer goes with the signed-in page tree. Its shared selection must not survive that owner's unmount.
    on_cleanup(move || {
        viewer.open.try_set(None);
    });
    // Turned sideways (fills an upright screen with wide media); showing a screenshot at full size.
    let rotated = RwSignal::new(false);
    let actual = RwSignal::new(false);
    let decided = RwSignal::new(false);
    let close = move || {
        viewer.open.set(None);
        rotated.set(false);
        actual.set(false);
        decided.set(false);
    };
    // Wide media opens sideways on an upright screen, once its size is known.
    let fit_to_screen = move |w: f64, h: f64| {
        if !decided.get_untracked() && w > 0.0 && h > 0.0 {
            decided.set(true);
            rotated.set(w > h * 1.15 && upright_screen());
        }
    };
    let on_close = Callback::new(move |_| close());
    let video_ref = NodeRef::<leptos::html::Video>::new();
    view! {
        <Show when=move || viewer.open.with(|m| m.is_some())>
            {move || viewer.open.get().map(|m| {
                let caption = caption_line(&m);
                let is_video = m.kind == "video";
                view! {
                    <OverlayPortal on_close=on_close>
                    <div class="viewer" role="dialog" aria-modal="true" aria-label=if is_video { "Video" } else { "Screenshot" }
                        data-rotated=move || rotated.get().then_some("1") data-actual=move || actual.get().then_some("1")>
                        <div class="viewer-bar">
                            <button class="viewer-btn" on:click=move |_| { actual.set(false); rotated.update(|r| *r = !*r); }>"Rotate"</button>
                            {is_video.then(|| view! {
                                <button class="viewer-btn" on:click=move |_| { if let Some(v) = video_ref.get_untracked() { system_full_screen(&v); } }>"System player"</button>
                            })}
                            <button class="viewer-btn close" on:click=move |_| close()>"Close"</button>
                        </div>
                        <div class="viewer-stage" on:click=move |_| { if !is_video && actual.get_untracked() { actual.set(false); } }>
                            <div class="viewer-frame">
                                {if is_video {
                                    view! {
                                        <video node_ref=video_ref src=m.url.clone() poster=m.poster_url.clone().unwrap_or_default()
                                            controls=true playsinline=true autoplay=true muted=true prop:muted=true preload="auto"
                                            on:loadedmetadata=move |ev| {
                                                let v: web_sys::HtmlVideoElement = event_target(&ev);
                                                fit_to_screen(v.video_width() as f64, v.video_height() as f64);
                                            }></video>
                                    }.into_any()
                                } else {
                                    view! {
                                        <img src=m.url.clone() alt=caption.clone()
                                            on:load=move |ev| {
                                                let i: web_sys::HtmlImageElement = event_target(&ev);
                                                fit_to_screen(i.natural_width() as f64, i.natural_height() as f64);
                                            }
                                            on:click=move |e| { e.stop_propagation(); actual.update(|a| *a = !*a); }/>
                                    }.into_any()
                                }}
                            </div>
                        </div>
                        <p class="viewer-caption">
                            {(!caption.is_empty()).then(|| view! { <span>{caption.clone()}</span> })}
                            <span class="viewer-hint">{move || match (is_video, actual.get()) {
                                (true, _) => "Sound is off until you turn it on in the player.",
                                (false, true) => "Drag to look around. Tap to fit it to the screen again.",
                                (false, false) => "Tap the picture to see it at full size.",
                            }}</span>
                        </p>
                    </div>
                    </OverlayPortal>
                }
            })}
        </Show>
    }
}
