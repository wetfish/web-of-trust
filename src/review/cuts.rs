//! Cuts: nick changes made by someone else at the keyboard. Every change
//! rebuilds identities, since cuts change how sessions link.

use axum::Router;
use axum::extract::{Form, Query, State};
use axum::response::Redirect;
use axum::routing::{get, post};
use maud::{Markup, html};
use serde::Deserialize;

use super::layout::{self, back_field, day, number};
use super::{
    App, Flash, PageError, PageResult, back_to, done, failed, filled, internal, rebuild, with_db,
};
use crate::decide;
use crate::stats::{self, Cut};

pub(super) fn routes() -> Router<App> {
    Router::new()
        .route("/cuts", get(cuts_page))
        .route("/cuts/add", post(add))
        .route("/cuts/remove", post(remove))
}

async fn cuts_page(State(app): State<App>, Query(flash): Query<Flash>) -> PageResult {
    with_db(&app, move |conn| {
        let cuts = stats::cuts(conn).map_err(internal)?;
        Ok(layout::page("Cuts", &flash, page(&cuts)))
    })
    .await
}

fn page(cuts: &[Cut]) -> Markup {
    html! {
        h1 { "Cuts" }
        p.note {
            "A cut marks a nick change made by someone else at the keyboard. The session ends at "
            "the nick change, and the new nick starts a session that keeps no host and is linked "
            "to nothing else. Cut from the nick changes on an identity's page, or by line here. "
            "A stale cut's line no longer holds the nick change it was made on, so it is not "
            "applied. Every change rebuilds identities."
        }
        @if cuts.is_empty() {
            p { "No cuts yet." }
        } @else {
            p.lead { (number(cuts.len() as i64)) " cuts." }
            div.scroll {
                table {
                    thead {
                        tr {
                            th.num { "Id" }
                            th { "Line" }
                            th { "Nick change" }
                            th { "Status" }
                            th { "Added" }
                            th { "Note" }
                            th { "" }
                        }
                    }
                    tbody {
                        @for cut in cuts {
                            tr {
                                td.num { (cut.id) }
                                td { code title=(cut.path) { (cut.file) ":" (cut.line) } }
                                td { (cut.expected) }
                                td {
                                    @if cut.applied { "applied" } @else { span.warning { "stale" } }
                                }
                                td { (day(Some(cut.created_at.as_str()))) }
                                td.wrap { (cut.note.clone().unwrap_or_default()) }
                                td {
                                    form.inline method="post" action="/cuts/remove" {
                                        input type="hidden" name="id" value=(cut.id);
                                        button.secondary type="submit" { "Remove" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        h2 { "New cut" }
        form.stacked method="post" action="/cuts/add" {
            (back_field("/cuts"))
            label {
                "Nick change line, as " code { "tag/target.log:LINE" } " or the full path and "
                code { ":LINE" } " "
                input type="text" name="at" required placeholder="wetfish/#channel.log:1234";
            }
            label { "Note (optional) " input type="text" name="note"; }
            button type="submit" { "Cut" }
        }
    }
}

#[derive(Deserialize)]
struct AddForm {
    at: String,
    note: Option<String>,
    back: Option<String>,
}

async fn add(State(app): State<App>, Form(form): Form<AddForm>) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        let back = back_to(form.back.as_deref(), "/cuts");
        let change = match decide::add_cut(conn, form.at.trim(), filled(&form.note)) {
            Ok(change) => change,
            Err(err) => return Ok(failed(&back, err)),
        };
        let rebuilt = rebuild(conn)?;
        Ok(done(
            &back,
            &format!(
                "Cut at \"{change}\". The session after it is now an identity of its own; \
                 find it by nick to add it to a person. {rebuilt}"
            ),
        ))
    })
    .await
}

#[derive(Deserialize)]
struct RemoveForm {
    id: i64,
}

async fn remove(
    State(app): State<App>,
    Form(form): Form<RemoveForm>,
) -> Result<Redirect, PageError> {
    with_db(&app, move |conn| {
        if let Err(err) = decide::remove_cut(conn, form.id) {
            return Ok(failed("/cuts", err));
        }
        let rebuilt = rebuild(conn)?;
        Ok(done(
            "/cuts",
            &format!("Removed cut {}. {rebuilt}", form.id),
        ))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_stale_cuts() {
        let cut = Cut {
            id: 3,
            path: "/logs/wetfish/#dryfish.log".into(),
            file: "wetfish/#dryfish.log".into(),
            line: 2050,
            expected: "quokka is now known as rachelaway".into(),
            applied: false,
            note: Some("rachel on a friend's computer".into()),
            created_at: "2026-11-02 12:00:00".into(),
        };
        let page = page(&[cut]).into_string();
        assert!(page.contains("wetfish/#dryfish.log:2050"));
        assert!(page.contains(r#"title="/logs/wetfish/#dryfish.log""#));
        assert!(page.contains(r#"<span class="warning">stale</span>"#));
        assert!(page.contains(r#"name="id" value="3""#));
    }
}
