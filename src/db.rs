//! The SQLite database that `wot analyze` imports log lines into.
//!
//! Every line of every log file becomes a row in `events`, apart from blank
//! lines and day changed lines, whose date is stored on the lines after them.
//! The `files` table records how far each file has been imported, so later
//! runs only read the lines added since. A file that shrank or whose first
//! bytes changed was replaced, and is imported again from the start.

use std::error::Error;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension, params};

use crate::discover::{LogFile, TargetKind};
use crate::irssi::{self, DateTracker, Event, Line};

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// Stored in SQLite's `user_version`. Raised whenever the schema changes.
const SCHEMA_VERSION: i64 = 1;

/// How many bytes from the start of each file are kept to recognize it.
const HEAD_LEN: u64 = 1024;

const SCHEMA: &str = "
CREATE TABLE files (
    id INTEGER PRIMARY KEY,
    -- Absolute path of the log file.
    path TEXT NOT NULL UNIQUE,
    tag TEXT NOT NULL,
    target TEXT NOT NULL,
    -- 'channel' or 'query'.
    kind TEXT NOT NULL,
    -- Import resumes from here. Only complete lines are imported.
    bytes_read INTEGER NOT NULL DEFAULT 0,
    lines_read INTEGER NOT NULL DEFAULT 0,
    -- The date in effect after the last imported line, as YYYY-MM-DD.
    date TEXT,
    -- The first bytes of the file, to notice when it was replaced.
    head BLOB NOT NULL DEFAULT x'',
    -- UTC, as YYYY-MM-DD HH:MM:SS.
    imported_at TEXT
);

CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files (id),
    -- Line number in the file, starting at 1.
    line INTEGER NOT NULL,
    -- YYYY-MM-DD, or NULL before the first line giving a date.
    date TEXT,
    -- HH:MM for events, HH:MM:SS for log_opened and log_closed, NULL for
    -- unparsed lines.
    time TEXT,
    kind TEXT NOT NULL,
    nick TEXT,
    ident TEXT,
    host TEXT,
    other_nick TEXT,
    text TEXT,
    detail TEXT
);

CREATE INDEX events_by_file_date ON events (file_id, date);
";

const INSERT_EVENT: &str = "
INSERT INTO events (file_id, line, date, time, kind, nick, ident, host, other_nick, text, detail)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
";

/// Opens the database, creating it and its tables when the file is new.
pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;

    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    match version {
        0 => conn.execute_batch(&format!(
            "BEGIN; {SCHEMA} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"
        ))?,
        SCHEMA_VERSION => {}
        other => {
            return Err(format!(
                "it uses database schema version {other}, but this version of wot uses \
                 {SCHEMA_VERSION}. The database is rebuilt from the logs, so delete it or \
                 pass a different --db"
            )
            .into());
        }
    }
    Ok(conn)
}

#[derive(Default)]
pub struct ImportSummary {
    pub new_files: usize,
    pub grown_files: usize,
    pub replaced_files: usize,
    pub unchanged_files: usize,
    /// Lines read from the log files during this import.
    pub lines: u64,
    pub errors: Vec<(PathBuf, String)>,
}

enum Outcome {
    New,
    Grown,
    Replaced,
    Unchanged,
}

/// Imports the lines added to each file since the last import. Each file is
/// imported in its own transaction, so an interrupted import resumes cleanly.
pub fn import(conn: &mut Connection, files: &[LogFile]) -> ImportSummary {
    let mut summary = ImportSummary::default();
    for file in files {
        match import_file(conn, file) {
            Ok((outcome, lines)) => {
                summary.lines += lines;
                match outcome {
                    Outcome::New => summary.new_files += 1,
                    Outcome::Grown => summary.grown_files += 1,
                    Outcome::Replaced => summary.replaced_files += 1,
                    Outcome::Unchanged => summary.unchanged_files += 1,
                }
            }
            Err(err) => summary.errors.push((file.path.clone(), err.to_string())),
        }
    }
    summary
}

