//! People and bots: who is behind identities, as decided by hand. Both are
//! person records, told apart by their kind.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use axum::Router;
use axum::extract::{Form, Query, State};
use axum::response::Redirect;
use axum::routing::{get, post};
use chrono::NaiveDate;
use maud::{Markup, html};
use serde::Deserialize;

use super::activity::activity_breakdown;
use super::history::{person_history_href, said};
use super::identities::{
    Ticks, identity_table, lurker_of, nick_rows, recent_connections, summary_connections,
    summary_nicks, summary_real_names,
};
use super::layout::{
    self, back_field, day, duration, lapse_flag, lurker_field, number, person_href, silent_flag,
    url_encode,
};
use super::{App, Flash, PageError, PageResult, back_to, done, failed, filled, internal, with_db};
use crate::decide;
use crate::history::{self, Day, Line};
use crate::lookup::{self, Filter};
use crate::stats::{self, Kind, Person, Rating};
use crate::trust::{self, ActivityScore};

pub(super) fn routes() -> Router<App> {
    Router::new()
        .route("/people", get(people_page))
        .route("/person", get(person_page))
        .route("/people/create", post(create))
        .route("/people/add", post(add))
        .route("/people/remove", post(remove))
        .route("/people/delete", post(delete))
        .route("/people/kind", post(set_kind))
        .route("/people/rate", post(rate))
        .route("/people/invited-by", post(invited_by))
        .route("/people/notes/add", post(add_note))
        .route("/people/notes/delete", post(delete_note))
        .route("/people/link", post(link))
}

async fn people_page(State(app): State<App>, Query(flash): Query<Flash>) -> PageResult {
    with_db(&app, move |conn| {
        let people = stats::people(conn).map_err(internal)?;
        let newest = trust::newest_month(conn).map_err(internal)?;
        let mut scores = HashMap::new();
        for person in &people {
            let score =
                trust::activity_score(conn, &person.identity_ids(), newest).map_err(internal)?;
            scores.insert(person.name.clone(), score);
        }
        Ok(layout::page(
            "People and bots",
            &flash,
            list(&people, &scores),
        ))
    })
    .await
}

fn list(people: &[Person], scores: &HashMap<String, ActivityScore>) -> Markup {
    let of = |kind: Kind| {
        people
            .iter()
            .filter(|person| person.kind == kind)
            .collect::<Vec<&Person>>()
    };
    html! {
        h1 { "People and bots" }
        p.note {
            "A person is a human behind one or more identities, from any network tag, and a bot "
            "is the same for a program. Activity rankings leave bots out unless asked. Identities "
            "belong to at most one person or bot. Add identities from a search, from their page, "
            "or by id here."
        }
        h2 { "People" }
        p.note {
            "Most trusted first. The trust score, 0 to 99, is your rating, which sets a band of "
            "20 points, and the activity score, which places them within it."
        }
        (records_table(&of(Kind::Person), "No people yet.", scores))
        h2 { "Bots" }
        (records_table(&of(Kind::Bot), "No bots yet.", scores))
        h2 { "New person or bot" }
        form.stacked method="post" action="/people/create" {
            (back_field("/people"))
            label { "Name " input type="text" name="name" required; }
            label {
                "Kind "
                select name="kind" {
                    option value="person" { "person" }
                    option value="bot" { "bot" }
                }
            }
            label {
                "Identity ids, separated by spaces (optional) "
                input type="text" name="ids";
            }
            label { "Note (optional) " input type="text" name="note"; }
            button type="submit" { "Create" }
        }
    }
}

