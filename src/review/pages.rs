//! The contents of each review page.

use maud::{Markup, html};

use super::layout::{channel_href, day, number};
use crate::stats::{ChannelDays, Overview, TagSummary};

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
                li { strong { (number(tag.identities)) } " identities" }
                li { strong { (number(tag.people)) } " people" }
                li { strong { (number(tag.host_groups)) } " host groups" }
                li { strong { (number(tag.cuts)) } " cuts" }
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
                                th.num title="Identities with a session in the channel, apart from the logging client" { "Identities" }
                                th.num title="Of those identities, the ones that spoke at least once" { "Spoke" }
                                th.num title="The share of identities that never spoke" { "Silent" }
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

pub fn channel(channel: &ChannelDays) -> Markup {
    let spoken: i64 = channel.days.iter().map(|day| day.spoken).sum();
    let busiest = channel.days.iter().map(|day| day.spoken).max().unwrap_or(0);
    html! {
        p.crumbs { a href="/" { "Overview" } " / " (channel.tag) }
        h1 { (channel.name) }
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
                    first: Some("2026-11-01 01:00".into()),
                    last: Some("2026-11-02 09:00".into()),
                }],
                queries: QuerySummary::default(),
            }],
        };
        let page = super::overview(&overview).into_string();
        assert!(page.contains(r#"href="/channel?tag=wetfish&amp;name=%23dry%20fish""#));
        assert!(page.contains("50%"));
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
        let page = super::channel(&channel).into_string();
        assert!(page.contains("width: 100.0%"));
        assert!(page.contains("width: 25.0%"));
        assert!(page.contains("from 2026-11-01 to 2026-11-02"));
    }
}