/// Imports one file and returns what happened and how many lines were read.
fn import_file(conn: &mut Connection, file: &LogFile) -> Result<(Outcome, u64)> {
    let path = fs::canonicalize(&file.path)
        .unwrap_or_else(|_| file.path.clone())
        .to_string_lossy()
        .into_owned();

    let mut handle = File::open(&file.path)?;
    // Sizes, offsets and line numbers are i64, SQLite's integer type.
    let len = i64::try_from(handle.metadata()?.len())?;
    let mut head = Vec::new();
    (&mut handle).take(HEAD_LEN).read_to_end(&mut head)?;

    let tx = conn.transaction()?;
    let stored: Option<(i64, i64, i64, Option<String>, Vec<u8>)> = tx
        .query_row(
            "SELECT id, bytes_read, lines_read, date, head FROM files WHERE path = ?1",
            params![path],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;

    let (file_id, outcome, mut offset, mut line_number, mut dates) = match stored {
        None => {
            tx.execute(
                "INSERT INTO files (path, tag, target, kind) VALUES (?1, ?2, ?3, ?4)",
                params![path, file.tag, file.target, kind_name(file.kind)],
            )?;
            let id = tx.last_insert_rowid();
            (id, Outcome::New, 0, 0, DateTracker::default())
        }
        Some((id, bytes_read, lines_read, date, stored_head))
            if len >= bytes_read && head.starts_with(&stored_head) =>
        {
            if len == bytes_read {
                return Ok((Outcome::Unchanged, 0));
            }
            let date = date.map(|date| date.parse::<NaiveDate>()).transpose()?;
            let dates = DateTracker::starting_at(date);
            (id, Outcome::Grown, bytes_read, lines_read, dates)
        }
        Some((id, ..)) => {
            tx.execute("DELETE FROM events WHERE file_id = ?1", params![id])?;
            (id, Outcome::Replaced, 0, 0, DateTracker::default())
        }
    };

    handle.seek(SeekFrom::Start(u64::try_from(offset)?))?;
    let mut reader = BufReader::with_capacity(64 * 1024, handle);
    let mut buf = Vec::new();
    let mut lines = 0;
    {
        let mut insert = tx.prepare_cached(INSERT_EVENT)?;
        loop {
            buf.clear();
            let read = reader.read_until(b'\n', &mut buf)?;
            if read == 0 || buf.last() != Some(&b'\n') {
                // End of file, or a last line irssi has not finished writing.
                // It is imported once it is complete.
                break;
            }
            offset += i64::try_from(read)?;
            line_number += 1;
            lines += 1;

            // Old logs may not be valid UTF-8.
            let text = String::from_utf8_lossy(&buf);
            let line = irssi::parse_line(&text);
            dates.update(&line);

            if let Some(row) = event_row(&line, &text) {
                insert.execute(params![
                    file_id,
                    line_number,
                    dates.date().map(|date| date.to_string()),
                    row.time,
                    row.kind,
                    row.nick,
                    row.ident,
                    row.host,
                    row.other_nick,
                    row.text,
                    row.detail,
                ])?;
            }
        }
    }

    tx.execute(
        "UPDATE files
         SET bytes_read = ?1, lines_read = ?2, date = ?3, head = ?4, imported_at = datetime('now')
         WHERE id = ?5",
        params![
            offset,
            line_number,
            dates.date().map(|date| date.to_string()),
            head,
            file_id
        ],
    )?;
    tx.commit()?;

    // A file that only gained part of a line has nothing new yet.
    let outcome = match outcome {
        Outcome::Grown if lines == 0 => Outcome::Unchanged,
        other => other,
    };
    Ok((outcome, lines))
}

fn kind_name(kind: TargetKind) -> &'static str {
    match kind {
        TargetKind::Channel => "channel",
        TargetKind::Query => "query",
    }
}

/// The columns of one `events` row, apart from the file, line and date.
#[derive(Default)]
struct EventRow<'a> {
    time: Option<String>,
    kind: &'static str,
    nick: Option<&'a str>,
    ident: Option<&'a str>,
    host: Option<&'a str>,
    other_nick: Option<&'a str>,
    text: Option<&'a str>,
    detail: Option<&'a str>,
}

