//! People and bots: who is behind identities, as decided by hand. Both are
//! person records, told apart by their kind.

use std::collections::{HashMap, HashSet};

use axum::Router;
use axum::extract::{Form, Query, State};
use axum::response::Redirect;
use axum::routing::{get, post};
use maud::{Markup, html};
use serde::Deserialize;

use super::activity::activity_breakdown;
use super::identities::{Ticks, identity_table};
use super::layout::{self, back_field, day, number, person_href};
use super::{App, Flash, PageError, PageResult, back_to, done, failed, filled, internal, with_db};
use crate::decide;
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
        let mut scores = HashMap::new();
        for person in &people {
            let score = trust::activity_score(conn, &person.identity_ids()).map_err(internal)?;
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
                                td { (day(last_seen(person))) }
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
        let context = PersonContext {
            all: filled(&query.all) == Some("yes"),
            activity: trust::activity_score(conn, &person.identity_ids()).map_err(internal)?,
            inviters: stats::people_names(conn)
                .map_err(internal)?
                .into_iter()
                .filter(|(name, kind)| *kind == Kind::Person && *name != person.name)
                .map(|(name, _)| name)
                .collect(),
        };
        Ok(layout::page(
            &person.name,
            &flash,
            page(&person, &views, &context),
        ))
    })
    .await
}

/// What the person page shows besides the person.
struct PersonContext {
    /// Whether every identity starts ticked.
    all: bool,
    activity: ActivityScore,
    /// Everyone who could have invited the person.
    inviters: Vec<String>,
}

fn page(person: &Person, views: &[lookup::IdentityView], context: &PersonContext) -> Markup {
    let here = person_href(&person.name);
    let spoken: i64 = views.iter().map(|view| view.spoken()).sum();
    let minutes: i64 = views.iter().map(|view| view.channel_minutes()).sum();
    let (kind, other) = match person.kind {
        Kind::Person => ("person", Kind::Bot),
        Kind::Bot => ("bot", Kind::Person),
    };
    html! {
        p.crumbs { a href="/people" { "People and bots" } }
        h1 { (person.name) }
        @if let Some(note) = &person.note {
            p.lead { (note) }
        }
        p.note {
            @if person.kind == Kind::Bot {
                "A bot: activity rankings leave its identities out unless asked. "
            }
            (number(views.len() as i64)) " identities with sessions, "
            (layout::duration(minutes)) " in channels, " (number(spoken)) " spoken lines. "
            "Added " (day(Some(person.created_at.as_str()))) "."
        }
        form.inline method="post" action="/people/kind" {
            input type="hidden" name="name" value=(person.name);
            input type="hidden" name="kind" value=(other.as_str());
            button.secondary type="submit" {
                @match other {
                    Kind::Bot => "Make it a bot",
                    Kind::Person => "Make it a person",
                }
            }
        }

        section.decision {
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

        section.decision {
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

        h2 { "Identities" }
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
        h2 { "Add identities" }
        form.inline method="post" action="/people/add" {
            (back_field(&here))
            input type="hidden" name="name" value=(person.name);
            label { "Identity ids, separated by spaces " input type="text" name="id" required; }
            button type="submit" { "Add" }
        }
        h2 { "Delete" }
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
        }
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
