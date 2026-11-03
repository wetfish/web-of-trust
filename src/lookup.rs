//! Finds identities by nick, ident, host or person, and loads their sessions
//! for the identity views of `wot analyze`. Also follows identity merges, so
//! that people keep their identities when a rebuild merges them.

use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use chrono::NaiveDateTime;
use rusqlite::{Connection, OptionalExtension, params};

use crate::db::Result;

/// What to look up. `nick`, `ident` and `host` may use `*` to match any
/// characters, and matching ignores case. An identity matches when one of its
/// nick and `ident@host` combinations matches every one of those given, and
/// it belongs to `person` if that is given.
#[derive(Default)]
pub struct Filter {
    pub nick: Option<String>,
    pub ident: Option<String>,
    pub host: Option<String>,
    pub person: Option<String>,
}

impl Filter {
    pub fn is_empty(&self) -> bool {
        self.nick.is_none() && self.ident.is_none() && self.host.is_none() && self.person.is_none()
    }

    fn matches_keys(&self) -> bool {
        self.nick.is_some() || self.ident.is_some() || self.host.is_some()
    }

    /// The filter in words, such as `nick "rachel", host "Fish-*"`.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        for (name, value) in [
            ("nick", &self.nick),
            ("ident", &self.ident),
            ("host", &self.host),
            ("person", &self.person),
        ] {
            if let Some(value) = value {
                parts.push(format!("{name} \"{value}\""));
            }
        }
        parts.join(", ")
    }
}

/// Minutes present in channels. Sessions that overlap, such as one in each
/// of several channels or the same channel logged twice, count once, so the
/// time can never be more than the time from the first session to the last.
pub fn present_minutes<'a>(sessions: impl IntoIterator<Item = &'a SessionRow>) -> i64 {
    let at = |at: &str| NaiveDateTime::parse_from_str(at, "%Y-%m-%d %H:%M").ok();
    let mut spans: Vec<(NaiveDateTime, NaiveDateTime)> = sessions
        .into_iter()
        .filter(|session| session.channel)
        .filter_map(|session| Some((at(&session.start_at)?, at(&session.end_at)?)))
        .filter(|(start, end)| end > start)
        .collect();
    spans.sort();
    let mut minutes = 0;
    let mut open: Option<(NaiveDateTime, NaiveDateTime)> = None;
    for (start, end) in spans {
        if let Some((_, until)) = open.as_mut()
            && start <= *until
        {
            *until = (*until).max(end);
            continue;
        }
        if let Some((from, until)) = open.replace((start, end)) {
            minutes += (until - from).num_minutes();
        }
    }
    if let Some((from, until)) = open {
        minutes += (until - from).num_minutes();
    }
    minutes
}

/// One session of an identity, with the file it is in.
pub struct SessionRow {
    pub nicks: Vec<String>,
    pub ident: Option<String>,
    pub host: Option<String>,
    /// `host`, `inferred`, `nick_only` or `cut`.
    pub link: String,
    /// `YYYY-MM-DD HH:MM`.
    pub start_at: String,
    pub end_at: String,
    pub minutes: i64,
    pub spoken: i64,
    pub logger: bool,
    /// Whether the file is a channel log rather than a private message log.
    pub channel: bool,
    /// The channel or private message target.
    pub target: String,
    pub file_id: i64,
    /// The file as `tag/target.log`.
    pub file: String,
    /// The file's full path.
    pub path: String,
    pub start_line: i64,
    pub end_line: Option<i64>,
    /// For inferred sessions, the session they were attributed from, as
    /// `tag/target.log:line`.
    pub inferred_from: Option<String>,
}

pub struct IdentityView {
    pub id: String,
    pub tag: String,
    /// The people this identity belongs to. More than one means identities
    /// added to different people were merged by a rebuild since.
    pub people: Vec<String>,
    /// In order of when they started.
    pub sessions: Vec<SessionRow>,
}

impl IdentityView {
    /// When the identity was last seen, as `YYYY-MM-DD HH:MM`.
    pub fn last_seen(&self) -> &str {
        self.sessions
            .iter()
            .map(|session| session.end_at.as_str())
            .max()
            .unwrap_or_default()
    }

    /// When the identity was first seen, as `YYYY-MM-DD HH:MM`.
    pub fn first_seen(&self) -> &str {
        self.sessions
            .first()
            .map(|session| session.start_at.as_str())
            .unwrap_or_default()
    }