/// The row to store for a line, or `None` for lines that are not stored.
fn event_row<'a>(line: &Line<'a>, raw: &'a str) -> Option<EventRow<'a>> {
    let row = match line {
        Line::Blank | Line::DayChanged(_) => return None,
        Line::LogOpened(at) => EventRow {
            time: Some(at.format("%H:%M:%S").to_string()),
            kind: "log_opened",
            ..EventRow::default()
        },
        Line::LogClosed(at) => EventRow {
            time: Some(at.format("%H:%M:%S").to_string()),
            kind: "log_closed",
            ..EventRow::default()
        },
        Line::Unrecognized => EventRow {
            kind: "unparsed",
            text: Some(raw.trim_end()),
            ..EventRow::default()
        },
        Line::Event(time, event) => EventRow {
            time: Some(time.format("%H:%M").to_string()),
            ..event_columns(event)
        },
    };
    Some(row)
}

/// The columns for an event. The README lists what each column holds for
/// each kind.
fn event_columns<'a>(event: &Event<'a>) -> EventRow<'a> {
    let row = EventRow::default();
    match *event {
        Event::Message { mode, nick, text } => EventRow {
            kind: "message",
            nick: Some(nick),
            text: Some(text),
            detail: mode_str(mode),
            ..row
        },
        Event::Action { nick, text } => EventRow {
            kind: "action",
            nick: Some(nick),
            text: Some(text),
            ..row
        },
        Event::Notice { nick, text } => EventRow {
            kind: "notice",
            nick: Some(nick),
            text: Some(text),
            ..row
        },
        Event::ServerNotice { server, text } => EventRow {
            kind: "server_notice",
            text: Some(text),
            detail: Some(server),
            ..row
        },
        Event::Join {
            nick,
            ident,
            host,
            channel,
        } => EventRow {
            kind: "join",
            nick: Some(nick),
            ident: Some(ident),
            host: Some(host),
            detail: Some(channel),
            ..row
        },
        Event::Part {
            nick,
            ident,
            host,
            channel,
            reason,
        } => EventRow {
            kind: "part",
            nick: Some(nick),
            ident: Some(ident),
            host: Some(host),
            text: Some(reason),
            detail: Some(channel),
            ..row
        },
        Event::Quit {
            nick,
            ident,
            host,
            reason,
        } => EventRow {
            kind: "quit",
            nick: Some(nick),
            ident: Some(ident),
            host: Some(host),
            text: Some(reason),
            ..row
        },
        Event::Kick {
            nick,
            channel,
            by,
            reason,
        } => EventRow {
            kind: "kick",
            nick: Some(nick),
            other_nick: Some(by),
            text: Some(reason),
            detail: Some(channel),
            ..row
        },
        Event::NickChange { old, new } => EventRow {
            kind: "nick_change",
            nick: Some(old),
            other_nick: Some(new),
            ..row
        },
        Event::OwnNickChange { new } => EventRow {
            kind: "own_nick_change",
            other_nick: Some(new),
            ..row
        },
        Event::Topic {
            nick,
            channel,
            topic,
        } => EventRow {
            kind: "topic",
            nick: Some(nick),
            text: Some(topic),
            detail: Some(channel),
            ..row
        },
        Event::TopicUnset { nick, channel } => EventRow {
            kind: "topic_unset",
            nick: Some(nick),
            detail: Some(channel),
            ..row
        },
        Event::Mode {
            channel,
            modes,
            by,
            server,
        } => EventRow {
            kind: if server { "server_mode" } else { "mode" },
            nick: Some(by),
            text: Some(modes),
            detail: Some(channel),
            ..row
        },
        Event::Netsplit { servers, nicks } => EventRow {
            kind: "netsplit",
            text: Some(nicks),
            detail: Some(servers),
            ..row
        },
        Event::NetsplitOver { nicks } => EventRow {
            kind: "netsplit_over",
            text: Some(nicks),
            ..row
        },
        Event::Client { text } => EventRow {
            kind: "client",
            text: Some(text),
            ..row
        },
    }
}

