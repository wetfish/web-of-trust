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
//! Everything is rebuilt from the events on each run. Identity ids are random,
//! and kept stable through `identity_keys`, which records which nick and
//! `ident@host` combinations belong to which id. When two identities merge,
//! the older id wins and the other records `merged_into`.

use std::collections::HashMap;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use rusqlite::{Connection, params};

use crate::db::Result;

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
}

/// Whether sessions have been built at all, for example after upgrading a
/// database that predates them.
pub fn is_built(conn: &Connection) -> Result<bool> {
    let built = conn.query_row("SELECT EXISTS (SELECT 1 FROM sessions)", [], |row| {
        row.get(0)
    })?;
    Ok(built)
}

/// Rebuilds all sessions and identities from the events, in one transaction.
pub fn rebuild(conn: &mut Connection) -> Result<RebuildSummary> {
    let tx = conn.transaction()?;
    let sessions = replay_events(&tx)?;

    let mut summary = RebuildSummary {
        sessions: sessions.len(),
        ..RebuildSummary::default()
    };
    let links = link_sessions(&sessions);
    let ids = assign_ids(&tx, &links, &mut summary)?;

    tx.execute("DELETE FROM sessions", [])?;
    {
        let mut insert = tx.prepare(
            "INSERT INTO sessions (id, file_id, identity_id, link, inferred_from, nicks, ident,
                 host, start_at, start_line, joined, end_at, end_line, end_reason, minutes,
                 spoken, logger)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        )?;
        for (index, session) in sessions.iter().enumerate() {
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
            ])?;
        }
    }

    tx.commit()?;
    Ok(summary)
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
}

/// Replays every file's events in line order and returns their sessions.
fn replay_events(conn: &Connection) -> Result<Vec<Session>> {
    let tags: HashMap<i64, String> = {
        let mut stmt = conn.prepare("SELECT id, tag FROM files")?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };

    let mut stmt = conn.prepare(&format!(
        "SELECT file_id, line, date, time, kind, nick, ident, host, other_nick, text
         FROM events
         WHERE kind IN ({SESSION_KINDS}) AND date IS NOT NULL AND time IS NOT NULL
         ORDER BY file_id, line"
    ))?;
    let mut rows = stmt.query([])?;

    let mut sessions = Vec::new();
    let mut replay: Option<FileReplay> = None;
    while let Some(row) = rows.next()? {
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
        };

        if replay.as_ref().is_none_or(|r| r.file_id != event.file_id) {
            if let Some(finished) = replay.take() {
                finished.finish(&mut sessions);
            }
            let tag = tags.get(&event.file_id).cloned().unwrap_or_default();
            replay = Some(FileReplay::new(event.file_id, tag));
        }
        if let Some(replay) = replay.as_mut() {
            replay.event(&event, &mut sessions);
        }
    }
    if let Some(finished) = replay.take() {
        finished.finish(&mut sessions);
    }
    Ok(sessions)
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
    /// The lowercased nick of the most recent join.
    last_join: Option<String>,
}

impl FileReplay {
    fn new(file_id: i64, tag: String) -> Self {
        FileReplay {
            file_id,
            tag,
            open: HashMap::new(),
            visible_since: None,
            last_seen: None,
            last_join: None,
        }
    }

    fn event(&mut self, event: &Row, sessions: &mut Vec<Session>) {
        let nick = event.nick.as_deref().unwrap_or_default();
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
                    close(&mut sessions[index], event, "rejoined");
                }
                let index = self.start(sessions, nick, event, true);
                learn_host(&mut sessions[index], event);
                self.last_join = Some(nick.to_lowercase());
            }
            "message" | "action" => {
                let index = self.session_for(sessions, nick, event);
                sessions[index].spoken += 1;
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
                close(&mut sessions[index], event, reason);
            }
            "nick_change" => {
                let new = event.other_nick.as_deref().unwrap_or_default();
                let index = self.session_for(sessions, nick, event);
                self.rename(sessions, index, nick, new, event);
            }
            "own_nick_change" => {
                let new = event.other_nick.as_deref().unwrap_or_default();
                let own = self
                    .open
                    .iter()
                    .find(|&(_, &index)| sessions[index].logger)
                    .map(|(old, &index)| (old.clone(), index));
                if let Some((old, index)) = own {
                    self.rename(sessions, index, &old, new, event);
                }
            }
            "client" => {
                // irssi writes "#channel: Total of N nicks ..." right after
                // the logging client's own join.
                let text = event.text.as_deref().unwrap_or_default();
                if text.contains(": Total of ") {
                    let own = self.last_join.as_ref().and_then(|nick| self.open.get(nick));
                    if let Some(&index) = own {
                        sessions[index].logger = true;
                    }
                }
            }
            _ => {}
        }
        self.last_seen = Some(event.at);
    }

    /// Closes the sessions still open when the file ends.
    fn finish(mut self, sessions: &mut [Session]) {
        if let Some(end) = self.last_seen {
            self.close_all(sessions, end, None, "end_of_log");
        }
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
        });
        let index = sessions.len() - 1;
        self.open.insert(nick.to_lowercase(), index);
        index
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
            close(&mut sessions[stale], event, "nick_taken");
        }
        sessions[index].nicks.push(new.to_string());
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
        }
    }
}

fn close(session: &mut Session, event: &Row, reason: &'static str) {
    session.end = event.at;
    session.end_line = Some(event.line);
    session.end_reason = reason;
}

/// Records the `ident@host` from a join, part or quit line, if the session
/// does not have one yet.
fn learn_host(session: &mut Session, event: &Row) {
    if session.host.is_none() {
        if let (Some(ident), Some(host)) = (&event.ident, &event.host) {
            session.ident = Some(ident.clone());
            session.host = Some(host.clone());
        }
    }
}

/// A network tag, lowercased nick, ident and host. Nick-only keys have an
/// empty ident and host.
type Key = (String, String, String, String);

#[derive(Clone, Copy)]
enum Link {
    /// The session showed a host. The node is one of its keys.
    Host(usize),
    /// Attributed to the identity of this other session, which has a host.
    Inferred(usize),
    /// The nick never appears with a host. The node is one of its keys.
    NickOnly(usize),
}

/// Keys joined into clusters, one cluster per identity.
struct Links {
    keys: Vec<Key>,
    parent: Vec<usize>,
    /// One entry per session.
    of_session: Vec<Link>,
}

impl Links {
    fn node(&mut self, index: &mut HashMap<Key, usize>, key: Key) -> usize {
        *index.entry(key).or_insert_with_key(|key| {
            self.keys.push(key.clone());
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
fn link_sessions(sessions: &[Session]) -> Links {
    let mut links = Links {
        keys: Vec::new(),
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
        let mut nodes = Vec::with_capacity(session.nicks.len());
        for nick in &session.nicks {
            let nick = nick.to_lowercase();
            let key = (
                session.tag.clone(),
                nick.clone(),
                ident.clone(),
                host.clone(),
            );
            nodes.push(links.node(&mut index, key));
            with_host
                .entry((session.tag.clone(), nick))
                .or_default()
                .push((session.start, i));
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
                        links.node(&mut index, key)
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
        insert.execute(params![tag, nick, ident, host, ids[&links.root(node)]])?;
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
}
