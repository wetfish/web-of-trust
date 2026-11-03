//! Builds sessions and identities from the imported events.
//!
//! A session is one continuous stretch of a nick being present in one log
//! file: from a join, or from when logging started if the nick was already
//! there, until a part, quit, kick or the log closing. A nick change continues
//! the same session under the new nick. Sessions record how long someone was
//! present and how much they spoke, which later reports use to judge trust.
//!
//! Sessions are grouped into identities, one per person as far as the logs
//! can tell:
//!
//! - Sessions that showed `ident@host` (from a join, part or quit line) are
//!   linked when they share a nick and `ident@host`. A nick change inside a
//!   session links its nicks too. These sessions have the link `host`.
//! - Sessions that never showed a host are attributed to the identity of the
//!   nearest session in time that used the same nick with a host, preferring
//!   an earlier one. These have the link `inferred`, so they can be audited.
//! - A nick that never appears with a host gets its own identity, with the
//!   link `nick_only`.
//!
//! Two kinds of decisions made by hand change this:
//!
//! - Hosts in the same host group count as one host. When the group's
//!   `ident_is_user` is on, sessions with the same ident anywhere in the group
//!   are linked whatever their nick, since bridges and bouncers give each
//!   person their own ident.
//! - A cut on a nick change line ends the session there. The new nick starts
//!   a session with the link `cut`, which is linked to nothing else and keeps
//!   no host, so someone using another person's connection is kept apart.
//!
//! Everything is rebuilt from the events on each run. Identity ids are random,
//! and kept stable through `identity_keys`, which records which nick and
//! `ident@host` combinations belong to which id. When two identities merge,
//! the older id wins and the other records `merged_into`.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, TimeDelta};
use rusqlite::{Connection, params};

use crate::activity;
use crate::db::Result;
use crate::progress::{Progress, Unit};
use crate::report::thousands;

/// The event kinds that open, change or close sessions.
const SESSION_KINDS: &str = "'log_opened', 'log_closed', 'join', 'part', 'quit', 'kick', \
                             'nick_change', 'own_nick_change', 'client', 'message', 'action'";

#[derive(Default)]
pub struct RebuildSummary {
    pub sessions: usize,
    /// Identities with at least one session.
    pub identities: usize,
    pub new_identities: usize,
    pub merged_identities: usize,
    pub inferred_sessions: usize,
    pub nick_only_sessions: usize,
    /// Sessions started by a cut.
    pub cut_sessions: usize,
    pub cuts_applied: usize,
    /// Cuts whose line no longer holds the nick change they were made on.
    pub cuts_stale: usize,
}

/// The text of a nick change line, as irssi writes it: `alice is now known as
/// bob`, or `You're now known as bob` for the logging client. Cuts store this
/// to notice when their line no longer holds the same nick change.
pub fn nick_change_text(kind: &str, nick: Option<&str>, new: Option<&str>) -> Option<String> {
    match kind {
        "nick_change" => Some(format!("{} is now known as {}", nick?, new?)),
        "own_nick_change" => Some(format!("You're now known as {}", new?)),
        _ => None,
    }
}

/// Decisions made by hand that change how sessions are built and linked.
struct Rules {
    /// By tag and ASCII-lowercased host.
    groups: HashMap<(String, String), GroupRule>,
    cuts: Cuts,
}

#[derive(Clone, Copy)]
struct GroupRule {
    id: i64,
    ident_is_user: bool,
}

struct Cuts {
    /// By file id and line: the cut's id and the nick change it expects.
    at: HashMap<(i64, i64), (i64, String)>,
    applied: HashSet<i64>,
    total: usize,
}

impl Cuts {
    /// The cut on this event's line, if there is one and the line still holds
    /// the nick change it was made on.
    fn take(&mut self, event: &Row) -> Option<i64> {
        let (id, expected) = self.at.get(&(event.file_id, event.line))?;
        let actual = nick_change_text(
            &event.kind,
            event.nick.as_deref(),
            event.other_nick.as_deref(),
        )?;
        if *expected != actual {
            return None;
        }
        let id = *id;
        self.applied.insert(id);
        Some(id)
    }
}

