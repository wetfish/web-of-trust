//! The activity ranking and the activity section of an identity's page.

use axum::Router;
use axum::extract::{Query, State};
use axum::routing::get;
use maud::{Markup, html};
use serde::Deserialize;

use super::layout::{self, duration, identity_href, number, person_href};
use super::{App, Flash, PageResult, filled, internal, with_db};
use crate::activity::{self, Order, RankFilter, RankRow, Totals};

/// Identities shown in a ranking.
const MAX_ROWS: usize = 200;
/// The days present an identity needs to be ranked, unless chosen otherwise.
const DEFAULT_MIN_DAYS: i64 = 7;

pub(super) fn routes() -> Router<App> {
    Router::new().route("/activity", get(activity_page))
}

#[derive(Default, Deserialize)]
struct RankQuery {
    /// `TAG CHANNEL`, or empty for all channels.
    channel: Option<String>,
    year: Option<String>,
    /// `least`, `most` or `longest`.
    order: Option<String>,
    min_days: Option<String>,
    /// `yes` to include bots.
    bots: Option<String>,
}

impl RankQuery {
    fn filter(&self) -> RankFilter {
        let (tag, channel) = match filled(&self.channel).and_then(|value| value.split_once(' ')) {
            Some((tag, channel)) => (Some(tag.to_string()), Some(channel.to_string())),
            None => (None, None),
        };
        RankFilter {
            tag,
            channel,
            year: filled(&self.year).map(str::to_string),
            min_days: filled(&self.min_days)
                .and_then(|days| days.parse().ok())
                .unwrap_or(DEFAULT_MIN_DAYS)
                .max(0),
            include_bots: filled(&self.bots) == Some("yes"),
            order: match filled(&self.order) {
                Some("most") => Order::MostActive,
                Some("longest") => Order::LongestPresent,
                _ => Order::LeastActive,
            },
            limit: MAX_ROWS,
        }
    }
}

async fn activity_page(
    State(app): State<App>,
    Query(query): Query<RankQuery>,
    Query(flash): Query<Flash>,
) -> PageResult {
    with_db(&app, move |conn| {
        let filter = query.filter();
        let (rows, matched) = activity::ranking(conn, &filter).map_err(internal)?;
        let (years, channels) = activity::choices(conn).map_err(internal)?;
        Ok(layout::page(
            "Activity",
            &flash,
            page(&query, &filter, &rows, matched, &years, &channels),
        ))
    })
    .await
}

