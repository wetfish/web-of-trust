//! Statistics and decisions read from the database, for the review pages and
//! the list commands. Unlike the inventory and daily activity reports of
//! `wot analyze`, these never read the log files, only what was imported.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};

use crate::db::Result;
use crate::identity;
use crate::lookup;

/// Everything on the overview page.
pub struct Overview {
    /// When a log file was last imported, in UTC.
    pub last_import: Option<String>,
    pub files: i64,
    pub lines: i64,
    pub tags: Vec<TagSummary>,
}

pub struct TagSummary {
    pub tag: String,
    /// Identities with at least one session.
    pub identities: i64,
    /// People with an identity in the tag.
    pub people: i64,
    /// Bots with an identity in the tag.
    pub bots: i64,
    pub host_groups: i64,
    pub cuts: i64,
    pub channels: Vec<TargetSummary>,
    /// All private message logs of the tag together.
    pub queries: QuerySummary,
}

/// A channel, from every file that logged it.
pub struct TargetSummary {
    pub name: String,
    pub files: i64,
    pub lines: i64,
    /// Messages and actions.
    pub spoken: i64,
    /// Sessions that started with a join line.
    pub joins: i64,
    /// Identities with a session in the channel, apart from the logging
    /// client.
    pub identities: i64,
    /// Of those, the identities that spoke at least once.
    pub speakers: i64,
    /// `YYYY-MM-DD HH:MM` of the first and last session.
    pub first: Option<String>,
    pub last: Option<String>,
}

#[derive(Default)]
pub struct QuerySummary {
    pub targets: i64,
    pub files: i64,
    pub lines: i64,
    pub spoken: i64,
}

pub fn overview(conn: &Connection) -> Result<Overview> {
    let (last_import, files, lines) = conn.query_row(
        "SELECT max(imported_at), count(*), ifnull(sum(lines_read), 0) FROM files",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;

    // Identities seen and identities that spoke, by tag and channel, as the
    // last rebuild counted them.
    let mut people_in: HashMap<(String, String), (i64, i64)> = HashMap::new();
    {
        let mut stmt =
            conn.prepare("SELECT tag, channel, identities, speakers FROM channel_totals")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            people_in.insert((row.get(0)?, row.get(1)?), (row.get(2)?, row.get(3)?));
        }
    }

    let mut tags: Vec<TagSummary> = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT f.tag, f.kind, f.target, count(*), sum(f.lines_read),
                ifnull(sum(t.spoken), 0), ifnull(sum(t.joins), 0), min(t.first_at), max(t.last_at)
         FROM files f LEFT JOIN file_totals t ON t.file_id = f.id
         GROUP BY f.tag, f.kind, f.target
         ORDER BY f.tag, f.target",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let tag: String = row.get(0)?;
        let kind: String = row.get(1)?;
        let name: String = row.get(2)?;
        if tags.last().is_none_or(|last| last.tag != tag) {
            tags.push(tag_summary(conn, &tag)?);
        }
        let summary = tags.last_mut().expect("pushed above");
        let files: i64 = row.get(3)?;
        let lines: i64 = row.get(4)?;
        let spoken: i64 = row.get(5)?;
        if kind == "channel" {
            let (identities, speakers) = people_in
                .get(&(tag.clone(), name.clone()))
                .copied()
                .unwrap_or_default();
            summary.channels.push(TargetSummary {
                name,
                files,
                lines,
                spoken,
                joins: row.get(6)?,
                identities,
                speakers,
                first: row.get(7)?,
                last: row.get(8)?,
            });
        } else {
            summary.queries.targets += 1;
            summary.queries.files += files;
            summary.queries.lines += lines;
            summary.queries.spoken += spoken;
        }
    }

    Ok(Overview {
        last_import,
        files,
        lines,
        tags,
    })
}

