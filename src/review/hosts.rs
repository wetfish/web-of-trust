//! Host groups: hosts that are one bridge or bouncer. Every change rebuilds
//! identities, since groups change how sessions link.

use axum::Router;
use axum::extract::{Form, Query, State};
use axum::response::Redirect;
use axum::routing::{get, post};
use maud::{Markup, html};
use serde::Deserialize;

use super::layout::{self, day, number, search_href};
use super::{
    App, Flash, PageError, PageResult, back_to, done, failed, filled, internal, rebuild, with_db,
};
use crate::decide;
use crate::stats::{self, HostGroup, SharedHost};

/// Shared hosts listed as candidates for a group.
const MAX_SHARED: i64 = 50;

pub(super) fn routes() -> Router<App> {
    Router::new()
        .route("/hosts", get(hosts_page))
        .route("/hosts/add-group", post(add_group))
        .route("/hosts/edit-group", post(edit_group))
        .route("/hosts/add", post(add_hosts))
        .route("/hosts/remove", post(remove_host))
        .route("/hosts/remove-group", post(remove_group))
}

#[derive(Default, Deserialize)]
struct HostsQuery {
    /// A host to fill in the forms that add hosts to a group.
    add: Option<String>,
}

async fn hosts_page(
    State(app): State<App>,
    Query(query): Query<HostsQuery>,
    Query(flash): Query<Flash>,
) -> PageResult {
    with_db(&app, move |conn| {
        let groups = stats::host_groups(conn).map_err(internal)?;
        let shared = stats::shared_hosts(conn, MAX_SHARED).map_err(internal)?;
        let tags = stats::tags(conn).map_err(internal)?;
        let add = filled(&query.add).unwrap_or_default();
        Ok(layout::page(
            "Host groups",
            &flash,
            page(&groups, &shared, &tags, add),
        ))
    })
    .await
}

