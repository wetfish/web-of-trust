//! The page frame shared by every review page, and small helpers for links,
//! numbers and forms.

use maud::{DOCTYPE, Markup, html};

use super::Flash;

/// A complete page with the site navigation and any notice or error.
pub fn page(title: &str, flash: &Flash, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " · wot review" }
                link rel="stylesheet" href="/style.css";
            }
            body {
                header {
                    nav {
                        a.brand href="/" { "wot review" }
                        a href="/" { "Overview" }
                        a href="/channels" { "Channels" }
                        a href="/activity" { "Activity" }
                        a href="/identities" { "Identities" }
                        a href="/people" { "People and bots" }
                        a href="/hosts" { "Host groups" }
                        a href="/cuts" { "Cuts" }
                    }
                }
                main {
                    @if let Some(notice) = &flash.notice {
                        p.flash.notice role="status" { (notice) }
                    }
                    @if let Some(error) = &flash.error {
                        p.flash.error role="alert" { (error) }
                    }
                    (body)
                }
            }
        }
    }
}

/// A hidden field telling an action which page to return to.
pub fn back_field(back: &str) -> Markup {
    html! { input type="hidden" name="back" value=(back); }
}

/// A link to a channel's page.
pub fn channel_href(tag: &str, name: &str) -> String {
    format!("/channel?tag={}&name={}", url_encode(tag), url_encode(name))
}

/// A link to an identity's page.
pub fn identity_href(id: &str) -> String {
    format!("/identity?id={}", url_encode(id))
}

/// A link to a person's page.
pub fn person_href(name: &str) -> String {
    format!("/person?name={}", url_encode(name))
}

/// A link to an identity search, from `(field, value)` pairs.
pub fn search_href(fields: &[(&str, &str)]) -> String {
    let query: Vec<String> = fields
        .iter()
        .map(|(field, value)| format!("{field}={}", url_encode(value)))
        .collect();
    format!("/identities?{}", query.join("&"))
}

/// Percent-encodes a query string value, leaving only unreserved characters.
pub fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// A red flag on someone present for many days who never said a line.
pub fn silent_flag(days: i64) -> Markup {
    html! {
        span.flag title=(format!(
            "Present on {} days without saying a line: watching, not taking part",
            number(days)
        )) {
            "\u{2691} Silent " (number(days)) " days"
        }
    }
}

/// A count with thousands separators, such as `12,345`.
pub fn number(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// The `YYYY-MM-DD` part of a timestamp, or a dash when there is none.
pub fn day(at: Option<&str>) -> &str {
    match at {
        Some(at) if !at.is_empty() => at.get(..10).unwrap_or(at),
        _ => "-",
    }
}

/// Minutes as `45m` below an hour, otherwise whole hours such as `1,234h`.
pub fn duration(minutes: i64) -> String {
    if minutes < 60 {
        format!("{}m", minutes.max(0))
    } else {
        format!("{}h", number(minutes / 60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_values_for_links() {
        assert_eq!(url_encode("#wetfish"), "%23wetfish");
        assert_eq!(url_encode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(
            channel_href("wetfish", "#dry fish"),
            "/channel?tag=wetfish&name=%23dry%20fish"
        );
        assert_eq!(
            search_href(&[("ident", "quokkawet"), ("host", "bridge.example.net")]),
            "/identities?ident=quokkawet&host=bridge.example.net"
        );
    }

    #[test]
    fn formats_numbers_days_and_durations() {
        assert_eq!(number(0), "0");
        assert_eq!(number(1_234_567), "1,234,567");
        assert_eq!(number(-1_000), "-1,000");
        assert_eq!(day(Some("2026-11-02 12:43")), "2026-11-02");
        assert_eq!(day(Some("")), "-");
        assert_eq!(day(None), "-");
        assert_eq!(duration(45), "45m");
        assert_eq!(duration(60 * 1_500), "1,500h");
    }

    #[test]
    fn escapes_text_and_shows_messages() {
        let flash = Flash {
            notice: Some("Saved <ok>".into()),
            error: None,
        };
        let page = page("<b>", &flash, html! { p { "a < b" } }).into_string();
        assert!(page.contains("<title>&lt;b&gt; · wot review</title>"));
        assert!(page.contains("<p>a &lt; b</p>"));
        assert!(page.contains("Saved &lt;ok&gt;"));
    }
}
