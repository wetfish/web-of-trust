//! The overview and channel pages, and the error page.

use axum::Router;
use axum::extract::{Query, State};
use axum::routing::get;
use maud::{Markup, html};
use serde::Deserialize;

use super::layout::{self, channel_href, day, number};
use super::{App, Flash, PageError, PageResult, channels, internal, with_db};
use crate::stats::{self, ChannelDays, Overview, TagSummary};
use crate::trust;

pub(super) fn routes() -> Router<App> {
    Router::new()
        .route("/", get(overview_page))
        .route("/channel", get(channel_page))
}

async fn overview_page(State(app): State<App>, Query(flash): Query<Flash>) -> PageResult {
    with_db(&app, move |conn| {
        let overview = stats::overview(conn).map_err(internal)?;
        Ok(layout::page("Overview", &flash, self::overview(&overview)))
    })
    .await
}

#[derive(Deserialize)]
struct ChannelQuery {
    tag: String,
    name: String,
}

async fn channel_page(
    State(app): State<App>,
    Query(query): Query<ChannelQuery>,
    Query(flash): Query<Flash>,
) -> PageResult {
    with_db(&app, move |conn| {
        let channel = stats::channel_days(conn, &query.tag, &query.name)
            .map_err(internal)?
            .ok_or_else(|| {
                PageError::not_found(format!(
                    "No log files for {} in network tag {} have been imported.",
                    query.name, query.tag
                ))
            })?;
        let review = trust::review(conn, &channel.tag, &channel.name).map_err(internal)?;
        let who = trust::latest_who(conn, &channel.tag, &channel.name).map_err(internal)?;
        let checked = match &who {
            Some(list) => {
                trust::check(conn, &channel.tag, &channel.name, &list.entries).map_err(internal)?
            }
            None => Vec::new(),
        };
        let section = channels::trust_section(
            &channel.tag,
            &channel.name,
            review.as_ref(),
            who.as_ref(),
            &checked,
        );
        let title = format!("{} ({})", channel.name, channel.tag);
        Ok(layout::page(
            &title,
            &flash,
            self::channel(&channel, section),
        ))
    })
    .await
}

pub fn error(message: &str) -> Markup {
    html! {
        h1 { "Something went wrong" }
        p { (message) }
        p { a href="/" { "Back to the overview" } }
    }
}

pub fn overview(overview: &Overview) -> Markup {
    html! {
        h1 { "Overview" }
        p.lead {
            (number(overview.files)) " log files, " (number(overview.lines)) " lines imported."
            @if let Some(at) = &overview.last_import {
                " Last import " (at) " UTC."
            }
            " Run " code { "wot analyze" } " to import new lines."
        }
        @if overview.tags.is_empty() {
            p { "Nothing has been imported yet." }
        }
        @for tag in &overview.tags {
            (tag_section(tag))
        }
    }
}

fn tag_section(tag: &TagSummary) -> Markup {
    html! {
        section {
            h2 { "Network tag " (tag.tag) }
            ul.stats {
                li { a href="/identities" { strong { (number(tag.identities)) } " identities" } }
                li { a href="/people" { strong { (number(tag.people)) } " people" } }
                li { a href="/people" { strong { (number(tag.bots)) } " bots" } }
                li { a href="/hosts" { strong { (number(tag.host_groups)) } " host groups" } }
                li { a href="/cuts" { strong { (number(tag.cuts)) } " cuts" } }
            }
            p.note {
                "Not needed for this project? " code { "wot tags ignore " (tag.tag) }
                " deletes its lines and leaves it out of later imports."
            }
            @if tag.channels.is_empty() {
                p { "No channel logs." }
            } @else {
                div.scroll {
                    table {
                        thead {
                            tr {
                                th { "Channel" }
                                th.num { "Log files" }
                                th.num { "Lines" }
                                th.num { "Spoken" }
                                th.num { "Joins" }
                                th.num title="Identities with a session in the channel, the logging client included" { "Identities" }
                                th.num title="Of those identities, the ones that spoke at least once" { "Spoke" }
                                th.num title="The share of identities that never spoke" { "Silent" }
                                th.num title="Lines logged twice, by a client and a bouncer or two instances, counted once" { "Merged" }
                                th { "First seen" }
                                th { "Last seen" }
                            }
                        }
                        tbody {
                            @for channel in &tag.channels {
                                tr {
                                    td { a href=(channel_href(&tag.tag, &channel.name)) { (channel.name) } }
                                    td.num { (number(channel.files)) }
                                    td.num { (number(channel.lines)) }
                                    td.num { (number(channel.spoken)) }
                                    td.num { (number(channel.joins)) }
                                    td.num { (number(channel.identities)) }
                                    td.num { (number(channel.speakers)) }
                                    td.num { (silent_share(channel.identities, channel.speakers)) }
                                    td.num {
                                        @if channel.merged > 0 {
                                            (number(channel.merged))
                                        } @else {
                                            span.muted { "-" }
                                        }
                                    }
                                    td { (day(channel.first.as_deref())) }
                                    td { (day(channel.last.as_deref())) }
                                }
                            }
                        }
                    }
                }
            }
            p {
                "Private messages: " (number(tag.queries.targets)) " nicks, "
                (number(tag.queries.lines)) " lines, "
                (number(tag.queries.spoken)) " spoken."
            }
        }
    }
}

