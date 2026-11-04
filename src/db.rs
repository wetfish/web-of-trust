//! The SQLite database that `wot analyze` imports log lines into.
//!
//! Every line of every log file becomes a row in `events`, apart from blank
//! lines and day changed lines, whose date is stored on the lines after them.
//! The `files` table records how far each file has been imported, so later
//! runs only read the lines added since. A file that shrank or whose first
//! bytes changed was replaced, and is imported again from the start.
//!
//! The `identities`, `identity_keys` and `sessions` tables are filled by the
//! `identity` module, and rebuilt from the events whenever needed.
//!
//! The decision tables hold what a person decided by hand, and nothing
//! rebuilds them: `host_groups` and `host_group_members` for bridges and
//! bouncers, `people` and `person_identities` for who is who, humans and bots
//! alike, with `person_notes`, `session_cuts` for nick changes made by someone
//! else at the keyboard, and `channel_reviews`. `who_lists` and `who_entries`
//! hold channel user lists pasted into `wot review`, which nothing rebuilds
//! either. The `activity` table is rebuilt with the identities, by the
//! `activity` module.

use std::collections::HashMap;
use std::error::Error;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension, params};

use crate::discover::{LogFile, TargetKind};
use crate::irssi::{self, DateTracker, Event, Line};
use crate::progress::{Progress, Unit};
use crate::report::thousands;

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// How many bytes from the start of each file are kept to recognize it.
const HEAD_LEN: u64 = 1024;

/// The schema, as the steps that build it. SQLite's `user_version` stores how
/// many steps a database has had, so an older database is brought up to date
/// by running the steps it is missing.
const MIGRATIONS: [&str; 12] = [
    SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7, SCHEMA_V8,
    SCHEMA_V9, SCHEMA_V10, SCHEMA_V11, SCHEMA_V12,
];

const SCHEMA_V1: &str = "
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

const SCHEMA_V2: &str = "
-- One row per person, as far as the logs can tell.
CREATE TABLE identities (
    -- Random, 12 hexadecimal characters.
    id TEXT PRIMARY KEY,
    tag TEXT NOT NULL,
    -- UTC, as YYYY-MM-DD HH:MM:SS. The older id wins when two merge.
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    -- Set when this identity was merged into another.
    merged_into TEXT REFERENCES identities (id)
);

-- Which nick and ident@host combinations belong to which identity. This is
-- what keeps identity ids stable between rebuilds.
CREATE TABLE identity_keys (
    tag TEXT NOT NULL,
    -- Lowercased.
    nick TEXT NOT NULL,
    -- Both empty for a nick that never appeared with a host.
    ident TEXT NOT NULL,
    host TEXT NOT NULL,
    identity_id TEXT NOT NULL REFERENCES identities (id),
    PRIMARY KEY (tag, nick, ident, host)
);

-- One continuous stretch of a nick being present in one log file.
CREATE TABLE sessions (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files (id),
    identity_id TEXT NOT NULL REFERENCES identities (id),
    -- 'host', 'inferred' or 'nick_only'.
    link TEXT NOT NULL,
    -- For inferred sessions, the id of the session with a host they were
    -- attributed from. Deliberately not a foreign key: SQLite checks a
    -- deferred self-reference by scanning the table on every insert, which
    -- makes rebuilding hundreds of thousands of sessions take hours.
    inferred_from INTEGER,
    -- Space separated, in the order the session used them.
    nicks TEXT NOT NULL,
    ident TEXT,
    host TEXT,
    -- YYYY-MM-DD HH:MM.
    start_at TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    -- 1 when the session started with a join line, 0 when the nick was
    -- already present and start_at is when logging started.
    joined INTEGER NOT NULL,
    end_at TEXT NOT NULL,
    end_line INTEGER,
    end_reason TEXT NOT NULL,
    minutes INTEGER NOT NULL,
    -- Messages and actions.
    spoken INTEGER NOT NULL,
    -- 1 for the logging client's own session.
    logger INTEGER NOT NULL
);

CREATE INDEX sessions_by_identity ON sessions (identity_id);
";

