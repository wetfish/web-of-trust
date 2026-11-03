//! The history page: what an identity or a person said in channels, line by
//! line, and the days they were present.

use std::collections::{BTreeMap, BTreeSet};

use axum::Router;
use axum::extract::{Query, State};
use axum::routing::get;
use chrono::NaiveDate;
use maud::{Markup, html};
use serde::Deserialize;

use super::layout::{self, identity_href, number, person_href, url_encode};
use super::{App, Flash, PageError, PageResult, filled, internal, with_db};
use crate::history::{self, Day, Filter, Line, PAGE_SIZE};
use crate::lookup;
use crate::stats;

pub(super) fn routes() -> Router<App> {
    Router::new().route("/history", get(history_page))
}

/// A link to an identity's history.
pub(super) fn identity_history_href(id: &str) -> String {
    format!("/history?id={}", url_encode(id))
}

/// A link to a person's or bot's history.
pub(super) fn person_history_href(name: &str) -> String {
    format!("/history?person={}", url_encode(name))
}

#[derive(Default, Deserialize)]
struct HistoryQuery {
    /// An identity id, or
    id: Option<String>,
    /// a person's or bot's name.
    person: Option<String>,
    channel: Option<String>,
    /// `YYYY-MM-DD`.
    day: Option<String>,
    /// Text the lines contain.
    q: Option<String>,
    /// From 0.
    page: Option<String>,
}

impl HistoryQuery {
    fn filter(&self) -> Filter {
        Filter {
            day: filled(&self.day)
                .filter(|day| NaiveDate::parse_from_str(day, "%Y-%m-%d").is_ok())
                .map(str::to_string),
            channel: filled(&self.channel).map(str::to_string),
            text: filled(&self.q).map(str::to_string),
            page: filled(&self.page)
                .and_then(|page| page.parse().ok())
                .unwrap_or(0),
        }
    }

    /// The page's own address, for the person or identity alone.
    fn base(&self) -> String {
        match (filled(&self.id), filled(&self.person)) {
            (Some(id), _) => identity_history_href(id),
            (None, Some(person)) => person_history_href(person),
            (None, None) => "/history?".to_string(),
        }
    }

    /// The address of this history with a filter and page.
    fn href(&self, filter: &Filter) -> String {
        let mut href = self.base();
        let fields = [
            ("channel", filter.channel.as_deref()),
            ("day", filter.day.as_deref()),
            ("q", filter.text.as_deref()),
        ];
        for (name, value) in fields {
            if let Some(value) = value {
                href.push_str(&format!("&{name}={}", url_encode(value)));
            }
        }
        if filter.page > 0 {
            href.push_str(&format!("&page={}", filter.page));
        }
        href
    }
}

/// Whose history it is.
struct Subject {
    /// For the heading, such as `identity 1a2b3c4d5e6f` or `quokka`.
    title: Markup,
    /// The same as plain text, for the window title.
    label: String,
    /// The page to go back to.
    back: String,
    identities: Vec<String>,
}

async fn history_page(
    State(app): State<App>,
    Query(query): Query<HistoryQuery>,
    Query(flash): Query<Flash>,
) -> PageResult {
    with_db(&app, move |conn| {
        let subject = match (filled(&query.id), filled(&query.person)) {
            (Some(id), _) => {
                let view = lookup::by_id(conn, id).map_err(internal)?.ok_or_else(|| {
                    PageError::not_found(format!("There is no identity {id} with sessions."))
                })?;
                Subject {
                    title: html! { "identity " code { (view.id) } },
                    label: format!("identity {}", view.id),
                    back: identity_href(&view.id),
                    identities: vec![view.id],
                }
            }
            (None, Some(name)) => {
                let person = stats::person(conn, name)
                    .map_err(internal)?
                    .ok_or_else(|| {
                        PageError::not_found(format!("There is no person or bot named {name}."))
                    })?;
                Subject {
                    title: html! { (person.name) },
                    label: person.name.clone(),
                    back: person_href(&person.name),
                    identities: person.identity_ids(),
                }
            }
            (None, None) => {
                return Err(PageError::not_found(
                    "Open a history from an identity's or a person's page.",
                ));
            }
        };
        let filter = query.filter();
        let days = history::days(conn, &subject.identities).map_err(internal)?;
        let (lines, more) = history::lines(conn, &subject.identities, &filter).map_err(internal)?;
        let title = format!("History of {}", subject.label);
        Ok(layout::page(
            &title,
            &flash,
            page(&query, &subject, &filter, &days, &lines, more),
        ))
    })
    .await
}

