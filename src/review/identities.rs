//! Searching identities, and the page of one identity with the decisions
//! that can be made from it.

use std::collections::{BTreeMap, HashMap, HashSet};

use axum::Router;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use maud::{Markup, html};
use serde::Deserialize;

use super::activity::identity_section;
use super::layout::{
    self, back_field, channel_href, day, duration, identity_href, number, person_href, search_href,
};
use super::people::record_options;
use super::{App, Flash, PageError, PageResult, filled, internal, with_db};
use crate::activity::{self, Totals};
use crate::lookup::{self, Filter, IdentityView, SessionRow};
use crate::stats::{self, Kind, MatchingHost, NickChange};

/// Search results show at most this many identities.
const MAX_RESULTS: usize = 500;
/// Sessions with nick changes shown on an identity's page.
const MAX_CHANGE_SESSIONS: usize = 20;
/// Inferred sessions shown on an identity's page.
const MAX_INFERRED: usize = 20;

pub(super) fn routes() -> Router<App> {
    Router::new()
        .route("/identities", get(search_page))
        .route("/identity", get(identity_page))
}

#[derive(Default, Deserialize)]
struct Search {
    nick: Option<String>,
    ident: Option<String>,
    host: Option<String>,
    person: Option<String>,
    /// `yes` to tick every result.
    all: Option<String>,
}

impl Search {
    fn ticked(&self) -> bool {
        filled(&self.all) == Some("yes")
    }

    fn filter(&self) -> Filter {
        Filter {
            nick: filled(&self.nick).map(str::to_string),
            ident: filled(&self.ident).map(str::to_string),
            host: filled(&self.host).map(str::to_string),
            person: filled(&self.person).map(str::to_string),
        }
    }
}

async fn search_page(
    State(app): State<App>,
    Query(search): Query<Search>,
    Query(flash): Query<Flash>,
) -> PageResult {
    with_db(&app, move |conn| {
        let filter = search.filter();
        let views = if filter.is_empty() {
            None
        } else {
            Some(lookup::find(conn, &filter).map_err(internal)?)
        };
        let people = stats::people_names(conn).map_err(internal)?;
        let bots = activity::bots(conn).map_err(internal)?;
        let hosts = match &filter.host {
            Some(pattern) => Some(HostPanel {
                hosts: stats::matching_hosts(conn, pattern, MAX_HOSTS).map_err(internal)?,
                groups: stats::host_groups(conn)
                    .map_err(internal)?
                    .into_iter()
                    .map(|group| (group.tag, group.name))
                    .collect(),
            }),
            None => None,
        };
        Ok(layout::page(
            "Identities",
            &flash,
            self::search(
                &search,
                &filter,
                views.as_deref(),
                hosts.as_ref(),
                &people,
                &bots,
            ),
        ))
    })
    .await
}

/// Hosts listed with a search by host, so they can be grouped from there.
const MAX_HOSTS: i64 = 50;

/// The hosts matching a search by host, and the host groups to add them to.
struct HostPanel {
    hosts: Vec<MatchingHost>,
    /// Every host group, as tag and name.
    groups: Vec<(String, String)>,
}

impl Search {
    /// The address of this search, for actions to return to.
    fn href(&self) -> String {
        let fields: Vec<(&str, &str)> = [
            ("nick", &self.nick),
            ("ident", &self.ident),
            ("host", &self.host),
            ("person", &self.person),
        ]
        .into_iter()
        .filter_map(|(name, value)| Some((name, filled(value)?)))
        .collect();
        search_href(&fields)
    }
}