fn page(
    query: &RankQuery,
    filter: &RankFilter,
    rows: &[RankRow],
    matched: usize,
    years: &[String],
    channels: &[(String, String)],
) -> Markup {
    let chosen_channel = filled(&query.channel).unwrap_or_default();
    // Identities belong to one network tag, so a ranking of one channel has
    // one tag and only the ranking over all channels needs the column.
    let show_tag = filter.tag.is_none();
    html! {
        h1 { "Activity" }
        p.note {
            "Activity in channels, by identity. Private messages are left out, since they only "
            "show conversations with the logging client, and so is the logging client itself. "
            strong { "Heard" } " counts the lines said in a channel while the identity was there, "
            "its own included; " strong { "share" } " is how much of that it said itself. "
            "Someone present for a long time with a share near zero is listening, not talking."
        }
        form.search method="get" action="/activity" {
            label {
                "Channel "
                select name="channel" {
                    option value="" { "All channels" }
                    @for (tag, channel) in channels {
                        @let value = format!("{tag} {channel}");
                        option value=(value) selected[value == chosen_channel] {
                            (channel) @if channels.iter().any(|(other, _)| other != tag) { " (" (tag) ")" }
                        }
                    }
                }
            }
            label {
                "Year "
                select name="year" {
                    option value="" { "All time" }
                    @for year in years {
                        option value=(year) selected[filter.year.as_deref() == Some(year.as_str())] { (year) }
                    }
                }
            }
            label {
                "Order "
                select name="order" {
                    option value="least" selected[filter.order == Order::LeastActive] { "Least active first" }
                    option value="most" selected[filter.order == Order::MostActive] { "Most active first" }
                    option value="longest" selected[filter.order == Order::LongestPresent] { "Longest present first" }
                }
            }
            label {
                "Days present, at least "
                input type="number" name="min_days" min="0" value=(filter.min_days);
            }
            label {
                input type="checkbox" name="bots" value="yes" checked[filter.include_bots];
                " Include bots"
            }
            button type="submit" { "Show" }
        }
        @if rows.is_empty() {
            p {
                "No identities match. Activity is built by " code { "wot analyze" }
                "; if nothing shows at all, run it once to build activity for an older database."
            }
        } @else {
            p.lead {
                (number(matched as i64)) " identities were present on at least "
                (number(filter.min_days)) " days"
                @if let Some(year) = &filter.year { " in " (year) }
                "."
                @if matched > rows.len() { " Showing the first " (rows.len()) "." }
            }
            div.scroll {
                table {
                    thead {
                        tr {
                            th.num { "#" }
                            th { "Identity" }
                            @if show_tag { th { "Tag" } }
                            th { "Person" }
                            th { "Nicks" }
                            th.num title="Days with a session in the channel" { "Days present" }
                            th.num title="Days on which it said anything" { "Active days" }
                            th.num { "Time" }
                            th.num title="Lines said while it was present, its own included" { "Heard" }
                            th.num { "Spoken" }
                            th.num title="Spoken lines as a share of the lines heard" { "Share" }
                            th.num title="Messages sent in private messages to the logging client" { "Private" }
                            th { "First" }
                            th { "Last" }
                        }
                    }
                    tbody {
                        @for (rank, row) in rows.iter().enumerate() {
                            tr {
                                td.num { (rank + 1) }
                                td {
                                    a href=(identity_href(&row.identity_id)) { code { (row.identity_id) } }
                                    @if row.bot { " " span.warning { "bot" } }
                                }
                                @if show_tag { td { (row.tag) } }
                                td {
                                    @for (i, person) in row.people.iter().enumerate() {
                                        @if i > 0 { ", " }
                                        a href=(person_href(person)) { (person) }
                                    }
                                }
                                td.wrap { (row.nicks.join(", ")) }
                                td.num { (number(row.totals.days_present)) }
                                td.num { (number(row.totals.active_days)) }
                                td.num { (duration(row.totals.minutes)) }
                                td.num { (number(row.totals.heard)) }
                                td.num { (number(row.totals.spoken)) }
                                td.num { (share(&row.totals)) }
                                td.num { (number(row.private_spoken)) }
                                td { (row.first_month) }
                                td { (row.last_month) }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A share as a percentage, with a decimal below 10%.
pub(super) fn share(totals: &Totals) -> String {
    match totals.share() {
        None => "-".to_string(),
        Some(share) if share < 0.1 => format!("{:.1}%", share * 100.0),
        Some(share) => format!("{:.0}%", share * 100.0),
    }
}

/// The activity section of an identity's page: channel activity by year and
/// month, and private messages when it never spoke in channels.
pub(super) fn identity_section(
    months: &[(String, Totals)],
    private_spoken: i64,
    bot: bool,
) -> Markup {
    let years = activity::years(months);
    let mut total = Totals::default();
    for (_, totals) in &years {
        total.days_present += totals.days_present;
        total.active_days += totals.active_days;
        total.minutes += totals.minutes;
        total.heard += totals.heard;
        total.spoken += totals.spoken;
    }
    let busiest = months
        .iter()
        .map(|(_, totals)| totals.days_present)
        .max()
        .unwrap_or(0);
    html! {
        h2 { "Activity" }
        @if bot {
            p.note { "It belongs to a bot, so activity rankings leave it out." }
        }
        @if months.is_empty() {
            p.muted { "No channel activity, apart from any as the logging client." }
        } @else {
            p.lead {
                "Present on " (number(total.days_present)) " days, spoke on "
                (number(total.active_days)) ", " (number(total.spoken)) " of "
                (number(total.heard)) " lines heard (" (share(&total)) ")."
            }
        }
        @if total.spoken == 0 {
            p.note {
                @if private_spoken > 0 {
                    "Never spoke in a channel, but sent " (number(private_spoken))
                    " private messages to the logging client."
                } @else {
                    "Never spoke in a channel or in private messages."
                }
            }
        }
        @if !months.is_empty() {
            (totals_table("Year", &years, None))
            details {
                summary { "By month" }
                (totals_table("Month", months, Some(busiest)))
            }
        }
    }
}

/// A table of activity by period. With `busiest`, each row also has a bar of
/// its days present compared with the busiest period.
fn totals_table(period: &str, rows: &[(String, Totals)], busiest: Option<i64>) -> Markup {
    html! {
        div.scroll {
            table {
                thead {
                    tr {
                        th { (period) }
                        th.num { "Days present" }
                        th.num { "Active days" }
                        th.num { "Time" }
                        th.num { "Heard" }
                        th.num { "Spoken" }
                        th.num { "Share" }
                        @if busiest.is_some() {
                            th.bar-cell { span.visually-hidden { "Days present, compared with the busiest month" } }
                        }
                    }
                }
                tbody {
                    @for (name, totals) in rows.iter().rev() {
                        tr {
                            td { (name) }
                            td.num { (number(totals.days_present)) }
                            td.num { (number(totals.active_days)) }
                            td.num { (duration(totals.minutes)) }
                            td.num { (number(totals.heard)) }
                            td.num { (number(totals.spoken)) }
                            td.num { (share(totals)) }
                            @if let Some(busiest) = busiest {
                                td.bar-cell {
                                    span.bar style=(format!("width: {:.1}%", percent(totals.days_present, busiest))) {}
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn percent(count: i64, largest: i64) -> f64 {
    if largest <= 0 {
        0.0
    } else {
        count as f64 * 100.0 / largest as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn totals(heard: i64, spoken: i64) -> Totals {
        Totals {
            days_present: 3,
            active_days: 1,
            minutes: 90,
            heard,
            spoken,
        }
    }

    #[test]
    fn shows_small_shares_with_a_decimal() {
        assert_eq!(share(&totals(0, 0)), "-");
        assert_eq!(share(&totals(1000, 3)), "0.3%");
        assert_eq!(share(&totals(10, 5)), "50%");
    }

    #[test]
    fn reads_the_ranking_filters() {
        let query = RankQuery {
            channel: Some("wetfish #analysis".into()),
            year: Some("2025".into()),
            order: Some("most".into()),
            min_days: Some("30".into()),
            bots: None,
        };
        let filter = query.filter();
        assert_eq!(filter.tag.as_deref(), Some("wetfish"));
        assert_eq!(filter.channel.as_deref(), Some("#analysis"));
        assert_eq!((filter.min_days, filter.order), (30, Order::MostActive));
        assert!(!filter.include_bots);

        let defaults = RankQuery::default().filter();
        assert_eq!(
            (defaults.min_days, defaults.order),
            (DEFAULT_MIN_DAYS, Order::LeastActive)
        );
        assert_eq!(defaults.channel, None);
    }

    #[test]
    fn shows_the_tag_only_when_ranking_all_channels() {
        let rows = [RankRow {
            identity_id: "1a2b3c4d5e6f".into(),
            tag: "wetfish".into(),
            totals: totals(40, 0),
            first_month: "2026-10".into(),
            last_month: "2026-11".into(),
            nicks: vec!["quokka".into()],
            people: Vec::new(),
            bot: false,
            private_spoken: 0,
        }];
        let channels = [("wetfish".to_string(), "#analysis".to_string())];
        let render = |query: RankQuery| {
            page(&query, &query.filter(), &rows, 1, &[], &channels).into_string()
        };

        let all = render(RankQuery::default());
        assert!(all.contains("<th>Tag</th>"));
        assert!(all.contains("<td>wetfish</td>"));

        let one = render(RankQuery {
            channel: Some("wetfish #analysis".into()),
            ..RankQuery::default()
        });
        assert!(!one.contains("<th>Tag</th>"));
        assert!(!one.contains("<td>wetfish</td>"));
    }

    #[test]
    fn notes_private_messages_of_identities_silent_in_channels() {
        let months = [("2026-11".to_string(), totals(40, 0))];
        let page = identity_section(&months, 12, true).into_string();
        assert!(page.contains("Never spoke in a channel, but sent 12 private messages"));
        assert!(page.contains("belongs to a bot"));
        assert!(page.contains("<td>2026</td>"));
    }
}
