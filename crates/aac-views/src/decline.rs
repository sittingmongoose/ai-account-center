//! L1: the one named reason the service gives when it declines a request, so the TS front door
//! answers instead. It travels as `x-aac-decline: <word>` on the private 503; TS counts it per
//! (route, reason). Composer-level words (`js::Decline`) map onto it in one place.
use crate::js;

/// Why Rust declined. The wire words are fixed: TS counts and logs them per route.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Decline {
    /// The input is not the shape the composer proved (keys, types, envelope).
    Shape,
    /// A number outside the proved formatting or rounding domain.
    Number,
    /// A date or time the composer cannot read as the engine does.
    Date,
    /// Display-zone facts that do not prove the page's zone.
    Zone,
    /// Locale or engine words the composer cannot reproduce.
    Locale,
    /// A sort pair outside the proved collation ranks.
    Collation,
    /// The request names a generation Rust no longer (or does not yet) hold.
    Stale,
    /// The bounded blocking pool is full.
    Busy,
    /// The route's budget ran out before an answer was ready.
    Deadline,
    /// An input or route the composer does not support.
    Unsupported,
    /// A failure inside the service (a task that ended without an answer).
    Internal,
}

impl Decline {
    /// Every reason, in wire order.
    pub const ALL: [Decline; 11] = [
        Decline::Shape,
        Decline::Number,
        Decline::Date,
        Decline::Zone,
        Decline::Locale,
        Decline::Collation,
        Decline::Stale,
        Decline::Busy,
        Decline::Deadline,
        Decline::Unsupported,
        Decline::Internal,
    ];

    /// The `x-aac-decline` header word.
    pub fn as_str(self) -> &'static str {
        match self {
            Decline::Shape => "shape",
            Decline::Number => "number",
            Decline::Date => "date",
            Decline::Zone => "zone",
            Decline::Locale => "locale",
            Decline::Collation => "collation",
            Decline::Stale => "stale",
            Decline::Busy => "busy",
            Decline::Deadline => "deadline",
            Decline::Unsupported => "unsupported",
            Decline::Internal => "internal",
        }
    }
}

impl From<js::Decline> for Decline {
    /// The composers' detail words. `fact` is a missing engine (Intl) fact; `case-map` is a
    /// case mapping the collation ranks do not cover; `type`, `coercion`, `input` and
    /// `proto-key` are shapes; `open-progress` is a Claude Open the view does not draw yet;
    /// `registry` is the service's own embedded table failing to load.
    fn from(detail: js::Decline) -> Self {
        match detail.0 {
            "number" => Decline::Number,
            "date" => Decline::Date,
            "zone" => Decline::Zone,
            "locale" | "fact" => Decline::Locale,
            "collation" | "case-map" => Decline::Collation,
            "shape" | "type" | "coercion" | "input" | "proto-key" => Decline::Shape,
            "registry" => Decline::Internal,
            _ => Decline::Unsupported,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_unique_lowercase_and_cover_the_design_list() {
        let words: Vec<_> = Decline::ALL.iter().map(|d| d.as_str()).collect();
        assert_eq!(
            words,
            [
                "shape",
                "number",
                "date",
                "zone",
                "locale",
                "collation",
                "stale",
                "busy",
                "deadline",
                "unsupported",
                "internal"
            ]
        );
    }

    #[test]
    fn composer_details_map_to_a_named_reason() {
        for (detail, reason) in [
            ("number", Decline::Number),
            ("date", Decline::Date),
            ("zone", Decline::Zone),
            ("fact", Decline::Locale),
            ("locale", Decline::Locale),
            ("collation", Decline::Collation),
            ("case-map", Decline::Collation),
            ("type", Decline::Shape),
            ("proto-key", Decline::Shape),
            ("shape", Decline::Shape),
            ("coercion", Decline::Shape),
            ("input", Decline::Shape),
            ("registry", Decline::Internal),
            ("open-progress", Decline::Unsupported),
            ("anything-else", Decline::Unsupported),
        ] {
            assert_eq!(Decline::from(js::Decline(detail)), reason, "{detail}");
        }
    }
}
