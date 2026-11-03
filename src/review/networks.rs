//! Networks: network tags linked into one network, such as a server's old
//! and new hostnames, or a bouncer's and a client's logs of the same server.

use axum::Router;
use axum::extract::{Form, Query, State};
use axum::response::Redirect;
use axum::routing::{get, post};
use maud::{Markup, html};
use serde::Deserialize;

use super::layout::{self, back_field, day, number};
use super::{App, Flash, PageError, PageResult, done, failed, internal, rebuild, with_db};
use crate::decide::{self, FolderTag, Network};
use crate::progress;

pub(super) fn routes() -> Router<App> {
    Router::new()
        .route("/networks", get(networks_page))
        .route("/networks/link", post(link))
        .route("/networks/delete", post(delete))
}

/// Ignored tags: tag, note and since when.
type Ignored = Vec<(String, Option<String>, String)>;

async fn networks_page(State(app): State<App>, Query(flash): Query<Flash>) -> PageResult {
    with_db(&app, move |conn| {
        let tags = decide::folder_tags(conn).map_err(internal)?;
        let networks = decide::networks(conn).map_err(internal)?;
        let ignored = decide::ignored_tag_list(conn).map_err(internal)?;
        Ok(layout::page(
            "Networks",
            &flash,
            page(&tags, &networks, &ignored),
        ))
    })
    .await
}

fn page(tags: &[FolderTag], networks: &[Network], ignored: &Ignored) -> Markup {
    html! {
        h1 { "Networks" }
        p.note {
            "Each folder irssi logged a server into is a network tag. Link the tags that are one "
            "network, such as a server's old and new hostnames, or a bouncer's and a client's "
            "logs of it, and they are one from then on: identities are linked across them, and "
            "channels of the same name become one, with lines logged twice counted once. "
            "Mark the tags logged through a bouncer such as ZNC: only their playback counts "
            "as replayed, and their copy of a line logged twice is the one left out. "
            "Every change rebuilds identities, which takes a while for a large database."
        }

        h2 { "Network tags" }
        @if tags.is_empty() {
            p { "Nothing imported yet." }
        } @else {
            form method="post" action="/networks/link" {
                (back_field("/networks"))
                div.scroll {
                    table {
                        thead {
                            tr {
                                th { span.visually-hidden { "Select" } }
                                th { "Tag" }
                                th { "Network" }
                                th.num { "Files" }
                                th.num { "Channels" }
                                th.num { "Lines" }
                                th.num { "Size" }
                            }
                        }
                        tbody {
                            @for tag in tags {
                                tr {
                                    td {
                                        input type="checkbox" name="tag" value=(tag.folder)
                                            aria-label=(format!("Select {}", tag.folder));
                                    }
                                    td {
                                        (tag.folder)
                                        @if tag.bouncer { " " span.muted { "bouncer" } }
                                    }
                                    td {
                                        @match &tag.network {
                                            Some(network) => (network),
                                            None => span.muted { "-" },
                                        }
                                    }
                                    td.num { (number(tag.files)) }
                                    td.num { (number(tag.channels)) }
                                    td.num { (number(tag.lines)) }
                                    td.num { (progress::bytes(tag.bytes.max(0) as u64)) }
                                }
                            }
                        }
                    }
                }
                div.bulk {
                    span.muted { "With the ticked tags:" }
                    label {
                        "Name "
                        input type="text" name="name" aria-label="Name of the new network";
                    }
                    button type="submit" name="action" value="create" { "Create network" }
                    @if !networks.is_empty() {
                        label {
                            "or add to "
                            select name="network" {
                                @for network in networks {
                                    option value=(network.name) { (network.name) }
                                }
                            }
                        }
                        button type="submit" name="action" value="add" { "Add" }
                    }
                    button.secondary type="submit" name="action" value="remove" {
                        "Remove from their network"
                    }
                    button.secondary type="submit" name="action" value="bouncer"
                        title="Logged through a bouncer such as ZNC: its playback is read for replayed lines, and its copies of lines logged twice are left out" {
                        "Mark as bouncer"
                    }
                    button.secondary type="submit" name="action" value="no_bouncer" {
                        "Not a bouncer"
                    }
                }
            }
        }

        h2 { "Networks" }
        @if networks.is_empty() {
            p.muted { "No networks yet. Tick the tags that are one network above." }
        } @else {
            @for network in networks {
                section.group {
                    h3 { (network.name) }
                    @if let Some(note) = &network.note {
                        p.note { (note) }
                    }
                    p {
                        @if network.tags.is_empty() {
                            span.muted { "No tags linked." }
                        } @else {
                            "Tags: " (network.tags.join(", "))
                        }
                    }
                    form.inline method="post" action="/networks/delete" {
                        input type="hidden" name="name" value=(network.name);
                        label {
                            input type="checkbox" name="confirm" value="yes" required;
                            " Delete " (network.name) "; its tags become separate again"
                        }
                        button.danger type="submit" { "Delete network" }
                    }
                }
            }
        }

        @if !ignored.is_empty() {
            h2 { "Ignored tags" }
            p.note {
                "Left out of imports. " code { "wot tags unignore TAG" }
                " imports one again on the next run."
            }
            ul {
                @for (tag, note, since) in ignored {
                    li {
                        strong { (tag) } span.muted { ", since " (day(Some(since.as_str()))) }
                        @if let Some(note) = note { ": " (note) }
                    }
                }
            }
        }
    }
}

