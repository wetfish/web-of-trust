//! Channels: which ones to review, and checking a channel's current users,
//! pasted from irssi's `/who`, against the logs and your decisions.

use axum::Router;
use axum::extract::{Form, Query, State};
use axum::response::Redirect;
use axum::routing::{get, post};
use maud::{Markup, html};
use serde::Deserialize;

use super::layout::{
    self, channel_href, day, identity_href, lapse_flag, number, person_href, search_href,
    silent_flag,
};
use super::{App, Flash, PageError, PageResult, done, failed, internal, with_db};
use crate::decide;
use crate::trust::{
    self, ChannelStatus, Checked, Match, REVIEW_VALID_MONTHS, Review, Verdict, WhoList,
};

pub(super) fn routes() -> Router<App> {
    Router::new()
        .route("/channels", get(channels_page))
        .route("/channels/reviewed", post(reviewed))
        .route("/channels/who", post(who))
        .route("/channels/who/delete", post(delete_who))
}

async fn channels_page(State(app): State<App>, Query(flash): Query<Flash>) -> PageResult {
    with_db(&app, move |conn| {
        let channels = trust::channels(conn).map_err(internal)?;
        Ok(layout::page("Channels", &flash, list(&channels)))
    })
    .await
}

fn list(channels: &[ChannelStatus]) -> Markup {
    let tags = channels
        .iter()
        .any(|channel| channel.tag != channels[0].tag);
    html! {
        h1 { "Channels" }
        p.note {
            "Channels with activity, the busiest over the last " (trust::RECENT_MONTHS)
            " months first. Open a channel to check its current users and mark it reviewed; "
            "a review stays valid for " (REVIEW_VALID_MONTHS) " months."
        }
        @if channels.is_empty() {
            p { "No channel activity yet. Import logs with " code { "wot analyze" } "." }
        } @else {
            div.scroll {
                table {
                    thead {
                        tr {
                            th { "Channel" }
                            @if tags { th { "Tag" } }
                            th.num title=(format!("Spoken lines over the last {} months", trust::RECENT_MONTHS)) {
                                "Recent spoken"
                            }
                            th.num title=(format!("Identities present over the last {} months", trust::RECENT_MONTHS)) {
                                "Recent identities"
                            }
                            th { "Last active" }
                            th.num { "Spoken, all time" }
                            th { "Review" }
                            th { "Last user list" }
                        }
                    }
                    tbody {
                        @for channel in channels {
                            tr {
                                td { a href=(channel_href(&channel.tag, &channel.name)) { (channel.name) } }
                                @if tags { td { (channel.tag) } }
                                td.num { (number(channel.recent_spoken)) }
                                td.num { (number(channel.recent_identities)) }
                                td { (channel.last_month) }
                                td.num { (number(channel.spoken)) }
                                td { (review_status(channel.review.as_ref())) }
                                td { (day(channel.last_who.as_deref())) }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A channel's review status in a few words.
fn review_status(review: Option<&Review>) -> Markup {
    html! {
        @match review {
            None => span.muted { "Never reviewed" },
            Some(review) if review.valid => span.verdict-trusted {
                "Reviewed " (day(Some(review.reviewed_at.as_str())))
            },
            Some(review) => span.warning {
                "Expired " (day(Some(review.valid_until.as_str())))
            },
        }
    }
}

/// The part of a channel's page for reviewing its users: the review status,
/// a form to paste `/who` output, and the users of the latest list.
pub(super) fn trust_section(
    tag: &str,
    name: &str,
    review: Option<&Review>,
    who: Option<&WhoList>,
    checked: &[Checked],
) -> Markup {
    html! {
        section.decision {
            h2 { "Review" }
            div.review-status {
                @match review {
                    None => span.warning { "Never reviewed." },
                    Some(review) if review.valid => span.verdict-trusted {
                        "Reviewed " (day(Some(review.reviewed_at.as_str()))) ", valid until "
                        (day(Some(review.valid_until.as_str()))) "."
                    },
                    Some(review) => span.warning {
                        "The review of " (day(Some(review.reviewed_at.as_str())))
                        " expired on " (day(Some(review.valid_until.as_str()))) "."
                    },
                }
                " "
                form.inline method="post" action="/channels/reviewed" {
                    input type="hidden" name="tag" value=(tag);
                    input type="hidden" name="channel" value=(name);
                    button type="submit" { "Mark reviewed" }
                }
            }
            p.note {
                "Mark the channel reviewed once you have gone through its users. A review stays "
                "valid for " (REVIEW_VALID_MONTHS) " months."
            }
        }

        section.decision {
            h2 { "Who is here" }
            form.stacked method="post" action="/channels/who" {
                input type="hidden" name="tag" value=(tag);
                input type="hidden" name="channel" value=(name);
                label {
                    "Run " code { "/who " (name) } " in irssi and paste what it prints"
                    textarea name="text" rows="6" required
                        placeholder=(format!("-!- {name} quokka H 0 quokka@example.net [Quokka]")) {}
                }
                button type="submit" { "Check users" }
            }
            @if let Some(list) = who {
                (who_results(tag, name, list, checked))
            }
        }
    }
}

fn who_results(tag: &str, name: &str, list: &WhoList, checked: &[Checked]) -> Markup {
    let of = |verdict: Verdict| {
        let mut users: Vec<&Checked> = checked.iter().filter(|c| c.verdict == verdict).collect();
        users.sort_by(|a, b| {
            let order = match verdict {
                // Silent watchers first, the longest silent first, then
                // those never seen, then the least trusted.
                Verdict::Untrusted => b
                    .silent_days
                    .cmp(&a.silent_days)
                    .then(a.trust.unwrap_or(-1).cmp(&b.trust.unwrap_or(-1))),
                _ => b.trust.unwrap_or(-1).cmp(&a.trust.unwrap_or(-1)),
            };
            order.then_with(|| {
                a.entry
                    .nick
                    .to_lowercase()
                    .cmp(&b.entry.nick.to_lowercase())
            })
        });
        users
    };
    let groups = [
        (
            "Trusted",
            "Rated vouched or known, from a nick and ident@host they used before.",
            of(Verdict::Trusted),
        ),
        (
            "Inactive",
            "Not rated yet, and spoke in at least 3 months, but not in the 6 months before the \
             newest log line: probably someone who used to take part. Rate them to move them.",
            of(Verdict::Inactive),
        ),
        (
            "Untrusted",
            "Silent for weeks in this channel, silent for months after saying very little, never \
             seen in the logs, or rated wary or distrusted.",
            of(Verdict::Untrusted),
        ),
        (
            "Undecided",
            "Not rated yet, not linked to a person, or only their nick was seen before.",
            of(Verdict::Undecided),
        ),
        ("Bots", "Identities that belong to a bot.", of(Verdict::Bot)),
    ];
    let silent = checked.iter().filter(|c| c.silent_days.is_some()).count();
    html! {
        p.lead {
            (number(checked.len() as i64)) " users, pasted " (list.pasted_at) " UTC: "
            @for (i, (title, _, users)) in groups.iter().enumerate() {
                @if i > 0 { ", " }
                (number(users.len() as i64)) " " (title.to_lowercase())
            }
            "."
        }
        @if silent > 0 {
            p.flag-banner {
                (silent_flag_count(silent))
                " present on at least " (trust::SILENT_DAYS) " days without a line: never in "
                (name) ", or nowhere since saying very little more than "
                (trust::INACTIVE_MONTHS) " months ago. They are listed as untrusted until you "
                "rate them vouched or known."
            }
        }
        @for (title, about, users) in &groups {
            @if !users.is_empty() {
                h3 { (title) " (" (users.len()) ")" }
                p.note { (about) }
                (users_table(users))
            }
        }
        p.note {
            @if list.earlier > 0 {
                (number(list.earlier)) " earlier lists are kept for the real names they hold. "
            }
            "Each list is kept, so real names show on identity pages."
        }
        form.inline method="post" action="/channels/who/delete" {
            input type="hidden" name="id" value=(list.id);
            input type="hidden" name="tag" value=(tag);
            input type="hidden" name="channel" value=(name);
            button.danger type="submit" { "Delete this list" }
        }
    }
}

fn users_table(users: &[&Checked]) -> Markup {
    html! {
        div.scroll {
            table {
                thead {
                    tr {
                        th { "Nick" }
                        th { "ident@host" }
                        th { "Real name" }
                        th { "Person or bot" }
                        th.num title="Rating band plus activity, 0 to 99" { "Trust" }
                        th.num title="Out of 100" { "Activity" }
                        th { "Identity" }
                        th { "Match" }
                    }
                }
                tbody {
                    @for user in users {
                        tr.flagged[user.silent_days.is_some()] {
                            td {
                                (user.entry.nick)
                                " " span.muted title="Status from /who: H here, G away, * IRC operator, then channel modes" {
                                    (user.entry.status)
                                }
                            }
                            td { code { (user.entry.ident) "@" (user.entry.host) } }
                            td.wrap { (user.entry.realname) }
                            td {
                                @match &user.record {
                                    Some(record) => {
                                        a href=(person_href(&record.name)) { (record.name) }
                                        @if record.kind == crate::stats::Kind::Bot {
                                            " " span.muted { "bot" }
                                        } @else {
                                            " " span.muted { (record.rating.label()) }
                                        }
                                    },
                                    None => span.muted { "-" },
                                }
                            }
                            td.num {
                                @match user.trust {
                                    Some(trust) => (trust),
                                    None => span.muted { "-" },
                                }
                            }
                            td.num {
                                @if user.how == Match::Unknown {
                                    span.muted { "-" }
                                } @else {
                                    (user.activity.score)
                                }
                            }
                            td {
                                @match &user.identity {
                                    Some(id) => a href=(identity_href(id)) { code { (id) } },
                                    None => a href=(search_href(&[("host", user.entry.host.as_str())])) {
                                        "search host"
                                    },
                                }
                            }
                            td.wrap {
                                @if let Some(days) = user.silent_days {
                                    (silent_flag(days)) " "
                                } @else if user.verdict != Verdict::Bot && user.how != Match::Unknown {
                                    (lapse_flag(&user.activity, user.verdict == Verdict::Trusted)) " "
                                }
                                (match_note(user))
                            }
                        }
                    }
                }
            }
        }
    }
}

/// "1 user" or "N users", in the flag's colours.
fn silent_flag_count(count: usize) -> Markup {
    html! {
        span.flag {
            "\u{2691} " (number(count as i64)) @if count == 1 { " user" } @else { " users" }
        }
    }
}

/// How a user was matched, when it is worth saying.
fn match_note(user: &Checked) -> Markup {
    html! {
        @match user.how {
            Match::Exact => {},
            Match::NewNick => "New nick, from an ident@host this identity used.",
            Match::NickOnly => span.warning {
                "Nick seen before, but never from this ident@host. Anyone can take a nick."
            },
            Match::Unknown => span.verdict-untrusted { "Never seen in the logs." },
        }
        @if user.candidates > 1 {
            " " (user.candidates) " identities match; the most recently seen is shown."
        }
    }
}

#[derive(Deserialize)]
struct ChannelForm {
    tag: String,
    channel: String,
}

async fn reviewed(
    State(app): State<App>,
    Form(form): Form<ChannelForm>,
) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        let back = channel_href(&form.tag, &form.channel);
        Ok(
            match decide::mark_reviewed(conn, &form.tag, &form.channel) {
                Ok(()) => done(&back, &format!("Marked {} reviewed.", form.channel)),
                Err(err) => failed(&back, err),
            },
        )
    })
    .await
}

#[derive(Deserialize)]
struct WhoForm {
    tag: String,
    channel: String,
    text: String,
}

async fn who(State(app): State<App>, Form(form): Form<WhoForm>) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        let back = channel_href(&form.tag, &form.channel);
        let parsed = trust::parse_who(&form.text, &form.channel);
        if parsed.entries.is_empty() {
            let error = if parsed.other_channels.is_empty() {
                format!(
                    "No /who replies found. Run /who {} in irssi and paste the lines it prints.",
                    form.channel
                )
            } else {
                format!(
                    "These replies are for {}, not {}.",
                    parsed.other_channels.join(", "),
                    form.channel
                )
            };
            return Ok(failed(&back, error));
        }
        Ok(
            match trust::save_who(conn, &form.tag, &form.channel, &parsed.entries) {
                Ok(_) => {
                    let mut notice = format!("Checked {} users.", parsed.entries.len());
                    if !parsed.other_channels.is_empty() {
                        notice.push_str(&format!(
                            " Left out replies for {}.",
                            parsed.other_channels.join(", ")
                        ));
                    }
                    if parsed.skipped > 0 {
                        notice.push_str(&format!(" Ignored {} other lines.", parsed.skipped));
                    }
                    done(&back, &notice)
                }
                Err(err) => failed(&back, err),
            },
        )
    })
    .await
}

#[derive(Deserialize)]
struct DeleteWhoForm {
    id: i64,
    tag: String,
    channel: String,
}

async fn delete_who(
    State(app): State<App>,
    Form(form): Form<DeleteWhoForm>,
) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        let back = channel_href(&form.tag, &form.channel);
        Ok(match trust::delete_who(conn, form.id) {
            Ok(()) => done(&back, "Deleted the user list."),
            Err(err) => failed(&back, err),
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::{ActivityScore, Lapse, Record, WhoEntry};

    fn user(nick: &str, how: Match, verdict: Verdict, trust: Option<i64>) -> Checked {
        Checked {
            entry: WhoEntry {
                nick: nick.into(),
                ident: nick.into(),
                host: "example.net".into(),
                status: "H".into(),
                realname: format!("{nick} <real>"),
            },
            how,
            identity: (how != Match::Unknown).then(|| "1a2b3c4d5e6f".to_string()),
            candidates: usize::from(how != Match::Unknown),
            record: (verdict == Verdict::Trusted).then(|| Record {
                name: nick.into(),
                kind: crate::stats::Kind::Person,
                rating: crate::stats::Rating::Known,
                identities: Vec::new(),
            }),
            activity: ActivityScore::default(),
            trust,
            silent_days: None,
            verdict,
        }
    }

    #[test]
    fn sorts_a_channels_users_into_trusted_untrusted_and_undecided() {
        let checked = [
            user("marlowe", Match::NickOnly, Verdict::Undecided, Some(45)),
            user("quokka", Match::Exact, Verdict::Trusted, Some(90)),
            user("stranger", Match::Unknown, Verdict::Untrusted, None),
            user("wary", Match::Exact, Verdict::Untrusted, Some(30)),
            Checked {
                silent_days: Some(120),
                ..user("watcher", Match::Exact, Verdict::Untrusted, Some(45))
            },
            Checked {
                activity: ActivityScore {
                    score: 20,
                    full_score: 100,
                    cut: true,
                    active_months: 30,
                    last_spoke: Some(2025 * 12 + 2),
                    lapse: Some(Lapse::Inactive),
                    ..ActivityScore::default()
                },
                ..user("regular", Match::Exact, Verdict::Inactive, Some(43))
            },
        ];
        let list = WhoList {
            id: 3,
            pasted_at: "2026-11-03 07:52:00".into(),
            entries: Vec::new(),
            earlier: 2,
        };
        let page = who_results("wetfish", "#analysis", &list, &checked).into_string();
        assert!(page.contains(
            "6 users, pasted 2026-11-03 07:52:00 UTC: 1 trusted, 1 inactive, 3 untrusted, \
             1 undecided, 0 bots."
        ));
        assert!(page.contains("\u{2691} 1 user</span> present on at least 30 days"));
        assert!(page.contains("never in #analysis"));
        assert!(page.contains("\u{2691} Silent 120 days"));
        assert!(page.contains(r#"<tr class="flagged">"#));
        let at = |text: &str| page.find(text).unwrap();
        assert!(at("<h3>Trusted (1)</h3>") < at("<h3>Inactive (1)</h3>"));
        assert!(at("<h3>Inactive (1)</h3>") < at("<h3>Untrusted (3)</h3>"));
        assert!(at("<h3>Inactive (1)</h3>") < at("Inactive since 2025-03"));
        assert!(page.contains(r#"<span class="flag-inactive" title="Spoke in 30 months"#));
        assert!(at("<h3>Untrusted (3)</h3>") < at("<h3>Undecided (1)</h3>"));
        assert!(!page.contains("<h3>Bots"));
        // Silent watchers come first, then never seen, then the least trusted.
        assert!(at("watcher &lt;real&gt;") < at("stranger &lt;real&gt;"));
        assert!(at("stranger &lt;real&gt;") < at("wary &lt;real&gt;"));
        assert!(page.contains("Anyone can take a nick."));
        assert!(page.contains("/identities?host=example.net"));
        assert!(page.contains(r#"href="/person?name=quokka""#));
        assert!(page.contains(r#"name="id" value="3""#));
    }

    #[test]
    fn shows_whether_a_review_is_still_valid() {
        let review = |valid| Review {
            reviewed_at: "2026-11-03 07:00:00".into(),
            valid_until: "2027-05-03 07:00:00".into(),
            valid,
        };
        let section = |review: Option<&Review>| {
            trust_section("wetfish", "#analysis", review, None, &[]).into_string()
        };
        assert!(section(None).contains("Never reviewed."));
        assert!(section(Some(&review(true))).contains("valid until 2027-05-03"));
        assert!(section(Some(&review(false))).contains("expired on 2027-05-03"));
        assert!(section(None).contains(r#"action="/channels/who""#));
    }
}
