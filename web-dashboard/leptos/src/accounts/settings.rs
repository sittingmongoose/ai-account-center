//! The settings cards (Slint `ac-settings.slint`): Sign-in & connection, Settings, History snapshots, Usage log sources
//! and About. Every word and state comes from the view (`signinFacts`, `policies`, `timezoneView`, `cleanupView`,
//! `logSourcesView`); the controls send the same kinds and values the Slint page sent.
//!
//! The change-password fields are real password inputs (so a password manager can fill and save them) and stay here
//! until they are submitted; they are cleared when the form closes.

use super::Page;
use super::model::{Fact, Policy, Signin};
use super::provider::{Choices, Stepper};
use super::{Act, CardHead};
use crate::ui::*;
use leptos::prelude::*;

fn signin(page: Page) -> Memo<Signin> {
    Memo::new(move |_| {
        page.view
            .with(|v| v.as_ref().map(|v| v.signin.clone()).unwrap_or_default())
    })
}

/// One fact: its label in ink-3 and the value.
fn fact(label: &'static str, value: String, strong: bool, title: Option<String>) -> impl IntoView {
    view! {
        <dt>{label}</dt>
        <dd class:strong=strong title=title>{value}</dd>
    }
}

#[component]
pub fn SigninCard(page: Page) -> impl IntoView {
    let s = signin(page);
    let busy = page.busy;
    let connection = Memo::new(move |_| {
        page.view
            .with(|v| v.as_ref().map(|v| v.connection.clone()).unwrap_or_default())
    });
    view! {
        <section class="plate ac-card set" data-probe="signin-card">
            <CardHead title="Sign-in & connection"/>
            <div class="set-body">
                <dl class="facts ac-facts">
                    {move || {
                        let v = s.get();
                        let session = if v.session_sub.is_empty() { v.session.clone() } else { format!("{} · {}", v.session, v.session_sub) };
                        view! {
                            {fact("Signed in as", v.username.clone(), true, None)}
                            {fact("Connection", v.connection.clone(), false, None)}
                            {fact("This session", session, false, Some(v.session_tip.clone()))}
                            {fact("Other browsers", v.others_text.clone(), false, None)}
                        }
                    }}
                </dl>
                <div class="set-line">
                    <button type="button" class="btn sm" data-probe="others-out"
                        title=move || s.with(|v| v.others_tip.clone())
                        disabled=move || !s.with(|v| v.others_enabled) || busy.get()
                        on:click=move |_| page.send("others-out", "")>
                        {move || s.with(|v| v.others_busy).then(|| view! { <Spinner/> })}
                        {move || if s.with(|v| v.others_busy) { "Signing out" } else { "Sign out other browsers" }}
                    </button>
                </div>
            </div>
            <div class="set-item">
                <p class="si-head"><span class="si-label">"Session lifetime"</span>
                    <span class="si-aside q3">{move || if s.with(|v| v.lifetime.busy) { "Saving" } else { "" }}</span></p>
                {move || {
                    let l = s.with(|v| v.lifetime.clone());
                    let items = [("1", "1 day"), ("7", "7 days"), ("30", "30 days"), ("90", "90 days"), ("365", "1 year")]
                        .into_iter().map(|(v, t)| super::model::Choice { value: v.into(), label: t.into() }).collect();
                    view! {
                        <div title=l.tip.clone() data-probe="session-lifetime">
                            <Choices items=items value=l.value.clone() enabled=l.enabled && !busy.get()
                                send=Callback::new(move |v: String| page.send("session-lifetime", &v))/>
                        </div>
                        <p class="si-note q3 touch-only">{l.tip.clone()}</p>
                    }
                }}
            </div>
            <div class="set-item">
                <p class="si-head"><span class="si-label">"Local network"</span>
                    {move || {
                        let n = s.with(|v| v.network.clone());
                        (!n.act.is_empty()).then(|| {
                            let act = n.act.clone();
                            view! {
                                <button type="button" class=if n.on { "btn sm" } else { "btn sm accent-line" } data-probe="network"
                                    title=n.tip.clone() disabled=!n.act_enabled || busy.get()
                                    on:click=move |_| page.send(&act, "")>
                                    {n.busy.then(|| view! { <Spinner/> })}{n.act_label.clone()}
                                </button>
                            }
                        })
                    }}
                </p>
                <p class="si-value tnum">{move || s.with(|v| v.network.line.clone())}</p>
                <p class="icon-note q3">
                    <Icon name="lock"/>
                    <span>{move || s.with(|v| v.network.note.clone())}</span>
                </p>
            </div>
            <div class="set-item">
                <p class="si-head"><span class="si-label">"Connection"</span></p>
                <dl class="facts ac-facts">
                    {move || connection.get().into_iter().map(|f: Fact| view! {
                        <dt>{f.label.clone()}</dt><dd class:mono=f.mono>{f.value.clone()}</dd>
                    }).collect_view()}
                </dl>
            </div>
            <PasswordItem page=page s=s/>
            <DevicesItem page=page s=s/>
            <div class="set-item">
                <p class="icon-note"><Icon name="info"/><span>{move || runs_view(s.with(|v| v.pairing_note.clone()))}</span></p>
                <div class="set-line">
                    <button type="button" class="btn sm" data-probe="logout" disabled=move || busy.get()
                        on:click=move |_| page.ctx.sign_out()>
                        {move || if s.with(|v| v.sign_out_busy) { view! { <Spinner/> }.into_any() } else { view! { <Icon name="logout"/> }.into_any() }}
                        {move || if s.with(|v| v.sign_out_busy) { "Signing out" } else { "Sign out" }}
                    </button>
                </div>
            </div>
        </section>
    }
}