/// A tag's totals, with no channels yet.
fn tag_summary(conn: &Connection, tag: &str) -> Result<TagSummary> {
    let count = |sql: &str| -> rusqlite::Result<i64> {
        conn.query_row(sql, params![tag], |row| row.get(0))
    };
    Ok(TagSummary {
        tag: tag.to_string(),
        // Identities of the tag that have a session, found through the
        // identities rather than by adding up every session.
        identities: count(
            "SELECT count(*) FROM identities i
             WHERE i.tag = ?1 AND i.merged_into IS NULL
               AND EXISTS (SELECT 1 FROM sessions s WHERE s.identity_id = i.id)",
        )?,
        people: count(
            "SELECT count(DISTINCT pi.person_id) FROM person_identities pi
             JOIN identities i ON i.id = pi.identity_id
             JOIN people p ON p.id = pi.person_id WHERE i.tag = ?1 AND p.kind = 'person'",
        )?,
        bots: count(
            "SELECT count(DISTINCT pi.person_id) FROM person_identities pi
             JOIN identities i ON i.id = pi.identity_id
             JOIN people p ON p.id = pi.person_id WHERE i.tag = ?1 AND p.kind = 'bot'",
        )?,
        host_groups: count("SELECT count(*) FROM host_groups WHERE tag = ?1")?,
        cuts: count(
            "SELECT count(*) FROM session_cuts c JOIN files f ON f.path = c.path
             WHERE f.tag = ?1",
        )?,
        channels: Vec::new(),
        queries: QuerySummary::default(),
    })
}

/// One channel's activity day by day.
pub struct ChannelDays {
    pub tag: String,
    pub name: String,
    /// The log files the channel was read from.
    pub paths: Vec<String>,
    /// Only days with at least one spoken line, oldest first.
    pub days: Vec<Day>,
    /// Distinct nicks, ignoring case, over all days.
    pub nicks: i64,
}

pub struct Day {
    /// `YYYY-MM-DD`.
    pub date: String,
    /// Messages and actions.
    pub spoken: i64,
    /// Distinct nicks that spoke, ignoring case.
    pub nicks: i64,
}

