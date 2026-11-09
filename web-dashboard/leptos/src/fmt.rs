//! Words for ages and times. Display only: the server owns every reading.

/// "just now", "4m ago", "2h 5m ago", "3d ago". Negative ages (clock skew) read as "just now".
pub fn age(seconds: i64) -> String {
    let s = seconds.max(0);
    if s < 45 {
        "just now".into()
    } else if s < 3_600 {
        format!("{}m ago", ((s + 30) / 60).max(1))
    } else if s < 86_400 {
        let h = s / 3_600;
        let m = (s % 3_600) / 60;
        if m == 0 || h >= 6 {
            format!("{h}h ago")
        } else {
            format!("{h}h {m}m ago")
        }
    } else {
        format!("{}d ago", s / 86_400)
    }
}

/// "47 min ago", "2 h ago", "3 d ago", as the question list and cards say it. Empty when the time can't be read.
pub fn age_words_since(iso: &str, now_ms: f64) -> String {
    let at = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(iso)).get_time();
    if at.is_nan() {
        return String::new();
    }
    long_age(((now_ms - at) / 1000.0).floor() as i64)
}

pub fn long_age(seconds: i64) -> String {
    let s = seconds.max(0);
    if s < 45 {
        "just now".into()
    } else if s < 3_600 {
        format!("{} min ago", ((s + 30) / 60).max(1))
    } else if s < 86_400 {
        format!("{} h ago", s / 3_600)
    } else {
        format!("{} d ago", s / 86_400)
    }
}

/// A length of time: "40s", "4m", "2h 5m", "3d".
pub fn span(seconds: i64) -> String {
    let s = seconds.max(0);
    if s < 60 {
        format!("{s}s")
    } else {
        age(s.max(45)).trim_end_matches(" ago").to_string()
    }
}

/// An age that has kept running since the view was generated.
pub fn live_age(age_at_fetch: Option<i64>, elapsed_seconds: i64) -> Option<i64> {
    age_at_fetch.map(|a| a + elapsed_seconds.max(0))
}

/// "4:12 PM" in the device's own time zone, or "Nov 5, 4:12 PM" when not today. Empty when unreadable.
pub fn clock(iso: &str) -> String {
    if iso.is_empty() {
        return String::new();
    }
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(iso));
    if date.get_time().is_nan() {
        return String::new();
    }
    let today = js_sys::Date::new_0();
    let same_day = date.get_full_year() == today.get_full_year()
        && date.get_month() == today.get_month()
        && date.get_date() == today.get_date();
    let options = js_sys::Object::new();
    let set = |k: &str, v: &str| {
        let _ = js_sys::Reflect::set(&options, &k.into(), &v.into());
    };
    set("hour", "numeric");
    set("minute", "2-digit");
    if !same_day {
        set("month", "short");
        set("day", "numeric");
    }
    date.to_locale_string("en-US", &options).into()
}

/// "Nov 4" in the device's own time zone. Empty when unreadable.
pub fn short_date(iso: &str) -> String {
    if iso.is_empty() {
        return String::new();
    }
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(iso));
    if date.get_time().is_nan() {
        return String::new();
    }
    let options = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&options, &"month".into(), &"short".into());
    let _ = js_sys::Reflect::set(&options, &"day".into(), &"numeric".into());
    date.to_locale_string("en-US", &options).into()
}

/// One decimal below 10, whole numbers above: "6.4 GB", "104 GB".
pub fn gb(value: f64) -> String {
    if value < 10.0 {
        format!("{value:.1} GB")
    } else {
        format!("{value:.0} GB")
    }
}

pub fn pct(value: f64) -> String {
    format!("{value:.0}%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_read_plainly() {
        assert_eq!(span(40), "40s");
        assert_eq!(span(240), "4m");
        assert_eq!(span(7_500), "2h 5m");
        assert_eq!(span(-5), "0s");
    }

    #[test]
    fn ages_read_plainly() {
        assert_eq!(age(-5), "just now");
        assert_eq!(age(10), "just now");
        assert_eq!(age(50), "1m ago");
        assert_eq!(age(299), "5m ago");
        assert_eq!(age(3_600), "1h ago");
        assert_eq!(age(3_900), "1h 5m ago");
        assert_eq!(age(7 * 3_600 + 600), "7h ago");
        assert_eq!(age(2 * 86_400 + 5), "2d ago");
    }

    #[test]
    fn live_ages_keep_missing_missing() {
        assert_eq!(live_age(None, 30), None);
        assert_eq!(live_age(Some(10), 30), Some(40));
        assert_eq!(live_age(Some(10), -3), Some(10));
    }

    #[test]
    fn sizes_round_by_magnitude() {
        assert_eq!(gb(6.44), "6.4 GB");
        assert_eq!(gb(104.4), "104 GB");
        assert_eq!(pct(58.6), "59%");
    }
}