/// The share of identities that never spoke, as a whole percentage.
fn silent_share(identities: i64, speakers: i64) -> String {
    if identities == 0 {
        return "-".to_string();
    }
    let silent = (identities - speakers) as f64 / identities as f64;
    format!("{:.0}%", silent * 100.0)
}

/// A channel's page: its review, then its activity day by day.
pub fn channel(channel: &ChannelDays, review: Markup) -> Markup {
    let spoken: i64 = channel.days.iter().map(|day| day.spoken).sum();
    let busiest = channel.days.iter().map(|day| day.spoken).max().unwrap_or(0);
    html! {
        p.crumbs { a href="/channels" { "Channels" } " / " (channel.tag) }
        h1 { (channel.name) }
        (review)
        h2 { "Activity" }
        p {
            a href=(format!(
                "/activity?channel={}",
                layout::url_encode(&format!("{} {}", channel.tag, channel.name))
            )) { "Who is active and who is quiet in " (channel.name) }
        }
        @if channel.days.is_empty() {
            p.lead { "Nobody has spoken in this channel." }
        } @else {
            p.lead {
                (number(spoken)) " spoken lines by " (number(channel.nicks)) " nicks on "
                (number(channel.days.len() as i64)) " days, from "
                (channel.days[0].date) " to " (channel.days[channel.days.len() - 1].date) "."
            }
            p.note {
                "Nicks are counted as written, ignoring case, so a person counts once per nick. "
                "Days without spoken lines are left out. Most recent first."
            }
            div.scroll {
                table {
                    thead {
                        tr {
                            th { "Day" }
                            th.num { "Spoken" }
                            th.num { "Nicks" }
                            th.bar-cell { span.visually-hidden { "Spoken, compared with the busiest day" } }
                        }
                    }
                    tbody {
                        @for day in channel.days.iter().rev() {
                            tr {
                                td { (day.date) }
                                td.num { (number(day.spoken)) }
                                td.num { (number(day.nicks)) }
                                td.bar-cell {
                                    span.bar style=(format!("width: {:.1}%", share(day.spoken, busiest))) {}
                                }
                            }
                        }
                    }
                }
            }
        }
        details {
            summary { "Log files read for this channel" }
            ul {
                @for path in &channel.paths {
                    li { code { (path) } }
                }
            }
        }
    }
}

/// A count as a percentage of the largest count.
fn share(count: i64, largest: i64) -> f64 {
    if largest <= 0 {
        0.0
    } else {
        count as f64 * 100.0 / largest as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::{Day, QuerySummary, TargetSummary};

    #[test]
    fn shows_the_share_of_silent_identities() {
        assert_eq!(silent_share(0, 0), "-");
        assert_eq!(silent_share(4, 1), "75%");
        assert_eq!(silent_share(3, 3), "0%");
    }

    #[test]
    fn links_channels_from_the_overview() {
        let overview = Overview {
            last_import: Some("2026-11-02 18:00:00".into()),
            files: 1,
            lines: 10,
            tags: vec![TagSummary {
                tag: "wetfish".into(),
                identities: 2,
                people: 0,
                bots: 0,
                host_groups: 0,
                cuts: 0,
                channels: vec![TargetSummary {
                    name: "#dry fish".into(),
                    files: 1,
                    lines: 10,
                    spoken: 3,
                    joins: 2,
                    identities: 2,
                    speakers: 1,
                    merged: 4,
                    first: Some("2026-11-01 01:00".into()),
                    last: Some("2026-11-02 09:00".into()),
                }],
                queries: QuerySummary::default(),
            }],
        };
        let page = super::overview(&overview).into_string();
        assert!(page.contains(r#"href="/channel?tag=wetfish&amp;name=%23dry%20fish""#));
        assert!(page.contains("50%"));
        assert!(page.contains("<code>wot tags ignore wetfish</code>"));
    }

    #[test]
    fn draws_bars_relative_to_the_busiest_day() {
        let channel = ChannelDays {
            tag: "wetfish".into(),
            name: "#analysis".into(),
            paths: vec!["/logs/wetfish/#analysis.log".into()],
            days: vec![
                Day {
                    date: "2026-11-01".into(),
                    spoken: 4,
                    nicks: 2,
                },
                Day {
                    date: "2026-11-02".into(),
                    spoken: 1,
                    nicks: 1,
                },
            ],
            nicks: 2,
        };
        let page = super::channel(&channel, html! {}).into_string();
        assert!(page.contains("width: 100.0%"));
        assert!(page.contains("width: 25.0%"));
        assert!(page.contains("from 2026-11-01 to 2026-11-02"));
    }
}