/// People most trusted first, or bots most active first.
fn records_table(
    records: &[&Person],
    empty: &str,
    scores: &HashMap<String, ActivityScore>,
) -> Markup {
    let activity = |person: &Person| scores.get(&person.name).map_or(0, |score| score.score);
    let trust = |person: &Person| trust::trust_score(person.rating, activity(person));
    let people = records.iter().any(|person| person.kind == Kind::Person);
    let mut records = records.to_vec();
    records.sort_by(|a, b| {
        let key = |person: &Person| {
            if people {
                trust(person)
            } else {
                activity(person)
            }
        };
        key(b).cmp(&key(a)).then(a.name.cmp(&b.name))
    });
    html! {
        @if records.is_empty() {
            p.muted { (empty) }
        } @else {
            div.scroll {
                table {
                    thead {
                        tr {
                            th { "Name" }
                            @if people {
                                th { "Rating" }
                                th.num title="Rating band plus activity, 0 to 99" { "Trust" }
                            }
                            th.num title="Out of 100" { "Activity" }
                            th.num { "Identities" }
                            th { "Nicks" }
                            th { "Last seen" }
                            th { "Note" }
                        }
                    }
                    tbody {
                        @for person in &records {
                            tr {
                                td { a href=(person_href(&person.name)) { (person.name) } }
                                @if people {
                                    td { (person.rating.label()) }
                                    td.num { (trust(person)) }
                                }
                                td.num { (activity(person)) }
                                td.num { (number(person.identities.len() as i64)) }
                                td.wrap { (nick_summary(person)) }
                                td {
                                    (day(last_seen(person)))
                                    @if let (Kind::Person, Some(score)) = (person.kind, scores.get(&person.name)) {
                                        " " (lapse_flag(score, person.rating.trusted()))
                                    }
                                }
                                td.wrap { (latest_note(person)) }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The newest note, or the one given when the record was created.
fn latest_note(person: &Person) -> &str {
    person
        .notes
        .last()
        .map(|note| note.text.as_str())
        .or(person.note.as_deref())
        .unwrap_or_default()
}

/// Options for choosing a person or bot by name, with bots in their own
/// group.
pub(super) fn record_options(records: &[(String, Kind)]) -> Markup {
    let of = |kind: Kind| {
        records
            .iter()
            .filter(move |(_, k)| *k == kind)
            .map(|(name, _)| name)
    };
    html! {
        @for name in of(Kind::Person) {
            option value=(name) { (name) }
        }
        @if of(Kind::Bot).next().is_some() {
            optgroup label="Bots" {
                @for name in of(Kind::Bot) {
                    option value=(name) { (name) }
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct PersonQuery {
    name: String,
    /// `yes` to tick every identity.
    all: Option<String>,
}

async fn person_page(
    State(app): State<App>,
    Query(query): Query<PersonQuery>,
    Query(flash): Query<Flash>,
) -> PageResult {
    with_db(&app, move |conn| {
        let person = stats::person(conn, &query.name)
            .map_err(internal)?
            .ok_or_else(|| {
                PageError::not_found(format!("There is no person named {}.", query.name))
            })?;
        let filter = Filter {
            person: Some(person.name.clone()),
            ..Filter::default()
        };
        let views = lookup::find(conn, &filter).map_err(internal)?;
        let ids = person.identity_ids();
        let (mut last_said, _) =
            history::lines(conn, &ids, &history::Filter::default()).map_err(internal)?;
        last_said.truncate(LAST_SAID);
        // Each real name once, with when it was last seen on any identity.
        let mut real_names: HashMap<String, String> = HashMap::new();
        for id in &ids {
            for (name, seen) in trust::real_names(conn, id).map_err(internal)? {
                let last = real_names.entry(name).or_default();
                if seen > *last {
                    *last = seen;
                }
            }
        }
        let mut real_names: Vec<(String, String)> = real_names.into_iter().collect();
        real_names.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let today: String = conn
            .query_row("SELECT date('now')", [], |row| row.get(0))
            .map_err(internal)?;
        let context = PersonContext {
            all: filled(&query.all) == Some("yes"),
            activity: trust::activity_score(
                conn,
                &ids,
                trust::newest_month(conn).map_err(internal)?,
            )
            .map_err(internal)?,
            inviters: stats::people_names(conn)
                .map_err(internal)?
                .into_iter()
                .filter(|(name, kind)| *kind == Kind::Person && *name != person.name)
                .map(|(name, _)| name)
                .collect(),
            days: history::days(conn, &ids).map_err(internal)?,
            last_said,
            real_names,
            today: NaiveDate::parse_from_str(&today, "%Y-%m-%d").ok(),
        };
        Ok(layout::page(
            &person.name,
            &flash,
            page(&person, &views, &context),
        ))
    })
    .await
}

/// Lines shown under "Last said" on a person's page.
const LAST_SAID: usize = 10;
/// Channels listed in a person's summary.
const MAX_SUMMARY_CHANNELS: usize = 5;

/// What the person page shows besides the person.
struct PersonContext {
    /// Whether every identity starts ticked.
    all: bool,
    activity: ActivityScore,
    /// Everyone who could have invited the person.
    inviters: Vec<String>,
    /// The days any of their identities was present in a channel.
    days: BTreeMap<NaiveDate, Day>,
    /// The most recent lines they said in channels, newest first.
    last_said: Vec<Line>,
    /// Real names `/who` showed for any of their identities, with when each
    /// was last seen, the most recent first.
    real_names: Vec<(String, String)>,
    /// For how long ago they were last seen.
    today: Option<NaiveDate>,
}

/// How long before `today` a `YYYY-MM-DD` day, or a timestamp starting with
/// one, was: `today`, `yesterday` or `12 days ago`.
fn ago(today: Option<NaiveDate>, at: &str) -> Option<String> {
    let at = NaiveDate::parse_from_str(at.get(..10)?, "%Y-%m-%d").ok()?;
    Some(match (today? - at).num_days() {
        ..=0 => "today".to_string(),
        1 => "yesterday".to_string(),
        days => format!("{} days ago", number(days)),
    })
}

/// The channels present in on the most days, with their days, most first.
fn top_channels(days: &BTreeMap<NaiveDate, Day>) -> Vec<(&str, i64)> {
    let mut counts: HashMap<&str, i64> = HashMap::new();
    for day in days.values() {
        for channel in &day.channels {
            *counts.entry(channel.as_str()).or_default() += 1;
        }
    }
    let mut channels: Vec<(&str, i64)> = counts.into_iter().collect();
    channels.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    channels
}

/// Who the person is across all their identities, when they were last
/// active, and how much they take part, before anything to decide.
fn summary(person: &Person, views: &[lookup::IdentityView], context: &PersonContext) -> Markup {
    let sessions = || views.iter().flat_map(|view| &view.sessions);
    let nicks = nick_rows(sessions());
    let nicks: Vec<&str> = nicks.iter().map(|row| row.nick.as_str()).collect();
    let connections = recent_connections(sessions());
    let last_seen = sessions().max_by(|a, b| a.end_at.cmp(&b.end_at));
    let tags: BTreeSet<&str> = views.iter().map(|view| view.tag.as_str()).collect();
    let days = &context.days;
    let present = days.len() as i64;
    let spoke_on = days.values().filter(|day| day.spoken > 0).count() as i64;
    let spoken: i64 = days.values().map(|day| day.spoken).sum();
    let channels = top_channels(days);
    let silent =
        person.kind == Kind::Person && !person.rating.trusted() && trust::silent(present, spoken);
    let lurker = lurker_of(person.kind, person.rating, sessions(), days);
    let history = person_history_href(&person.name);
    html! {
        dl.summary {
            dt { "Nicks" }
            dd { (summary_nicks(&nicks, "No nicks seen")) }
            (summary_real_names(&context.real_names))
            dt { "Connects from" }
            dd { (summary_connections(&connections)) }
            dt { "Identities" }
            dd {
                a href="#identities" { (number(views.len() as i64)) }
                @if !tags.is_empty() {
                    " on " (tags.into_iter().collect::<Vec<_>>().join(", "))
                }
            }
            @if person.kind == Kind::Person {
                dt { "Trust" }
                dd {
                    "Trust score " strong { (trust::trust_score(person.rating, context.activity.score)) }
                    " of 99, rated " (person.rating.label())
                    @if let Some(by) = &person.invited_by {
                        ", invited by " a href=(person_href(by)) { (by) }
                    }
                }
            } @else {
                dt { "Kind" }
                dd { "A bot: not rated, and left out of activity rankings unless asked" }
            }
            dt { "Last seen" }
            dd {
                @match last_seen {
                    Some(session) => {
                        strong { (day(Some(session.end_at.as_str()))) }
                        @if let Some(ago) = ago(context.today, &session.end_at) {
                            " (" (ago) ")"
                        }
                        @if session.channel {
                            " in " (session.target)
                        } @else {
                            " in a private message"
                        }
                    },
                    None => span.muted { "Never: no identity has sessions" },
                }
            }
            dt { "Last spoke" }
            dd {
                @match context.last_said.first() {
                    Some(line) if !line.date.is_empty() => {
                        strong { (line.date) }
                        @if let Some(ago) = ago(context.today, &line.date) {
                            " (" (ago) ")"
                        }
                        " in " (line.channel) ", " a href="#said" { "below" }
                    },
                    Some(line) => {
                        "In " (line.channel) ", on a day the log does not give, "
                        a href="#said" { "below" }
                    },
                    None => span.muted { "Never in a channel" },
                }
            }
            dt { "Present" }
            dd {
                @match (days.first_key_value(), days.last_key_value()) {
                    (Some((first, _)), Some((last, _))) => {
                        "On " (number(present)) " days from " (first) " to " (last) ", "
                        (duration(lookup::present_minutes(sessions()))) " in channels; spoke on "
                        (number(spoke_on)) " of them, " (number(spoken)) " lines; activity score "
                        (context.activity.score) " of 100"
                        @if silent {
                            " " (silent_flag(present))
                        } @else if person.kind == Kind::Person {
                            " " (lapse_flag(&context.activity, person.rating.trusted()))
                        }
                    },
                    _ => span.muted { "Never in a channel" },
                }
            }
            @if !channels.is_empty() {
                dt { "Mostly in" }
                dd {
                    @for (i, (channel, count)) in channels.iter().take(MAX_SUMMARY_CHANNELS).enumerate() {
                        @if i > 0 { ", " }
                        a href=(format!("{history}&channel={}", url_encode(channel))) { (channel) }
                        span.muted { " (" (number(*count)) " days)" }
                    }
                    @if channels.len() > MAX_SUMMARY_CHANNELS {
                        " and " (channels.len() - MAX_SUMMARY_CHANNELS) " more"
                    }
                }
            }
            @if let Some(lurker) = &lurker {
                dt { "Lurker" }
                dd { (lurker_field(lurker)) }
            }
        }
    }
}

/// The most recent lines, each day a link to that day in the history.
fn last_said(person: &Person, lines: &[Line]) -> Markup {
    let history = person_history_href(&person.name);
    html! {
        @if lines.is_empty() {
            p.muted { "Nothing said in channels." }
        } @else {
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
                                        a href=(format!("{history}&day={}", url_encode(&line.date))) {
                                            (line.date)
                                        }
                                    }
                                }
                                td { (line.time) }
                                td { (line.channel) }
                                td.wrap { (said(line)) }
                            }
                        }
                    }
                }
            }
        }
        p { a href=(history) { "Everything they said, and the days they were present" } }
    }
}

fn page(person: &Person, views: &[lookup::IdentityView], context: &PersonContext) -> Markup {
    let here = person_href(&person.name);
    let (kind, other) = match person.kind {
        Kind::Person => ("person", Kind::Bot),
        Kind::Bot => ("bot", Kind::Person),
    };
    html! {
        p.crumbs { a href="/people" { "People and bots" } }
        h1 { (person.name) " " span.id-label { (kind) } }
        @if let Some(note) = &person.note {
            p.lead { (note) }
        }
        (summary(person, views, context))

        nav.on-page aria-label="On this page" {
            a href="#said" { "Last said" }
            a href="#trust" { "Trust" }
            a href="#notes" { "Notes" }
            a href="#identities" { "Identities" }
            a href=(person_history_href(&person.name)) { "Everything they said" }
            a href="#manage" { "Manage" }
        }

        section id="said" {
            h2 { "Last said" }
            (last_said(person, &context.last_said))
        }

        section.decision id="trust" {
            h2 { "Trust" }
            @if person.kind == Kind::Person {
                p.lead {
                    "Trust score " strong { (trust::trust_score(person.rating, context.activity.score)) }
                    ", out of 99: rated " (person.rating.label()) ", activity score "
                    (context.activity.score) " of 100."
                }
            } @else {
                p.lead {
                    "Activity score " strong { (context.activity.score) } " of 100. "
                    "Bots are not rated and have no trust score."
                }
            }
            (activity_breakdown(&context.activity))
            @if person.kind == Kind::Person {
                (rating_slider(&person.name, person.rating))
                (invited_by_form(person, &context.inviters))
            }
        }

        section.decision id="notes" {
            h2 { "Notes" }
            @if person.notes.is_empty() {
                p.muted { "No notes yet." }
            } @else {
                ul.notes {
                    @for note in &person.notes {
                        li {
                            span.muted { (day(Some(note.created_at.as_str()))) " " }
                            span.note-text { (note.text) }
                            form.inline method="post" action="/people/notes/delete" {
                                input type="hidden" name="name" value=(person.name);
                                input type="hidden" name="id" value=(note.id);
                                button.secondary type="submit" title="Delete this note" { "Delete" }
                            }
                        }
                    }
                }
            }
            form.stacked method="post" action="/people/notes/add" {
                input type="hidden" name="name" value=(person.name);
                label {
                    "New note "
                    textarea name="text" rows="3" required
                        placeholder="How you know them, who introduced them, what they work on" {}
                }
                button type="submit" { "Add note" }
            }
        }

        h2 id="identities" { "Identities" }
        @if views.is_empty() {
            p { "None of this " (kind) "'s identities have sessions." }
        } @else {
            form method="post" action="/people/link" {
                (back_field(&here))
                (identity_table(views, &Ticks::toggled(&here, context.all), &HashSet::new()))
                div.bulk {
                    span.muted { "With the ticked identities:" }
                    button.secondary type="submit" name="action" value="remove" {
                        "Remove from " (person.name)
                    }
                }
            }
        }
        section.decision id="manage" {
            h2 { "Manage" }
            h3 { "Add identities" }
            form.inline method="post" action="/people/add" {
                (back_field(&here))
                input type="hidden" name="name" value=(person.name);
                label { "Identity ids, separated by spaces " input type="text" name="id" required; }
                button type="submit" { "Add" }
            }
            h3 { "Person or bot" }
            form.inline method="post" action="/people/kind" {
                input type="hidden" name="name" value=(person.name);
                input type="hidden" name="kind" value=(other.as_str());
                span.muted { "Added " (day(Some(person.created_at.as_str()))) " as a " (kind) ". " }
                button.secondary type="submit" {
                    @match other {
                        Kind::Bot => "Make it a bot",
                        Kind::Person => "Make it a person",
                    }
                }
            }
            h3 { "Delete" }
            form.inline method="post" action="/people/delete" {
                input type="hidden" name="name" value=(person.name);
                label {
                    input type="checkbox" name="confirm" value="yes" required;
                    " Delete " (person.name) "; their identities are kept and belong to no one"
                }
                button.danger type="submit" { "Delete " (kind) }
            }
        }
    }
}

/// The rating, as radio buttons drawn as stops on a slider. The chosen
/// rating's explanation shows below it, with no script: the stylesheet
/// shows the one whose button is checked.
fn rating_slider(name: &str, current: Rating) -> Markup {
    html! {
        form.rating method="post" action="/people/rate" {
            input type="hidden" name="name" value=(name);
            fieldset.slider {
                legend { "How well you know them" }
                div.stops {
                    @for rating in Rating::ALL {
                        label {
                            input type="radio" name="rating" value=(rating.as_str())
                                checked[rating == current];
                            span { (rating.label()) }
                        }
                    }
                }
                @for rating in Rating::ALL {
                    p class=(format!("explain explain-{}", rating.as_str())) {
                        strong { (rating.label()) ": " } (rating.explanation())
                    }
                }
            }
            button type="submit" { "Save rating" }
        }
    }
}

fn invited_by_form(person: &Person, inviters: &[String]) -> Markup {
    html! {
        form.inline method="post" action="/people/invited-by" {
            input type="hidden" name="name" value=(person.name);
            label {
                "Invited or vouched for by "
                select name="by" {
                    option value="" selected[person.invited_by.is_none()] { "nobody recorded" }
                    @for name in inviters {
                        option value=(name)
                            selected[person.invited_by.as_deref() == Some(name.as_str())] { (name) }
                    }
                }
            }
            button.secondary type="submit" { "Save" }
        }
        @if let Some(by) = &person.invited_by {
            p.note { "Invited by " a href=(person_href(by)) { (by) } "." }
        }
        @if !person.invited.is_empty() {
            p.note {
                "Invited "
                @for (i, name) in person.invited.iter().enumerate() {
                    @if i > 0 { ", " }
                    a href=(person_href(name)) { (name) }
                }
                "."
            }
        }
    }
}

/// The ticked identities of a results table, and what to do with them.
async fn link(
    State(app): State<App>,
    Form(fields): Form<Vec<(String, String)>>,
) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        let field = |name: &str| {
            fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.trim())
                .filter(|value| !value.is_empty())
        };
        let back = back_to(field("back"), "/identities");
        let ids: Vec<String> = fields
            .iter()
            .filter(|(key, _)| key == "id")
            .map(|(_, value)| value.clone())
            .collect();
        if ids.is_empty() {
            return Ok(failed(&back, "Tick at least one identity first."));
        }
        let count = match ids.len() {
            1 => "1 identity".to_string(),
            n => format!("{n} identities"),
        };
        let result: crate::db::Result<String> = match field("action") {
            Some(action @ ("create" | "create_bot")) => {
                let kind = if action == "create_bot" {
                    Kind::Bot
                } else {
                    Kind::Person
                };
                match field("new_person") {
                    Some(name) => decide::create_person(conn, name, kind, &ids, None)
                        .map(|()| format!("Created the {} {name} with {count}.", kind.as_str())),
                    None => Err(format!("Give the new {} a name.", kind.as_str()).into()),
                }
            }
            Some("add") => match field("person") {
                Some(name) => decide::add_to_person(conn, name, &ids)
                    .map(|()| format!("Added {count} to {name}.")),
                None => Err("Choose a person or bot to add them to.".into()),
            },
            Some("remove") => decide::remove_from_person(conn, &ids)
                .map(|()| format!("Took {count} out of their person or bot.")),
            _ => Err("Choose what to do with the ticked identities.".into()),
        };
        Ok(match result {
            Ok(notice) => done(&back, &notice),
            Err(err) => failed(&back, err),
        })
    })
    .await
}

/// The distinct nicks of a person's identities, sorted.
fn nick_summary(person: &Person) -> String {
    let mut nicks: Vec<&str> = person
        .identities
        .iter()
        .flat_map(|identity| identity.nicks.iter().map(String::as_str))
        .collect();
    nicks.sort_unstable();
    nicks.dedup();
    nicks.join(", ")
}

/// When any of a person's identities was last seen.
fn last_seen(person: &Person) -> Option<&str> {
    person
        .identities
        .iter()
        .filter_map(|identity| identity.last_seen.as_deref())
        .max()
}

/// Identity ids from a form field, separated by spaces or commas.
fn ids(field: &str) -> Vec<String> {
    field
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect()
}

#[derive(Deserialize)]
struct CreateForm {
    name: String,
    /// `person` or `bot`; a person when left out.
    kind: Option<String>,
    ids: Option<String>,
    note: Option<String>,
    back: Option<String>,
}

async fn create(
    State(app): State<App>,
    Form(form): Form<CreateForm>,
) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        let name = form.name.trim().to_string();
        let ids = ids(form.ids.as_deref().unwrap_or_default());
        let back = back_to(form.back.as_deref(), "/people");
        let Some(kind) = Kind::parse(filled(&form.kind).unwrap_or("person")) else {
            return Ok(failed(&back, "Choose a person or a bot."));
        };
        let notice = format!("Created the {} {name}.", kind.as_str());
        Ok(
            match decide::create_person(conn, &name, kind, &ids, filled(&form.note)) {
                Ok(()) if back == "/people" => done(&person_href(&name), &notice),
                Ok(()) => done(&back, &notice),
                Err(err) => failed(&back, err),
            },
        )
    })
    .await
}