fn page(
    query: &HistoryQuery,
    subject: &Subject,
    filter: &Filter,
    days: &BTreeMap<NaiveDate, Day>,
    lines: &[Line],
    more: bool,
) -> Markup {
    let spoke_on = days.values().filter(|day| day.spoken > 0).count();
    let spoken: i64 = days.values().map(|day| day.spoken).sum();
    let channels: BTreeSet<&String> = days.values().flat_map(|day| &day.channels).collect();
    let filtered = filter.day.is_some() || filter.channel.is_some() || filter.text.is_some();
    let at_page = |page: usize| {
        query.href(&Filter {
            page,
            ..filter.clone()
        })
    };
    html! {
        p.crumbs { a href=(subject.back) { "Back to " (subject.title) } }
        h1 { "History of " (subject.title) }
        p.lead {
            "Present in channels on " (number(days.len() as i64)) " days and spoke on "
            (number(spoke_on as i64)) " of them: " (number(spoken)) " lines."
        }
        p.note {
            "Private messages and the logging client's own lines are left out. A session "
            "counts on every day it lasted, so a bouncer's days add up quickly."
        }

        h2 { "Lines" }
        form.search method="get" action="/history" {
            @if let Some(id) = filled(&query.id) {
                input type="hidden" name="id" value=(id);
            } @else if let Some(person) = filled(&query.person) {
                input type="hidden" name="person" value=(person);
            }
            label {
                "Channel "
                select name="channel" {
                    option value="" { "All channels" }
                    @for channel in &channels {
                        option value=(channel)
                            selected[filter.channel.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(channel))] {
                            (channel)
                        }
                    }
                }
            }
            label { "Day " input type="date" name="day" value=[filter.day.as_deref()]; }
            label { "Contains " input type="text" name="q" value=[filter.text.as_deref()]; }
            button type="submit" { "Show" }
            @if filtered {
                a href=(query.base()) { "Clear" }
            }
        }
        @if lines.is_empty() {
            p {
                @if filtered {
                    "No lines match."
                } @else {
                    "Nothing said in channels. If lines are missing, run "
                    code { "wot analyze" } " once to rebuild the sessions."
                }
            }
        } @else {
            p.note {
                "Most recent first, " (PAGE_SIZE) " to a page"
                @if filter.page > 0 { ", page " (filter.page + 1) }
                "."
            }
            div.scroll {
                table.lines {
                    thead {
                        tr {
                            th { "Day" }
                            th { "Time" }
                            th { "Channel" }
                            th { "Line" }
                        }
                    }
                    tbody {
                        @for line in lines {
                            tr {
                                td {
                                    @if line.date.is_empty() {
                                        span.muted { "-" }
                                    } @else {
                                        a href=(query.href(&Filter { day: Some(line.date.clone()), page: 0, ..filter.clone() })) {
                                            (line.date)
                                        }
                                    }
                                }
                                td { (line.time) }
                                td { (line.channel) }
                                td.wrap {
                                    @if line.action {
                                        "* " strong { (line.nick) } " " (line.text)
                                    } @else {
                                        span.muted { "<" } strong { (line.nick) } span.muted { ">" }
                                        " " (line.text)
                                    }
                                }
                            }
                        }
                    }
                }
            }
            p.pager {
                @if filter.page > 0 {
                    a href=(at_page(filter.page - 1)) { "Newer" }
                }
                @if more {
                    a href=(at_page(filter.page + 1)) { "Older" }
                }
            }
        }

        h2 { "Days" }
        @if days.is_empty() {
            p { "Never present in a channel." }
        } @else {
            (months(query, filter, days))
        }
    }
}

