use super::*;
use chrono::{DateTime, Utc};
use std::fmt::Write;
fn amount(v: &Value) -> String {
    v.as_f64()
        .map(|n| {
            if n.fract() == 0.0 {
                format!("{n:.0}")
            } else {
                format!("{n:.2}")
            }
        })
        .unwrap_or_else(|| "?".into())
}
fn span(ms: i64) -> String {
    js::span(ms as f64 / 1000.)
}
fn clock(v: &Value, now: i64) -> String {
    let Some(t) = js::date_parse(v) else {
        return "?".into();
    };
    let Some(dt) = DateTime::<Utc>::from_timestamp_millis(t) else {
        return "?".into();
    };
    let today = DateTime::<Utc>::from_timestamp_millis(now).map(|n| n.date_naive());
    let label = if Some(dt.date_naive()) == today {
        dt.format("%H:%M UTC").to_string()
    } else {
        dt.format("%b %d %H:%M UTC").to_string()
    };
    if t > now {
        format!("{label} (in {})", span(t - now))
    } else {
        format!("{label} ({} ago)", span(now - t))
    }
}
fn stale(v: &Value, now: i64) -> Option<String> {
    let Some(t) = time(v, "generatedAt") else {
        return Some("WARNING: the snapshot has no time; treat it as stale.".into());
    };
    let limit = stale_limit(v);
    (now-t> (limit*1000.) as i64).then(||format!("WARNING: this snapshot is {} old (stale after {}): the AI Account Center dashboard stopped updating it, so these numbers may be out of date.",span(now-t),js::span(limit)))
}
fn label<'a>(labels: &'a Value, host: &'a str) -> &'a str {
    labels[host][if host == "ubuntu" {
        "machineLabel"
    } else {
        "label"
    }]
    .as_str()
    .unwrap_or(host)
}
fn burn(b: &Value, now: i64, used: &Value) -> Option<String> {
    let pace = b["pace"].as_f64()?;
    let minutes = b["paceMinutes"].as_f64().unwrap_or(0.);
    let over = if minutes < 60. {
        format!("{} min", minutes as i64)
    } else {
        format!("{} h", (minutes / 60.).round_ties_even() as i64)
    };
    if pace < 0. {
        return Some(format!(
            "usage falling {}%/h over {over}",
            amount(&b["pace"])
        ));
    }
    let mut parts = vec![format!("pace {}%/h over {over}", amount(&b["pace"]))];
    if minutes < 60. && !b["lastHour"].is_null() {
        parts.push(format!("{}%/h over 1 h", amount(&b["lastHour"])));
    }
    if let Some(t) = time(b, "projectedFullAt") {
        if t <= now {
            parts.push(if used.as_f64().is_some_and(|u| u >= 100.) {
                format!(
                    "at its limit{}",
                    if b["runsOutBeforeReset"] == true {
                        " until the reset"
                    } else {
                        ""
                    }
                )
            } else {
                "reaching its limit about now at this pace".into()
            });
        } else {
            parts.push(format!(
                "full in {}{}",
                span(t - now),
                match b["runsOutBeforeReset"].as_bool() {
                    Some(true) => " BEFORE the reset",
                    Some(false) => " (after the reset)",
                    _ => "",
                }
            ));
        }
    }
    Some(parts.join(", "))
}
fn window(w: &Value, a: &Value, now: i64, limit: f64) -> String {
    let mut parts = Vec::new();
    if w["unlimited"] == true {
        parts.push("unlimited".into());
    } else if !w["usedPercent"].is_null() {
        parts.push(format!(
            "{}% used{}",
            amount(&w["usedPercent"]),
            if !w["remainingPercent"].is_null() {
                format!(" ({}% left)", amount(&w["remainingPercent"]))
            } else {
                String::new()
            }
        ));
    }
    let key = if !w["remaining"].is_null() {
        "remaining"
    } else {
        "used"
    };
    if !w[key].is_null() {
        parts.push(format!(
            "{}{}{} {}",
            amount(&w[key]),
            if !w["limit"].is_null() {
                format!(" of {}", amount(&w["limit"]))
            } else {
                String::new()
            },
            if s(w, "unit").is_empty() {
                String::new()
            } else {
                format!(" {}", s(w, "unit"))
            },
            if key == "remaining" { "left" } else { "used" }
        ));
    }
    let passed = passed(w, a, now);
    if js::truthy(&w["resetAt"]) {
        parts.push(if passed {
            format!(
                "reset at {} passed, new reading pending",
                clock(&w["resetAt"], now)
            )
        } else {
            format!("resets {}", clock(&w["resetAt"], now))
        });
    }
    if js::truthy(&w["expiresAt"]) {
        parts.push(format!("expires {}", clock(&w["expiresAt"], now)));
    }
    if !passed
        && current(w, a, now, limit)
        && let Some(b) = burn(&w["burn"], now, &w["usedPercent"])
    {
        parts.push(b);
    }
    if js::truthy(&w["cached"]) {
        parts.push(
            time(w, "sampledAt")
                .map(|t| format!("earlier reading from {} ago", span(now - t)))
                .unwrap_or_else(|| "earlier reading, time unknown".into()),
        );
    }
    let name = if s(w, "label").is_empty() {
        s(w, "key")
    } else {
        s(w, "label")
    };
    format!(
        "    {name}{}: {}",
        if s(w, "pool").is_empty() {
            String::new()
        } else {
            format!(" [{}]", s(w, "pool"))
        },
        if parts.is_empty() {
            "no number reported".into()
        } else {
            parts.join("; ")
        }
    )
}
fn notices(v: &Value, now: i64) -> String {
    let mut out = String::new();
    for n in rows(v, "needsYou") {
        if n.is_object() {
            let _ = writeln!(
                out,
                "NEEDS YOU (Claude sign-in): {}",
                if s(n, "how").is_empty() {
                    s(n, "reason")
                } else {
                    s(n, "how")
                }
            );
        }
    }
    if let Some(t) = time(v, "generatedAt")
        && now - t
            > (v["staleAfterSeconds"]
                .as_f64()
                .filter(|n| *n != 0.)
                .unwrap_or(3600.)
                * 1000.) as i64
    {
        let _ = writeln!(
            out,
            "Claude sign-in renewal on {} last ran {} ago: sign-ins are not being renewed.",
            if s(v, "runner").is_empty() {
                "?"
            } else {
                s(v, "runner")
            },
            span(now - t)
        );
    }
    out
}
fn sign_in(v: &Value, o: &Options, now: i64) -> String {
    let mut list = rows(v, "signIn")
        .iter()
        .filter(|r| o.provider.as_ref().is_none_or(|p| s(r, "provider") == p))
        .collect::<Vec<_>>();
    if list.is_empty() {
        return String::new();
    }
    let rank = |r: &&Value| match s(r, "state") {
        "logged_out" => 0,
        "critical" => 1,
        "warn" => 2,
        "unavailable" => 3,
        "unsupported" => 4,
        "ok" => 5,
        _ => 9,
    };
    list.sort_by_key(rank);
    let mut out = "\nSign-in health\n".to_string();
    for r in list {
        let state = s(r, "state");
        let live = matches!(state, "ok" | "warn" | "critical");
        let crossed = live && time(r, "loggedOutAt").is_some_and(|t| t <= now);
        let refresh_crossed = live
            && !js::truthy(&r["loggedOutAt"])
            && time(r, "refreshExpiresAt").is_some_and(|t| t <= now);
        let word = if crossed {
            format!(
                "ended {} (past its deadline; awaiting recheck)",
                s(r, "loggedOutAt")
            )
        } else if refresh_crossed {
            format!(
                "refresh expired {} (awaiting recheck)",
                s(r, "refreshExpiresAt")
            )
        } else if matches!(state, "warn" | "critical") {
            if !s(r, "loggedOutAt").is_empty() {
                format!("ends {}", s(r, "loggedOutAt"))
            } else if !s(r, "refreshExpiresAt").is_empty() {
                format!("refresh expires {}", s(r, "refreshExpiresAt"))
            } else if !js::truthy(&r["accessExpiresAt"]) {
                "no access token in the store; the next run renews".into()
            } else {
                "exact end unknown".into()
            }
        } else {
            match state {
                "ok" => "healthy",
                "logged_out" => "already logged out",
                "unavailable" => "not readable on this machine",
                "unsupported" => "no expiry published",
                x => x,
            }
            .into()
        };
        let machine = if !s(r, "machine").is_empty() {
            s(r, "machine")
        } else if !s(r, "platform").is_empty() {
            s(r, "platform")
        } else {
            "unknown"
        };
        let mut line = format!(
            "  {} | {machine}/{}: {word}.",
            s(r, "provider"),
            s(r, "name")
        );
        let command = s(r, "fixCommand");
        let fix = if command.is_empty() {
            String::new()
        } else if command.is_ascii() {
            format!(" re-login: `{command}`")
        } else {
            " re-login command omitted (non-ASCII path).".into()
        };
        match state {
            "unavailable" => line.push_str(&if !s(r, "error").is_empty() {
                format!(" ({})", s(r, "error"))
            } else {
                " reason unknown.".into()
            }),
            "unsupported" => line.push_str(&format!(
                " no login end time is published by {}.",
                s(r, "provider")
            )),
            "logged_out" => {
                if time(r, "loggedOutAt").is_some_and(|t| t <= now) {
                    line.push_str(&format!(" sign-in ended {}.", s(r, "loggedOutAt")));
                } else if time(r, "loggedOutAt").is_none() {
                    line.push_str(" tokens are gone or expired.");
                }
                line.push_str(&fix);
            }
            _ if crossed || refresh_crossed => line.push_str(&fix),
            "warn" | "critical" => {
                if !js::truthy(&r["loggedOutAt"]) && !js::truthy(&r["accessExpiresAt"]) {
                    line.push_str(" the next run renews this login.");
                } else {
                    let advice = if js::truthy(&r["loggedOutAt"]) {
                        "re-login before then"
                    } else {
                        "re-login when the window ends"
                    };
                    if command.is_empty() {
                        line.push_str(&format!(" {advice}."));
                    } else if command.is_ascii() {
                        line.push_str(&format!(" {advice}: `{command}`"));
                    } else {
                        line.push_str(" re-login command omitted (non-ASCII path).");
                    }
                }
            }
            "ok" => {
                line.push_str(&if js::truthy(&r["loggedOutAt"]) {
                    format!(" sign-in ends {}.", s(r, "loggedOutAt"))
                } else if js::truthy(&r["refreshExpiresAt"]) {
                    format!(" refresh expires {}.", s(r, "refreshExpiresAt"))
                } else {
                    " no exact end published yet.".into()
                });
            }
            _ => (),
        }
        let _ = writeln!(out, "{line}");
    }
    out
}
pub(super) fn show(
    v: &Value,
    o: &Options,
    now: i64,
    folder: &Path,
    labels: &Value,
    signins: Option<&Value>,
) -> String {
    let age = time(v, "generatedAt")
        .map(|t| span(now - t))
        .unwrap_or_else(|| "?".into());
    let mut out = format!(
        "AI Account Center usage, snapshot {age} ago ({})\n",
        folder.join("usage.json").display()
    );
    if let Some(note) = stale(v, now) {
        let _ = writeln!(out, "{note}");
    }
    if let Some(v) = signins {
        out.push_str(&notices(v, now));
    }
    let accounts = rows(v, "accounts")
        .iter()
        .filter(|a| matches(a, o, "id"))
        .collect::<Vec<_>>();
    if accounts.is_empty() {
        out.push_str("No matching accounts.\n");
    }
    let mut provider = "";
    for a in accounts {
        if s(a, "providerLabel") != provider {
            provider = s(a, "providerLabel");
            let _ = writeln!(out, "\n{provider}");
        }
        let mut who = vec![if s(a, "label").is_empty() {
            "?".into()
        } else {
            s(a, "label").to_string()
        }];
        if !s(a, "email").is_empty() && s(a, "email") != s(a, "label") {
            who.push(s(a, "email").into());
        }
        if !s(a, "profile").is_empty() && s(a, "profile") != s(a, "label") {
            who.push(format!("profile {}", s(a, "profile")));
        }
        let mut facts = vec![];
        if !s(a, "plan").is_empty() {
            facts.push(s(a, "plan"));
        }
        let host = label(labels, s(a, "host"));
        if !host.is_empty() {
            facts.push(host);
        }
        if js::truthy(&a["active"]) {
            facts.push("ACTIVE");
        }
        let age = time(a, "sampledAt")
            .map(|t| {
                if now - t < 1000 {
                    "just now".into()
                } else {
                    format!("{} ago", span(now - t))
                }
            })
            .unwrap_or_else(|| "at an unknown time".into());
        let status = match s(a, "status") {
            "ok" => "current",
            "cached" => "cached earlier reading",
            "unavailable" => "no current reading",
            "error" => "reading failed",
            "needs_sign_in" => "needs sign-in",
            x => x,
        };
        let _ = writeln!(
            out,
            "  {}  [{}]  {status}, sampled {age}",
            who.join("  "),
            facts.join(", ")
        );
        if s(a, "status") == "cached" {
            let reason = match s(a, "cacheReason") {
                "rate_limited" => "provider refresh is rate limited",
                "read_failed" => "last provider read failed or was skipped",
                "refresh_pending" => "next provider read is pending",
                "source_changed" => "saved sign-in changed; next provider read is pending",
                _ => "last provider read outcome was not recorded",
            };
            let _ = writeln!(out, "    {reason}");
        }
        if matches!(s(a, "status"), "ok" | "cached") {
            for w in rows(a, "windows") {
                let _ = writeln!(out, "{}", window(w, a, now, stale_limit(v)));
            }
        }
    }
    out.push_str(&sign_in(v, o, now));
    out
}
pub(super) fn history(v: &Value, now: i64) -> String {
    let mut out = String::new();
    if let Some(note) = stale(v, now) {
        let _ = writeln!(out, "{note}");
    }
    for e in rows(v, "series") {
        let _ = writeln!(
            out,
            "{} {} ({}) {}{}:",
            s(e, "provider"),
            s(e, "label"),
            if s(e, "email").is_empty() {
                "-"
            } else {
                s(e, "email")
            },
            s(e, "windowLabel"),
            if s(e, "pool").is_empty() {
                String::new()
            } else {
                format!(" [{}]", s(e, "pool"))
            }
        );
        for p in rows(e, "points") {
            let mut values = vec![];
            if !p[1].is_null() {
                values.push(format!("{}%", amount(&p[1])));
            }
            if !p[2].is_null() {
                values.push(format!("{} left", amount(&p[2])));
            }
            let _ = writeln!(
                out,
                "  {}  {}",
                p[0].as_str().unwrap_or(""),
                values.join(", ")
            );
        }
    }
    out
}
pub(super) fn signins(v: &Value, o: &Options, now: i64, labels: &Value) -> String {
    let mut out = notices(v, now);
    let _ = writeln!(
        out,
        "Claude Code sign-ins (renewal runner on {}; each renews by itself {} hours before it ends)",
        if s(v, "runner").is_empty() {
            "?"
        } else {
            s(v, "runner")
        },
        if js::truthy(&v["renewWithinHours"]) {
            amount(&v["renewWithinHours"])
        } else {
            "?".into()
        }
    );
    for r in rows(v, "signIns") {
        if o.account
            .as_ref()
            .is_some_and(|n| !s(r, "email").to_lowercase().contains(n))
        {
            continue;
        }
        let ends = if time(r, "signInEndsAt").is_some() {
            format!("ends {}", clock(&r["signInEndsAt"], now))
        } else {
            "end not reported".into()
        };
        let _ = writeln!(
            out,
            "  {:8} {:11} {:30} {:11} {ends}{}",
            label(labels, s(r, "host")),
            s(r, "profile"),
            if s(r, "email").is_empty() {
                "?"
            } else {
                s(r, "email")
            },
            s(r, "state"),
            if s(r, "lastResult").is_empty() {
                String::new()
            } else {
                format!("; last renewal: {}", s(r, "lastResult").replace('_', " "))
            }
        );
    }
    out
}