const SCHEMA_V3: &str = "
-- Hosts that many people connect through, such as a Matrix bridge that moved
-- between servers, or a bouncer.
CREATE TABLE host_groups (
    id INTEGER PRIMARY KEY,
    tag TEXT NOT NULL,
    name TEXT NOT NULL,
    -- 'bridge' or 'bouncer'.
    kind TEXT NOT NULL CHECK (kind IN ('bridge', 'bouncer')),
    -- 1 when the ident identifies the person on these hosts, across nicks.
    ident_is_user INTEGER NOT NULL DEFAULT 1,
    note TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (tag, name)
);

-- The hosts in each group, exactly as they appear in the logs.
CREATE TABLE host_group_members (
    group_id INTEGER NOT NULL REFERENCES host_groups (id) ON DELETE CASCADE,
    tag TEXT NOT NULL,
    host TEXT NOT NULL COLLATE NOCASE,
    note TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    -- A host is in at most one group.
    PRIMARY KEY (tag, host)
);

CREATE INDEX host_group_members_by_group ON host_group_members (group_id);

-- The humans behind identities, as decided by hand. A person can hold
-- identities from any network tag.
CREATE TABLE people (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE,
    note TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Which identities belong to which person, at most one person each.
CREATE TABLE person_identities (
    identity_id TEXT PRIMARY KEY REFERENCES identities (id),
    person_id INTEGER NOT NULL REFERENCES people (id) ON DELETE CASCADE,
    note TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX person_identities_by_person ON person_identities (person_id);

-- Nick changes made by someone else at the keyboard. The session ends at the
-- line, and the new nick starts a session linked to nothing else.
CREATE TABLE session_cuts (
    id INTEGER PRIMARY KEY,
    -- The log file, as in files.path.
    path TEXT NOT NULL,
    line INTEGER NOT NULL,
    -- The nick change on that line, such as 'alice is now known as bob'. A cut
    -- whose line no longer says this is stale and is not applied.
    expected TEXT NOT NULL,
    note TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (path, line)
);
";

const SCHEMA_V4: &str = "
-- Messages and actions in the channel while each session was present, its
-- own included.
ALTER TABLE sessions ADD COLUMN heard INTEGER NOT NULL DEFAULT 0;

-- Sessions are built again, with heard lines and activity, by the next
-- wot analyze.
DELETE FROM sessions;

-- Channel activity of each identity by channel and month, rebuilt with the
-- identities. The channel '*' holds all channels together. The logging
-- client and private messages are left out.
CREATE TABLE activity (
    identity_id TEXT NOT NULL,
    tag TEXT NOT NULL,
    channel TEXT NOT NULL,
    -- YYYY-MM.
    month TEXT NOT NULL,
    -- Days with a session in the channel.
    days_present INTEGER NOT NULL,
    -- Days with at least one message or action.
    active_days INTEGER NOT NULL,
    minutes INTEGER NOT NULL,
    -- Messages and actions in the channel while present, its own included.
    heard INTEGER NOT NULL,
    -- Its own messages and actions.
    spoken INTEGER NOT NULL,
    PRIMARY KEY (identity_id, channel, month)
);

CREATE INDEX activity_by_channel ON activity (channel, tag, month);

-- What each person record stands for: a human, or a bot. A bot's identities
-- are left out of activity rankings unless they ask for bots.
ALTER TABLE people ADD COLUMN kind TEXT NOT NULL DEFAULT 'person'
    CHECK (kind IN ('person', 'bot'));
";

const SCHEMA_V5: &str = "
-- How well you know each person, from distrusted to known. Bots keep the
-- default and are not rated.
ALTER TABLE people ADD COLUMN rating TEXT NOT NULL DEFAULT 'neutral'
    CHECK (rating IN ('distrusted', 'wary', 'neutral', 'vouched', 'known'));

-- The person who invited or vouches for this one.
ALTER TABLE people ADD COLUMN invited_by INTEGER REFERENCES people (id) ON DELETE SET NULL;

-- Notes about a person or bot, such as how you know them.
CREATE TABLE person_notes (
    id INTEGER PRIMARY KEY,
    person_id INTEGER NOT NULL REFERENCES people (id) ON DELETE CASCADE,
    text TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX person_notes_by_person ON person_notes (person_id);

-- Each time a channel's users were reviewed. A review stays valid for six
-- months.
CREATE TABLE channel_reviews (
    id INTEGER PRIMARY KEY,
    tag TEXT NOT NULL,
    channel TEXT NOT NULL COLLATE NOCASE,
    reviewed_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX channel_reviews_by_channel ON channel_reviews (tag, channel);

-- A channel's users as irssi's /who listed them, pasted into wot review.
CREATE TABLE who_lists (
    id INTEGER PRIMARY KEY,
    tag TEXT NOT NULL,
    channel TEXT NOT NULL COLLATE NOCASE,
    pasted_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX who_lists_by_channel ON who_lists (tag, channel);

CREATE TABLE who_entries (
    list_id INTEGER NOT NULL REFERENCES who_lists (id) ON DELETE CASCADE,
    nick TEXT NOT NULL,
    ident TEXT NOT NULL,
    host TEXT NOT NULL,
    -- 'H' when here or 'G' when away, then '*' for an IRC operator, channel
    -- modes such as '@' and '+', and any flags the server adds.
    status TEXT NOT NULL,
    -- The real name, which the logs never show.
    realname TEXT NOT NULL,
    PRIMARY KEY (list_id, nick)
);
";

const SCHEMA_V6: &str = "
-- The lines over which a session held each of its nicks, for sessions that
-- spoke, apart from the logging client's. A message on these lines by that
-- nick was the session's. Rebuilt with the sessions.
CREATE TABLE session_nicks (
    session_id INTEGER NOT NULL,
    -- Lowercased.
    nick TEXT NOT NULL,
    from_line INTEGER NOT NULL,
    -- NULL when the session ran to the end of the file.
    to_line INTEGER
);

CREATE INDEX session_nicks_by_session ON session_nicks (session_id);

-- Finding the lines of a session.
CREATE INDEX events_by_file_line ON events (file_id, line);

-- Sessions are built again, with their nick spans, by the next wot analyze.
DELETE FROM sessions;
";

const SCHEMA_V7: &str = "
-- Totals for the overview, rebuilt with the sessions, so the page reads a
-- row per file and channel instead of adding up every session. Until the
-- next rebuild fills them, wot analyze rebuilds on its next run.
CREATE TABLE file_totals (
    file_id INTEGER PRIMARY KEY,
    spoken INTEGER NOT NULL,
    joins INTEGER NOT NULL,
    -- YYYY-MM-DD HH:MM, of the first session and the end of the last.
    first_at TEXT NOT NULL,
    last_at TEXT NOT NULL
);

-- Identities with a session in each channel, apart from the logging client,
-- and those of them that spoke.
CREATE TABLE channel_totals (
    tag TEXT NOT NULL,
    channel TEXT NOT NULL,
    identities INTEGER NOT NULL,
    speakers INTEGER NOT NULL,
    PRIMARY KEY (tag, channel)
);

-- The activity ranking reads every column it needs from this index, in the
-- order it groups them, instead of looking up each row.
DROP INDEX activity_by_channel;
CREATE INDEX activity_ranking ON activity
    (channel, tag, identity_id, month, days_present, active_days, minutes, heard, spoken);
";

const SCHEMA_V8: &str = "
-- Network tags left out of imports, such as large public networks the
-- project does not need. Their files are never read, and ignoring a tag
-- deletes what was imported from it.
CREATE TABLE ignored_tags (
    tag TEXT PRIMARY KEY COLLATE NOCASE,
    note TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
";

const SCHEMA_V9: &str = "
-- The network tag folder each file was found in. `tag` is the network the
-- file belongs to: the folder tag itself, or the name of the network it was
-- linked into.
ALTER TABLE files ADD COLUMN folder_tag TEXT NOT NULL DEFAULT '';
UPDATE files SET folder_tag = tag;

-- Folder tags that are one network, such as a server's old and new
-- hostnames, or a bouncer's and a client's, linked under one name.
CREATE TABLE networks (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE,
    note TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- The folder tags in each network, each in at most one.
CREATE TABLE network_tags (
    tag TEXT PRIMARY KEY COLLATE NOCASE,
    network_id INTEGER NOT NULL REFERENCES networks (id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Second copies of lines logged more than once, such as by a client and a
-- bouncer in the same channel. Rebuilt with the sessions; everything counted
-- from the logs leaves them out.
CREATE TABLE duplicate_lines (
    event_id INTEGER PRIMARY KEY
);

-- Lines of each channel that were second copies.
ALTER TABLE channel_totals ADD COLUMN duplicates INTEGER NOT NULL DEFAULT 0;

-- Without totals, the next wot analyze rebuilds, which finds the copies.
DELETE FROM file_totals;
";

const SCHEMA_V10: &str = "
-- Folder tags logged through a bouncer, such as ZNC, marked by hand. Only
-- their logs are read for the bouncer's playback, and their copies of lines
-- logged twice are the ones left out.
CREATE TABLE bouncer_tags (
    tag TEXT PRIMARY KEY COLLATE NOCASE,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Lines left out of every count, rebuilt with the sessions: second copies of
-- lines logged twice, with the copy kept, and a bouncer's own lines.
DROP TABLE duplicate_lines;
CREATE TABLE left_out_lines (
    event_id INTEGER PRIMARY KEY,
    -- 'duplicate' or 'bouncer'.
    reason TEXT NOT NULL CHECK (reason IN ('duplicate', 'bouncer')),
    -- For a duplicate, the copy that was kept.
    kept_id INTEGER
);

-- Lines a bouncer replayed from its buffer, between its playback markers.
CREATE TABLE replayed_lines (
    event_id INTEGER PRIMARY KEY
);

-- Without totals, the next wot analyze rebuilds with the bouncer's markers.
DELETE FROM file_totals;
";

const SCHEMA_V11: &str = "
-- The logging client's own sessions now count like anyone else's, in
-- activity, nick spans and channel totals. Without totals, the next
-- wot analyze rebuilds them.
DELETE FROM file_totals;
";

const SCHEMA_V12: &str = "
-- Spoken lines by file and nick, so finding what a session said reads only
-- its own lines, not every line it heard. Nicks compare ignoring case, as
-- lower() does.
CREATE INDEX events_spoken ON events (file_id, nick COLLATE NOCASE, line)
    WHERE kind IN ('message', 'action');

-- An identity's nicks, and the identities seen on a host, for the pages that
-- look these up once per row and for checking a channel's users.
CREATE INDEX identity_keys_by_identity ON identity_keys (identity_id, nick);
CREATE INDEX identity_keys_by_host ON identity_keys (tag, host COLLATE NOCASE, ident);

-- Spoken lines and distinct nicks by channel and day, and distinct nicks by
-- channel, rebuilt with the sessions, so a channel's page reads a row a day
-- instead of every line.
CREATE TABLE channel_days (
    tag TEXT NOT NULL,
    channel TEXT NOT NULL,
    -- YYYY-MM-DD.
    date TEXT NOT NULL,
    spoken INTEGER NOT NULL,
    nicks INTEGER NOT NULL,
    PRIMARY KEY (tag, channel, date)
) WITHOUT ROWID;
ALTER TABLE channel_totals ADD COLUMN nicks INTEGER NOT NULL DEFAULT 0;

-- Without totals, the next wot analyze rebuilds and fills channel_days.
DELETE FROM file_totals;
";

const INSERT_EVENT: &str = "
INSERT INTO events (file_id, line, date, time, kind, nick, ident, host, other_nick, text, detail)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
";

/// Opens the database, creating it when the file is new and adding any tables
/// an older database is missing.
pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;

    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let latest = MIGRATIONS.len() as i64;
    if !(0..=latest).contains(&version) {
        return Err(format!(
            "it uses database schema version {version}, but this version of wot only knows \
             up to {latest}. It holds your decisions and notes, so use a newer wot rather than \
             deleting it, or pass a different --db"
        )
        .into());
    }
    if version > 0 && version < latest {
        // Some steps index or empty large tables, which takes a while.
        eprintln!(
            "Upgrading the database from schema version {version} to {latest}, which can take \
             a minute for a large one..."
        );
    }
    for (step, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let version = step + 1;
        conn.execute_batch(&format!(
            "BEGIN; {sql} PRAGMA user_version = {version}; COMMIT;"
        ))?;
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

/// Imports quietly, for tests.
#[cfg(test)]
pub fn import(conn: &mut Connection, files: &[LogFile]) -> ImportSummary {
    import_with_progress(conn, files, &mut Progress::quiet())
}

/// Lines between progress reports while a file is read.
const PROGRESS_LINES: u64 = 10_000;

/// Imports the lines added to each file since the last import, reporting
/// progress by bytes as it goes. Each file is imported in its own
/// transaction, so an interrupted import resumes cleanly.
pub fn import_with_progress(
    conn: &mut Connection,
    files: &[LogFile],
    progress: &mut Progress,
) -> ImportSummary {
    let mut summary = ImportSummary::default();
    // New files go into the network their folder tag is linked into.
    let networks = match network_names(conn) {
        Ok(networks) => networks,
        Err(err) => {
            summary
                .errors
                .push((PathBuf::new(), format!("reading networks: {err}")));
            return summary;
        }
    };
    let total: u64 = files.iter().map(|file| file.bytes).sum();
    progress.stage("Importing log files", total, Unit::Bytes);
    let mut finished: u64 = 0;
    for (index, file) in files.iter().enumerate() {
        let lines_before = summary.lines;
        let mut tick = |offset: i64, lines: u64| {
            progress.update(finished + offset.max(0) as u64, || {
                format!(
                    "file {} of {}, {} new lines",
                    thousands(index as u64 + 1),
                    thousands(files.len() as u64),
                    thousands(lines_before + lines)
                )
            });
        };
        let tag = networks
            .get(&file.tag.to_lowercase())
            .map_or(file.tag.as_str(), String::as_str);
        let result = import_file(conn, file, tag, &mut tick);
        finished += file.bytes;
        if let Ok((_, lines)) = &result {
            let lines = lines_before + lines;
            progress.update(finished, || {
                format!(
                    "file {} of {}, {} new lines",
                    thousands(index as u64 + 1),
                    thousands(files.len() as u64),
                    thousands(lines)
                )
            });
        }
        match result {
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
    progress.done(&format!("checked {} files", thousands(files.len() as u64)));
    summary
}

/// The network each linked folder tag belongs to, by lowercased folder tag.
pub fn network_names(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare(
        "SELECT lower(t.tag), n.name FROM network_tags t JOIN networks n ON n.id = t.network_id",
    )?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Imports one file under the network tag `tag` and returns what happened
/// and how many lines were read. `tick` hears the offset reached and the
/// lines read so far, now and then.
fn import_file(
    conn: &mut Connection,
    file: &LogFile,
    tag: &str,
    tick: &mut dyn FnMut(i64, u64),
) -> Result<(Outcome, u64)> {
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
                "INSERT INTO files (path, tag, folder_tag, target, kind) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![path, tag, file.tag, file.target, kind_name(file.kind)],
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
            if lines % PROGRESS_LINES == 0 {
                tick(offset, lines);
            }

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
    fn upgrades_an_older_database() {
        let root = TempDir::new("db-upgrade");
        let path = root.0.join("wot.sqlite");
        Connection::open(&path)
            .unwrap()
            .execute_batch(&format!("{SCHEMA_V1} PRAGMA user_version = 1;"))
            .unwrap();

        let conn = open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
        let tables: i64 = conn
            .query_row(
                "SELECT (SELECT count(*) FROM sessions) + (SELECT count(*) FROM host_groups)
                        + (SELECT count(*) FROM people) + (SELECT count(*) FROM session_cuts)
                        + (SELECT count(*) FROM activity)
                        + (SELECT count(*) FROM people WHERE kind <> 'person')
                        + (SELECT count(*) FROM person_notes)
                        + (SELECT count(*) FROM channel_reviews)
                        + (SELECT count(*) FROM who_lists) + (SELECT count(*) FROM who_entries)
                        + (SELECT count(*) FROM session_nicks)
                        + (SELECT count(*) FROM file_totals) + (SELECT count(*) FROM channel_totals)
                        + (SELECT count(*) FROM ignored_tags)
                        + (SELECT count(*) FROM networks) + (SELECT count(*) FROM network_tags)
                        + (SELECT count(*) FROM left_out_lines)
                        + (SELECT count(*) FROM replayed_lines) + (SELECT count(*) FROM bouncer_tags)
                        + (SELECT count(*) FROM channel_days)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
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