/// A channel's activity, or `None` when no file logged it.
pub fn channel_days(conn: &Connection, tag: &str, name: &str) -> Result<Option<ChannelDays>> {
    let paths: Vec<String> = {
        let mut stmt =
            conn.prepare("SELECT path FROM files WHERE tag = ?1 AND target = ?2 ORDER BY path")?;
        let rows = stmt
            .query_map(params![tag, name], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    if paths.is_empty() {
        return Ok(None);
    }

    let spoken_in = "file_id IN (SELECT id FROM files WHERE tag = ?1 AND target = ?2)
                     AND kind IN ('message', 'action') AND date IS NOT NULL";
    let days: Vec<Day> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT date, count(*), count(DISTINCT lower(nick)) FROM events
             WHERE {spoken_in} GROUP BY date ORDER BY date"
        ))?;
        let rows = stmt
            .query_map(params![tag, name], |row| {
                Ok(Day {
                    date: row.get(0)?,
                    spoken: row.get(1)?,
                    nicks: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    let nicks = conn.query_row(
        &format!("SELECT count(DISTINCT lower(nick)) FROM events WHERE {spoken_in}"),
        params![tag, name],
        |row| row.get(0),
    )?;

    Ok(Some(ChannelDays {
        tag: tag.to_string(),
        name: name.to_string(),
        paths,
        days,
        nicks,
    }))
}

/// The network tags with imported logs.
pub fn tags(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT tag FROM files ORDER BY tag")?;
    let rows = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// A nick change line inside a session.
pub struct NickChange {
    pub line: i64,
    /// As irssi wrote it, such as `alice is now known as bob`.
    pub text: String,
    /// Whether a cut is on this line.
    pub cut: bool,
}

/// The nick change lines between two lines of a file that rename one of
/// `nicks` to another of them, as a session's nick changes do.
pub fn nick_changes(
    conn: &Connection,
    file_id: i64,
    start_line: i64,
    end_line: Option<i64>,
    nicks: &[String],
) -> Result<Vec<NickChange>> {
    let nicks: HashSet<String> = nicks.iter().map(|nick| nick.to_lowercase()).collect();
    let mut stmt = conn.prepare(
        "SELECT e.line, e.kind, e.nick, e.other_nick,
                EXISTS (SELECT 1 FROM session_cuts c WHERE c.path = f.path AND c.line = e.line)
         FROM events e JOIN files f ON f.id = e.file_id
         WHERE e.file_id = ?1 AND e.line BETWEEN ?2 AND ?3
           AND e.kind IN ('nick_change', 'own_nick_change')
         ORDER BY e.line",
    )?;
    let mut rows = stmt.query(params![file_id, start_line, end_line.unwrap_or(i64::MAX)])?;
    let mut changes = Vec::new();
    while let Some(row) = rows.next()? {
        let kind: String = row.get(1)?;
        let nick: Option<String> = row.get(2)?;
        let new: Option<String> = row.get(3)?;
        let renames_ours = new
            .as_deref()
            .is_some_and(|new| nicks.contains(&new.to_lowercase()))
            && (kind == "own_nick_change"
                || nick
                    .as_deref()
                    .is_some_and(|nick| nicks.contains(&nick.to_lowercase())));
        if !renames_ours {
            continue;
        }
        if let Some(text) = identity::nick_change_text(&kind, nick.as_deref(), new.as_deref()) {
            changes.push(NickChange {
                line: row.get(0)?,
                text,
                cut: row.get(4)?,
            });
        }
    }
    Ok(changes)
}

pub struct HostGroup {
    pub id: i64,
    pub tag: String,
    pub name: String,
    /// `bridge` or `bouncer`.
    pub kind: String,
    pub ident_is_user: bool,
    pub note: Option<String>,
    pub created_at: String,
    pub members: Vec<GroupMember>,
}

pub struct GroupMember {
    pub host: String,
    pub note: Option<String>,
    pub created_at: String,
    /// Identities that used the host.
    pub identities: i64,
}

pub fn host_groups(conn: &Connection) -> Result<Vec<HostGroup>> {
    let mut groups: Vec<HostGroup> = {
        let mut stmt = conn.prepare(
            "SELECT id, tag, name, kind, ident_is_user, note, created_at
             FROM host_groups ORDER BY tag, name",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(HostGroup {
                    id: row.get(0)?,
                    tag: row.get(1)?,
                    name: row.get(2)?,
                    kind: row.get(3)?,
                    ident_is_user: row.get(4)?,
                    note: row.get(5)?,
                    created_at: row.get(6)?,
                    members: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    let mut stmt = conn.prepare(
        "SELECT m.host, m.note, m.created_at,
                (SELECT count(DISTINCT k.identity_id) FROM identity_keys k
                 WHERE k.tag = m.tag AND k.host = m.host COLLATE NOCASE)
         FROM host_group_members m WHERE m.group_id = ?1 ORDER BY m.host",
    )?;
    for group in &mut groups {
        group.members = stmt
            .query_map(params![group.id], |row| {
                Ok(GroupMember {
                    host: row.get(0)?,
                    note: row.get(1)?,
                    created_at: row.get(2)?,
                    identities: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
    }
    Ok(groups)
}

/// The host group each grouped host is in, by tag and ASCII-lowercased host.
pub fn group_of_hosts(conn: &Connection) -> Result<HashMap<(String, String), String>> {
    let mut stmt = conn.prepare(
        "SELECT m.tag, lower(m.host), g.name
         FROM host_group_members m JOIN host_groups g ON g.id = m.group_id",
    )?;
    let rows = stmt
        .query_map([], |row| Ok(((row.get(0)?, row.get(1)?), row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// A host matching a search, with the group it is in.
pub struct MatchingHost {
    pub tag: String,
    pub host: String,
    /// Identities that used the host.
    pub identities: i64,
    /// The host group it is in, if any.
    pub group: Option<String>,
}

/// The hosts matching a pattern with `*` wildcards, ignoring case, the ones
/// used by the most identities first.
pub fn matching_hosts(conn: &Connection, pattern: &str, limit: i64) -> Result<Vec<MatchingHost>> {
    let mut stmt = conn.prepare(
        r"SELECT k.tag, k.host, count(DISTINCT k.identity_id),
                 (SELECT g.name FROM host_group_members m JOIN host_groups g ON g.id = m.group_id
                  WHERE m.tag = k.tag AND m.host = k.host)
          FROM identity_keys k
          WHERE k.host LIKE ?1 ESCAPE '\' AND k.host <> '' AND k.host NOT LIKE '#%'
          GROUP BY k.tag, k.host
          ORDER BY 3 DESC, 1, 2
          LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![lookup::like_pattern(pattern), limit], |row| {
            Ok(MatchingHost {
                tag: row.get(0)?,
                host: row.get(1)?,
                identities: row.get(2)?,
                group: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// A host used by more than one identity and not in a host group: possibly a
/// bridge, a bouncer, a shared connection or many accounts of one person.
pub struct SharedHost {
    pub tag: String,
    pub host: String,
    pub identities: i64,
    pub idents: i64,
    pub nicks: i64,
}

/// Shared hosts, the ones used by the most identities first.
pub fn shared_hosts(conn: &Connection, limit: i64) -> Result<Vec<SharedHost>> {
    let mut stmt = conn.prepare(
        "SELECT k.tag, k.host, count(DISTINCT k.identity_id), count(DISTINCT k.ident),
                count(DISTINCT k.nick)
         FROM identity_keys k
         WHERE k.host <> '' AND k.host NOT LIKE '#%'
           AND NOT EXISTS (SELECT 1 FROM host_group_members m
                           WHERE m.tag = k.tag AND m.host = k.host)
         GROUP BY k.tag, k.host
         HAVING count(DISTINCT k.identity_id) > 1
         ORDER BY 3 DESC, 1, 2
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit], |row| {
            Ok(SharedHost {
                tag: row.get(0)?,
                host: row.get(1)?,
                identities: row.get(2)?,
                idents: row.get(3)?,
                nicks: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// What a person record stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A human.
    Person,
    /// A bot, left out of activity rankings unless they ask for bots.
    Bot,
}

impl Kind {
    /// The value stored in `people.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Person => "person",
            Kind::Bot => "bot",
        }
    }

    pub fn parse(value: &str) -> Option<Kind> {
        match value {
            "person" => Some(Kind::Person),
            "bot" => Some(Kind::Bot),
            _ => None,
        }
    }

    /// The stored value read back, which the schema limits to the two kinds.
    fn stored(value: &str) -> Kind {
        Kind::parse(value).unwrap_or(Kind::Person)
    }
}

/// How well you know a person, from the least trusted to the most.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rating {
    Distrusted,
    Wary,
    Neutral,
    Vouched,
    Known,
}

impl Rating {
    /// From the least trusted to the most.
    pub const ALL: [Rating; 5] = [
        Rating::Distrusted,
        Rating::Wary,
        Rating::Neutral,
        Rating::Vouched,
        Rating::Known,
    ];

    /// The value stored in `people.rating`.
    pub fn as_str(self) -> &'static str {
        match self {
            Rating::Distrusted => "distrusted",
            Rating::Wary => "wary",
            Rating::Neutral => "neutral",
            Rating::Vouched => "vouched",
            Rating::Known => "known",
        }
    }

    pub fn parse(value: &str) -> Option<Rating> {
        Rating::ALL
            .into_iter()
            .find(|rating| rating.as_str() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            Rating::Distrusted => "Distrusted",
            Rating::Wary => "Wary",
            Rating::Neutral => "Neutral",
            Rating::Vouched => "Vouched",
            Rating::Known => "Known",
        }
    }

    /// What the rating means, for whoever chooses it.
    pub fn explanation(self) -> &'static str {
        match self {
            Rating::Distrusted => {
                "You know they are not who they claim to be, or should not be here."
            }
            Rating::Wary => "Something about them seems off.",
            Rating::Neutral => "No opinion yet. Everyone starts here.",
            Rating::Vouched => "Someone you trust invited them or vouches for them.",
            Rating::Known => "You know them personally, or have for years.",
        }
    }

    /// Vouched and known.
    pub fn trusted(self) -> bool {
        self >= Rating::Vouched
    }

    /// Wary and distrusted.
    pub fn untrusted(self) -> bool {
        self <= Rating::Wary
    }

    /// The stored value read back, which the schema limits to the five.
    fn stored(value: &str) -> Rating {
        Rating::parse(value).unwrap_or(Rating::Neutral)
    }
}

/// A person record: a human or a bot, with its identities.
pub struct Person {
    pub name: String,
    pub kind: Kind,
    /// Always neutral for bots.
    pub rating: Rating,
    /// The person who invited or vouches for this one.
    pub invited_by: Option<String>,
    /// The people this one invited, by name.
    pub invited: Vec<String>,
    /// The note given when the record was created.
    pub note: Option<String>,
    /// Notes added since, oldest first.
    pub notes: Vec<Note>,
    pub created_at: String,
    pub identities: Vec<PersonIdentity>,
}

impl Person {
    /// The current ids of its identities.
    pub fn identity_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .identities
            .iter()
            .map(|identity| identity.current.clone())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

pub struct Note {
    pub id: i64,
    pub text: String,
    /// UTC, as `YYYY-MM-DD HH:MM:SS`.
    pub created_at: String,
}

pub struct PersonIdentity {
    /// The id the identity was added under.
    pub recorded: String,
    /// The id it stands for now, after any merges.
    pub current: String,
    pub tag: Option<String>,
    pub nicks: Vec<String>,
    /// `YYYY-MM-DD HH:MM`.
    pub last_seen: Option<String>,
}

/// Everyone, humans and bots, by name.
pub fn people(conn: &Connection) -> Result<Vec<Person>> {
    load_people(conn, None)
}

/// One person by name, ignoring case.
pub fn person(conn: &Connection, name: &str) -> Result<Option<Person>> {
    Ok(load_people(conn, Some(name))?.into_iter().next())
}

/// A row of `people`, with the name of whoever invited them.
struct PersonRow {
    id: i64,
    name: String,
    kind: String,
    rating: String,
    invited_by: Option<String>,
    note: Option<String>,
    created_at: String,
}

fn load_people(conn: &Connection, name: Option<&str>) -> Result<Vec<Person>> {
    let merges = lookup::merges(conn)?;
    let rows: Vec<PersonRow> = {
        let mut stmt = conn.prepare(
            "SELECT p.id, p.name, p.kind, p.rating, i.name, p.note, p.created_at
             FROM people p LEFT JOIN people i ON i.id = p.invited_by
             WHERE ?1 IS NULL OR p.name = ?1 ORDER BY p.name",
        )?;
        let rows = stmt
            .query_map(params![name], |row| {
                Ok(PersonRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    rating: row.get(3)?,
                    invited_by: row.get(4)?,
                    note: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    let mut notes = conn.prepare(
        "SELECT id, text, created_at FROM person_notes WHERE person_id = ?1
         ORDER BY created_at, id",
    )?;
    let mut invited =
        conn.prepare("SELECT name FROM people WHERE invited_by = ?1 ORDER BY name")?;
    let mut members =
        conn.prepare("SELECT identity_id FROM person_identities WHERE person_id = ?1 ORDER BY 1")?;
    let mut nicks = conn
        .prepare("SELECT DISTINCT nick FROM identity_keys WHERE identity_id = ?1 ORDER BY nick")?;
    let mut seen = conn.prepare(
        "SELECT i.tag, (SELECT max(s.end_at) FROM sessions s WHERE s.identity_id = i.id)
         FROM identities i WHERE i.id = ?1",
    )?;

    let mut people = Vec::with_capacity(rows.len());
    for row in rows {
        let recorded: Vec<String> = members
            .query_map(params![row.id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut identities = Vec::with_capacity(recorded.len());
        for recorded in recorded {
            let current = lookup::current_id(&merges, &recorded);
            let nick_list: Vec<String> = nicks
                .query_map(params![current], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let (tag, last_seen) = seen
                .query_row(params![current], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()?
                .unwrap_or((None, None));
            identities.push(PersonIdentity {
                recorded,
                current,
                tag,
                nicks: nick_list,
                last_seen,
            });
        }
        let kind = Kind::stored(&row.kind);
        people.push(Person {
            name: row.name,
            kind,
            rating: match kind {
                Kind::Person => Rating::stored(&row.rating),
                Kind::Bot => Rating::Neutral,
            },
            invited_by: row.invited_by,
            invited: invited
                .query_map(params![row.id], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?,
            note: row.note,
            notes: notes
                .query_map(params![row.id], |row| {
                    Ok(Note {
                        id: row.get(0)?,
                        text: row.get(1)?,
                        created_at: row.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?,
            created_at: row.created_at,
            identities,
        });
    }
    Ok(people)
}

/// The names of everyone, humans and bots, with their kind, sorted by name.
pub fn people_names(conn: &Connection) -> Result<Vec<(String, Kind)>> {
    let mut stmt = conn.prepare("SELECT name, kind FROM people ORDER BY name")?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows
        .into_iter()
        .map(|(name, kind)| (name, Kind::stored(&kind)))
        .collect())
}

pub struct Cut {
    pub id: i64,
    pub path: String,
    /// `tag/target.log`, or the path when the file is no longer imported.
    pub file: String,
    pub line: i64,
    /// The nick change the cut was made on.
    pub expected: String,
    /// Whether the line still holds that nick change, so the cut applies.
    pub applied: bool,
    pub note: Option<String>,
    pub created_at: String,
}

pub fn cuts(conn: &Connection) -> Result<Vec<Cut>> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.path, f.tag, f.target, c.line, c.expected, c.note, c.created_at,
                e.kind, e.nick, e.other_nick
         FROM session_cuts c
         LEFT JOIN files f ON f.path = c.path
         LEFT JOIN events e ON e.file_id = f.id AND e.line = c.line
         ORDER BY c.id",
    )?;
    let rows = stmt
        .query_map([], |row| {
            let path: String = row.get(1)?;
            let tag: Option<String> = row.get(2)?;
            let target: Option<String> = row.get(3)?;
            let expected: String = row.get(5)?;
            let kind: Option<String> = row.get(8)?;
            let nick: Option<String> = row.get(9)?;
            let new: Option<String> = row.get(10)?;
            let actual = kind.and_then(|kind| {
                identity::nick_change_text(&kind, nick.as_deref(), new.as_deref())
            });
            Ok(Cut {
                id: row.get(0)?,
                file: match (tag, target) {
                    (Some(tag), Some(target)) => format!("{tag}/{target}.log"),
                    _ => path.clone(),
                },
                path,
                line: row.get(4)?,
                applied: actual.as_deref() == Some(expected.as_str()),
                expected,
                note: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::decide;
    use crate::discover::{self, tests::TempDir};
    use crate::{db, identity};

    fn setup(name: &str) -> (TempDir, Connection) {
        let root = TempDir::new(name);
        fs::create_dir_all(root.0.join("wetfish")).unwrap();
        fs::write(
            root.0.join("wetfish/#analysis.log"),
            "--- Log opened Sun Nov 01 01:00:00 2026\n\
             01:00 -!- rachel [rachel@hostR] has joined #analysis\n\
             01:00 -!- Irssi: #analysis: Total of 1 nicks [1 ops, 0 halfops, 0 voices, 0 normal]\n\
             01:00 -!- Irssi: Join to #analysis was synced in 0 secs\n\
             01:01 <@rachel> hi\n\
             01:02 -!- quokka [quokkawet@hostQ] has joined #analysis\n\
             01:03 -!- marlowe [marlowewet@hostM] has joined #analysis\n\
             01:04 <marlowe> hello\n\
             01:05  * Marlowe waves\n\
             --- Day changed Mon Nov 02 2026\n\
             09:00 <marlowe> morning\n",
        )
        .unwrap();
        fs::write(
            root.0.join("wetfish/marlowe.log"),
            "--- Log opened Sun Nov 01 02:00:00 2026\n02:00 <marlowe> psst\n02:01 <rachel> hey\n",
        )
        .unwrap();
        let mut conn = db::open(&root.0.join("wot.sqlite")).unwrap();
        let inventory = discover::discover(&[root.0.clone()]).unwrap();
        db::import(&mut conn, &inventory.files);
        identity::rebuild(&mut conn).unwrap();
        (root, conn)
    }

    #[test]
    fn summarizes_channels_and_private_messages() {
        let (_root, conn) = setup("stats-overview");
        let overview = overview(&conn).unwrap();
        assert_eq!((overview.files, overview.lines), (2, 14));
        assert!(overview.last_import.is_some());

        assert_eq!(overview.tags.len(), 1);
        let tag = &overview.tags[0];
        assert_eq!(tag.tag, "wetfish");
        assert_eq!(tag.identities, 3);

        assert_eq!(tag.channels.len(), 1);
        let channel = &tag.channels[0];
        assert_eq!(channel.name, "#analysis");
        assert_eq!((channel.lines, channel.spoken, channel.joins), (11, 4, 3));
        // The logging client is left out: quokka and marlowe were seen, and
        // only marlowe spoke.
        assert_eq!((channel.identities, channel.speakers), (2, 1));
        assert_eq!(channel.first.as_deref(), Some("2026-11-01 01:00"));
        assert_eq!(channel.last.as_deref(), Some("2026-11-02 09:00"));

        assert_eq!(tag.queries.targets, 1);
        assert_eq!((tag.queries.lines, tag.queries.spoken), (3, 2));
    }

    #[test]
    fn counts_spoken_lines_and_nicks_by_day() {
        let (_root, conn) = setup("stats-days");
        assert!(channel_days(&conn, "wetfish", "#nope").unwrap().is_none());

        let channel = channel_days(&conn, "wetfish", "#analysis")
            .unwrap()
            .unwrap();
        assert_eq!(channel.paths.len(), 1);
        let days: Vec<(&str, i64, i64)> = channel
            .days
            .iter()
            .map(|day| (day.date.as_str(), day.spoken, day.nicks))
            .collect();
        assert_eq!(days, [("2026-11-01", 3, 2), ("2026-11-02", 1, 1)]);
        assert_eq!(channel.nicks, 2);
    }

    #[test]
    fn lists_decisions_shared_hosts_and_nick_changes() {
        let (root, mut conn) = setup("stats-decisions");
        fs::write(
            root.0.join("wetfish/#other.log"),
            "--- Log opened Sun Nov 01 03:00:00 2026\n\
             03:00 -!- juniper [jun@shared.host] has joined #other\n\
             03:01 -!- tamsin [tam@shared.host] has joined #other\n\
             03:02 -!- tamsin is now known as tamsin_\n\
             03:03 -!- tamsin_ is now known as tamsin__\n",
        )
        .unwrap();
        let inventory = discover::discover(&[root.0.clone()]).unwrap();
        db::import(&mut conn, &inventory.files);
        identity::rebuild(&mut conn).unwrap();

        let matching = matching_hosts(&conn, "SHARED*", 10).unwrap();
        assert_eq!(matching.len(), 1);
        assert_eq!(
            (matching[0].identities, matching[0].group.as_deref()),
            (2, None)
        );

        let shared = shared_hosts(&conn, 10).unwrap();
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].host, "shared.host");
        assert_eq!((shared[0].identities, shared[0].idents), (2, 2));

        // A grouped host is no longer listed as shared.
        decide::add_group(&conn, "wetfish", "matrix", "bridge", true, None).unwrap();
        decide::add_hosts(
            &mut conn,
            "wetfish",
            "matrix",
            &["SHARED.host".into()],
            None,
        )
        .unwrap();
        assert!(shared_hosts(&conn, 10).unwrap().is_empty());
        let matching = matching_hosts(&conn, "shared.host", 10).unwrap();
        assert_eq!(matching[0].group.as_deref(), Some("matrix"));
        let groups = host_groups(&conn).unwrap();
        assert_eq!(groups[0].members[0].host, "SHARED.host");
        assert_eq!(groups[0].members[0].identities, 2);
        assert_eq!(
            group_of_hosts(&conn)
                .unwrap()
                .get(&("wetfish".into(), "shared.host".into())),
            Some(&"matrix".to_string())
        );

        let (file_id, start, end): (i64, i64, Option<i64>) = conn
            .query_row(
                "SELECT file_id, start_line, end_line FROM sessions WHERE nicks LIKE 'tamsin %'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        decide::add_cut(&conn, "wetfish/#other.log:5", None).unwrap();
        let nicks = ["tamsin".to_string(), "tamsin_".into(), "tamsin__".into()];
        let changes = nick_changes(&conn, file_id, start, end, &nicks).unwrap();
        let found: Vec<(i64, &str, bool)> = changes
            .iter()
            .map(|change| (change.line, change.text.as_str(), change.cut))
            .collect();
        assert_eq!(
            found,
            [
                (4, "tamsin is now known as tamsin_", false),
                (5, "tamsin_ is now known as tamsin__", true),
            ]
        );
        let cuts = cuts(&conn).unwrap();
        assert_eq!(
            (cuts[0].file.as_str(), cuts[0].line),
            ("wetfish/#other.log", 5)
        );
        assert!(cuts[0].applied);

        let marlowe: String = conn
            .query_row(
                "SELECT identity_id FROM identity_keys WHERE nick = 'marlowe'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        decide::create_person(&mut conn, "marlowe", Kind::Bot, &[marlowe.clone()], None).unwrap();
        let person = person(&conn, "MARLOWE").unwrap().unwrap();
        assert_eq!(person.kind, Kind::Bot);
        assert_eq!(person.identities[0].current, marlowe);
        assert_eq!(person.identities[0].nicks, ["marlowe"]);
        assert_eq!(
            people_names(&conn).unwrap(),
            [("marlowe".to_string(), Kind::Bot)]
        );
        assert_eq!(tags(&conn).unwrap(), ["wetfish"]);
    }
}
