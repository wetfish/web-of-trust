//! `wot review`: a web interface for reviewing the imported data and making
//! decisions about it, served on this computer only. New lines can be read
//! from the log folders already imported, as `wot analyze` does; a folder
//! never imported needs `wot analyze` once.
//!
//! Pages are built on the server with maud and use plain HTML forms, with no
//! JavaScript. A form posts to an action, which changes the database and
//! redirects back to a page with a notice or an error in the address, so
//! reloading a page never repeats an action. Each request opens the database
//! on a blocking thread, since SQLite calls block.

mod activity;
mod analyze;
mod channels;
mod cuts;
mod history;
mod hosts;
mod identities;
mod layout;
mod networks;
mod pages;
mod people;

use std::fmt::Display;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use maud::Markup;
use rusqlite::Connection;
use serde::Deserialize;

use crate::db;
use crate::identity::{self, RebuildSummary};
use crate::progress::Progress;

const STYLE: &str = include_str!("style.css");

#[derive(Clone)]
struct App {
    db: Arc<PathBuf>,
}

/// Serves the review pages on 127.0.0.1 until the process is stopped.
pub fn run(db_path: &Path, port: u16) -> db::Result<()> {
    if !db_path.exists() {
        return Err(format!(
            "there is no database at {}; import logs first with wot analyze",
            db_path.display()
        )
        .into());
    }
    // Upgrade the database once up front, so pages never have to.
    db::open(db_path)?;

    let app = App {
        db: Arc::new(db_path.to_path_buf()),
    };
    let router = Router::new()
        .merge(pages::routes())
        .merge(identities::routes())
        .merge(people::routes())
        .merge(hosts::routes())
        .merge(cuts::routes())
        .merge(activity::routes())
        .merge(channels::routes())
        .merge(history::routes())
        .merge(networks::routes())
        .merge(analyze::routes())
        .route("/style.css", get(style))
        .fallback(not_found)
        .with_state(app);

    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .map_err(|err| format!("listening on {address}: {err}"))?;
        eprintln!(
            "Reviewing {} at http://{address}/ (press Ctrl+C to stop)",
            db_path.display()
        );
        axum::serve(listener, router).await?;
        Ok(())
    })
}

/// An error page with an HTTP status.
struct PageError {
    status: StatusCode,
    message: String,
}

impl PageError {
    fn not_found(message: impl Into<String>) -> Self {
        PageError {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        PageError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl IntoResponse for PageError {
    fn into_response(self) -> Response {
        let title = if self.status == StatusCode::NOT_FOUND {
            "Not found"
        } else {
            "Error"
        };
        let page = layout::page(title, &Flash::default(), pages::error(&self.message));
        (self.status, page).into_response()
    }
}

type PageResult = Result<Markup, PageError>;

/// Runs database work on a blocking thread with its own connection.
async fn with_db<T, F>(app: &App, work: F) -> Result<T, PageError>
where
    T: Send + 'static,
    F: FnOnce(&mut Connection) -> Result<T, PageError> + Send + 'static,
{
    let path = Arc::clone(&app.db);
    tokio::task::spawn_blocking(move || {
        let mut conn = db::open(&path)
            .map_err(|err| PageError::internal(format!("opening the database: {err}")))?;
        work(&mut conn)
    })
    .await
    .map_err(|err| PageError::internal(format!("the page failed to build: {err}")))?
}

/// Turns a database error into an error page.
fn internal(err: impl Display) -> PageError {
    PageError::internal(format!("reading the database: {err}"))
}

/// A notice or error carried in the address after an action, shown at the
/// top of the page it redirected to.
#[derive(Default, Deserialize)]
struct Flash {
    notice: Option<String>,
    error: Option<String>,
}

/// A page to send the browser back to after an action. Only paths on this
/// site are allowed, so a form cannot redirect elsewhere.
fn back_to(back: Option<&str>, default: &str) -> String {
    match back {
        Some(back) if back.starts_with('/') && !back.starts_with("//") => back.to_string(),
        _ => default.to_string(),
    }
}

/// Redirects to `back` with a notice.
fn done(back: &str, notice: &str) -> Redirect {
    flash(back, "notice", notice)
}

/// Redirects to `back` with an error.
fn failed(back: &str, error: impl Display) -> Redirect {
    flash(back, "error", &error.to_string())
}

fn flash(back: &str, key: &str, message: &str) -> Redirect {
    // Drop an earlier notice or error from the address.
    let base = back.split(['?', '#']).next().unwrap_or(back);
    let query: Vec<&str> = back
        .split_once('?')
        .map(|(_, query)| query.split('#').next().unwrap_or(query))
        .unwrap_or("")
        .split('&')
        .filter(|pair| {
            !pair.is_empty() && !pair.starts_with("notice=") && !pair.starts_with("error=")
        })
        .collect();
    let mut url = base.to_string();
    url.push('?');
    for pair in query {
        url.push_str(pair);
        url.push('&');
    }
    url.push_str(key);
    url.push('=');
    url.push_str(&layout::url_encode(message));
    Redirect::to(&url)
}

/// A form field, or `None` when it was left blank.
fn filled(value: &Option<String>) -> Option<&str> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Rebuilds identities after a decision that changes them, and describes the
/// result for a notice. The page waits for it, so progress goes to the
/// terminal `wot review` runs in.
fn rebuild(conn: &mut Connection) -> Result<String, PageError> {
    let mut progress = Progress::to_stderr();
    progress.note("Rebuilding sessions and identities for a decision made in the browser...");
    let summary = identity::rebuild_with_progress(conn, &mut progress)
        .map_err(|err| PageError::internal(format!("rebuilding identities: {err}")))?;
    Ok(rebuild_text(&summary))
}

fn rebuild_text(summary: &RebuildSummary) -> String {
    let mut text = format!(
        "Identities rebuilt: {} identities from {} sessions, {} new, {} merged.",
        layout::number(summary.identities as i64),
        layout::number(summary.sessions as i64),
        layout::number(summary.new_identities as i64),
        layout::number(summary.merged_identities as i64),
    );
    if summary.cuts_stale > 0 {
        text.push_str(&format!(
            " {} cuts are stale.",
            layout::number(summary.cuts_stale as i64)
        ));
    }
    text
}

async fn style() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], STYLE)
}

async fn not_found() -> PageError {
    PageError::not_found("There is no page here.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn location(redirect: Redirect) -> String {
        let response = redirect.into_response();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        response.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn redirects_only_within_the_site() {
        assert_eq!(back_to(Some("/people"), "/"), "/people");
        assert_eq!(back_to(Some("//elsewhere.example"), "/"), "/");
        assert_eq!(back_to(Some("https://elsewhere.example"), "/"), "/");
        assert_eq!(back_to(None, "/hosts"), "/hosts");
    }

    #[test]
    fn carries_one_message_in_the_address() {
        assert_eq!(location(done("/people", "Saved.")), "/people?notice=Saved.");
        assert_eq!(
            location(failed("/person?name=rachel&notice=Old", "no such identity")),
            "/person?name=rachel&error=no%20such%20identity"
        );
    }

    #[test]
    fn treats_blank_fields_as_missing() {
        assert_eq!(filled(&Some("  ".into())), None);
        assert_eq!(filled(&Some(" x ".into())), Some("x"));
        assert_eq!(filled(&None), None);
    }
}
