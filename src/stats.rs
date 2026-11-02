//! Statistics read from the database for the review pages. Unlike the
//! inventory and daily activity reports of `wot analyze`, these never read
//! the log files, only what was imported.

use std::collections::HashMap;

use rusqlite::{Connection, params};

use crate::db::Result;

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
    pub people: i64,
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

    // Identities seen and identities that spoke, by tag and channel.
    let mut people_in: HashMap<(String, String), (i64, i64)> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT f.tag, f.target, count(DISTINCT s.identity_id),
                    count(DISTINCT CASE WHEN s.spoken > 0 THEN s.identity_id END)
             FROM sessions s JOIN files f ON f.id = s.file_id
             WHERE f.kind = 'channel' AND s.logger = 0
             GROUP BY f.tag, f.target",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            people_in.insert((row.get(0)?, row.get(1)?), (row.get(2)?, row.get(3)?));
        }
    }

    let mut tags: Vec<TagSummary> = Vec::new();
    let mut stmt = conn.prepare(
        "WITH per_file AS (
             SELECT file_id, sum(spoken) AS spoken, sum(joined) AS joins,
                    min(start_at) AS first, max(end_at) AS last
             FROM sessions GROUP BY file_id
         )
         SELECT f.tag, f.kind, f.target, count(*), sum(f.lines_read),
                ifnull(sum(p.spoken), 0), ifnull(sum(p.joins), 0), min(p.first), max(p.last)
         FROM files f LEFT JOIN per_file p ON p.file_id = f.id
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
        identities: count(
            "SELECT count(DISTINCT s.identity_id) FROM sessions s
             JOIN files f ON f.id = s.file_id WHERE f.tag = ?1",
        )?,
        people: count(
            "SELECT count(DISTINCT pi.person_id) FROM person_identities pi
             JOIN identities i ON i.id = pi.identity_id WHERE i.tag = ?1",
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
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
}