/// Change password: the toggle, then the current, new and confirmation fields with the strength line, "Also sign out
/// other browsers", and the button that turns busy, then done. A refused change shakes the form and names the field.
#[component]
fn PasswordItem(page: Page, s: Memo<Signin>) -> impl IntoView {
    let cur = RwSignal::new(String::new());
    let new = RwSignal::new(String::new());
    let conf = RwSignal::new(String::new());
    let others = RwSignal::new(true);
    let open = Memo::new(move |_| s.with(|v| v.password_open));
    let busy_pw = Memo::new(move |_| s.with(|v| v.password_busy));
    // the fields go when the form closes
    Effect::new(move |_| {
        if !open.get() {
            cur.set(String::new());
            new.set(String::new());
            conf.set(String::new());
            others.set(true);
        }
    });
    let nonce = Memo::new(move |_| s.with(|v| v.password_nonce));
    let shake = RwSignal::new(false);
    Effect::new(move |prev: Option<u64>| {
        let n = nonce.get();
        if prev.is_some_and(|p| p != n) {
            shake.set(false);
            let again = wasm_bindgen::closure::Closure::once_into_js(move || {
                let _ = shake.try_set(true);
            });
            let _ = window().request_animation_frame(wasm_bindgen::JsCast::unchecked_ref(&again));
        }
        n
    });
    let dash = page.ctx.dash;
    let strength = Memo::new(move |_| {
        let pushed = dash
            .strength
            .with(|j| serde_json::from_str::<super::model::Strength>(j).ok());
        pushed.unwrap_or_else(|| s.with(|v| v.strength.clone()))
    });
    let submit = move || {
        page.send(
            "pw-submit",
            &format!(
                "{}\n{}\n{}\n{}",
                cur.get_untracked(),
                new.get_untracked(),
                conf.get_untracked(),
                if others.get_untracked() { "1" } else { "0" }
            ),
        );
    };
    let field =
        Memo::new(move |_| s.with(|v| (v.password_field.clone(), v.password_error.clone())));
    let err = move |name: &'static str| {
        move || {
            field.with(|(f, e)| {
                (f == name && !e.is_empty())
                    .then(|| view! { <p class="field-err w-crit">{e.clone()}</p> })
            })
        }
    };
    view! {
        <div class="set-item">
            <p class="si-head"><span class="si-label">"Password"</span>
                <span class="si-aside q3">{move || s.with(|v| v.password_when.clone())}</span>
                <button type="button" class=move || if open.get() { "btn sm ghost" } else { "btn sm" } data-probe="pw-toggle"
                    title=move || s.with(|v| if v.password_can { String::new() } else { v.password_note.clone() })
                    disabled=move || !s.with(|v| v.password_can) || busy_pw.get()
                    on:click=move |_| page.send("pw-toggle", "")>
                    {move || if open.get() { "Close" } else { "Change password" }}
                </button>
            </p>
            {move || s.with(|v| (!v.password_can && !v.password_note.is_empty()).then(|| view! {
                <p class="icon-note q3"><Icon name="lock"/><span>{v.password_note.clone()}</span></p>
            }))}
            <Show when=move || open.get()>
                <form class="pw-form" class:shake=move || shake.get() autocomplete="on"
                    on:submit=move |e: web_sys::SubmitEvent| { e.prevent_default(); submit(); }
                    on:animationend=move |_| shake.set(false)>
                    // the account name, for password managers (they save the new password under it)
                    <input type="text" class="vh" name="username" autocomplete="username" tabindex="-1" aria-hidden="true"
                        prop:value=move || s.with(|v| v.username.clone()) readonly=true/>
                    <label class="field">
                        <span class="field-label">"Current password"</span>
                        <input class="field-input" type="password" autocomplete="current-password" data-probe="pw-cur"
                            class:bad=move || field.with(|f| f.0 == "cur") disabled=move || busy_pw.get()
                            prop:value=move || cur.get() on:input=move |e| cur.set(event_target_value(&e))/>
                    </label>
                    {err("cur")}
                    <label class="field">
                        <span class="field-label">"New password"</span>
                        <input class="field-input" type="password" autocomplete="new-password" data-probe="pw-new"
                            class:bad=move || field.with(|f| f.0 == "new") disabled=move || busy_pw.get()
                            prop:value=move || new.get()
                            on:input=move |e| { new.set(event_target_value(&e)); page.send("pw-typing", &format!("{}\n{}", new.get_untracked(), conf.get_untracked())); }/>
                    </label>
                    {move || {
                        let st = strength.get();
                        let tone = if st.lv >= 4 { "calm" } else if st.lv == 3 { "warn" } else if st.lv > 0 { "crit" } else { "" };
                        let word = if st.lv >= 4 { "w-good" } else if st.lv == 3 { "w-warn" } else if st.lv > 0 { "w-crit" } else { "q3" };
                        view! {
                            <div class="strength">
                                <div class="track thin"><div class=format!("fill {tone}") style=format!("width:{:.0}%", st.pct.clamp(0.0, 100.0))></div></div>
                                <p class="st-line"><span class="q3">{st.hint.clone()}</span><span class=word>{st.word.clone()}</span></p>
                            </div>
                        }
                    }}
                    {err("new")}
                    <label class="field">
                        <span class="field-label">"Confirm new password"
                            {move || strength.with(|s| s.matches).then(|| view! { <span class="w-good">" Matches"</span> })}</span>
                        <input class="field-input" type="password" autocomplete="new-password" data-probe="pw-conf"
                            class:bad=move || field.with(|f| f.0 == "conf") disabled=move || busy_pw.get()
                            prop:value=move || conf.get()
                            on:input=move |e| { conf.set(event_target_value(&e)); page.send("pw-typing", &format!("{}\n{}", new.get_untracked(), conf.get_untracked())); }/>
                    </label>
                    {err("conf")}
                    <CheckLine checked=others enabled=Signal::derive(move || !busy_pw.get()) text="Also sign out other browsers".to_string()
                        sub=Signal::derive(move || s.with(|v| v.password_others.clone())) probe="pw-others"/>
                    <div class="pw-acts">
                        <button type="submit" class=move || if s.with(|v| v.password_done) { "btn" } else { "btn primary" } data-probe="pw-submit"
                            disabled=move || s.with(|v| v.password_done)>
                            {move || if busy_pw.get() { Some(view! { <Spinner/> }.into_any()) } else if s.with(|v| v.password_done) { Some(view! { <Icon name="check"/> }.into_any()) } else { None }}
                            {move || if s.with(|v| v.password_done) { "Password changed" } else if busy_pw.get() { "Changing" } else { "Change password" }}
                        </button>
                        <button type="button" class="btn ghost" data-probe="pw-cancel" disabled=move || busy_pw.get()
                            on:click=move |_| page.send("pw-toggle", "")>"Cancel"</button>
                    </div>
                </form>
            </Show>
        </div>
    }
}