    /// The distinct nicks, most recently used first.
    pub fn nicks(&self) -> Vec<&str> {
        let mut seen = HashSet::new();
        let mut nicks = Vec::new();
        for session in self.sessions.iter().rev() {
            for nick in session.nicks.iter().rev() {
                if seen.insert(nick.to_lowercase()) {
                    nicks.push(nick.as_str());
                }
            }
        }
        nicks
    }

    /// The number of distinct `ident@host` combinations.
    pub fn connections(&self) -> usize {
        self.sessions
            .iter()
            .filter_map(|session| Some((session.ident.as_deref()?, session.host.as_deref()?)))
            .collect::<HashSet<_>>()
            .len()
    }

    /// Minutes present in channels, with overlapping sessions counted once.
    pub fn channel_minutes(&self) -> i64 {
        present_minutes(&self.sessions)
    }

    /// Messages and actions.
    pub fn spoken(&self) -> i64 {
        self.sessions
            .iter()
            .map(|session| session.spoken.max(0))
            .sum()
    }
}

/// Which identity each merged identity was merged into.
pub fn merges(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt =
        conn.prepare("SELECT id, merged_into FROM identities WHERE merged_into IS NOT NULL")?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// The identity an id stands for now, following merges.
pub fn current_id(merges: &HashMap<String, String>, id: &str) -> String {
    let mut id: &str = id;
    // A merge chain is short; the limit only guards against a loop.
    for _ in 0..100 {
        match merges.get(id) {
            Some(next) => id = next.as_str(),
            None => break,
        }
    }
    id.to_string()
}

/// The people each current identity belongs to, sorted by name.
pub fn people_by_identity(
    conn: &Connection,
    merges: &HashMap<String, String>,
) -> Result<HashMap<String, Vec<String>>> {
    let mut stmt = conn.prepare(
        "SELECT pi.identity_id, p.name
         FROM person_identities pi JOIN people p ON p.id = pi.person_id
         ORDER BY p.name",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut people: HashMap<String, Vec<String>> = HashMap::new();
    for (id, name) in rows {
        let names = people.entry(current_id(merges, &id)).or_default();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    Ok(people)
}

/// The identities matching the filter, most recently seen first.
pub fn find(conn: &Connection, filter: &Filter) -> Result<Vec<IdentityView>> {
    let merges = merges(conn)?;
    let people = people_by_identity(conn, &merges)?;

    let mut ids: Option<Vec<String>> = None;
    if filter.matches_keys() {
        let mut stmt = conn.prepare(
            r"SELECT DISTINCT identity_id FROM identity_keys
              WHERE (?1 IS NULL OR nick LIKE ?1 ESCAPE '\')
                AND (?2 IS NULL OR ident LIKE ?2 ESCAPE '\')
                AND (?3 IS NULL OR host LIKE ?3 ESCAPE '\')",
        )?;
        let rows = stmt
            .query_map(
                params![
                    filter
                        .nick
                        .as_deref()
                        .map(|nick| like_pattern(&nick.to_lowercase())),
                    filter.ident.as_deref().map(like_pattern),
                    filter.host.as_deref().map(like_pattern),
                ],
                |row| row.get(0),
            )?
            .collect::<rusqlite::Result<_>>()?;
        ids = Some(rows);
    }
    if let Some(person) = &filter.person {
        let of_person: HashSet<&String> = people
            .iter()
            .filter(|(_, names)| names.iter().any(|name| name.eq_ignore_ascii_case(person)))
            .map(|(id, _)| id)
            .collect();
        ids = Some(match ids {
            Some(ids) => ids
                .into_iter()
                .filter(|id| of_person.contains(id))
                .collect(),
            None => of_person.into_iter().cloned().collect(),
        });
    }
    let mut views = load_views(conn, ids.unwrap_or_default(), &people)?;
    views.sort_by(|a, b| {
        b.last_seen()
            .cmp(a.last_seen())
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(views)
}

/// One identity by id. An id that was merged into another gives that one;
/// `None` when there is no such identity or it has no sessions.
pub fn by_id(conn: &Connection, id: &str) -> Result<Option<IdentityView>> {
    let merges = merges(conn)?;
    let people = people_by_identity(conn, &merges)?;
    let current = current_id(&merges, id);
    Ok(load_views(conn, vec![current], &people)?.into_iter().next())
}

/// The identities with these ids that have sessions, in the same order.
fn load_views(
    conn: &Connection,
    ids: Vec<String>,
    people: &HashMap<String, Vec<String>>,
) -> Result<Vec<IdentityView>> {
    let mut tag_of = conn.prepare("SELECT tag FROM identities WHERE id = ?1")?;
    let mut stmt = conn.prepare(
        "SELECT s.nicks, s.ident, s.host, s.link, s.start_at, s.end_at, s.minutes, s.spoken,
                s.logger, f.kind, f.folder_tag, f.target, s.start_line, s.end_line,
                sf.folder_tag, sf.target, src.start_line, s.file_id, f.path
         FROM sessions s
         JOIN files f ON f.id = s.file_id
         LEFT JOIN sessions src ON src.id = s.inferred_from
         LEFT JOIN files sf ON sf.id = src.file_id
         WHERE s.identity_id = ?1
         ORDER BY s.start_at, s.id",
    )?;

    let mut views = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(tag) = tag_of
            .query_row(params![id], |row| row.get::<_, String>(0))
            .optional()?
        else {
            continue;
        };
        let sessions = stmt
            .query_map(params![id], |row| {
                let nicks: String = row.get(0)?;
                let kind: String = row.get(9)?;
                let file_tag: String = row.get(10)?;
                let target: String = row.get(11)?;
                let source_tag: Option<String> = row.get(14)?;
                let source_target: Option<String> = row.get(15)?;
                let source_line: Option<i64> = row.get(16)?;
                let inferred_from = match (source_tag, source_target, source_line) {
                    (Some(tag), Some(target), Some(line)) => {
                        Some(format!("{tag}/{target}.log:{line}"))
                    }
                    _ => None,
                };
                Ok(SessionRow {
                    nicks: nicks.split(' ').map(str::to_string).collect(),
                    ident: row.get(1)?,
                    host: row.get(2)?,
                    link: row.get(3)?,
                    start_at: row.get(4)?,
                    end_at: row.get(5)?,
                    minutes: row.get(6)?,
                    spoken: row.get(7)?,
                    logger: row.get(8)?,
                    channel: kind == "channel",
                    file: format!("{file_tag}/{target}.log"),
                    target,
                    file_id: row.get(17)?,
                    path: row.get(18)?,
                    start_line: row.get(12)?,
                    end_line: row.get(13)?,
                    inferred_from,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !sessions.is_empty() {
            let people = people.get(&id).cloned().unwrap_or_default();
            views.push(IdentityView {
                id,
                tag,
                people,
                sessions,
            });
        }
    }
    Ok(views)
}

/// Turns a pattern with `*` wildcards into a SQL `LIKE` pattern, escaping
/// `LIKE`'s own wildcards with a backslash. Use it with `ESCAPE '\'`.
pub fn like_pattern(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    for c in pattern.chars() {
        match c {
            '\\' | '%' | '_' => {
                let _ = write!(out, "\\{c}");
            }
            '*' => out.push('%'),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::discover::{self, tests::TempDir};
    use crate::{db, identity};

    #[test]
    fn turns_wildcards_into_like_patterns() {
        assert_eq!(like_pattern("Fish-*.chat"), "Fish-%.chat");
        assert_eq!(like_pattern("rachel_"), r"rachel\_");
        assert_eq!(like_pattern("100%"), r"100\%");
    }

    #[test]
    fn finds_identities_by_nick_ident_and_host() {
        let root = TempDir::new("lookup");
        fs::create_dir_all(root.0.join("wetfish")).unwrap();
        fs::write(
            root.0.join("wetfish/#analysis.log"),
            "--- Log opened Sun Nov 01 01:00:00 2026\n\
             01:00 -!- rachel [rachel@hostR] has joined #analysis\n\
             01:01 <rachel> hi\n\
             01:02 -!- quokka [quokkawet@bridge.chat] has joined #analysis\n\
             01:03 -!- marlowe [marlowewet@bridge.chat] has joined #analysis\n\
             01:04 <marlowe> hello\n\
             01:05 -!- marlowe is now known as marlowe_\n",
        )
        .unwrap();
        fs::write(
            root.0.join("wetfish/rachel.log"),
            "--- Log opened Sun Nov 01 02:00:00 2026\n02:00 <rachel> psst\n",
        )
        .unwrap();

        let path = root.0.join("wot.sqlite");
        let mut conn = db::open(&path).unwrap();
        let inventory = discover::discover(&[root.0.clone()]).unwrap();
        db::import(&mut conn, &inventory.files);
        identity::rebuild(&mut conn).unwrap();

        let nicks = |filter: Filter| -> Vec<String> {
            find(&conn, &filter)
                .unwrap()
                .iter()
                .map(|view| view.sessions[0].nicks.join(" "))
                .collect()
        };

        let rachel = Filter {
            nick: Some("RACHEL".into()),
            ..Filter::default()
        };
        assert_eq!(nicks(rachel), ["rachel"]);

        let bridge = Filter {
            host: Some("*.chat".into()),
            ..Filter::default()
        };
        let mut found = nicks(bridge);
        found.sort();
        assert_eq!(found, ["marlowe marlowe_", "quokka"]);

        let both = Filter {
            ident: Some("marlowe*".into()),
            host: Some("bridge.chat".into()),
            ..Filter::default()
        };
        assert_eq!(nicks(both), ["marlowe marlowe_"]);

        // The private message session is inferred from the channel session.
        let rachel = find(
            &conn,
            &Filter {
                nick: Some("rachel".into()),
                ..Filter::default()
            },
        )
        .unwrap();
        let inferred: Vec<Option<&str>> = rachel[0]
            .sessions
            .iter()
            .map(|session| session.inferred_from.as_deref())
            .collect();
        assert_eq!(inferred, [None, Some("wetfish/#analysis.log:2")]);

        // A person holds identities, including one merged since it was added.
        let rachel_id = rachel[0].id.clone();
        conn.execute_batch(&format!(
            "INSERT INTO identities (id, tag, merged_into) VALUES ('0ld1d', 'wetfish', '{rachel_id}');
             INSERT INTO people (name) VALUES ('rachel');
             INSERT INTO person_identities (identity_id, person_id) VALUES ('0ld1d', 1);"
        ))
        .unwrap();
        let found = find(
            &conn,
            &Filter {
                person: Some("RACHEL".into()),
                ..Filter::default()
            },
        )
        .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, rachel_id);
        assert_eq!(found[0].people, ["rachel"]);

        // Looking up the merged id gives the identity it was merged into.
        let view = by_id(&conn, "0ld1d").unwrap().unwrap();
        assert_eq!(view.id, rachel_id);
        assert_eq!(view.nicks(), ["rachel"]);
        assert_eq!((view.connections(), view.spoken()), (1, 2));
        assert_eq!(view.first_seen(), "2026-11-01 01:00");
        assert!(view.sessions[0].path.ends_with("#analysis.log"));
        assert!(by_id(&conn, "nope").unwrap().is_none());

        // Combined with a nick that is not the person's, nothing matches.
        let none = find(
            &conn,
            &Filter {
                nick: Some("quokka".into()),
                person: Some("rachel".into()),
                ..Filter::default()
            },
        )
        .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn counts_overlapping_channel_time_once() {
        let session = |start: &str, end: &str, channel: bool| SessionRow {
            nicks: vec!["quokka".into()],
            ident: None,
            host: None,
            link: "host".into(),
            start_at: format!("2026-11-01 {start}"),
            end_at: format!("2026-11-01 {end}"),
            minutes: 0,
            spoken: 0,
            logger: false,
            channel,
            target: "#analysis".into(),
            file_id: 1,
            file: "wetfish/#analysis.log".into(),
            path: "/logs/wetfish/#analysis.log".into(),
            start_line: 1,
            end_line: None,
            inferred_from: None,
        };
        let sessions = [
            // Two channels at once, then the second channel logged twice.
            session("10:00", "12:00", true),
            session("11:00", "13:00", true),
            session("11:30", "12:30", true),
            // A gap, then an hour on its own.
            session("15:00", "16:00", true),
            // Private messages are not time in channels.
            session("17:00", "20:00", false),
        ];
        assert_eq!(present_minutes(&sessions), 4 * 60);
        assert_eq!(present_minutes(&sessions[3..]), 60);
        assert_eq!(present_minutes(&sessions[..0]), 0);
    }

    #[test]
    fn follows_merge_chains() {
        let merges: HashMap<String, String> = [("a", "b"), ("b", "c")]
            .into_iter()
            .map(|(from, to)| (from.to_string(), to.to_string()))
            .collect();
        assert_eq!(current_id(&merges, "a"), "c");
        assert_eq!(current_id(&merges, "c"), "c");
    }
}