/// The ticked tags of the tags table, and what to do with them.
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
        let back = "/networks";
        let tags: Vec<String> = fields
            .iter()
            .filter(|(key, _)| key == "tag")
            .map(|(_, value)| value.clone())
            .collect();
        if tags.is_empty() {
            return Ok(failed(back, "Tick at least one tag first."));
        }
        let listed = tags.join(", ");
        let result: crate::db::Result<String> = match field("action") {
            Some("create") => match field("name") {
                Some(name) => decide::create_network(conn, name, &tags, None)
                    .map(|()| format!("Created network {name} from {listed}.")),
                None => Err("Give the new network a name.".into()),
            },
            Some("add") => match field("network") {
                Some(name) => decide::add_to_network(conn, name, &tags)
                    .map(|()| format!("Linked {listed} into {name}.")),
                None => Err("Choose a network to add them to.".into()),
            },
            Some("remove") => decide::remove_from_network(conn, &tags)
                .map(|()| format!("Took {listed} out of their network.")),
            Some("bouncer") => tags
                .iter()
                .try_for_each(|tag| decide::mark_bouncer(conn, tag))
                .map(|()| format!("Marked {listed} as logged through a bouncer.")),
            Some("no_bouncer") => tags
                .iter()
                .try_for_each(|tag| decide::unmark_bouncer(conn, tag))
                .map(|()| format!("{listed} no longer marked as a bouncer.")),
            _ => Err("Choose what to do with the ticked tags.".into()),
        };
        match result {
            Ok(notice) => {
                let rebuilt = rebuild(conn)?;
                Ok(done(back, &format!("{notice} {rebuilt}")))
            }
            Err(err) => Ok(failed(back, err)),
        }
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
        let back = "/networks";
        if form.confirm.as_deref() != Some("yes") {
            return Ok(failed(back, "Tick the box to confirm the deletion."));
        }
        match decide::delete_network(conn, &form.name) {
            Ok(()) => {
                let rebuilt = rebuild(conn)?;
                Ok(done(
                    back,
                    &format!(
                        "Deleted network {}; its tags are separate again. {rebuilt}",
                        form.name
                    ),
                ))
            }
            Err(err) => Ok(failed(back, err)),
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_tags_to_link_and_networks() {
        let tag = |folder: &str, network: Option<&str>| FolderTag {
            folder: folder.into(),
            network: network.map(str::to_string),
            bouncer: folder.starts_with("bouncer"),
            files: 3,
            channels: 2,
            lines: 1_500,
            bytes: 2048,
        };
        let tags = [
            tag("irc.example.net", Some("example")),
            tag("old.example.net", Some("example")),
            tag("bouncer.example.org", None),
        ];
        let networks = [Network {
            name: "example".into(),
            note: Some("renamed server".into()),
            tags: vec!["irc.example.net".into(), "old.example.net".into()],
        }];
        let ignored = vec![(
            "rizon".to_string(),
            Some("public network".to_string()),
            "2026-11-03 10:00:00".to_string(),
        )];
        let page = page(&tags, &networks, &ignored).into_string();
        assert!(page.contains(r#"name="tag" value="bouncer.example.org""#));
        assert!(page.contains(r#"bouncer.example.org <span class="muted">bouncer</span>"#));
        assert!(page.contains(r#"value="no_bouncer""#));
        assert!(page.contains(r#"<option value="example">example</option>"#));
        assert!(page.contains("Tags: irc.example.net, old.example.net"));
        assert!(page.contains("2.0 KB"));
        assert!(page.contains("<strong>rizon</strong>"));
        assert!(page.contains(r#"action="/networks/delete""#));
    }
}