/// The days, by month, most recent first, each month in a fold with the
/// newest open.
fn months(query: &HistoryQuery, filter: &Filter, days: &BTreeMap<NaiveDate, Day>) -> Markup {
    let mut by_month: BTreeMap<String, Vec<(&NaiveDate, &Day)>> = BTreeMap::new();
    for (date, day) in days {
        by_month
            .entry(date.format("%Y-%m").to_string())
            .or_default()
            .push((date, day));
    }
    html! {
        @for (i, (month, days)) in by_month.iter().rev().enumerate() {
            details open[i == 0] {
                summary {
                    (month) ": present " (days.len()) " days, spoke on "
                    (days.iter().filter(|(_, day)| day.spoken > 0).count()) ", "
                    (number(days.iter().map(|(_, day)| day.spoken).sum::<i64>())) " lines"
                }
                div.scroll {
                    table {
                        thead {
                            tr {
                                th { "Day" }
                                th.num { "Lines" }
                                th { "Channels" }
                            }
                        }
                        tbody {
                            @for (date, day) in days.iter().rev() {
                                tr {
                                    td {
                                        @if day.spoken > 0 {
                                            a href=(query.href(&Filter {
                                                day: Some(date.to_string()),
                                                page: 0,
                                                ..filter.clone()
                                            })) { (date) }
                                        } @else {
                                            (date)
                                        }
                                    }
                                    td.num {
                                        @if day.spoken > 0 {
                                            (number(day.spoken))
                                        } @else {
                                            span.muted { "silent" }
                                        }
                                    }
                                    td.wrap {
                                        @for (i, channel) in day.channels.iter().enumerate() {
                                            @if i > 0 { ", " }
                                            (channel)
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(id: &str) -> HistoryQuery {
        HistoryQuery {
            id: Some(id.into()),
            ..HistoryQuery::default()
        }
    }

    #[test]
    fn keeps_filters_in_page_links() {
        let query = HistoryQuery {
            channel: Some("#analysis".into()),
            q: Some("a b".into()),
            page: Some("2".into()),
            day: Some("not a day".into()),
            ..query("1a2b3c4d5e6f")
        };
        let filter = query.filter();
        assert_eq!(filter.page, 2);
        assert_eq!(filter.day, None, "an invalid day is ignored");
        assert_eq!(
            query.href(&filter),
            "/history?id=1a2b3c4d5e6f&channel=%23analysis&q=a%20b&page=2"
        );
        let person = HistoryQuery {
            person: Some("quokka".into()),
            ..HistoryQuery::default()
        };
        assert_eq!(person.base(), "/history?person=quokka");
    }

    #[test]
    fn shows_lines_days_and_pages() {
        let query = query("1a2b3c4d5e6f");
        let subject = Subject {
            title: html! { "quokka" },
            label: "quokka".into(),
            back: "/person?name=quokka".into(),
            identities: vec!["1a2b3c4d5e6f".into()],
        };
        let day = |d: u32| NaiveDate::from_ymd_opt(2026, 11, d).unwrap();
        let days = BTreeMap::from([
            (
                day(1),
                Day {
                    channels: BTreeSet::from(["#analysis".to_string()]),
                    spoken: 2,
                },
            ),
            (
                day(2),
                Day {
                    channels: BTreeSet::from(["#analysis".to_string(), "#other".to_string()]),
                    spoken: 0,
                },
            ),
        ]);
        let lines = [Line {
            date: "2026-11-01".into(),
            time: "22:11".into(),
            channel: "#analysis".into(),
            nick: "quokka".into(),
            action: false,
            text: "hello <there>".into(),
        }];
        let filter = Filter {
            page: 1,
            ..Filter::default()
        };
        let page = page(&query, &subject, &filter, &days, &lines, true).into_string();
        assert!(page.contains("Present in channels on 2 days and spoke on 1 of them: 2 lines."));
        assert!(page.contains("hello &lt;there&gt;"));
        assert!(page.contains(r#"href="/history?id=1a2b3c4d5e6f">Newer</a>"#));
        assert!(page.contains(r#"href="/history?id=1a2b3c4d5e6f&amp;page=2">Older</a>"#));
        assert!(page.contains("2026-11: present 2 days, spoke on 1, 2 lines"));
        assert!(page.contains("silent"));
        assert!(page.contains(r#"href="/history?id=1a2b3c4d5e6f&amp;day=2026-11-01""#));
    }
}
