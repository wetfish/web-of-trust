//! The page frame shared by every review page, and small formatting helpers.

use maud::{DOCTYPE, Markup, html};

/// A complete page with the site navigation.
pub fn page(title: &str, body: Markup) -> Markup {
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
                    }
                }
                main {
                    (body)
                }
            }
        }
    }
}

/// A link to a channel's page.
pub fn channel_href(tag: &str, name: &str) -> String {
    format!("/channel?tag={}&name={}", url_encode(tag), url_encode(name))
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
        Some(at) => at.get(..10).unwrap_or(at),
        None => "-",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_channel_names_for_links() {
        assert_eq!(url_encode("#wetfish"), "%23wetfish");
        assert_eq!(url_encode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(
            channel_href("wetfish", "#dry fish"),
            "/channel?tag=wetfish&name=%23dry%20fish"
        );
    }

    #[test]
    fn formats_numbers_and_days() {
        assert_eq!(number(0), "0");
        assert_eq!(number(1_234_567), "1,234,567");
        assert_eq!(number(-1_000), "-1,000");
        assert_eq!(day(Some("2026-11-02 12:43")), "2026-11-02");
        assert_eq!(day(None), "-");
    }

    #[test]
    fn escapes_text_in_pages() {
        let page = page("<b>", html! { p { "a < b" } }).into_string();
        assert!(page.contains("<title>&lt;b&gt; · wot review</title>"));
        assert!(page.contains("<p>a &lt; b</p>"));
    }
}