#[derive(Deserialize)]
struct AddForm {
    name: String,
    id: String,
    back: Option<String>,
}

async fn add(State(app): State<App>, Form(form): Form<AddForm>) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        let ids = ids(&form.id);
        let back = back_to(form.back.as_deref(), &person_href(&form.name));
        Ok(match decide::add_to_person(conn, &form.name, &ids) {
            Ok(()) => done(
                &back,
                &format!("Added {} to {}.", ids.join(", "), form.name),
            ),
            Err(err) => failed(&back, err),
        })
    })
    .await
}

#[derive(Deserialize)]
struct RemoveForm {
    id: String,
    back: Option<String>,
}

async fn remove(
    State(app): State<App>,
    Form(form): Form<RemoveForm>,
) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        let ids = ids(&form.id);
        let back = back_to(form.back.as_deref(), "/people");
        Ok(match decide::remove_from_person(conn, &ids) {
            Ok(()) => done(
                &back,
                &format!("Took {} out of their person or bot.", ids.join(", ")),
            ),
            Err(err) => failed(&back, err),
        })
    })
    .await
}

#[derive(Deserialize)]
struct DeleteForm {
    name: String,
    confirm: Option<String>,
}

async fn delete(
    State(app): State<App>,
    Form(form): Form<DeleteForm>,
) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        if form.confirm.as_deref() != Some("yes") {
            return Ok(failed(
                &person_href(&form.name),
                "Tick the box to confirm the deletion.",
            ));
        }
        Ok(match decide::delete_person(conn, &form.name) {
            Ok(()) => done(
                "/people",
                &format!(
                    "Deleted {}. Their identities belong to no one now.",
                    form.name
                ),
            ),
            Err(err) => failed(&person_href(&form.name), err),
        })
    })
    .await
}