/// The hosts of a search by host, each with its group or a way to add it to
/// one, and a form to create a group for the ungrouped ones.
fn host_panel(panel: &HostPanel, here: &str) -> Markup {
    let ungrouped: Vec<&MatchingHost> = panel.hosts.iter().filter(|h| h.group.is_none()).collect();
    // A new group belongs to one tag: that of the first ungrouped host.
    let new_tag = ungrouped.first().map(|host| host.tag.as_str());
    let new_hosts: Vec<&str> = ungrouped
        .iter()
        .filter(|host| Some(host.tag.as_str()) == new_tag)
        .map(|host| host.host.as_str())
        .collect();
    html! {
        section.panel {
            h2 { "Host groups for these hosts" }
            p.note {
                "Group the hosts of a bridge or bouncer here. Hosts in a group count as one host, "
                "and when the ident identifies the person, sessions with the same ident are linked "
                "across nicks. Every change rebuilds identities and comes back to this search."
            }
            @if panel.hosts.is_empty() {
                p.muted { "No host seen in the logs matches." }
            } @else {
                div.scroll {
                    table {
                        thead {
                            tr {
                                th { "Host" }
                                th.num { "Identities" }
                                th { "Host group" }
                                th { "" }
                            }
                        }
                        tbody {
                            @for host in &panel.hosts {
                                @let groups = panel
                                    .groups
                                    .iter()
                                    .filter(|(tag, _)| *tag == host.tag)
                                    .map(|(_, name)| name.as_str())
                                    .collect::<Vec<&str>>();
                                tr {
                                    td { a href=(search_href(&[("host", host.host.as_str())])) { (host.host) } }
                                    td.num { (number(host.identities)) }
                                    td {
                                        @match &host.group {
                                            Some(group) => a href="/hosts" { (group) },
                                            None => span.muted { "-" },
                                        }
                                    }
                                    td {
                                        @if host.group.is_some() {
                                            form.inline method="post" action="/hosts/remove" {
                                                (back_field(here))
                                                input type="hidden" name="tag" value=(host.tag);
                                                input type="hidden" name="host" value=(host.host);
                                                button.secondary type="submit" { "Remove from group" }
                                            }
                                        } @else if !groups.is_empty() {
                                            form.inline method="post" action="/hosts/add" {
                                                (back_field(here))
                                                input type="hidden" name="tag" value=(host.tag);
                                                input type="hidden" name="hosts" value=(host.host);
                                                select name="group" aria-label="Host group" {
                                                    @for group in &groups {
                                                        option value=(group) { (group) }
                                                    }
                                                }
                                                button type="submit" { "Add" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                @if panel.hosts.len() as i64 >= MAX_HOSTS {
                    p.note { "Showing the " (MAX_HOSTS) " hosts used by the most identities." }
                }
            }
            @if let Some(tag) = new_tag {
                details {
                    summary { "Create a host group for the ungrouped hosts" }
                    form.stacked method="post" action="/hosts/add-group" {
                        (back_field(here))
                        input type="hidden" name="tag" value=(tag);
                        label { "Name " input type="text" name="name" required placeholder="matrix"; }
                        label {
                            "Kind "
                            select name="kind" {
                                option value="bridge" { "bridge" }
                                option value="bouncer" { "bouncer" }
                            }
                        }
                        label {
                            input type="checkbox" name="ident_is_user" value="yes" checked;
                            " The ident identifies the person on these hosts"
                        }
                        label {
                            "Hosts to add, separated by spaces "
                            input type="text" name="hosts" value=(new_hosts.join(" "));
                        }
                        label { "Note (optional) " input type="text" name="note"; }
                        button type="submit" { "Create group" }
                    }
                }
            }
        }
    }
}

fn search(
    search: &Search,
    filter: &Filter,
    views: Option<&[IdentityView]>,
    hosts: Option<&HostPanel>,
    people: &[(String, Kind)],
    bots: &HashSet<String>,
) -> Markup {
    let here = search.href();
    let value = |field: &Option<String>| field.clone().unwrap_or_default();
    html! {
        h1 { "Identities" }
        form.search method="get" action="/identities" {
            label { "Nick " input type="text" name="nick" value=(value(&search.nick)); }
            label { "Ident " input type="text" name="ident" value=(value(&search.ident)); }
            label { "Host " input type="text" name="host" value=(value(&search.host)); }
            label { "Person " input type="text" name="person" value=(value(&search.person)); }
            button type="submit" { "Search" }
        }
        p.note {
            "Matching ignores case, and " code { "*" } " matches any characters, as in "
            code { "Fish-*.chat" } ". Fields that are filled must all match the same nick and "
            "ident@host, and the identity must belong to the person if one is given."
        }
        @if let Some(panel) = hosts {
            (host_panel(panel, &here))
        }
        @match views {
            None => {}
            Some([]) => p { "No identities match " (filter.describe()) "." },
            Some(views) => {
                p.lead {
                    (number(views.len() as i64)) " identities match " (filter.describe())
                    ", most recently seen first."
                    @if views.len() > MAX_RESULTS {
                        " Showing the first " (MAX_RESULTS) "; narrow the search to see the rest."
                    }
                }
                form method="post" action="/people/link" {
                    (back_field(&here))
                    (identity_table(
                        &views[..views.len().min(MAX_RESULTS)],
                        &Ticks::toggled(&here, search.ticked()),
                        bots,
                    ))
                    div.bulk {
                        span.muted { "With the ticked identities:" }
                        label {
                            "Name "
                            input type="text" name="new_person" aria-label="Name of the new person or bot";
                        }
                        button type="submit" name="action" value="create" { "Create person" }
                        button type="submit" name="action" value="create_bot" { "Create bot" }
                        @if !people.is_empty() {
                            label {
                                "or add to "
                                select name="person" { (record_options(people)) }
                            }
                            button type="submit" name="action" value="add" { "Add" }
                        }
                        button.secondary type="submit" name="action" value="remove" {
                            "Remove from their person or bot"
                        }
                    }
                }
            }
        }
    }
}

/// Whether the rows of an identity table start ticked, and the link that
/// ticks all of them or none. Pages have no scripts, so the link loads the
/// page again.
pub(super) struct Ticks {
    pub all: bool,
    /// The same page with the opposite of `all`.
    pub toggle: String,
}

impl Ticks {
    /// For a page at `here`, an address with a query and without `all`, that
    /// ticks every row when its address has `all=yes`.
    pub(super) fn toggled(here: &str, all: bool) -> Ticks {
        Ticks {
            all,
            toggle: if all {
                here.to_string()
            } else {
                format!("{here}&all=yes")
            },
        }
    }
}

/// A table of identities with their totals, labelling those in `bots`. Each
/// row has a checkbox named `id`, for a form around the table.
pub(super) fn identity_table(
    views: &[IdentityView],
    ticks: &Ticks,
    bots: &HashSet<String>,
) -> Markup {
    html! {
        div.scroll {
            table {
                thead {
                    tr {
                        th {
                            @if ticks.all {
                                a href=(ticks.toggle) title="Untick every identity" { "None" }
                            } @else {
                                a href=(ticks.toggle) title="Tick every identity" { "All" }
                            }
                        }
                        th { "Identity" }
                        th { "Person" }
                        th { "Nicks" }
                        th.num title="Distinct ident@host combinations" { "Hosts" }
                        th.num { "Sessions" }
                        th.num title="Time in channels, added up over channels" { "Channel time" }
                        th.num { "Spoken" }
                        th { "First seen" }
                        th { "Last seen" }
                    }
                }
                tbody {
                    @for view in views {
                        tr {
                            td {
                                input type="checkbox" name="id" value=(view.id) checked[ticks.all]
                                    aria-label=(format!("Select {}", view.id));
                            }
                            td {
                                a href=(identity_href(&view.id)) { code { (view.id) } }
                                @if bots.contains(&view.id) { " " span.warning { "bot" } }
                            }
                            td { (people_links(&view.people)) }
                            td.wrap { (nick_list(view, 4)) }
                            td.num { (number(view.connections() as i64)) }
                            td.num { (number(view.sessions.len() as i64)) }
                            td.num { (duration(view.channel_minutes())) }
                            td.num { (number(view.spoken())) }
                            td { (day(Some(view.first_seen()))) }
                            td { (day(Some(view.last_seen()))) }
                        }
                    }
                }
            }
        }
    }
}

fn nick_list(view: &IdentityView, limit: usize) -> String {
    let nicks = view.nicks();
    let mut list = nicks[..nicks.len().min(limit)].join(", ");
    if nicks.len() > limit {
        list.push_str(&format!(" +{}", nicks.len() - limit));
    }
    list
}

fn people_links(people: &[String]) -> Markup {
    html! {
        @match people {
            [] => span.muted { "-" },
            [person] => a href=(person_href(person)) { (person) },
            people => {
                span.warning { "conflict: " }
                @for (i, person) in people.iter().enumerate() {
                    @if i > 0 { ", " }
                    a href=(person_href(person)) { (person) }
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct IdentityQuery {
    id: String,
}

/// Everything on an identity's page besides the identity itself.
struct Context {
    people: Vec<(String, Kind)>,
    groups: HashMap<(String, String), String>,
    changes: Vec<(usize, Vec<NickChange>)>,
    /// Channel activity by month.
    months: Vec<(String, Totals)>,
    /// Messages sent in private messages to the logging client.
    private_spoken: i64,
    bot: bool,
}

async fn identity_page(
    State(app): State<App>,
    Query(query): Query<IdentityQuery>,
    Query(flash): Query<Flash>,
) -> Result<Response, PageError> {
    with_db(&app, move |conn| {
        let view = lookup::by_id(conn, &query.id)
            .map_err(internal)?
            .ok_or_else(|| {
                PageError::not_found(format!(
                    "There is no identity {} with sessions. Identities change when logs are \
                     imported or decisions are made; search for it by nick instead.",
                    query.id
                ))
            })?;
        if view.id != query.id {
            // The id was merged into another identity since.
            return Ok(Redirect::to(&identity_href(&view.id)).into_response());
        }

        // The nick change lines of the most recent sessions that changed nick.
        let mut changes = Vec::new();
        for (index, session) in view.sessions.iter().enumerate().rev() {
            if changes.len() >= MAX_CHANGE_SESSIONS {
                break;
            }
            if session.nicks.len() > 1 {
                let lines = stats::nick_changes(
                    conn,
                    session.file_id,
                    session.start_line,
                    session.end_line,
                    &session.nicks,
                )
                .map_err(internal)?;
                changes.push((index, lines));
            }
        }
        let context = Context {
            people: stats::people_names(conn).map_err(internal)?,
            groups: stats::group_of_hosts(conn).map_err(internal)?,
            changes,
            months: activity::months(conn, &view.id, None).map_err(internal)?,
            private_spoken: activity::private_spoken(conn)
                .map_err(internal)?
                .get(&view.id)
                .copied()
                .unwrap_or(0),
            bot: activity::bots(conn).map_err(internal)?.contains(&view.id),
        };
        let title = format!("Identity {}", view.id);
        Ok(layout::page(&title, &flash, identity(&view, &context)).into_response())
    })
    .await
}

fn identity(view: &IdentityView, context: &Context) -> Markup {
    let here = identity_href(&view.id);
    let sessions = &view.sessions;
    let count = |link: &str| sessions.iter().filter(|s| s.link == link).count() as i64;
    let logger = sessions.iter().filter(|s| s.logger).count() as i64;
    html! {
        p.crumbs { a href="/identities" { "Identities" } " / " (view.tag) }
        h1 { "Identity " code { (view.id) } }
        p.lead {
            "Seen " (day(Some(view.first_seen()))) " to " (day(Some(view.last_seen())))
            ", " (duration(view.channel_minutes())) " in channels, "
            (number(view.spoken())) " spoken lines."
        }
        p.note {
            (number(sessions.len() as i64)) " sessions: "
            (number(count("host"))) " with a host, "
            (number(count("inferred"))) " inferred, "
            (number(count("nick_only"))) " known by nick only"
            @if count("cut") > 0 { ", " (number(count("cut"))) " started by a cut" }
            @if logger > 0 { ", " (number(logger)) " as the logging client" }
            "."
        }

        section.decision {
            h2 { "Person or bot" }
            @match view.people.as_slice() {
                [] => {
                    p { "This identity does not belong to a person or bot yet." }
                    @if !context.people.is_empty() {
                        form.inline method="post" action="/people/add" {
                            (back_field(&here))
                            input type="hidden" name="id" value=(view.id);
                            label {
                                "Add to "
                                select name="name" { (record_options(&context.people)) }
                            }
                            button type="submit" { "Add" }
                        }
                    }
                    form.inline method="post" action="/people/create" {
                        (back_field(&here))
                        input type="hidden" name="ids" value=(view.id);
                        label { "Name " input type="text" name="name" required; }
                        button type="submit" name="kind" value="person" { "Create person" }
                        button type="submit" name="kind" value="bot" { "Create bot" }
                    }
                }
                people => {
                    p {
                        @if people.len() > 1 {
                            span.warning {
                                "Conflict: identities added to different people were merged "
                                "since. Remove it from the wrong one. "
                            }
                        }
                        "Belongs to " @if context.bot { "the bot " } (people_links(people)) "."
                    }
                    form.inline method="post" action="/people/remove" {
                        (back_field(&here))
                        input type="hidden" name="id" value=(view.id);
                        button type="submit" {
                            "Remove from " @if context.bot { "bot" } @else { "person" }
                        }
                    }
                }
            }
        }

        (identity_section(&context.months, context.private_spoken, context.bot))

        h2 { "Nicks" }
        (nicks_table(view))

        h2 { "Connections" }
        (connections_table(view, &context.groups))

        h2 { "Channels and private messages" }
        (channels_table(view))

        @if !context.changes.is_empty() {
            h2 { "Nick changes" }
            p.note {
                "Nick changes link a session's nicks into one identity. If someone else was at "
                "the keyboard after a nick change, cut there: the session ends at that line and "
                "the new nick starts a session linked to nothing else."
            }
            (changes_table(view, &context.changes, &here))
        }

        @let inferred = sessions
            .iter()
            .rev()
            .filter(|s| s.link == "inferred")
            .take(MAX_INFERRED)
            .collect::<Vec<&SessionRow>>();
        @if !inferred.is_empty() {
            h2 { "Inferred sessions" }
            p.note {
                "These sessions never showed a host, so they were attributed by nick to the "
                "nearest session that did. The " (MAX_INFERRED) " most recent are shown."
            }
            div.scroll {
                table {
                    thead {
                        tr {
                            th { "Day" }
                            th { "Line" }
                            th { "Nicks" }
                            th { "Inferred from" }
                        }
                    }
                    tbody {
                        @for session in inferred {
                            tr {
                                td { (day(Some(session.start_at.as_str()))) }
                                td { code { (session.file) ":" (session.start_line) } }
                                td { (session.nicks.join(" → ")) }
                                td { code { (session.inferred_from.clone().unwrap_or_default()) } }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn nicks_table(view: &IdentityView) -> Markup {
    // Lowercased nick: the nick as written, sessions, first and last seen.
    let mut nicks: BTreeMap<String, (String, i64, &str, &str)> = BTreeMap::new();
    for session in &view.sessions {
        for nick in &session.nicks {
            let entry = nicks.entry(nick.to_lowercase()).or_insert((
                nick.clone(),
                0,
                session.start_at.as_str(),
                session.end_at.as_str(),
            ));
            entry.1 += 1;
            entry.2 = entry.2.min(session.start_at.as_str());
            entry.3 = entry.3.max(session.end_at.as_str());
        }
    }
    let mut nicks: Vec<_> = nicks.into_values().collect();
    nicks.sort_by(|a, b| b.3.cmp(a.3));
    html! {
        div.scroll {
            table {
                thead {
                    tr {
                        th { "Nick" }
                        th.num { "Sessions" }
                        th { "First seen" }
                        th { "Last seen" }
                    }
                }
                tbody {
                    @for (nick, sessions, first, last) in &nicks {
                        tr {
                            td { a href=(search_href(&[("nick", nick.as_str())])) { (nick) } }
                            td.num { (number(*sessions)) }
                            td { (day(Some(*first))) }
                            td { (day(Some(*last))) }
                        }
                    }
                }
            }
        }
    }
}

fn connections_table(view: &IdentityView, groups: &HashMap<(String, String), String>) -> Markup {
    // By ident, host and link: sessions, spoken, first and last seen.
    type Totals<'a> = (i64, i64, &'a str, &'a str);
    let mut rows: BTreeMap<(Option<&str>, Option<&str>, &str), Totals> = BTreeMap::new();
    for session in &view.sessions {
        let entry = rows
            .entry((
                session.ident.as_deref(),
                session.host.as_deref(),
                session.link.as_str(),
            ))
            .or_insert((0, 0, session.start_at.as_str(), session.end_at.as_str()));
        entry.0 += 1;
        entry.1 += session.spoken.max(0);
        entry.2 = entry.2.min(session.start_at.as_str());
        entry.3 = entry.3.max(session.end_at.as_str());
    }
    let mut rows: Vec<_> = rows.into_iter().collect();
    rows.sort_by(|a, b| b.1.3.cmp(a.1.3));
    html! {
        div.scroll {
            table {
                thead {
                    tr {
                        th { "Ident" }
                        th { "Host" }
                        th { "Host group" }
                        th { "Linked by" }
                        th.num { "Sessions" }
                        th.num { "Spoken" }
                        th { "First seen" }
                        th { "Last seen" }
                    }
                }
                tbody {
                    @for ((ident, host, link), (count, spoken, first, last)) in &rows {
                        tr {
                            @match (ident, host) {
                                (Some(ident), Some(host)) => {
                                    td { a href=(search_href(&[("ident", *ident)])) { (ident) } }
                                    td { a href=(search_href(&[("host", *host)])) { (host) } }
                                    td {
                                        @match groups.get(&(view.tag.clone(), host.to_ascii_lowercase())) {
                                            Some(group) => a href="/hosts" { (group) },
                                            None => a.muted href=(search_href(&[("host", *host)])) { "group…" },
                                        }
                                    }
                                }
                                _ => {
                                    td.muted colspan="3" { "no host seen" }
                                }
                            }
                            td { (link_label(link)) }
                            td.num { (number(*count)) }
                            td.num { (number(*spoken)) }
                            td { (day(Some(*first))) }
                            td { (day(Some(*last))) }
                        }
                    }
                }
            }
        }
    }
}

fn link_label(link: &str) -> &str {
    match link {
        "host" => "nick and host",
        "inferred" => "inferred",
        "nick_only" => "nick only",
        "cut" => "cut",
        other => other,
    }
}

fn channels_table(view: &IdentityView) -> Markup {
    // By target: channel, sessions, minutes, spoken, last seen.
    let mut targets: BTreeMap<&str, (bool, i64, i64, i64, &str)> = BTreeMap::new();
    for session in &view.sessions {
        let entry = targets.entry(session.target.as_str()).or_insert((
            session.channel,
            0,
            0,
            0,
            session.end_at.as_str(),
        ));
        entry.1 += 1;
        if session.channel {
            entry.2 += session.minutes.max(0);
        }
        entry.3 += session.spoken.max(0);
        entry.4 = entry.4.max(session.end_at.as_str());
    }
    let mut targets: Vec<_> = targets.into_iter().collect();
    targets.sort_by(|a, b| b.1.4.cmp(a.1.4));
    html! {
        div.scroll {
            table {
                thead {
                    tr {
                        th { "Channel or nick" }
                        th.num { "Sessions" }
                        th.num { "Time" }
                        th.num { "Spoken" }
                        th { "Last seen" }
                    }
                }
                tbody {
                    @for (target, (channel, sessions, minutes, spoken, last)) in &targets {
                        tr {
                            td {
                                @if *channel {
                                    a href=(channel_href(&view.tag, target)) { (target) }
                                } @else {
                                    (target) span.muted { " (private)" }
                                }
                            }
                            td.num { (number(*sessions)) }
                            td.num {
                                @if *channel { (duration(*minutes)) } @else { "-" }
                            }
                            td.num { (number(*spoken)) }
                            td { (day(Some(*last))) }
                        }
                    }
                }
            }
        }
    }
}

fn changes_table(view: &IdentityView, changes: &[(usize, Vec<NickChange>)], here: &str) -> Markup {
    html! {
        div.scroll {
            table {
                thead {
                    tr {
                        th { "Day" }
                        th { "Line" }
                        th { "Nick change" }
                        th { "" }
                    }
                }
                tbody {
                    @for (index, lines) in changes {
                        @let session = &view.sessions[*index];
                        @for change in lines {
                            tr {
                                td { (day(Some(session.start_at.as_str()))) }
                                td { code { (session.file) ":" (change.line) } }
                                td { (change.text) }
                                td {
                                    @if change.cut {
                                        a href="/cuts" { "cut here" }
                                    } @else {
                                        form.inline method="post" action="/cuts/add" {
                                            (back_field(here))
                                            input type="hidden" name="at"
                                                value=(format!("{}:{}", session.path, change.line));
                                            button.secondary type="submit"
                                                title="Someone else was at the keyboard from this nick change on"
                                                { "Cut here" }
                                        }
                                    }
                                }
                            }
                        }
                        @if lines.is_empty() {
                            tr {
                                td { (day(Some(session.start_at.as_str()))) }
                                td { code { (session.file) ":" (session.start_line) } }
                                td.muted colspan="2" { (session.nicks.join(" → ")) }
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

    fn session(nicks: &[&str], link: &str, host: Option<&str>) -> SessionRow {
        SessionRow {
            nicks: nicks.iter().map(|nick| nick.to_string()).collect(),
            ident: host.map(|_| "quokkawet".to_string()),
            host: host.map(str::to_string),
            link: link.into(),
            start_at: "2026-11-01 01:00".into(),
            end_at: "2026-11-01 02:00".into(),
            minutes: 60,
            spoken: 3,
            logger: false,
            channel: true,
            target: "#dryfish".into(),
            file_id: 1,
            file: "wetfish/#dryfish.log".into(),
            path: "/logs/wetfish/#dryfish.log".into(),
            start_line: 2,
            end_line: Some(9),
            inferred_from: None,
        }
    }

    #[test]
    fn offers_a_cut_on_nick_changes_and_a_person_for_unassigned_identities() {
        let view = IdentityView {
            id: "1a2b3c4d5e6f".into(),
            tag: "wetfish".into(),
            people: Vec::new(),
            sessions: vec![session(
                &["quokka", "rachelaway"],
                "host",
                Some("bridge.example.net"),
            )],
        };
        let context = Context {
            people: vec![("rachel".into(), Kind::Person), ("relay".into(), Kind::Bot)],
            groups: HashMap::new(),
            months: Vec::new(),
            private_spoken: 0,
            bot: false,
            changes: vec![(
                0,
                vec![NickChange {
                    line: 5,
                    text: "quokka is now known as rachelaway".into(),
                    cut: false,
                }],
            )],
        };
        let page = identity(&view, &context).into_string();
        assert!(page.contains(r#"action="/cuts/add""#));
        assert!(page.contains(r#"value="/logs/wetfish/#dryfish.log:5""#));
        assert!(page.contains(r#"action="/people/add""#));
        assert!(page.contains(r#"<option value="rachel">rachel</option>"#));
        assert!(page.contains(r#"<optgroup label="Bots"><option value="relay">relay</option>"#));
        assert!(page.contains(r#"name="kind" value="bot""#));
        assert!(page.contains("/identities?host=bridge.example.net"));
    }

    #[test]
    fn shows_search_results_with_links() {
        let view = || IdentityView {
            id: "1a2b3c4d5e6f".into(),
            tag: "wetfish".into(),
            people: vec!["rachel".into()],
            sessions: vec![session(&["rachel"], "host", Some("hostR"))],
        };
        let bots = HashSet::from(["1a2b3c4d5e6f".to_string()]);
        let here = "/identities?nick=rachel";
        let page = identity_table(&[view()], &Ticks::toggled(here, false), &bots).into_string();
        assert!(
            page.contains(r#"<input type="checkbox" name="id" value="1a2b3c4d5e6f" aria-label"#)
        );
        assert!(page.contains(r#"href="/identities?nick=rachel&amp;all=yes""#));

        // Every row ticked, with a link back to none.
        let page = identity_table(&[view()], &Ticks::toggled(here, true), &bots).into_string();
        assert!(page.contains(r#"value="1a2b3c4d5e6f" checked aria-label"#));
        assert!(
            page.contains(
                r#"href="/identities?nick=rachel" title="Untick every identity">None</a>"#
            )
        );
        assert!(page.contains(r#"<span class="warning">bot</span>"#));
        assert!(page.contains(r#"href="/identity?id=1a2b3c4d5e6f""#));
        assert!(page.contains(r#"href="/person?name=rachel""#));
        assert!(page.contains("1h"));
    }

    #[test]
    fn groups_hosts_from_a_search_by_host() {
        let panel = HostPanel {
            hosts: vec![
                MatchingHost {
                    tag: "wetfish".into(),
                    host: "bridge.example.net".into(),
                    identities: 40,
                    group: Some("matrix".into()),
                },
                MatchingHost {
                    tag: "wetfish".into(),
                    host: "old-bridge.example.net".into(),
                    identities: 12,
                    group: None,
                },
            ],
            groups: vec![("wetfish".into(), "matrix".into())],
        };
        let here = "/identities?host=%2A.example.net";
        let page = host_panel(&panel, here).into_string();
        // The grouped host can be taken out, the other added to the group.
        assert!(page.contains(r#"action="/hosts/remove""#));
        assert!(page.contains(r#"action="/hosts/add""#));
        assert!(page.contains(r#"<option value="matrix">matrix</option>"#));
        // A new group starts with the ungrouped hosts, and every form comes
        // back to the search.
        assert!(page.contains(r#"name="hosts" value="old-bridge.example.net""#));
        assert!(page.contains(r#"name="back" value="/identities?host=%2A.example.net""#));

        let search = Search {
            host: Some("*.example.net".into()),
            nick: Some(" ".into()),
            ..Search::default()
        };
        assert_eq!(search.href(), here);
    }
}
