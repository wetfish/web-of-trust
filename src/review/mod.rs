//! `wot review`: a web interface for reviewing the imported data, served on
//! this computer only. Importing stays with `wot analyze`.
//!
//! Pages are built on the server with maud and use plain HTML, with no
//! JavaScript. Each request opens the database on a blocking thread, since
//! SQLite calls block.

mod layout;
mod pages;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use maud::Markup;
use rusqlite::Connection;
use serde::Deserialize;

use crate::{db, stats};

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
        .route("/", get(overview))
        .route("/channel", get(channel))
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
        (
            self.status,
            layout::page(title, pages::error(&self.message)),
        )
            .into_response()
    }
}

type PageResult = Result<Markup, PageError>;

/// Runs a page's database work on a blocking thread with its own connection.
async fn with_db<F>(app: &App, build: F) -> PageResult
where
    F: FnOnce(&Connection) -> PageResult + Send + 'static,
{
    let path = Arc::clone(&app.db);
    tokio::task::spawn_blocking(move || {
        let conn = db::open(&path)
            .map_err(|err| PageError::internal(format!("opening the database: {err}")))?;
        build(&conn)
    })
    .await
    .map_err(|err| PageError::internal(format!("the page failed to build: {err}")))?
}

/// Turns a database error into an error page.
fn internal(err: impl std::fmt::Display) -> PageError {
    PageError::internal(format!("reading the database: {err}"))
}

async fn overview(State(app): State<App>) -> PageResult {
    with_db(&app, |conn| {
        let overview = stats::overview(conn).map_err(internal)?;
        Ok(layout::page("Overview", pages::overview(&overview)))
    })
    .await
}

#[derive(Deserialize)]
struct ChannelQuery {
    tag: String,
    name: String,
}

async fn channel(State(app): State<App>, Query(query): Query<ChannelQuery>) -> PageResult {
    with_db(&app, move |conn| {
        let channel = stats::channel_days(conn, &query.tag, &query.name)
            .map_err(internal)?
            .ok_or_else(|| {
                PageError::not_found(format!(
                    "No log files for {} in network tag {} have been imported.",
                    query.name, query.tag
                ))
            })?;
        let title = format!("{} ({})", channel.name, channel.tag);
        Ok(layout::page(&title, pages::channel(&channel)))
    })
    .await
}

async fn style() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], STYLE)
}

async fn not_found() -> PageError {
    PageError::not_found("There is no page here.")
}