/// Paired tray apps: each with Revoke and its own confirmation, then Sign out all devices with one.
#[component]
fn DevicesItem(page: Page, s: Memo<Signin>) -> impl IntoView {
    let busy = page.busy;
    let revoke_id = RwSignal::new(String::new());
    let all_open = RwSignal::new(false);
    let count = Memo::new(move |_| s.with(|v| v.devices.len()));
    Effect::new(move |_| {
        count.track();
        revoke_id.set(String::new());
    });
    view! {
        <div class="set-item">
            <p class="si-head"><span class="si-label">"Paired tray apps"</span></p>
            {move || {
                let devices = s.with(|v| v.devices.clone());
                if devices.is_empty() {
                    return view! { <p class="icon-note q3"><Icon name="link"/><span>{s.with(|v| v.devices_note.clone())}</span></p> }.into_any();
                }
                view! {
                    <ul class="devices">
                        {devices.into_iter().map(|d| {
                            let id = d.id.clone();
                            let id2 = d.id.clone();
                            let did = StoredValue::new(d.id.clone());
                            let confirming = Memo::new(move |_| revoke_id.get() == id);
                            view! {
                                <li class="device" title=d.tip.clone() data-probe=d.probe.clone()>
                                    <div class="dv-row">
                                        {(!d.platform.is_empty()).then(|| view! { <PlatformGlyph id=if d.platform == "apple" { "mac".to_string() } else { d.platform.clone() }/> })}
                                        <span class="dv-text"><b>{d.name.clone()}</b><span class="q3 tnum">{d.sub.clone()}</span></span>
                                        <button type="button" class="btn sm" data-probe=format!("revoke:{}", d.id)
                                            disabled=move || busy.get() || confirming.get()
                                            on:click=move |_| { revoke_id.set(id2.clone()); all_open.set(false); }>"Revoke"</button>
                                    </div>
                                    <Show when=move || confirming.get()>
                                        <div class="row-line confirm">
                                            <Icon name="alert"/>
                                            <p class="rl-text">"It shows its pairing screen until someone signs in on it again."</p>
                                            <span class="rl-acts">
                                                <button type="button" class="btn sm danger-solid" data-probe=move || format!("revoke-do:{}", did.get_value())
                                                    on:click=move |_| page.send("device-revoke", &did.get_value())>
                                                    {move || (s.with(|v| v.revoke_busy == did.get_value())).then(|| view! { <Spinner/> })}"Revoke"
                                                </button>
                                                <button type="button" class="btn sm ghost" on:click=move |_| revoke_id.set(String::new())>"Cancel"</button>
                                            </span>
                                        </div>
                                    </Show>
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                }.into_any()
            }}
            <div class="set-line">
                <button type="button" class="btn sm danger-text" data-probe="all-ask"
                    disabled=move || !s.with(|v| v.revoke_all_enabled) || all_open.get() || busy.get()
                    on:click=move |_| { all_open.set(true); revoke_id.set(String::new()); }>
                    <Icon name="logout"/>"Sign out all devices"
                </button>
            </div>
            <Show when=move || all_open.get()>
                <div class="row-line confirm">
                    <Icon name="alert"/>
                    <p class="rl-text"><b>"Sign out every tray and every other browser?"</b>" Each tray shows its pairing screen. This browser stays signed in."</p>
                    <span class="rl-acts">
                        <button type="button" class="btn sm danger-solid" data-probe="all-do"
                            on:click=move |_| { page.send("devices-revoke-all", ""); all_open.set(false); }>
                            {move || s.with(|v| v.revoke_all_busy).then(|| view! { <Spinner/> })}"Sign out all devices"
                        </button>
                        <button type="button" class="btn sm ghost" data-probe="all-do:cancel"
                            disabled=move || s.with(|v| v.revoke_all_busy) on:click=move |_| all_open.set(false)>"Cancel"</button>
                    </span>
                </div>
            </Show>
        </div>
    }
}

#[component]
pub fn SettingsCard(page: Page) -> impl IntoView {
    let busy = page.busy;
    let ctx = page.ctx;
    let refresh = Memo::new(move |_| {
        page.view
            .with(|v| v.as_ref().map(|v| v.refresh.clone()).unwrap_or_default())
    });
    let tz = Memo::new(move |_| {
        page.view
            .with(|v| v.as_ref().map(|v| v.timezone.clone()).unwrap_or_default())
    });
    let install = Memo::new(move |_| {
        page.view
            .with(|v| v.as_ref().map(|v| v.install.clone()).unwrap_or_default())
    });
    let policies = Memo::new(move |_| {
        page.view
            .with(|v| v.as_ref().map(|v| v.policies.clone()).unwrap_or_default())
    });
    let shown = RwSignal::new(0_u32);
    view! {
        <section class="plate ac-card set" data-probe="settings-card">
            <CardHead title="Settings"/>
            <div class="set-item first">
                <p class="si-head"><span class="si-label">"Appearance"</span><span class="si-aside q3">"Auto follows the system"</span></p>
                <div class="seg ac-seg" role="radiogroup" aria-label="Appearance">
                    {[("light", "Light", "sun"), ("dark", "Dark", "moon"), ("auto", "Auto", "monitor")].into_iter().map(|(v, t, icon)| view! {
                        <button type="button" role="radio" class:on=move || ctx.theme.get() == v
                            aria-checked=move || if ctx.theme.get() == v { "true" } else { "false" }
                            on:click=move |_| ctx.set_theme(v)><Icon name=icon/>{t}</button>
                    }).collect_view()}
                </div>
            </div>
            {move || match install.get().as_str() {
                "chromium" => Some(view! {
                    <div class="set-item">
                        <p class="si-head"><span class="si-label">"Home screen app"</span></p>
                        <p class="q3">"Install AI Account Center as an app on this device."</p>
                        <div class="set-line"><button type="button" class="btn sm" data-probe="install-app" on:click=move |_| page.send("install-app", "")>"Install app"</button></div>
                    </div>
                }.into_any()),
                "ios" => Some(view! {
                    <div class="set-item">
                        <p class="si-head"><span class="si-label">"Home screen app"</span></p>
                        <p class="q3 inline-icon">"In Safari, open the Share menu "<Icon name="share"/>", then choose Add to Home Screen."</p>
                    </div>
                }.into_any()),
                _ => None,
            }}
            <div class="set-item">
                <p class="si-head"><span class="si-label">"Usage refresh"</span>
                    <span class="si-aside" class:q3=move || !refresh.with(|r| r.known)>{move || {
                        let r = refresh.get();
                        if !r.known { return "Not reported".to_string(); }
                        let s = shown.get();
                        format!("Every {}", interval_label(if s > 0 { s } else { r.seconds }))
                    }}</span></p>
                <RefreshSlider seconds=Signal::derive(move || refresh.with(|r| r.seconds))
                    enabled=Signal::derive(move || refresh.with(|r| r.known) && !busy.get())
                    shown=shown
                    commit=Callback::new(move |s: u32| page.send("refresh-interval", &format!("{s}s")))/>
                {move || (!refresh.with(|r| r.known)).then(|| view! { <p class="q3">"The saved interval could not be read, so it cannot be changed here."</p> })}
            </div>
            <div class="set-item">
                <p class="si-head"><span class="si-label">"Time zone"</span>
                    <span class="si-aside q3">{move || if tz.with(|t| t.busy) { "Saving" } else { "" }}</span></p>
                <select class="ac-select" data-probe="time-zone" title=move || tz.with(|t| t.tip.clone())
                    disabled=move || !tz.with(|t| t.enabled) || busy.get()
                    prop:value=move || tz.with(|t| t.value.clone())
                    on:change=move |e| page.send("time-zone", &event_target_value(&e))>
                    {move || tz.get().options.into_iter().map(|o| { let v = o.clone(); view! { <option value=v>{o}</option> } }).collect_view()}
                </select>
            </div>
            <div class="set-item">
                <p class="si-head"><span class="si-label">"Auto-switch policies"</span></p>
                <div class="policies">
                    <For each={move || policies.get().into_iter().map(|p| p.provider).collect::<Vec<_>>()} key=|p| p.clone() children=move |provider| {
                        let p = Memo::new(move |_| policies.with(|ps| ps.iter().find(|x| x.provider == provider).cloned().unwrap_or_default()));
                        view! { <PolicyRow page=page p=p/> }
                    }/>
                </div>
            </div>
        </section>
    }
}

/// One policy: the provider, its Auto-switch toggle and "at [%] used" (Codex: `automatic`, `threshold`; Antigravity:
/// `antigravity-automatic`, `antigravity-threshold`), with the stuck-switch sentence under it.
#[component]
fn PolicyRow(page: Page, p: Memo<Policy>) -> impl IntoView {
    let busy = page.busy;
    let ag = Memo::new(move |_| p.with(|p| p.provider == "antigravity"));
    let reason = RwSignal::new(String::new());
    view! {
        <div class="policy" class:off=move || !p.with(|p| p.toggle_enabled) title=move || p.with(|p| p.tip.clone())>
            <div class="pol-line">
                <span class="pol-name"><ProviderMark provider=p.with_untracked(|p| p.provider.clone()) size=18/><b>{move || p.with(|p| p.name.clone())}</b></span>
                <Toggle aria=format!("{} auto-switch", p.with_untracked(|p| p.name.clone()))
                    on=Signal::derive(move || p.with(|p| p.enabled))
                    enabled=Signal::derive(move || p.with(|p| p.toggle_enabled) && !busy.get())
                    tip=Signal::derive(move || p.with(|p| p.tip.clone()))
                    probe=format!("policy:{}", p.with_untracked(|p| p.provider.clone()))
                    on_toggle=Callback::new(move |on: bool| page.send(if ag.get_untracked() { "antigravity-automatic" } else { "automatic" }, if on { "true" } else { "false" }))
                    on_reason=Callback::new(move |_| reason.set(p.with_untracked(|p| p.tip.clone())))/>
                <span class="pol-at">
                    <span class="q3">"at"</span>
                    <Stepper reported=Signal::derive(move || p.with(|p| p.threshold))
                        bounds=Signal::derive(move || p.with(|p| (p.min, p.max)))
                        enabled=Signal::derive(move || p.with(|p| p.step_enabled && p.known) && !busy.get())
                        probe="policy-threshold"
                        send=Callback::new(move |v: i32| page.send(if ag.get_untracked() { "antigravity-threshold" } else { "threshold" }, &format!("{v}%")))/>
                    <span class="q3">"used"</span>
                </span>
            </div>
            {move || p.with(|p| (!p.sub.is_empty()).then(|| view! { <p class="pol-sub" class:w-warn=p.warn class:q3=!p.warn>{p.sub.clone()}</p> }))}
            {move || (!reason.get().is_empty()).then(|| view! { <p class="pol-sub q3 touch-only">{reason.get()}</p> })}
        </div>
    }
}

/// "30 s", "2 min", "2 min 30 s" (view-model.mjs `intervalLabel`).
pub fn interval_label(seconds: u32) -> String {
    if seconds < 60 {
        format!("{seconds} s")
    } else if seconds % 60 == 0 {
        format!("{} min", seconds / 60)
    } else {
        format!("{} min {} s", seconds / 60, seconds % 60)
    }
}

const SNAPS: [u32; 8] = [30, 60, 120, 300, 600, 900, 1800, 3600];

/// The slider's position (0..1) on its log scale (accounts-view.mjs `refreshPosition`).
pub fn refresh_position(seconds: u32) -> f64 {
    let (lo, hi) = ((30f64).ln(), (3600f64).ln());
    ((seconds.clamp(30, 3600) as f64).ln() - lo) / (hi - lo)
}

/// A position to whole seconds, snapping to the marks within 3.5% of the track (`refreshFromPosition`).
pub fn refresh_from_position(x: f64) -> u32 {
    let x = x.clamp(0.0, 1.0);
    if let Some(s) = SNAPS
        .iter()
        .find(|s| (refresh_position(**s) - x).abs() < 0.035)
    {
        return *s;
    }
    let (lo, hi) = ((30f64).ln(), (3600f64).ln());
    ((lo + x * (hi - lo)).exp().round() as u32).clamp(30, 3600)
}

/// The usage refresh slider: 30 s to 60 min on a log scale, marks at the snap points, saved on release. The value
/// dragged to shows until the saved one arrives (or for 5 s).
#[component]
fn RefreshSlider(
    #[prop(into)] seconds: Signal<u32>,
    #[prop(into)] enabled: Signal<bool>,
    shown: RwSignal<u32>,
    commit: Callback<u32>,
) -> impl IntoView {
    let dragging = RwSignal::new(false);
    let serial = RwSignal::new(0_u64);
    Effect::new(move |_| {
        seconds.track();
        if !dragging.get_untracked() {
            shown.set(0);
        }
    });
    let value = move || {
        let s = shown.get();
        if s > 0 { s } else { seconds.get() }
    };
    let release = move |pos: f64| {
        let s = refresh_from_position(pos);
        shown.set(s);
        dragging.set(false);
        commit.run(s);
        serial.update(|n| *n += 1);
        let mine = serial.get_untracked();
        let done = wasm_bindgen::closure::Closure::once_into_js(move || {
            if serial.try_get_untracked() == Some(mine)
                && dragging.try_get_untracked() == Some(false)
            {
                let _ = shown.try_set(0);
            }
        });
        let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(
            wasm_bindgen::JsCast::unchecked_ref(&done),
            5000,
        );
    };
    view! {
        <div class="slider" class:off=move || !enabled.get()>
            <input type="range" min="0" max="1000" step="1" aria-label="Usage refresh interval" data-probe="refresh-slider"
                aria-valuetext=move || interval_label(value())
                disabled=move || !enabled.get()
                prop:value=move || ((refresh_position(value()) * 1000.0).round() as i32).to_string()
                style=move || format!("--pos:{:.2}%", refresh_position(value()) * 100.0)
                on:input=move |e| {
                    dragging.set(true);
                    let x = event_target_value(&e).parse::<f64>().unwrap_or(0.0) / 1000.0;
                    shown.set(refresh_from_position(x));
                }
                on:change=move |e| {
                    let x = event_target_value(&e).parse::<f64>().unwrap_or(0.0) / 1000.0;
                    release(x);
                }/>
            <div class="sl-marks" aria-hidden="true">
                {[(30u32, "30 s"), (120, "2 min"), (600, "10 min"), (3600, "60 min")].into_iter().map(|(s, t)| view! {
                    <span style=format!("left:{:.2}%", refresh_position(s) * 100.0)>{t}</span>
                }).collect_view()}
            </div>
        </div>
    }
}

#[component]
pub fn CleanupCard(page: Page) -> impl IntoView {
    let busy = page.busy;
    let c = Memo::new(move |_| {
        page.view
            .with(|v| v.as_ref().map(|v| v.cleanup.clone()).unwrap_or_default())
    });
    let checked = RwSignal::new(true);
    Effect::new(move |_| {
        let (auto, _) = c.with(|c| (c.auto, c.enabled));
        checked.set(auto);
    });
    Effect::new(move |prev: Option<bool>| {
        let now = checked.get();
        if let Some(prev) = prev {
            if prev != now && c.with_untracked(|c| c.auto != now) {
                page.send("cleanup-auto", if now { "true" } else { "false" });
            }
        }
        now
    });
    view! {
        <section class="plate ac-card set" data-probe="cleanup-card">
            <CardHead title="History snapshots"/>
            <div class="set-item first" title=move || c.with(|c| c.tip.clone())>
                <CheckLine checked=checked enabled=Signal::derive(move || c.with(|c| c.enabled) && !busy.get())
                    text="Clean up after each copy".to_string()
                    sub=Signal::derive(|| "keeps the newest 3 snapshot folders per profile".to_string()) probe="cleanup-auto"/>
                <div class="set-line">
                    <button type="button" class="btn sm" data-probe="cleanup-now"
                        disabled=move || !c.with(|c| c.enabled) || busy.get()
                        on:click=move |_| page.send("cleanup-now", "")>
                        {move || c.with(|c| c.busy).then(|| view! { <Spinner/> })}
                        {move || if c.with(|c| c.busy) { "Cleaning" } else { "Clean up now" }}
                    </button>
                </div>
                {move || c.with(|c| (!c.note.is_empty()).then(|| view! { <p class="q3">{c.note.clone()}</p> }))}
            </div>
        </section>
    }
}

#[component]
pub fn LogSourcesCard(page: Page) -> impl IntoView {
    let busy = page.busy;
    let l = Memo::new(move |_| {
        page.view.with(|v| {
            v.as_ref()
                .map(|v| v.log_sources.clone())
                .unwrap_or_default()
        })
    });
    let tool = RwSignal::new("omp".to_string());
    let host = RwSignal::new("Ubuntu".to_string());
    let path = RwSignal::new(String::new());
    let map: [RwSignal<String>; 5] = std::array::from_fn(|_| RwSignal::new(String::new()));
    // a saved location (its nonce) clears the draft
    Effect::new(move |prev: Option<u64>| {
        let n = l.with(|l| l.nonce);
        if prev.is_some_and(|p| p != n) {
            path.set(String::new());
            for m in map {
                m.set(String::new());
            }
        }
        n
    });
    let host_options = Memo::new(move |_| {
        let t = tool.get();
        l.with(|l| l.host_labels.get(&t).cloned().unwrap_or_default())
    });
    // the host follows the tool: keep it when the tool is scanned there too
    Effect::new(move |_| {
        let opts = host_options.get();
        if !opts.is_empty() && !opts.contains(&host.get_untracked()) {
            host.set(opts[0].clone());
        }
    });
    let form_enabled = Signal::derive(move || l.with(|l| l.enabled && !l.busy) && !busy.get());
    let act: Act = page.act;
    let add = move || {
        let t = tool.get_untracked();
        let mapping = if t == "jsonl" {
            map.iter()
                .map(|m| m.get_untracked())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            String::new()
        };
        act.run((
            "logsource-add".into(),
            format!(
                "{t}\n{}\n{}\n{mapping}",
                host.get_untracked(),
                path.get_untracked()
            ),
        ));
    };
    let fields = [
        ("Timestamp", "ts"),
        ("Model", "model"),
        ("Input tokens (optional)", "usage.input_tokens"),
        ("Output tokens (optional)", "usage.output_tokens"),
        ("Cost in USD (optional)", "cost"),
    ];
    view! {
        <section class="plate ac-card set" data-probe="logsources-card">
            <CardHead title="Usage log sources"/>
            <div class="set-item first" title=move || l.with(|l| l.tip.clone())>
                <p class="si-head"><span class="si-label">"Extra locations"</span></p>
                {move || {
                    let sources = l.with(|l| l.sources.clone());
                    if sources.is_empty() {
                        return view! { <p class="q3">"No extras yet. Built-in locations are always scanned."</p> }.into_any();
                    }
                    view! {
                        <ul class="sources">
                            {sources.into_iter().map(|s| {
                                let id = s.id.clone();
                                view! {
                                    <li class="source">
                                        <span class="src-text">
                                            <b>{format!("{} · {}", s.tool_label, if s.host_label.is_empty() { s.host.clone() } else { s.host_label.clone() })}</b>
                                            <span class="src-path mono">{s.path.clone()}</span>
                                            {(!s.mapping.is_empty()).then(|| view! { <span class="q3">{s.mapping.clone()}</span> })}
                                        </span>
                                        <button type="button" class="btn sm ghost" disabled=move || busy.get() || l.with(|l| l.busy)
                                            on:click=move |_| page.send("logsource-remove", &id)><Icon name="trash"/>"Remove"</button>
                                    </li>
                                }
                            }).collect_view()}
                        </ul>
                    }.into_any()
                }}
            </div>
            <div class="set-item">
                <p class="si-head"><span class="si-label">"Add a location"</span></p>
                <div class="ls-pick">
                    <label class="field"><span class="field-label">"Tool"</span>
                        <select class="ac-select" disabled=move || !form_enabled.get() prop:value=move || tool.get()
                            on:change=move |e| tool.set(event_target_value(&e))>
                            {move || l.with(|l| l.tools.clone()).into_iter().map(|t| view! { <option value=t.tool.clone()>{t.label.clone()}</option> }).collect_view()}
                        </select>
                    </label>
                    <label class="field"><span class="field-label">"Host"</span>
                        <select class="ac-select" disabled=move || !form_enabled.get() prop:value=move || host.get()
                            on:change=move |e| host.set(event_target_value(&e))>
                            {move || host_options.get().into_iter().map(|h| view! { <option value=h.clone()>{h.clone()}</option> }).collect_view()}
                        </select>
                    </label>
                </div>
                <label class="field"><span class="field-label">"Path"</span>
                    <input class="field-input mono-in" type="text" autocomplete="off" spellcheck="false" autocapitalize="off"
                        placeholder=move || l.with(|l| l.hints.get(&tool.get()).cloned()).map(|h| format!("/absolute/path or C:\\absolute\\path ({h})")).unwrap_or_else(|| "/absolute/path or C:\\absolute\\path".into())
                        disabled=move || !form_enabled.get() prop:value=move || path.get() on:input=move |e| path.set(event_target_value(&e))/>
                </label>
                <Show when=move || tool.get() == "jsonl">
                    <p class="si-sub">"Row field mapping: dot paths into each JSON record"</p>
                    <div class="ls-map">
                        {fields.into_iter().enumerate().map(|(i, (label, ph))| view! {
                            <label class="field"><span class="field-label">{label}</span>
                                <input class="field-input mono-in" type="text" autocomplete="off" spellcheck="false" autocapitalize="off" placeholder=ph
                                    disabled=move || !form_enabled.get() prop:value=move || map[i].get() on:input=move |e| map[i].set(event_target_value(&e))/>
                            </label>
                        }).collect_view()}
                    </div>
                </Show>
                {move || l.with(|l| (!l.error.is_empty()).then(|| view! { <p class="w-crit">{l.error.clone()}</p> }))}
                <div class="set-line">
                    <button type="button" class="btn sm" data-probe="logsource-add" disabled=move || !form_enabled.get() on:click=move |_| add()>
                        {move || l.with(|l| l.busy).then(|| view! { <Spinner/> })}
                        {move || if l.with(|l| l.busy) { "Adding" } else { "Add location" }}
                    </button>
                </div>
            </div>
        </section>
    }
}

#[component]
pub fn AboutCard(page: Page) -> impl IntoView {
    let version = Memo::new(move |_| {
        page.view.with(|v| {
            v.as_ref()
                .map(|v| v.about.version.clone())
                .unwrap_or_default()
        })
    });
    view! {
        <section class="plate ac-card set about" data-probe="about-card">
            <CardHead title="About"/>
            <div class="set-item first about-line">
                <Logo/>
                <span class="ab-text">
                    <b>"AI Account Center"</b>
                    <span class="q3">{move || format!("{} · this page built with Leptos · Home and Analytics built with Slint 1.18.1", version.get())}</span>
                    <a class="link" href="https://slint.dev" target="_blank" rel="noopener">"Made with Slint"</a>
                </span>
            </div>
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slider_snaps_like_the_slint_one() {
        assert_eq!(refresh_from_position(0.0), 30);
        assert_eq!(refresh_from_position(1.0), 3600);
        assert_eq!(refresh_from_position(refresh_position(300) + 0.02), 300);
        let between = refresh_from_position((refresh_position(300) + refresh_position(600)) / 2.0);
        assert!(between > 300 && between < 600);
        assert_eq!(interval_label(45), "45 s");
        assert_eq!(interval_label(120), "2 min");
        assert_eq!(interval_label(150), "2 min 30 s");
    }
}