#[derive(Deserialize)]
struct KindForm {
    name: String,
    kind: String,
}

async fn set_kind(
    State(app): State<App>,
    Form(form): Form<KindForm>,
) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        let back = person_href(&form.name);
        let Some(kind) = Kind::parse(&form.kind) else {
            return Ok(failed(&back, "Choose a person or a bot."));
        };
        Ok(match decide::set_kind(conn, &form.name, kind) {
            Ok(()) => done(&back, &format!("{} is a {} now.", form.name, kind.as_str())),
            Err(err) => failed(&back, err),
        })
    })
    .await
}

#[derive(Deserialize)]
struct RateForm {
    name: String,
    rating: String,
}

async fn rate(State(app): State<App>, Form(form): Form<RateForm>) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        let back = person_href(&form.name);
        let Some(rating) = Rating::parse(&form.rating) else {
            return Ok(failed(&back, "Choose a rating."));
        };
        Ok(match decide::set_rating(conn, &form.name, rating) {
            Ok(()) => done(&back, &format!("Rated {} {}.", form.name, rating.label())),
            Err(err) => failed(&back, err),
        })
    })
    .await
}

#[derive(Deserialize)]
struct InvitedByForm {
    name: String,
    /// Empty to clear it.
    by: Option<String>,
}

async fn invited_by(
    State(app): State<App>,
    Form(form): Form<InvitedByForm>,
) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        let back = person_href(&form.name);
        let by = filled(&form.by);
        let notice = match by {
            Some(by) => format!("{} was invited by {by}.", form.name),
            None => format!("Cleared who invited {}.", form.name),
        };
        Ok(match decide::set_invited_by(conn, &form.name, by) {
            Ok(()) => done(&back, &notice),
            Err(err) => failed(&back, err),
        })
    })
    .await
}

