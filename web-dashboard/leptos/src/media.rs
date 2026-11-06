//! Screenshots and videos on cards. Every image opens full screen when tapped (tap again to see it at full size);
//! every video plays inline and has a Full screen button (on an iPhone that is the system video player).

use crate::api::Media;
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

/// Video full screen: the iPhone only offers its own player (`webkitEnterFullscreen`); elsewhere the standard API.
fn video_full_screen(video: &web_sys::HtmlVideoElement) {
    let target: &wasm_bindgen::JsValue = video.as_ref();
    if let Ok(f) = js_sys::Reflect::get(target, &"webkitEnterFullscreen".into()) {
        if let Some(f) = f.dyn_ref::<js_sys::Function>() {
            let _ = f.call0(target);
            return;
        }
    }
    let _ = video.request_fullscreen();
}

#[component]
fn MediaItem(media: Media) -> impl IntoView {
    let viewer = expect_context::<Viewer>();
    let caption = caption_line(&media);
    if media.kind == "video" {
        let video_ref = NodeRef::<leptos::html::Video>::new();
        view! {
            <figure class="media" data-kind="video">
                <video node_ref=video_ref src=media.url.clone() poster=media.poster_url.clone().unwrap_or_default()
                    controls=true playsinline=true preload="metadata"></video>
                <figcaption>
                    <span>{caption}</span>
                    <button class="link" on:click=move |_| {
                        if let Some(v) = video_ref.get_untracked() {
                            video_full_screen(&v);
                        }
                    }>"Full screen"</button>
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
        let open = media.clone();
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

/// The full-screen image viewer, once per page. Tap the image to switch between fit-to-screen and full size;
/// the close button, Escape or a tap outside the image closes it.
#[component]
pub fn ViewerOverlay() -> impl IntoView {
    let viewer = expect_context::<Viewer>();
    let zoomed = RwSignal::new(false);
    let close = move || {
        viewer.open.set(None);
        zoomed.set(false);
    };
    let _ = window_event_listener(leptos::ev::keydown, move |e| {
        if e.key() == "Escape" && viewer.open.with_untracked(|m| m.is_some()) {
            close();
        }
    });
    // Keep the page behind still while the viewer is open.
    Effect::new(move |_| {
        let open = viewer.open.with(|m| m.is_some());
        if let Some(body) = document().body() {
            let _ = body
                .style()
                .set_property("overflow", if open { "hidden" } else { "" });
        }
    });
    view! {
        <Show when=move || viewer.open.with(|m| m.is_some())>
            {move || viewer.open.get().map(|m| {
                let caption = caption_line(&m);
                view! {
                    <div class="viewer" role="dialog" aria-modal="true" aria-label="Screenshot" on:click=move |_| close()>
                        <button class="viewer-close" aria-label="Close" on:click=move |e| { e.stop_propagation(); close(); }>"Close"</button>
                        <div class="viewer-scroll" data-zoomed=move || zoomed.get().then_some("1")>
                            <img src=m.url.clone() alt=caption.clone() on:click=move |e| { e.stop_propagation(); zoomed.update(|z| *z = !*z); }/>
                        </div>
                        {(!caption.is_empty()).then(|| view! { <p class="viewer-caption">{caption.clone()}</p> })}
                        <p class="viewer-hint">{move || if zoomed.get() { "Tap the image to fit it to the screen" } else { "Tap the image to see it at full size" }}</p>
                    </div>
                }
            })}
        </Show>
    }
}