fn page(groups: &[HostGroup], shared: &[SharedHost], tags: &[String], add: &str) -> Markup {
    html! {
        h1 { "Host groups" }
        p.note {
            "A host group says that several hosts are one bridge or bouncer. Hosts in a group "
            "count as one host. When the ident identifies the person, sessions with the same "
            "ident anywhere in the group are linked whatever their nick. Hosts are added exactly "
            "as they appear in the logs, ignoring case. Every change rebuilds identities."
        }
        @if !add.is_empty() {
            p.flash.notice { "Choose a group below to add " code { (add) } " to, or create one." }
        }

        @for group in groups {
            (group_section(group, add))
        }

        h2 { "New host group" }
        form.stacked method="post" action="/hosts/add-group" {
            @if tags.len() > 1 {
                label {
                    "Network tag "
                    select name="tag" {
                        @for tag in tags { option value=(tag) { (tag) } }
                    }
                }
            }
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
            label { "Note (optional) " input type="text" name="note"; }
            button type="submit" { "Create group" }
        }

        h2 { "Shared hosts" }
        p.note {
            "Hosts used by more than one identity that are not in a group, the most used first. "
            "Bridges and bouncers show many identities with different idents; many identities "
            "with one ident on one host may be one person, or many accounts."
        }
        @if shared.is_empty() {
            p { "No shared hosts outside groups." }
        } @else {
            div.scroll {
                table {
                    thead {
                        tr {
                            @if tags.len() > 1 { th { "Tag" } }
                            th { "Host" }
                            th.num { "Identities" }
                            th.num { "Idents" }
                            th.num { "Nicks" }
                            th { "" }
                        }
                    }
                    tbody {
                        @for host in shared {
                            tr {
                                @if tags.len() > 1 { td { (host.tag) } }
                                td { a href=(search_href(&[("host", host.host.as_str())])) { (host.host) } }
                                td.num { (number(host.identities)) }
                                td.num { (number(host.idents)) }
                                td.num { (number(host.nicks)) }
                                td {
                                    a href=(search_href(&[("host", host.host.as_str())])) {
                                        "review and group…"
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

fn group_section(group: &HostGroup, add: &str) -> Markup {
    html! {
        section.group {
            h2 { (group.name) " " span.muted { "(" (group.kind) ", " (group.tag) ")" } }
            p {
                @if group.ident_is_user {
                    "The ident identifies the person on these hosts, across nicks."
                } @else {
                    "These hosts count as one host; nick and ident must both match."
                }
                @if let Some(note) = &group.note { " " em { (note) } }
                " " span.muted { "Created " (day(Some(group.created_at.as_str()))) "." }
            }
            @if group.members.is_empty() {
                p.muted { "No hosts yet." }
            } @else {
                div.scroll {
                    table {
                        thead {
                            tr {
                                th { "Host" }
                                th.num { "Identities" }
                                th { "Added" }
                                th { "Note" }
                                th { "" }
                            }
                        }
                        tbody {
                            @for member in &group.members {
                                tr {
                                    td { a href=(search_href(&[("host", member.host.as_str())])) { (member.host) } }
                                    td.num { (number(member.identities)) }
                                    td { (day(Some(member.created_at.as_str()))) }
                                    td.wrap { (member.note.clone().unwrap_or_default()) }
                                    td {
                                        form.inline method="post" action="/hosts/remove" {
                                            input type="hidden" name="tag" value=(group.tag);
                                            input type="hidden" name="host" value=(member.host);
                                            button.secondary type="submit" { "Remove" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            form.inline method="post" action="/hosts/add" {
                input type="hidden" name="tag" value=(group.tag);
                input type="hidden" name="group" value=(group.name);
                label {
                    "Add hosts, separated by spaces "
                    input type="text" name="hosts" required value=(add);
                }
                button type="submit" { "Add" }
            }
            details {
                summary { "Settings" }
                form.stacked method="post" action="/hosts/edit-group" {
                    input type="hidden" name="tag" value=(group.tag);
                    input type="hidden" name="name" value=(group.name);
                    label {
                        "Kind "
                        select name="kind" {
                            option value="bridge" selected[group.kind == "bridge"] { "bridge" }
                            option value="bouncer" selected[group.kind == "bouncer"] { "bouncer" }
                        }
                    }
                    label {
                        input type="checkbox" name="ident_is_user" value="yes" checked[group.ident_is_user];
                        " The ident identifies the person on these hosts"
                    }
                    label {
                        "Note "
                        input type="text" name="note" value=(group.note.clone().unwrap_or_default());
                    }
                    button type="submit" { "Save" }
                }
                form.inline method="post" action="/hosts/remove-group" {
                    input type="hidden" name="tag" value=(group.tag);
                    input type="hidden" name="name" value=(group.name);
                    label {
                        input type="checkbox" name="confirm" value="yes" required;
                        " Delete " (group.name) " and take its hosts out of it"
                    }
                    button.danger type="submit" { "Delete group" }
                }
            }
        }
    }
}

/// The page to return to after an action, keeping the host being added.
const BACK: &str = "/hosts";

fn checked(value: &Option<String>) -> bool {
    value.as_deref() == Some("yes")
}

/// Splits hosts from a form field, separated by spaces or commas.
fn hosts(field: &str) -> Vec<String> {
    field
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|host| !host.is_empty())
        .map(str::to_string)
        .collect()
}

#[derive(Deserialize)]
struct AddGroupForm {
    tag: Option<String>,
    name: String,
    kind: String,
    ident_is_user: Option<String>,
    note: Option<String>,
    /// Hosts to add to the new group straight away.
    hosts: Option<String>,
    back: Option<String>,
}

async fn add_group(
    State(app): State<App>,
    Form(form): Form<AddGroupForm>,
) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        let back = back_to(form.back.as_deref(), BACK);
        if !matches!(form.kind.as_str(), "bridge" | "bouncer") {
            return Ok(failed(&back, "The kind must be bridge or bouncer."));
        }
        let name = form.name.trim();
        let tag = match decide::resolve_tag(conn, filled(&form.tag)) {
            Ok(tag) => tag,
            Err(err) => return Ok(failed(&back, err)),
        };
        if let Err(err) = decide::add_group(
            conn,
            &tag,
            name,
            &form.kind,
            checked(&form.ident_is_user),
            filled(&form.note),
        ) {
            return Ok(failed(&back, err));
        }
        let hosts = hosts(form.hosts.as_deref().unwrap_or_default());
        if hosts.is_empty() {
            // An empty group changes nothing, so there is nothing to rebuild.
            return Ok(done(
                &back,
                &format!("Created {} group {name}. Add its hosts below.", form.kind),
            ));
        }
        if let Err(err) = decide::add_hosts(conn, &tag, name, &hosts, None) {
            return Ok(failed(
                &back,
                format!(
                    "Created {} group {name}, but adding its hosts failed: {err}",
                    form.kind
                ),
            ));
        }
        let rebuilt = rebuild(conn)?;
        Ok(done(
            &back,
            &format!(
                "Created {} group {name} with {}. {rebuilt}",
                form.kind,
                hosts.join(", ")
            ),
        ))
    })
    .await
}

#[derive(Deserialize)]
struct EditGroupForm {
    tag: String,
    name: String,
    kind: String,
    ident_is_user: Option<String>,
    note: Option<String>,
}

async fn edit_group(
    State(app): State<App>,
    Form(form): Form<EditGroupForm>,
) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        if !matches!(form.kind.as_str(), "bridge" | "bouncer") {
            return Ok(failed(BACK, "The kind must be bridge or bouncer."));
        }
        let note = form.note.as_deref().unwrap_or_default();
        if let Err(err) = decide::edit_group(
            conn,
            &form.tag,
            &form.name,
            Some(&form.kind),
            Some(checked(&form.ident_is_user)),
            Some(note),
        ) {
            return Ok(failed(BACK, err));
        }
        let rebuilt = rebuild(conn)?;
        Ok(done(BACK, &format!("Saved {}. {rebuilt}", form.name)))
    })
    .await
}

#[derive(Deserialize)]
struct AddHostsForm {
    tag: String,
    group: String,
    hosts: String,
    back: Option<String>,
}

async fn add_hosts(
    State(app): State<App>,
    Form(form): Form<AddHostsForm>,
) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        let hosts = hosts(&form.hosts);
        // Back to the page the form was on, or to this page with the hosts
        // still filled in after an error.
        let back = back_to(form.back.as_deref(), BACK);
        if hosts.is_empty() {
            return Ok(failed(&back, "Give at least one host."));
        }
        if let Err(err) = decide::add_hosts(conn, &form.tag, &form.group, &hosts, None) {
            let back = match form.back {
                Some(_) => back,
                None => format!("{BACK}?add={}", layout::url_encode(&form.hosts)),
            };
            return Ok(failed(&back, err));
        }
        let rebuilt = rebuild(conn)?;
        Ok(done(
            &back,
            &format!("Added {} to {}. {rebuilt}", hosts.join(", "), form.group),
        ))
    })
    .await
}

#[derive(Deserialize)]
struct RemoveHostForm {
    tag: String,
    host: String,
    back: Option<String>,
}

async fn remove_host(
    State(app): State<App>,
    Form(form): Form<RemoveHostForm>,
) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        let back = back_to(form.back.as_deref(), BACK);
        if let Err(err) = decide::remove_hosts(conn, &form.tag, &[form.host.clone()]) {
            return Ok(failed(&back, err));
        }
        let rebuilt = rebuild(conn)?;
        Ok(done(
            &back,
            &format!("Took {} out of its group. {rebuilt}", form.host),
        ))
    })
    .await
}

#[derive(Deserialize)]
struct RemoveGroupForm {
    tag: String,
    name: String,
    confirm: Option<String>,
}

async fn remove_group(
    State(app): State<App>,
    Form(form): Form<RemoveGroupForm>,
) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        if !checked(&form.confirm) {
            return Ok(failed(BACK, "Tick the box to confirm the deletion."));
        }
        if let Err(err) = decide::remove_group(conn, &form.tag, &form.name) {
            return Ok(failed(BACK, err));
        }
        let rebuilt = rebuild(conn)?;
        Ok(done(
            BACK,
            &format!("Deleted host group {}. {rebuilt}", form.name),
        ))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::GroupMember;

    #[test]
    fn splits_hosts() {
        assert_eq!(
            hosts("a.example b.example, c.example"),
            ["a.example", "b.example", "c.example"]
        );
    }

    #[test]
    fn shows_groups_with_their_settings_and_a_host_to_add() {
        let group = HostGroup {
            id: 1,
            tag: "wetfish".into(),
            name: "matrix".into(),
            kind: "bouncer".into(),
            ident_is_user: false,
            note: None,
            created_at: "2026-11-02 12:00:00".into(),
            members: vec![GroupMember {
                host: "bridge.example.net".into(),
                note: None,
                created_at: "2026-11-02 12:00:00".into(),
                identities: 12,
            }],
        };
        let page = page(&[group], &[], &["wetfish".into()], "old-bridge.example.net").into_string();
        assert!(page.contains(r#"<option value="bouncer" selected>bouncer</option>"#));
        assert!(page.contains(r#"name="hosts" required value="old-bridge.example.net""#));
        assert!(page.contains(r#"action="/hosts/remove""#));
        // With one tag there is no tag to choose.
        assert!(!page.contains(r#"select name="tag""#));
        assert!(page.contains("Created 2026-11-02."));
    }
}