fn load_rules(conn: &Connection) -> Result<Rules> {
    let groups = {
        let mut stmt = conn.prepare(
            "SELECT m.tag, lower(m.host), g.id, g.ident_is_user
             FROM host_group_members m JOIN host_groups g ON g.id = m.group_id",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    (row.get(0)?, row.get(1)?),
                    GroupRule {
                        id: row.get(2)?,
                        ident_is_user: row.get(3)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    let at = {
        let mut stmt = conn.prepare(
            "SELECT f.id, c.line, c.id, c.expected
             FROM session_cuts c JOIN files f ON f.path = c.path",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(((row.get(0)?, row.get(1)?), (row.get(2)?, row.get(3)?)))
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    let total: i64 = conn.query_row("SELECT count(*) FROM session_cuts", [], |row| row.get(0))?;
    Ok(Rules {
        groups,
        cuts: Cuts {
            at,
            applied: HashSet::new(),
            total: total as usize,
        },
    })
}

/// The stand-in host that joins the hosts of a group, and the one that keeps
/// the sessions after a cut together. Real hosts never contain `#`.
fn group_host(id: i64) -> String {
    format!("#group:{id}")
}

fn cut_host(id: i64) -> String {
    format!("#cut:{id}")
}

/// Whether sessions have been built at all, for example after upgrading a
/// database that predates them.
pub fn is_built(conn: &Connection) -> Result<bool> {
    // The totals are filled with the sessions; a database upgraded since the
    // last rebuild has sessions but no totals yet.
    let built = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sessions) AND EXISTS (SELECT 1 FROM file_totals)",
        [],
        |row| row.get(0),
    )?;
    Ok(built)
}

/// Rebuilds all sessions and identities from the events, in one transaction,
/// quietly, for tests.
#[cfg(test)]
pub fn rebuild(conn: &mut Connection) -> Result<RebuildSummary> {
    rebuild_with_progress(conn, &mut Progress::quiet())
}

/// Events between progress reports while events are replayed, and sessions
/// between reports while they are saved.
const PROGRESS_EVENTS: u64 = 100_000;
const PROGRESS_SESSIONS: usize = 10_000;

/// Rebuilds all sessions and identities, reporting each stage as it goes.
pub fn rebuild_with_progress(
    conn: &mut Connection,
    progress: &mut Progress,
) -> Result<RebuildSummary> {
    let tx = conn.transaction()?;
    let mut rules = load_rules(&tx)?;
    let (sessions, day_marks) = replay_events(&tx, &mut rules.cuts, progress)?;

    let mut summary = RebuildSummary {
        sessions: sessions.len(),
        cuts_applied: rules.cuts.applied.len(),
        cuts_stale: rules.cuts.total.saturating_sub(rules.cuts.applied.len()),
        ..RebuildSummary::default()
    };
    progress.stage(
        "Linking sessions into identities",
        0,
        Unit::Items("sessions"),
    );
    let links = link_sessions(&sessions, &rules.groups);
    let ids = assign_ids(&tx, &links, &mut summary)?;
    progress.done(&format!(
        "{} identities, {} of them new",
        thousands(summary.identities as u64),
        thousands(summary.new_identities as u64)
    ));

    progress.stage(
        "Saving sessions",
        sessions.len() as u64,
        Unit::Items("sessions"),
    );

    tx.execute("DELETE FROM sessions", [])?;
    tx.execute("DELETE FROM session_nicks", [])?;
    // The identity of each session, for its activity.
    let mut identity_of: Vec<&str> = Vec::with_capacity(sessions.len());
    {
        let mut insert_span = tx.prepare(
            "INSERT INTO session_nicks (session_id, nick, from_line, to_line)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        let mut insert = tx.prepare(
            "INSERT INTO sessions (id, file_id, identity_id, link, inferred_from, nicks, ident,
                 host, start_at, start_line, joined, end_at, end_line, end_reason, minutes,
                 spoken, logger, heard)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                     ?18)",
        )?;
        for (index, session) in sessions.iter().enumerate() {
            if index % PROGRESS_SESSIONS == 0 {
                progress.update(index as u64, String::new);
            }
            let (identity, link, inferred_from) = match links.of_session[index] {
                Link::Host(node) => (&ids[&links.root(node)], "host", None),
                Link::Inferred(target) => {
                    let Link::Host(node) = links.of_session[target] else {
                        unreachable!("sessions are only inferred from sessions with a host");
                    };
                    summary.inferred_sessions += 1;
                    (
                        &ids[&links.root(node)],
                        "inferred",
                        Some(session_id(target)),
                    )
                }
                Link::NickOnly(node) => {
                    summary.nick_only_sessions += 1;
                    (&ids[&links.root(node)], "nick_only", None)
                }
                Link::Cut(node) => {
                    summary.cut_sessions += 1;
                    (&ids[&links.root(node)], "cut", None)
                }
            };
            let minutes = session
                .end
                .signed_duration_since(session.start)
                .num_minutes()
                .max(0);
            insert.execute(params![
                session_id(index),
                session.file_id,
                identity,
                link,
                inferred_from,
                session.nicks.join(" "),
                session.ident,
                session.host,
                format_at(session.start),
                session.start_line,
                session.joined,
                format_at(session.end),
                session.end_line,
                session.end_reason,
                minutes,
                session.spoken,
                session.logger,
                session.heard_to - session.heard_from,
            ])?;
            identity_of.push(identity.as_str());
            if session.spoken > 0 && !session.logger {
                for (nick, from, to) in nick_spans(session) {
                    insert_span.execute(params![session_id(index), nick, from, to])?;
                }
            }
        }
    }

    save_totals(&tx, &sessions, &identity_of)?;
    progress.done(&format!("{} sessions", thousands(sessions.len() as u64)));

    // Channel activity, leaving out private messages and the logging client.
    let presences: Vec<activity::Presence> = sessions
        .iter()
        .zip(identity_of.iter().copied())
        .filter(|(session, _)| session.channel && !session.logger)
        .map(|(session, identity)| activity::Presence {
            identity,
            tag: &session.tag,
            channel: &session.target,
            start: session.start,
            end: session.end,
            heard_from: session.heard_from,
            heard_to: session.heard_to,
            day_marks: day_marks
                .get(&session.file_id)
                .map_or(&[][..], Vec::as_slice),
            spoken_days: &session.spoken_days,
        })
        .collect();
    progress.stage("Building channel activity", 0, Unit::Items("sessions"));
    activity::rebuild(&tx, &presences)?;
    progress.done(&format!(
        "from {} channel sessions",
        thousands(presences.len() as u64)
    ));

    progress.stage("Writing to the database", 0, Unit::Bytes);
    tx.commit()?;
    progress.done("saved");
    Ok(summary)
}

/// Saves the totals the overview shows: per file, its spoken lines, joins and
/// first and last session; per channel, its identities and those that spoke,
/// apart from the logging client.
fn save_totals(tx: &Connection, sessions: &[Session], identity_of: &[&str]) -> Result<()> {
    tx.execute("DELETE FROM file_totals", [])?;
    tx.execute("DELETE FROM channel_totals", [])?;

    let mut files: HashMap<i64, (i64, i64, NaiveDateTime, NaiveDateTime)> = HashMap::new();
    type People<'a> = (HashSet<&'a str>, HashSet<&'a str>);
    let mut channels: HashMap<(&str, &str), People<'_>> = HashMap::new();
    for (session, identity) in sessions.iter().zip(identity_of.iter().copied()) {
        let file = files
            .entry(session.file_id)
            .or_insert((0, 0, session.start, session.end));
        file.0 += session.spoken;
        file.1 += i64::from(session.joined);
        file.2 = file.2.min(session.start);
        file.3 = file.3.max(session.end);
        if session.channel && !session.logger {
            let people = channels
                .entry((session.tag.as_str(), session.target.as_str()))
                .or_default();
            people.0.insert(identity);
            if session.spoken > 0 {
                people.1.insert(identity);
            }
        }
    }

    let mut insert = tx.prepare(
        "INSERT INTO file_totals (file_id, spoken, joins, first_at, last_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (file_id, (spoken, joins, first, last)) in files {
        insert.execute(params![
            file_id,
            spoken,
            joins,
            format_at(first),
            format_at(last)
        ])?;
    }
    let mut insert = tx.prepare(
        "INSERT OR REPLACE INTO channel_totals (tag, channel, identities, speakers)
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    for ((tag, channel), (identities, speakers)) in channels {
        insert.execute(params![
            tag,
            channel,
            identities.len() as i64,
            speakers.len() as i64
        ])?;
    }
    Ok(())
}

/// The lines over which a session held each of its nicks: the lowercased
/// nick, the first line and the last, which is open when the session ran to
/// the end of the file. A message on these lines by that nick was the
/// session's.
fn nick_spans(session: &Session) -> Vec<(String, i64, Option<i64>)> {
    session
        .nicks
        .iter()
        .enumerate()
        .map(|(i, nick)| {
            let to = match session.nick_lines.get(i + 1) {
                Some(next) => Some(next - 1),
                None => session.end_line,
            };
            (nick.to_lowercase(), session.nick_lines[i], to)
        })
        .collect()
}

/// Session row ids are their position in the rebuilt list, starting at 1.
fn session_id(index: usize) -> i64 {
    index as i64 + 1
}

fn format_at(at: NaiveDateTime) -> String {
    at.format("%Y-%m-%d %H:%M").to_string()
}

struct Session {
    file_id: i64,
    tag: String,
    /// In the order the session used them, as written in the log.
    nicks: Vec<String>,
    /// The line from which the session held each of `nicks`.
    nick_lines: Vec<i64>,
    ident: Option<String>,
    host: Option<String>,
    start: NaiveDateTime,
    /// The line of the join, or the first line where the nick appeared.
    start_line: i64,
    /// Whether the session started with a join line. When it did not, the
    /// nick was already present and `start` is when logging started.
    joined: bool,
    end: NaiveDateTime,
    /// The line that ended the session, if one did.
    end_line: Option<i64>,
    end_reason: &'static str,
    /// Messages and actions.
    spoken: i64,
    /// The logging client's own session.
    logger: bool,
    /// The cut that started this session, if one did.
    cut: Option<i64>,
    /// Whether the file is a channel log rather than a private message log.
    channel: bool,
    /// The channel or private message target.
    target: String,
    /// The file's count of spoken lines when the session started and ended,
    /// so the session heard the lines in between, its own included.
    heard_from: i64,
    heard_to: i64,
    /// Messages and actions by day.
    spoken_days: BTreeMap<NaiveDate, i64>,
}

/// One event, as read back from the database.
struct Row {
    file_id: i64,
    line: i64,
    at: NaiveDateTime,
    kind: String,
    nick: Option<String>,
    ident: Option<String>,
    host: Option<String>,
    other_nick: Option<String>,
    text: Option<String>,
    detail: Option<String>,
}

/// For each file, the count of spoken lines at the start of each day.
type DayMarks = HashMap<i64, Vec<(NaiveDate, i64)>>;

/// Replays every file's events in line order and returns their sessions, and
/// each file's day marks.
fn replay_events(
    conn: &Connection,
    cuts: &mut Cuts,
    progress: &mut Progress,
) -> Result<(Vec<Session>, DayMarks)> {
    let files: HashMap<i64, (String, bool, String)> = {
        let mut stmt = conn.prepare("SELECT id, tag, kind = 'channel', target FROM files")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get(0)?, (row.get(1)?, row.get(2)?, row.get(3)?)))
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    let mut day_marks: DayMarks = HashMap::new();
    // Progress is counted in files, which are replayed one after another,
    // with the events read so far as detail.
    progress.stage("Reading events", files.len() as u64, Unit::Items("files"));
    let mut files_done: u64 = 0;
    let mut events: u64 = 0;

    let mut stmt = conn.prepare(&format!(
        "SELECT file_id, line, date, time, kind, nick, ident, host, other_nick, text, detail
         FROM events
         WHERE kind IN ({SESSION_KINDS}) AND date IS NOT NULL AND time IS NOT NULL
         ORDER BY file_id, line"
    ))?;
    let mut rows = stmt.query([])?;

    let mut sessions = Vec::new();
    let mut replay: Option<FileReplay> = None;
    while let Some(row) = rows.next()? {
        events += 1;
        if events % PROGRESS_EVENTS == 0 {
            let found = sessions.len();
            progress.update(files_done, || {
                format!(
                    "{} events, {} sessions",
                    thousands(events),
                    thousands(found as u64)
                )
            });
        }
        let date: String = row.get(2)?;
        let time: String = row.get(3)?;
        let Some(at) = parse_at(&date, &time) else {
            continue;
        };
        let event = Row {
            file_id: row.get(0)?,
            line: row.get(1)?,
            at,
            kind: row.get(4)?,
            nick: row.get(5)?,
            ident: row.get(6)?,
            host: row.get(7)?,
            other_nick: row.get(8)?,
            text: row.get(9)?,
            detail: row.get(10)?,
        };

        if replay.as_ref().is_none_or(|r| r.file_id != event.file_id) {
            if let Some(finished) = replay.take() {
                let (file_id, marks) = finished.finish(&mut sessions);
                day_marks.insert(file_id, marks);
                files_done += 1;
            }
            let (tag, channel, target) = files.get(&event.file_id).cloned().unwrap_or_default();
            replay = Some(FileReplay::new(event.file_id, tag, channel, target));
        }
        if let Some(replay) = replay.as_mut() {
            replay.event(&event, &mut sessions, cuts);
        }
    }
    if let Some(finished) = replay.take() {
        let (file_id, marks) = finished.finish(&mut sessions);
        day_marks.insert(file_id, marks);
    }
    progress.done(&format!(
        "{} events in {} files, {} sessions",
        thousands(events),
        thousands(files.len() as u64),
        thousands(sessions.len() as u64)
    ));
    Ok((sessions, day_marks))
}

fn parse_at(date: &str, time: &str) -> Option<NaiveDateTime> {
    let date: NaiveDate = date.parse().ok()?;
    let time = NaiveTime::parse_from_str(time, "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(time, "%H:%M"))
        .ok()?;
    Some(date.and_time(time))
}

/// Session state while replaying one file.
struct FileReplay {
    file_id: i64,
    tag: String,
    /// Open sessions by lowercased nick, as indexes into the session list.
    open: HashMap<String, usize>,
    /// When logging last started in this file. Nicks first seen without a
    /// join line have been present since at least then.
    visible_since: Option<NaiveDateTime>,
    /// The time of the previous event.
    last_seen: Option<NaiveDateTime>,
    /// The most recent join, kept to recognize the logging client's own.
    last_join: Option<RecentJoin>,
    /// A join followed by irssi's names line, waiting for irssi's sync line
    /// to confirm it was the logging client's own.
    candidate: Option<RecentJoin>,
    /// The logging client's own session, once recognized.
    own: Option<usize>,
    /// Whether the file is a channel log, and its channel or nick.
    channel: bool,
    target: String,
    /// Messages and actions so far in the file.
    said: i64,
    /// `said` at the start of each day, in file order.
    day_marks: Vec<(NaiveDate, i64)>,
}

struct RecentJoin {
    line: i64,
    at: NaiveDateTime,
    session: usize,
    /// Lowercased.
    channel: String,
}

impl FileReplay {
    fn new(file_id: i64, tag: String, channel: bool, target: String) -> Self {
        FileReplay {
            file_id,
            tag,
            open: HashMap::new(),
            visible_since: None,
            last_seen: None,
            last_join: None,
            candidate: None,
            own: None,
            channel,
            target,
            said: 0,
            day_marks: Vec::new(),
        }
    }

    fn event(&mut self, event: &Row, sessions: &mut Vec<Session>, cuts: &mut Cuts) {
        let nick = event.nick.as_deref().unwrap_or_default();
        let date = event.at.date();
        if self.day_marks.last().is_none_or(|&(last, _)| last != date) {
            self.day_marks.push((date, self.said));
        }
        match event.kind.as_str() {
            "log_opened" => {
                // Without a close line, the logger's view ended at the last
                // line written before this one.
                let end = self.last_seen.unwrap_or(event.at);
                self.close_all(sessions, end, Some(event.line), "log_reopened");
                self.visible_since = Some(event.at);
            }
            "log_closed" => self.close_all(sessions, event.at, Some(event.line), "log_closed"),
            "join" => {
                if let Some(index) = self.open.remove(&nick.to_lowercase()) {
                    close(&mut sessions[index], event, "rejoined", self.said);
                }
                let index = self.start(sessions, nick, event, true);
                learn_host(&mut sessions[index], event);
                self.last_join = Some(RecentJoin {
                    line: event.line,
                    at: event.at,
                    session: index,
                    channel: event.detail.as_deref().unwrap_or_default().to_lowercase(),
                });
            }
            "message" | "action" => {
                let index = self.session_for(sessions, nick, event);
                self.said += 1;
                let session = &mut sessions[index];
                session.spoken += 1;
                *session.spoken_days.entry(date).or_default() += 1;
            }
            "part" | "quit" | "kick" => {
                let index = self.session_for(sessions, nick, event);
                learn_host(&mut sessions[index], event);
                self.open.remove(&nick.to_lowercase());
                let reason = match event.kind.as_str() {
                    "part" => "part",
                    "quit" => "quit",
                    _ => "kick",
                };
                close(&mut sessions[index], event, reason, self.said);
            }
            "nick_change" => {
                let new = event.other_nick.as_deref().unwrap_or_default();
                let index = self.session_for(sessions, nick, event);
                match cuts.take(event) {
                    Some(cut) => self.cut(sessions, index, nick, new, event, cut),
                    None => self.rename(sessions, index, nick, new, event),
                }
            }
            "own_nick_change" => {
                // Only the logging client's own session is renamed, and only
                // while it is still open.
                let new = event.other_nick.as_deref().unwrap_or_default();
                let own = self.own.and_then(|index| {
                    let current = sessions[index].nicks.last()?.to_lowercase();
                    (self.open.get(&current) == Some(&index)).then_some((index, current))
                });
                if let Some((index, current)) = own {
                    match cuts.take(event) {
                        Some(cut) => self.cut(sessions, index, &current, new, event, cut),
                        None => self.rename(sessions, index, &current, new, event),
                    }
                }
            }
            "client" => {
                let text = event.text.as_deref().unwrap_or_default().to_lowercase();
                self.recognize_own_join(&text, event, sessions);
            }
            _ => {}
        }
        self.last_seen = Some(event.at);
    }

    /// Recognizes the logging client's own join from two `Irssi:` lines that
    /// irssi only writes for its own joins:
    ///
    /// 1. `#channel: Total of N nicks ...` on the line right after the join,
    ///    within a minute, as soon as the server's names list arrives. irssi
    ///    also writes this for `/names` at any time, so on its own it is only
    ///    a candidate.
    /// 2. `Join to #channel was synced in N secs`, once irssi has finished
    ///    querying the channel. `/names` never writes this. The join must
    ///    have happened about N seconds earlier, within a minute.
    fn recognize_own_join(&mut self, text: &str, event: &Row, sessions: &mut [Session]) {
        if let Some(join) = self.last_join.take() {
            let total = format!("{}: total of ", join.channel);
            let minutes = event.at.signed_duration_since(join.at).num_minutes();
            if event.line == join.line + 1 && (0..=1).contains(&minutes) && text.starts_with(&total)
            {
                self.candidate = Some(join);
                return;
            }
            self.last_join = Some(join);
        }

        let Some((channel, secs)) = parse_synced(text) else {
            return;
        };
        let Some(join) = self.candidate.take() else {
            return;
        };
        if join.channel != channel {
            self.candidate = Some(join);
            return;
        }
        let Some(joined_at) =
            TimeDelta::try_seconds(secs).and_then(|secs| event.at.checked_sub_signed(secs))
        else {
            return;
        };
        let minutes = joined_at.signed_duration_since(join.at).num_minutes().abs();
        let index = join.session;
        let still_open = sessions[index]
            .nicks
            .last()
            .is_some_and(|nick| self.open.get(&nick.to_lowercase()) == Some(&index));
        if minutes <= 1 && still_open {
            sessions[index].logger = true;
            self.own = Some(index);
        }
    }

    /// Closes the sessions still open when the file ends, and gives the
    /// file's day marks.
    fn finish(mut self, sessions: &mut [Session]) -> (i64, Vec<(NaiveDate, i64)>) {
        if let Some(end) = self.last_seen {
            self.close_all(sessions, end, None, "end_of_log");
        }
        (self.file_id, self.day_marks)
    }

    /// The open session for a nick, or a new one if the nick was already
    /// present when logging started.
    fn session_for(&mut self, sessions: &mut Vec<Session>, nick: &str, event: &Row) -> usize {
        match self.open.get(&nick.to_lowercase()) {
            Some(&index) => index,
            None => self.start(sessions, nick, event, false),
        }
    }

    fn start(
        &mut self,
        sessions: &mut Vec<Session>,
        nick: &str,
        event: &Row,
        joined: bool,
    ) -> usize {
        let start = if joined {
            event.at
        } else {
            self.visible_since.unwrap_or(event.at)
        };
        sessions.push(Session {
            file_id: self.file_id,
            tag: self.tag.clone(),
            nicks: vec![nick.to_string()],
            nick_lines: vec![event.line],
            ident: None,
            host: None,
            start,
            start_line: event.line,
            joined,
            end: event.at,
            end_line: None,
            end_reason: "end_of_log",
            spoken: 0,
            logger: false,
            cut: None,
            channel: self.channel,
            target: self.target.clone(),
            heard_from: self.said,
            heard_to: self.said,
            spoken_days: BTreeMap::new(),
        });
        let index = sessions.len() - 1;
        self.open.insert(nick.to_lowercase(), index);
        index
    }

    /// Ends a session at a nick change marked by a cut, and starts a new
    /// session under the new nick that carries the cut instead of a host.
    fn cut(
        &mut self,
        sessions: &mut Vec<Session>,
        index: usize,
        old: &str,
        new: &str,
        event: &Row,
        cut: i64,
    ) {
        self.open.remove(&old.to_lowercase());
        close(&mut sessions[index], event, "cut", self.said);
        if let Some(stale) = self.open.remove(&new.to_lowercase()) {
            close(&mut sessions[stale], event, "nick_taken", self.said);
        }
        let logger = sessions[index].logger;
        // Started as if joined, so it starts at the cut rather than when
        // logging started.
        let next = self.start(sessions, new, event, true);
        let session = &mut sessions[next];
        session.joined = false;
        session.cut = Some(cut);
        session.logger = logger;
        if self.own == Some(index) {
            self.own = Some(next);
        }
    }

    /// Moves a session from its old nick to a new one.
    fn rename(
        &mut self,
        sessions: &mut [Session],
        index: usize,
        old: &str,
        new: &str,
        event: &Row,
    ) {
        self.open.remove(&old.to_lowercase());
        if let Some(stale) = self.open.remove(&new.to_lowercase()) {
            close(&mut sessions[stale], event, "nick_taken", self.said);
        }
        sessions[index].nicks.push(new.to_string());
        sessions[index].nick_lines.push(event.line);
        self.open.insert(new.to_lowercase(), index);
    }

    fn close_all(
        &mut self,
        sessions: &mut [Session],
        end: NaiveDateTime,
        line: Option<i64>,
        reason: &'static str,
    ) {
        for (_, index) in self.open.drain() {
            let session = &mut sessions[index];
            session.end = end;
            session.end_line = line;
            session.end_reason = reason;
            session.heard_to = self.said;
        }
        self.last_join = None;
        self.candidate = None;
    }
}

/// Reads irssi's `Join to #channel was synced in N secs` line, lowercased,
/// as the channel and N.
fn parse_synced(text: &str) -> Option<(&str, i64)> {
    let rest = text.strip_prefix("join to ")?;
    let (channel, rest) = rest.split_once(" was synced in ")?;
    let secs = rest.strip_suffix(" secs")?.parse().ok()?;
    Some((channel, secs))
}

/// Ends a session at an event. `said` is the file's count of spoken lines so
/// far, which ends what the session heard.
fn close(session: &mut Session, event: &Row, reason: &'static str, said: i64) {
    session.end = event.at;
    session.end_line = Some(event.line);
    session.end_reason = reason;
    session.heard_to = said;
}

/// Records the `ident@host` from a join, part or quit line, if the session
/// does not have one yet. Sessions started by a cut keep no host, since the
/// connection belongs to the person before the cut.
fn learn_host(session: &mut Session, event: &Row) {
    if session.host.is_none() && session.cut.is_none() {
        if let (Some(ident), Some(host)) = (&event.ident, &event.host) {
            session.ident = Some(ident.clone());
            session.host = Some(host.clone());
        }
    }
}

/// A network tag, lowercased nick, ident and host. Nick-only keys have an
/// empty ident and host. Keys for host groups and cuts use a stand-in host
/// starting with `#`.
type Key = (String, String, String, String);

#[derive(Clone, Copy)]
enum Link {
    /// The session showed a host. The node is one of its keys.
    Host(usize),
    /// Attributed to the identity of this other session, which has a host.
    Inferred(usize),
    /// The nick never appears with a host. The node is one of its keys.
    NickOnly(usize),
    /// Started by a cut. The node is one of its keys.
    Cut(usize),
}

/// Keys joined into clusters, one cluster per identity.
struct Links {
    keys: Vec<Key>,
    /// Whether each key is stored in `identity_keys`. Keys that only join a
    /// host group's hosts are not, since the real keys they join already
    /// keep the identity's id stable.
    stored: Vec<bool>,
    parent: Vec<usize>,
    /// One entry per session.
    of_session: Vec<Link>,
}

impl Links {
    fn node(&mut self, index: &mut HashMap<Key, usize>, key: Key, stored: bool) -> usize {
        *index.entry(key).or_insert_with_key(|key| {
            self.keys.push(key.clone());
            self.stored.push(stored);
            self.parent.push(self.parent.len());
            self.parent.len() - 1
        })
    }

    /// The node that represents a node's cluster. Fast after `flatten`.
    fn root(&self, mut node: usize) -> usize {
        while self.parent[node] != node {
            node = self.parent[node];
        }
        node
    }

    /// Like `root`, but shortens the path on the way, so that clusters built
    /// from many links stay quick to look up.
    fn find(&mut self, mut node: usize) -> usize {
        while self.parent[node] != node {
            self.parent[node] = self.parent[self.parent[node]];
            node = self.parent[node];
        }
        node
    }

    /// Joins the clusters of all the nodes and returns the first node.
    /// Every session has at least one nick, so `nodes` is never empty.
    fn union_all(&mut self, nodes: &[usize]) -> usize {
        let first = nodes[0];
        for &node in &nodes[1..] {
            let (a, b) = (self.find(first), self.find(node));
            if a != b {
                self.parent[b] = a;
            }
        }
        first
    }

    /// Points every node straight at its cluster's root.
    fn flatten(&mut self) {
        for node in 0..self.parent.len() {
            let root = self.find(node);
            self.parent[node] = root;
        }
    }
}

/// Links every session to a cluster of keys, following the rules in the
/// module documentation.
fn link_sessions(sessions: &[Session], groups: &HashMap<(String, String), GroupRule>) -> Links {
    let mut links = Links {
        keys: Vec::new(),
        stored: Vec::new(),
        parent: Vec::new(),
        of_session: Vec::with_capacity(sessions.len()),
    };
    let mut index: HashMap<Key, usize> = HashMap::new();
    let mut slots: Vec<Option<Link>> = vec![None; sessions.len()];

    // Sessions with a host, and for each tag and nick, when they started.
    let mut with_host: HashMap<(String, String), Vec<(NaiveDateTime, usize)>> = HashMap::new();
    for (i, session) in sessions.iter().enumerate() {
        let (Some(ident), Some(host)) = (&session.ident, &session.host) else {
            continue;
        };
        let tag = &session.tag;
        let group = groups.get(&(tag.clone(), host.to_ascii_lowercase()));
        let mut nodes = Vec::with_capacity(session.nicks.len() + 1);
        for nick in &session.nicks {
            let nick = nick.to_lowercase();
            let key = (tag.clone(), nick.clone(), ident.clone(), host.clone());
            nodes.push(links.node(&mut index, key, true));
            // In a group whose idents do not identify people, the same nick
            // and ident still link across the group's hosts.
            if let Some(group) = group.filter(|group| !group.ident_is_user) {
                let key = (
                    tag.clone(),
                    nick.clone(),
                    ident.clone(),
                    group_host(group.id),
                );
                nodes.push(links.node(&mut index, key, false));
            }
            with_host
                .entry((tag.clone(), nick))
                .or_default()
                .push((session.start, i));
        }
        // In a group whose idents identify people, the ident links across
        // the group's hosts whatever the nick.
        if let Some(group) = group.filter(|group| group.ident_is_user) {
            let key = (
                tag.clone(),
                String::new(),
                ident.clone(),
                group_host(group.id),
            );
            nodes.push(links.node(&mut index, key, false));
        }
        slots[i] = Some(Link::Host(links.union_all(&nodes)));
    }
    for starts in with_host.values_mut() {
        starts.sort();
    }

    // Sessions without a host.
    for (i, session) in sessions.iter().enumerate() {
        if slots[i].is_some() {
            continue;
        }
        if let Some(cut) = session.cut {
            let nodes: Vec<usize> = session
                .nicks
                .iter()
                .map(|nick| {
                    let key = (
                        session.tag.clone(),
                        nick.to_lowercase(),
                        String::new(),
                        cut_host(cut),
                    );
                    links.node(&mut index, key, true)
                })
                .collect();
            slots[i] = Some(Link::Cut(links.union_all(&nodes)));
            continue;
        }
        let nearest = session.nicks.iter().find_map(|nick| {
            let starts = with_host.get(&(session.tag.clone(), nick.to_lowercase()))?;
            let after = starts.partition_point(|&(start, _)| start <= session.start);
            let (_, target) = if after > 0 {
                starts[after - 1]
            } else {
                starts[0]
            };
            Some(target)
        });
        slots[i] = Some(match nearest {
            Some(target) => Link::Inferred(target),
            None => {
                let nodes: Vec<usize> = session
                    .nicks
                    .iter()
                    .map(|nick| {
                        let key = (
                            session.tag.clone(),
                            nick.to_lowercase(),
                            String::new(),
                            String::new(),
                        );
                        links.node(&mut index, key, true)
                    })
                    .collect();
                Link::NickOnly(links.union_all(&nodes))
            }
        });
    }

    links.flatten();
    links.of_session = slots
        .into_iter()
        .map(|slot| slot.expect("every session was linked"))
        .collect();
    links
}

/// Gives every cluster an identity id, reusing the ids its keys had before,
/// and stores the keys. Returns the id for each cluster's root node.
fn assign_ids(
    conn: &Connection,
    links: &Links,
    summary: &mut RebuildSummary,
) -> Result<HashMap<usize, String>> {
    let previous: HashMap<Key, String> = {
        let mut stmt =
            conn.prepare("SELECT tag, nick, ident, host, identity_id FROM identity_keys")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?),
                    row.get(4)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    let created: HashMap<String, String> = {
        let mut stmt = conn.prepare("SELECT id, created_at FROM identities")?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };

    let mut clusters: HashMap<usize, Vec<usize>> = HashMap::new();
    for node in 0..links.keys.len() {
        clusters.entry(links.root(node)).or_default().push(node);
    }
    // Larger clusters first, so if data was removed and an old identity is
    // now split in two, the larger part keeps its id.
    let mut clusters: Vec<(usize, Vec<usize>)> = clusters.into_iter().collect();
    clusters.sort_by_key(|(root, nodes)| (std::cmp::Reverse(nodes.len()), *root));

    let mut ids: HashMap<usize, String> = HashMap::new();
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (root, nodes) in &clusters {
        let mut candidates: Vec<&String> = nodes
            .iter()
            .filter_map(|&node| previous.get(&links.keys[node]))
            .filter(|id| !used.contains(*id))
            .collect();
        candidates.sort_by_key(|&id| (created.get(id), id));
        candidates.dedup();

        let id = match candidates.split_first() {
            Some((&winner, merged)) => {
                let mut merge =
                    conn.prepare_cached("UPDATE identities SET merged_into = ?1 WHERE id = ?2")?;
                for &loser in merged {
                    merge.execute(params![winner, loser])?;
                    summary.merged_identities += 1;
                }
                merge.execute(params![None::<String>, winner])?;
                winner.clone()
            }
            None => {
                summary.new_identities += 1;
                let tag = &links.keys[*root].0;
                conn.prepare_cached(
                    "INSERT INTO identities (id, tag) VALUES (lower(hex(randomblob(6))), ?1)
                     RETURNING id",
                )?
                .query_row(params![tag], |row| row.get(0))?
            }
        };
        used.insert(id.clone());
        ids.insert(*root, id);
    }
    summary.identities = ids.len();

    conn.execute("DELETE FROM identity_keys", [])?;
    let mut insert = conn.prepare(
        "INSERT INTO identity_keys (tag, nick, ident, host, identity_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (node, (tag, nick, ident, host)) in links.keys.iter().enumerate() {
        if links.stored[node] {
            insert.execute(params![tag, nick, ident, host, ids[&links.root(node)]])?;
        }
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::db;
    use crate::discover::{self, tests::TempDir};

    /// Writes log files under one tag, imports them and rebuilds identities.
    fn build(root: &TempDir, files: &[(&str, &str)]) -> (Connection, RebuildSummary) {
        let tag = root.0.join("wetfish");
        fs::create_dir_all(&tag).unwrap();
        for (name, text) in files {
            fs::write(tag.join(name), text).unwrap();
        }
        let mut conn = db::open(&root.0.join("wot.sqlite")).unwrap();
        let summary = rebuild_after_import(&root.0, &mut conn);
        (conn, summary)
    }

    fn rebuild_after_import(root: &Path, conn: &mut Connection) -> RebuildSummary {
        let inventory = discover::discover(&[root.to_path_buf()]).unwrap();
        let imported = db::import(conn, &inventory.files);
        assert!(imported.errors.is_empty(), "{:?}", imported.errors);
        rebuild(conn).unwrap()
    }

    /// "nicks ident@host start end reason spoken link logger" per session.
    fn sessions(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT nicks || ' ' || ifnull(ident || '@' || host, '-') || ' ' || start_at
                        || ' ' || end_at || ' ' || end_reason || ' ' || spoken || ' ' || link
                        || ' ' || logger
                 FROM sessions ORDER BY id",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap();
        rows
    }

    /// The identity id of each session, in session order.
    fn identity_ids(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT identity_id FROM sessions ORDER BY id")
            .unwrap();
        let rows = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap();
        rows
    }

    const OPEN: &str = "--- Log opened Sun Nov 01 01:00:00 2026\n";

    #[test]
    fn a_nick_change_continues_the_session() {
        let root = TempDir::new("id-nick-change");
        let log = format!(
            "{OPEN}\
             01:16 -!- tester [rachel@hostA] has joined #analysis\n\
             01:17 < tester> Helllo\n\
             01:23 -!- tester is now known as tester2\n\
             01:24 < tester2> now I'm tester2\n\
             01:30 -!- tester2 [rachel@hostA] has quit [bye]\n"
        );
        let (conn, summary) = build(&root, &[("#analysis.log", log.as_str())]);

        assert_eq!(
            sessions(&conn),
            ["tester tester2 rachel@hostA 2026-11-01 01:16 2026-11-01 01:30 quit 2 host 0"]
        );
        assert_eq!((summary.identities, summary.new_identities), (1, 1));
        let keys: i64 = conn
            .query_row("SELECT count(*) FROM identity_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(keys, 2);
    }

    #[test]
    fn learns_the_host_from_a_quit_and_starts_when_logging_started() {
        let root = TempDir::new("id-quit-host");
        let log = format!(
            "{OPEN}\
             01:10 <alice> I was already here\n\
             01:20 -!- alice [al@hostB] has quit [Quit: bye]\n\
             01:25 <bob> nobody saw me join\n\
             --- Log closed Sun Nov 01 02:00:00 2026\n"
        );
        let (conn, _) = build(&root, &[("#analysis.log", log.as_str())]);

        assert_eq!(
            sessions(&conn),
            [
                "alice al@hostB 2026-11-01 01:00 2026-11-01 01:20 quit 1 host 0",
                "bob - 2026-11-01 01:00 2026-11-01 02:00 log_closed 1 nick_only 0",
            ]
        );
    }

    #[test]
    fn links_the_same_nick_and_host_but_not_a_different_host() {
        let root = TempDir::new("id-hosts");
        let first = format!(
            "{OPEN}\
             01:00 -!- carol [c@hostC] has joined #one\n\
             01:05 -!- carol [c@hostC] has left #one []\n"
        );
        let second = format!(
            "{OPEN}\
             01:10 -!- carol [c@hostC] has joined #two\n\
             01:15 -!- carol [c@hostC] has left #two []\n\
             01:20 -!- carol [c@other] has joined #two\n"
        );
        let (conn, summary) = build(
            &root,
            &[("#one.log", first.as_str()), ("#two.log", second.as_str())],
        );

        let ids = identity_ids(&conn);
        assert_eq!(ids.len(), 3);
        assert_eq!(ids[0], ids[1], "same nick and host in two channels");
        assert_ne!(ids[1], ids[2], "same nick from a different host");
        assert_eq!(summary.identities, 2);
    }

    #[test]
    fn infers_hostless_sessions_from_the_nearest_session_with_a_host() {
        let root = TempDir::new("id-inferred");
        let channel = format!(
            "{OPEN}\
             01:00 -!- dave [d@hostD] has joined #analysis\n\
             01:05 <dave> hi\n"
        );
        let query = format!("{OPEN}01:10 <dave> psst\n01:11 <erin> hello\n");
        let (conn, summary) = build(
            &root,
            &[
                ("#analysis.log", channel.as_str()),
                ("dave.log", query.as_str()),
            ],
        );

        let rows: Vec<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT nicks || ' ' || link || ' ' || ifnull(inferred_from, '-')
                     FROM sessions ORDER BY id",
                )
                .unwrap();
            let rows = stmt
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<String>>>()
                .unwrap();
            rows
        };
        assert_eq!(rows, ["dave host -", "dave inferred 1", "erin nick_only -"]);

        let ids = identity_ids(&conn);
        assert_eq!(ids[0], ids[1]);
        assert_eq!(
            (summary.inferred_sessions, summary.nick_only_sessions),
            (1, 1)
        );
    }

    #[test]
    fn follows_the_logging_clients_own_nick_change() {
        let root = TempDir::new("id-logger");
        let log = format!(
            "{OPEN}\
             01:00 -!- rachel [rachel@NETADMIN.VHOST] has joined #analysis\n\
             01:00 -!- Irssi: #analysis: Total of 1 nicks [1 ops, 0 halfops, 0 voices, 0 normal]\n\
             01:00 -!- Irssi: Join to #analysis was synced in 0 secs\n\
             01:05 -!- You're now known as rachel_\n\
             01:06 <@rachel_> still me\n"
        );
        let (conn, _) = build(&root, &[("#analysis.log", log.as_str())]);

        assert_eq!(
            sessions(&conn),
            [
                "rachel rachel_ rachel@NETADMIN.VHOST 2026-11-01 01:00 2026-11-01 01:06 end_of_log 1 host 1"
            ]
        );
    }

    #[test]
    fn names_output_does_not_mark_someone_else_as_the_logger() {
        let root = TempDir::new("id-names");
        let log = format!(
            "{OPEN}\
             01:00 -!- rachel [rachel@hostR] has joined #analysis\n\
             01:00 -!- Irssi: #analysis: Total of 1 nicks [1 ops, 0 halfops, 0 voices, 0 normal]\n\
             01:00 -!- Irssi: Join to #analysis was synced in 0 secs\n\
             01:05 -!- marlowe [marlowewet@bridge] has joined #analysis\n\
             01:05 < marlowe> hi\n\
             01:06 -!- Irssi: #analysis: Total of 2 nicks [1 ops, 0 halfops, 0 voices, 1 normal]\n\
             01:07 -!- You're now known as rachelaway\n\
             01:08 <@rachelaway> still me\n"
        );
        let (conn, _) = build(&root, &[("#analysis.log", log.as_str())]);

        assert_eq!(
            sessions(&conn),
            [
                "rachel rachelaway rachel@hostR 2026-11-01 01:00 2026-11-01 01:08 end_of_log 1 host 1",
                "marlowe marlowewet@bridge 2026-11-01 01:05 2026-11-01 01:08 end_of_log 1 host 0",
            ]
        );
    }

    #[test]
    fn names_hours_after_a_join_does_not_mark_the_joiner_as_the_logger() {
        let root = TempDir::new("id-names-later");
        let log = format!(
            "{OPEN}\
             01:00 -!- rachel [rachel@hostR] has joined #anime\n\
             01:00 -!- Irssi: #anime: Total of 1 nicks [1 ops, 0 halfops, 0 voices, 0 normal]\n\
             01:00 -!- Irssi: Join to #anime was synced in 1 secs\n\
             14:06 -!- tamsin [tamsin@hostL] has joined #anime\n\
             20:03 -!- Irssi: #anime: Total of 37 nicks [3 ops, 0 halfops, 0 voices, 34 normal]\n\
             20:04 -!- You're now known as rachel_\n"
        );
        let (conn, _) = build(&root, &[("#anime.log", log.as_str())]);

        assert_eq!(
            sessions(&conn),
            [
                "rachel rachel_ rachel@hostR 2026-11-01 01:00 2026-11-01 20:04 end_of_log 0 host 1",
                "tamsin tamsin@hostL 2026-11-01 14:06 2026-11-01 20:04 end_of_log 0 host 0",
            ]
        );
    }

    #[test]
    fn names_in_the_same_minute_as_a_join_needs_irssis_sync_line() {
        let root = TempDir::new("id-names-same-minute");
        let log = format!(
            "{OPEN}\
             01:00 -!- rachel [rachel@hostR] has joined #botspam\n\
             01:00 -!- Irssi: #botspam: Total of 1 nicks [1 ops, 0 halfops, 0 voices, 0 normal]\n\
             01:00 -!- Irssi: Join to #botspam was synced in 0 secs\n\
             21:48 -!- oriel [oriel@hostO] has joined #botspam\n\
             21:48 -!- Irssi: #botspam: Total of 25 nicks [0 ops, 0 halfops, 0 voices, 25 normal]\n\
             21:50 -!- You're now known as rachel_\n"
        );
        let (conn, _) = build(&root, &[("#botspam.log", log.as_str())]);

        assert_eq!(
            sessions(&conn),
            [
                "rachel rachel_ rachel@hostR 2026-11-01 01:00 2026-11-01 21:50 end_of_log 0 host 1",
                "oriel oriel@hostO 2026-11-01 21:48 2026-11-01 21:50 end_of_log 0 host 0",
            ]
        );
    }

    #[test]
    fn keeps_ids_between_runs_and_the_older_id_wins_a_merge() {
        let root = TempDir::new("id-stable");
        let log = format!(
            "{OPEN}\
             01:00 -!- frank [f@hostF] has joined #analysis\n\
             01:05 -!- frank [f@hostF] has quit [bye]\n\
             01:10 -!- gina [f@hostF] has joined #analysis\n\
             01:15 -!- gina [f@hostF] has quit [bye]\n"
        );
        let (mut conn, _) = build(&root, &[("#analysis.log", log.as_str())]);
        let before = identity_ids(&conn);
        assert_ne!(
            before[0], before[1],
            "different nicks are not linked by host alone"
        );

        // Make frank's identity the older one.
        conn.execute(
            "UPDATE identities SET created_at = '2000-01-01 00:00:00' WHERE id = ?1",
            params![before[0]],
        )
        .unwrap();

        // A rebuild with nothing new keeps the ids.
        rebuild(&mut conn).unwrap();
        assert_eq!(identity_ids(&conn), before);

        // A nick change from frank to gina on the same host links them.
        fs::write(
            root.0.join("wetfish/#other.log"),
            format!(
                "{OPEN}\
                 02:00 -!- frank [f@hostF] has joined #other\n\
                 02:01 -!- frank is now known as gina\n\
                 02:02 -!- gina [f@hostF] has quit [bye]\n"
            ),
        )
        .unwrap();
        let summary = rebuild_after_import(&root.0, &mut conn);
        assert_eq!(summary.merged_identities, 1);
        assert!(identity_ids(&conn).iter().all(|id| *id == before[0]));

        let merged_into: String = conn
            .query_row(
                "SELECT merged_into FROM identities WHERE id = ?1",
                params![before[1]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(merged_into, before[0]);
    }

    fn add_group(conn: &Connection, ident_is_user: bool, hosts: &[&str]) {
        conn.execute(
            "INSERT INTO host_groups (tag, name, kind, ident_is_user)
             VALUES ('wetfish', 'matrix', 'bridge', ?1)",
            params![ident_is_user],
        )
        .unwrap();
        let group = conn.last_insert_rowid();
        for host in hosts {
            conn.execute(
                "INSERT INTO host_group_members (group_id, tag, host) VALUES (?1, 'wetfish', ?2)",
                params![group, host],
            )
            .unwrap();
        }
    }

    const BRIDGE_LOG: &str = "--- Log opened Sun Nov 01 01:00:00 2026\n\
                              01:00 -!- marlowe [marlowewet@old.bridge] has joined #analysis\n\
                              01:05 -!- marlowe [marlowewet@old.bridge] has quit [bye]\n\
                              02:00 -!- marlowe_m [marlowewet@new.bridge] has joined #analysis\n\
                              02:01 -!- marlowe [marlowewet@new.bridge] has joined #analysis\n\
                              02:05 -!- quokka [quokkawet@new.bridge] has joined #analysis\n";

    #[test]
    fn a_bridge_group_links_sessions_by_ident_across_its_hosts() {
        let root = TempDir::new("id-bridge");
        let (mut conn, summary) = build(&root, &[("#analysis.log", BRIDGE_LOG)]);
        assert_eq!(summary.identities, 4, "without a group nothing links");

        // Host names are matched ignoring case.
        add_group(&conn, true, &["old.bridge", "NEW.BRIDGE"]);
        let summary = rebuild(&mut conn).unwrap();
        let ids = identity_ids(&conn);
        assert_eq!(ids[0], ids[1], "another nick with the same ident");
        assert_eq!(
            ids[0], ids[2],
            "same nick and ident on another host of the group"
        );
        assert_ne!(ids[0], ids[3], "another ident is another person");
        assert_eq!(summary.identities, 2);

        // Only real nick and ident@host combinations are stored.
        let stand_ins: i64 = conn
            .query_row(
                "SELECT count(*) FROM identity_keys WHERE host LIKE '#%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stand_ins, 0);
    }

    #[test]
    fn a_group_without_ident_users_links_only_the_same_nick() {
        let root = TempDir::new("id-bouncer");
        let (mut conn, _) = build(&root, &[("#analysis.log", BRIDGE_LOG)]);
        add_group(&conn, false, &["old.bridge", "new.bridge"]);
        rebuild(&mut conn).unwrap();

        let ids = identity_ids(&conn);
        assert_eq!(
            ids[0], ids[2],
            "same nick and ident on another host of the group"
        );
        assert_ne!(
            ids[0], ids[1],
            "another nick with the same ident stays apart"
        );
    }

    #[test]
    fn a_cut_ends_the_session_and_starts_one_linked_to_nothing_else() {
        let root = TempDir::new("id-cut");
        let log = format!(
            "{OPEN}\
             01:00 -!- quokka [quokkawet@hostQ] has joined #dryfish\n\
             01:01 <quokka> hi\n\
             01:02 -!- quokka is now known as rachelaway\n\
             01:03 <rachelaway> borrowing a friend's computer\n\
             01:04 -!- rachelaway is now known as rachel\n\
             01:05 -!- rachel [quokkawet@hostQ] has quit [bye]\n\
             02:00 -!- quokka [quokkawet@hostQ] has joined #dryfish\n"
        );
        let (mut conn, _) = build(&root, &[("#dryfish.log", log.as_str())]);
        let path: String = conn
            .query_row("SELECT path FROM files", [], |row| row.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO session_cuts (path, line, expected)
             VALUES (?1, 4, 'quokka is now known as rachelaway')",
            params![path],
        )
        .unwrap();

        let summary = rebuild(&mut conn).unwrap();
        assert_eq!((summary.cuts_applied, summary.cuts_stale), (1, 0));
        assert_eq!(
            sessions(&conn),
            [
                "quokka quokkawet@hostQ 2026-11-01 01:00 2026-11-01 01:02 cut 1 host 0",
                "rachelaway rachel - 2026-11-01 01:02 2026-11-01 01:05 quit 1 cut 0",
                "quokka quokkawet@hostQ 2026-11-01 02:00 2026-11-01 02:00 end_of_log 0 host 0",
            ]
        );
        let ids = identity_ids(&conn);
        assert_ne!(ids[0], ids[1]);
        assert_eq!(ids[0], ids[2]);

        // A rebuild keeps the cut's identity id.
        rebuild(&mut conn).unwrap();
        assert_eq!(identity_ids(&conn), ids);

        // A cut whose line no longer holds its nick change is stale.
        conn.execute(
            "UPDATE session_cuts SET expected = 'someone else is now known as rachelaway'",
            [],
        )
        .unwrap();
        let summary = rebuild(&mut conn).unwrap();
        assert_eq!((summary.cuts_applied, summary.cuts_stale), (0, 1));
        assert_eq!(sessions(&conn).len(), 2);
    }

    #[test]
    fn describes_nick_change_lines() {
        assert_eq!(
            nick_change_text("nick_change", Some("quokka"), Some("rachel")).as_deref(),
            Some("quokka is now known as rachel")
        );
        assert_eq!(
            nick_change_text("own_nick_change", None, Some("rachel_")).as_deref(),
            Some("You're now known as rachel_")
        );
        assert_eq!(nick_change_text("join", Some("quokka"), None), None);
    }

    #[test]
    fn counts_the_lines_each_session_heard() {
        let root = TempDir::new("id-heard");
        let log = format!(
            "{OPEN}\
             01:00 -!- quokka [q@hostQ] has joined #analysis\n\
             01:01 <alice> one\n\
             01:02 -!- marlowe [m@hostM] has joined #analysis\n\
             01:03 <alice> two\n\
             01:04 -!- quokka [q@hostQ] has quit [bye]\n\
             01:05 <marlowe> three\n"
        );
        let (conn, _) = build(&root, &[("#analysis.log", log.as_str())]);
        let mut stmt = conn
            .prepare("SELECT nicks || ' ' || heard FROM sessions ORDER BY id")
            .unwrap();
        let heard: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(heard, ["quokka 2", "alice 3", "marlowe 2"]);
    }
}