#[derive(Deserialize)]
struct NoteForm {
    name: String,
    text: String,
}

async fn add_note(
    State(app): State<App>,
    Form(form): Form<NoteForm>,
) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        let back = person_href(&form.name);
        Ok(match decide::add_note(conn, &form.name, &form.text) {
            Ok(()) => done(&back, "Added the note."),
            Err(err) => failed(&back, err),
        })
    })
    .await
}

#[derive(Deserialize)]
struct DeleteNoteForm {
    name: String,
    id: i64,
}

async fn delete_note(
    State(app): State<App>,
    Form(form): Form<DeleteNoteForm>,
) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        let back = person_href(&form.name);
        Ok(match decide::delete_note(conn, &form.name, form.id) {
            Ok(()) => done(&back, "Deleted the note."),
            Err(err) => failed(&back, err),
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookup::{IdentityView, SessionRow};
    use crate::review::identities::tests::session;

    #[test]
    fn splits_identity_ids() {
        assert_eq!(ids(" a1, b2  c3 "), ["a1", "b2", "c3"]);
        assert!(ids("").is_empty());
    }

    fn record(name: &str, kind: Kind) -> Person {
        Person {
            name: name.into(),
            kind,
            rating: Rating::Neutral,
            invited_by: None,
            invited: Vec::new(),
            note: None,
            notes: Vec::new(),
            created_at: "2026-11-02 12:00:00".into(),
            identities: Vec::new(),
        }
    }

    fn context() -> PersonContext {
        PersonContext {
            all: false,
            activity: ActivityScore::default(),
            inviters: vec!["rachel".into()],
            days: BTreeMap::new(),
            last_said: Vec::new(),
            real_names: Vec::new(),
            today: NaiveDate::from_ymd_opt(2026, 11, 3),
        }
    }

    #[test]
    fn sums_up_a_person_across_identities() {
        let at = |start: &str, end: &str, mut session: SessionRow| {
            session.start_at = start.into();
            session.end_at = end.into();
            session
        };
        let views = [
            IdentityView {
                id: "1a2b3c4d5e6f".into(),
                tag: "wetfish".into(),
                people: vec!["quokka".into()],
                sessions: vec![at(
                    "2024-12-11 10:00",
                    "2024-12-11 12:00",
                    session(&["quokka_old"], "host", Some("old.example.net")),
                )],
            },
            IdentityView {
                id: "6f5e4d3c2b1a".into(),
                tag: "wetfish-bouncer".into(),
                people: vec!["quokka".into()],
                sessions: vec![
                    at(
                        "2026-10-01 10:00",
                        "2026-10-01 12:00",
                        session(&["quokka"], "host", Some("new.example.net")),
                    ),
                    // The same hour logged in a second channel counts once.
                    at(
                        "2026-10-01 11:00",
                        "2026-10-01 12:00",
                        session(&["quokka"], "host", Some("new.example.net")),
                    ),
                ],
            },
        ];
        let day = |date: &str, spoken: i64| {
            (
                NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap(),
                Day {
                    channels: BTreeSet::from(["#dryfish".to_string()]),
                    spoken,
                },
            )
        };
        let context = PersonContext {
            days: BTreeMap::from([day("2024-12-11", 0), day("2026-10-01", 2)]),
            last_said: vec![Line {
                date: "2026-10-01".into(),
                time: "11:30".into(),
                channel: "#dryfish".into(),
                nick: "quokka".into(),
                action: false,
                text: "see you <soon>".into(),
            }],
            real_names: vec![("Quokka Wet".into(), "2026-10-02 07:00:00".into())],
            ..context()
        };
        let person = Person {
            rating: Rating::Vouched,
            invited_by: Some("rachel".into()),
            ..record("quokka", Kind::Person)
        };
        let page = page(&person, &views, &context).into_string();
        let at = |text: &str| {
            page.find(text)
                .unwrap_or_else(|| panic!("{text} in {page}"))
        };

        // Nicks and hosts of every identity, the most recent first.
        assert!(at(r#"href="/identities?nick=quokka">"#) < at(r#"nick=quokka_old""#));
        assert!(at("new.example.net") < at("old.example.net"));
        assert!(page.contains("<strong>Quokka Wet</strong> (2026-10-02)"));
        assert!(page.contains("on wetfish, wetfish-bouncer"));
        assert!(page.contains(r#", invited by <a href="/person?name=rachel">rachel</a>"#));

        // When they were last there and last spoke.
        assert!(
            page.contains("<strong>2026-10-01</strong> (33 days ago) in #dryfish"),
            "{page}"
        );
        assert!(page.contains("On 2 days from 2024-12-11 to 2026-10-01, 4h in channels"));
        assert!(page.contains("spoke on 1 of them, 2 lines"));
        assert!(page.contains(r#"href="/history?person=quokka&amp;channel=%23dryfish""#));

        // The last lines come before the decisions, each day a link.
        assert!(page.contains("see you &lt;soon&gt;"));
        assert!(page.contains(r#"href="/history?person=quokka&amp;day=2026-10-01""#));
        assert!(at(r#"<section id="said">"#) < at(r#"id="trust""#));
        assert!(at(r#"id="trust""#) < at(r#"id="manage""#));
        assert!(at(r#"id="manage""#) < at(r#"name="kind" value="bot""#));
        assert!(!page.contains("Silent"));

        // The command for lurker, by their latest nick, in the summary.
        assert!(page.contains(r#"value="/msg lurker trusted quokka grace=5m silent=2026-10-01""#));
        assert!(at(r#"id="lurker-line""#) < at(r#"<section id="said">"#));
        // Bots are not lurkers.
        let bot = self::page(&record("relay", Kind::Bot), &views, &context).into_string();
        assert!(!bot.contains("lurker-line"));
    }

    #[test]
    fn flags_a_silent_watcher_and_says_how_long_ago() {
        let days: BTreeMap<NaiveDate, Day> = (0..40)
            .map(|n| {
                let date = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap() + chrono::Days::new(n);
                (date, Day::default())
            })
            .collect();
        let silent = PersonContext { days, ..context() };
        let page = page(&record("marlowe", Kind::Person), &[], &silent).into_string();
        assert!(page.contains("Silent 40 days"));
        assert!(page.contains("Nothing said in channels."));
        // With no nick seen, there is no line for lurker.
        assert!(!page.contains("lurker-line"));

        // Someone who used to talk is inactive, not a silent watcher, and
        // the cut is explained.
        let days: BTreeMap<NaiveDate, Day> = [("2024-05-01", 3), ("2026-10-01", 0)]
            .into_iter()
            .map(|(date, spoken)| {
                let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap();
                (
                    date,
                    Day {
                        spoken,
                        ..Day::default()
                    },
                )
            })
            .collect();
        let inactive = PersonContext {
            days,
            activity: ActivityScore {
                score: 12,
                full_score: 60,
                cut: true,
                active_months: 14,
                tenure_months: 30,
                last_spoke: Some(2024 * 12 + 4),
                lapse: Some(trust::Lapse::Inactive),
                ..ActivityScore::default()
            },
            ..context()
        };
        let page = self::page(&record("lindqvist", Kind::Person), &[], &inactive).into_string();
        assert!(page.contains("Inactive since 2024-05"));
        assert!(!page.contains("Silent"));
        assert!(page.contains("cut to a fifth of 60"));
        assert!(page.contains("they count as inactive"));

        let today = NaiveDate::from_ymd_opt(2026, 11, 3);
        assert_eq!(ago(today, "2026-11-03 01:00").as_deref(), Some("today"));
        assert_eq!(ago(today, "2026-11-02").as_deref(), Some("yesterday"));
        assert_eq!(ago(today, "2024-11-03").as_deref(), Some("730 days ago"));
        assert_eq!(ago(None, "2026-11-02"), None);
        assert_eq!(ago(today, ""), None);
    }

    #[test]
    fn asks_to_confirm_a_deletion() {
        let person = Person {
            note: Some("the logs' owner".into()),
            ..record("rachel", Kind::Person)
        };
        let page = page(&person, &[], &context()).into_string();
        assert!(page.contains(r#"action="/people/delete""#));
        // With no identities there is nothing to tick.
        assert!(!page.contains(r#"action="/people/link""#));
        assert!(page.contains(r#"name="confirm" value="yes" required"#));
        assert!(page.contains("the logs' owner"));
        // A person can be made a bot.
        assert!(page.contains(r#"name="kind" value="bot""#));
        assert!(page.contains("Delete person"));
        assert!(page.contains(r#"href="/history?person=rachel""#));
    }

    #[test]
    fn rates_people_and_keeps_notes() {
        let person = Person {
            rating: Rating::Vouched,
            invited_by: Some("rachel".into()),
            notes: vec![stats::Note {
                id: 7,
                text: "met at a <meetup>".into(),
                created_at: "2026-11-03 07:00:00".into(),
            }],
            ..record("quokka", Kind::Person)
        };
        let page = page(&person, &[], &context()).into_string();
        // The slider has every rating, the current one checked, and each
        // explanation for the stylesheet to show.
        assert!(page.contains(r#"value="vouched" checked"#));
        assert_eq!(page.matches(r#"type="radio" name="rating""#).count(), 5);
        assert!(page.contains("explain-distrusted"));
        assert!(page.contains("Trust score <strong>60</strong>"));
        assert!(page.contains(r#"<option value="rachel" selected>rachel</option>"#));
        assert!(page.contains("met at a &lt;meetup&gt;"));
        assert!(page.contains(r#"name="id" value="7""#));

        // Bots have no rating.
        let bot = self::page(&record("relay", Kind::Bot), &[], &context()).into_string();
        assert!(!bot.contains(r#"name="rating""#));
        assert!(bot.contains("Bots are not rated"));
    }

    #[test]
    fn lists_people_and_bots_apart() {
        let quokka = Person {
            rating: Rating::Known,
            ..record("quokka", Kind::Person)
        };
        let records = [
            record("rachel", Kind::Person),
            quokka,
            record("relay", Kind::Bot),
        ];
        let page = list(&records, &HashMap::new()).into_string();
        let bots = page.find("<h2>Bots</h2>").unwrap();
        let rachel = page.find(r#"href="/person?name=rachel""#).unwrap();
        // Most trusted first.
        assert!(page.find(r#"href="/person?name=quokka""#).unwrap() < rachel);
        assert!(rachel < bots);
        assert!(page.find(r#"href="/person?name=relay""#).unwrap() > bots);

        let options =
            record_options(&[("rachel".into(), Kind::Person), ("relay".into(), Kind::Bot)])
                .into_string();
        assert_eq!(
            options,
            r#"<option value="rachel">rachel</option><optgroup label="Bots"><option value="relay">relay</option></optgroup>"#
        );
        let people_only = record_options(&[("rachel".into(), Kind::Person)]).into_string();
        assert!(!people_only.contains("optgroup"));
    }
}