fn mode_str(mode: Option<char>) -> Option<&'static str> {
    Some(match mode? {
        '@' => "@",
        '+' => "+",
        '%' => "%",
        '~' => "~",
        '&' => "&",
        '!' => "!",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use super::*;
    use crate::discover::{self, tests::TempDir};

    const LOG: &str = "--- Log opened Sun Nov 01 01:16:13 2026\n\
                       01:16 -!- rachel [rachel@NETADMIN.VHOST] has joined #analysis\n\
                       01:16 <@rachel> Hello World.\n\
                       01:20 -!- Something new happened\n\
                       --- Day changed Mon Nov 02 2026\n";

    fn setup(name: &str) -> (TempDir, PathBuf, Connection) {
        let root = TempDir::new(name);
        fs::create_dir_all(root.0.join("wetfish")).unwrap();
        let log = root.0.join("wetfish/#analysis.log");
        fs::write(&log, LOG).unwrap();
        let conn = open(&root.0.join("wot.sqlite")).unwrap();
        (root, log, conn)
    }

    fn run(root: &TempDir, conn: &mut Connection) -> ImportSummary {
        let inventory = discover::discover(&[root.0.clone()]).unwrap();
        let summary = import(conn, &inventory.files);
        assert!(summary.errors.is_empty(), "{:?}", summary.errors);
        summary
    }

    fn append(path: &Path, text: &str) {
        let mut file = OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    /// "line date time kind nick" for every event in line order, with `-`
    /// for empty columns.
    fn events(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT line || ' ' || ifnull(date, '-') || ' ' || ifnull(time, '-')
                        || ' ' || kind || ' ' || ifnull(nick, '-')
                 FROM events ORDER BY line",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap();
        rows
    }

    #[test]
    fn imports_every_line_with_its_date() {
        let (root, _, mut conn) = setup("db-import");
        let summary = run(&root, &mut conn);
        assert_eq!(summary.new_files, 1);
        assert_eq!(summary.lines, 5);

        assert_eq!(
            events(&conn),
            [
                "1 2026-11-01 01:16:13 log_opened -",
                "2 2026-11-01 01:16 join rachel",
                "3 2026-11-01 01:16 message rachel",
                "4 2026-11-01 - unparsed -",
            ]
        );

        let join: String = conn
            .query_row(
                "SELECT ident || ' ' || host || ' ' || detail FROM events WHERE kind = 'join'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(join, "rachel NETADMIN.VHOST #analysis");
    }

    #[test]
    fn imports_only_new_complete_lines() {
        let (root, log, mut conn) = setup("db-grow");
        run(&root, &mut conn);

        let summary = run(&root, &mut conn);
        assert_eq!((summary.unchanged_files, summary.lines), (1, 0));

        // A line irssi has not finished writing waits for its newline.
        append(&log, "00:05 < tester> mor");
        let summary = run(&root, &mut conn);
        assert_eq!((summary.unchanged_files, summary.lines), (1, 0));

        append(&log, "ning\n");
        let summary = run(&root, &mut conn);
        assert_eq!((summary.grown_files, summary.lines), (1, 1));

        // The date from the day changed line carries over between imports.
        let events = events(&conn);
        assert_eq!(events.len(), 5);
        assert_eq!(events[4], "6 2026-11-02 00:05 message tester");
    }

    #[test]
    fn imports_a_replaced_file_again() {
        let (root, log, mut conn) = setup("db-replace");
        run(&root, &mut conn);

        fs::write(
            &log,
            "--- Log opened Tue Nov 03 09:00:00 2026\n09:00 <bob> hi\n",
        )
        .unwrap();
        let summary = run(&root, &mut conn);
        assert_eq!((summary.replaced_files, summary.lines), (1, 2));

        assert_eq!(
            events(&conn),
            [
                "1 2026-11-03 09:00:00 log_opened -",
                "2 2026-11-03 09:00 message bob",
            ]
        );
    }

    #[test]
    fn refuses_a_database_from_another_schema_version() {
        let root = TempDir::new("db-version");
        let path = root.0.join("wot.sqlite");
        open(&path)
            .unwrap()
            .execute_batch("PRAGMA user_version = 99;")
            .unwrap();
        assert!(open(&path).is_err());
    }
}
