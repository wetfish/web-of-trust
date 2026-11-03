//! History: the days a set of identities was present in channels, and what
//! it said there, line by line. Lines are found through each session's nick
//! spans, so a nick taken by someone else after a nick change is never
//! mistaken for the identity. Private messages are left out, as in activity.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Days, NaiveDate, NaiveDateTime};
use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};

use crate::db::Result;

/// Lines shown on one page of the history.
pub const PAGE_SIZE: usize = 200;
/// A session longer than this counts only its first and last days, so one
/// broken session cannot fill years.
const MAX_SESSION_DAYS: u64 = 366;

/// One day in channels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Day {
    /// Channels with a session that day, sorted.
    pub channels: BTreeSet<String>,
    /// Lines said that day.
    pub spoken: i64,
}

/// Every day the identities were present in a channel, with the lines they
/// said, oldest first.
pub fn days(conn: &Connection, identities: &[String]) -> Result<BTreeMap<NaiveDate, Day>> {
    let mut days: BTreeMap<NaiveDate, Day> = BTreeMap::new();
    if identities.is_empty() {
        return Ok(days);
    }
    let ids = placeholders(1, identities.len());
    let values: Vec<Value> = identities.iter().cloned().map(Value::from).collect();

    let mut stmt = conn.prepare(&format!(
        "SELECT s.start_at, s.end_at, f.target FROM sessions s JOIN files f ON f.id = s.file_id
         WHERE s.identity_id IN ({ids}) AND s.logger = 0 AND f.kind = 'channel'"
    ))?;
    let sessions: Vec<(String, String, String)> = stmt
        .query_map(params_from_iter(values.iter()), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (start, end, channel) in sessions {
        let (Some(start), Some(end)) = (day_of(&start), day_of(&end)) else {
            continue;
        };
        let span = (end - start).num_days().max(0) as u64;
        let mut add = |day: NaiveDate| {
            days.entry(day)
                .or_default()
                .channels
                .insert(channel.clone());
        };
        if span > MAX_SESSION_DAYS {
            add(start);
            add(end);
            continue;
        }
        let mut day = start;
        while day <= end {
            add(day);
            let Some(next) = day.checked_add_days(Days::new(1)) else {
                break;
            };
            day = next;
        }
    }

    let mut stmt = conn.prepare(&format!(
        "SELECT e.date, f.target, count(*) {SPOKEN_FROM}
         WHERE s.identity_id IN ({ids}) AND e.date IS NOT NULL
         GROUP BY e.date, f.target"
    ))?;
    let spoken: Vec<(String, String, i64)> = stmt
        .query_map(params_from_iter(values.iter()), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (date, channel, count) in spoken {
        let Ok(date) = NaiveDate::parse_from_str(&date, "%Y-%m-%d") else {
            continue;
        };
        let day = days.entry(date).or_default();
        day.channels.insert(channel);
        day.spoken += count;
    }
    Ok(days)
}

/// The lines of channel logs said by a session while it held the nick, apart
/// from the logging client's.
const SPOKEN_FROM: &str = "
    FROM sessions s
    JOIN session_nicks n ON n.session_id = s.id
    JOIN files f ON f.id = s.file_id AND f.kind = 'channel'
    JOIN events e ON e.file_id = s.file_id AND e.line >= n.from_line
                 AND (n.to_line IS NULL OR e.line <= n.to_line)
                 AND e.kind IN ('message', 'action') AND lower(e.nick) = n.nick";

/// `?first` to `?first + count - 1`, separated by commas.
fn placeholders(first: usize, count: usize) -> String {
    (first..first + count)
        .map(|n| format!("?{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The day of a `YYYY-MM-DD HH:MM` timestamp.
fn day_of(at: &str) -> Option<NaiveDate> {
    NaiveDateTime::parse_from_str(at, "%Y-%m-%d %H:%M")
        .ok()
        .map(|at| at.date())
}

/// Which lines to show.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    /// `YYYY-MM-DD`.
    pub day: Option<String>,
    pub channel: Option<String>,
    /// Text the line contains, ignoring case for ASCII letters.
    pub text: Option<String>,
    /// From 0, `PAGE_SIZE` lines each, most recent first.
    pub page: usize,
}

/// One line said in a channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// `YYYY-MM-DD`, or empty before the log gave a date.
    pub date: String,
    /// `HH:MM`.
    pub time: String,
    pub channel: String,
    pub nick: String,
    /// An action, shown as `* nick text`.
    pub action: bool,
    pub text: String,
}

/// A page of lines, most recent first, and whether older lines follow.
pub fn lines(
    conn: &Connection,
    identities: &[String],
    filter: &Filter,
) -> Result<(Vec<Line>, bool)> {
    if identities.is_empty() {
        return Ok((Vec::new(), false));
    }
    let mut values: Vec<Value> = identities.iter().cloned().map(Value::from).collect();
    let mut conditions = vec![format!(
        "s.identity_id IN ({})",
        placeholders(1, identities.len())
    )];
    if let Some(day) = &filter.day {
        values.push(Value::from(day.clone()));
        conditions.push(format!("e.date = ?{}", values.len()));
    }
    if let Some(channel) = &filter.channel {
        values.push(Value::from(channel.clone()));
        conditions.push(format!("f.target = ?{} COLLATE NOCASE", values.len()));
    }
    if let Some(text) = &filter.text {
        values.push(Value::from(format!("%{}%", escape_like(text))));
        conditions.push(format!("e.text LIKE ?{} ESCAPE '\\'", values.len()));
    }
    // One more than a page, to know whether another page follows.
    values.push(Value::from(PAGE_SIZE as i64 + 1));
    let limit = values.len();
    values.push(Value::from((filter.page * PAGE_SIZE) as i64));
    let offset = values.len();
    let mut stmt = conn.prepare(&format!(
        "SELECT coalesce(e.date, ''), coalesce(e.time, ''), f.target, e.nick, e.kind,
                coalesce(e.text, '')
         {SPOKEN_FROM}
         WHERE {}
         ORDER BY e.date DESC, e.time DESC, e.file_id, e.line DESC
         LIMIT ?{limit} OFFSET ?{offset}",
        conditions.join(" AND ")
    ))?;
    let mut lines: Vec<Line> = stmt
        .query_map(params_from_iter(values.iter()), |row| {
            Ok(Line {
                date: row.get(0)?,
                time: row.get(1)?,
                channel: row.get(2)?,
                nick: row.get(3)?,
                action: row.get::<_, String>(4)? == "action",
                text: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let more = lines.len() > PAGE_SIZE;
    lines.truncate(PAGE_SIZE);
    Ok((lines, more))
}

/// Escapes `%`, `_` and `\` for a LIKE pattern that uses `\` as its escape.
fn escape_like(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rusqlite::params;

    use super::*;
    use crate::discover::{self, tests::TempDir};
    use crate::{db, identity};

    #[test]
    fn finds_what_an_identity_said_and_on_which_days() {
        let root = TempDir::new("history");
        fs::create_dir_all(root.0.join("wetfish")).unwrap();
        fs::write(
            root.0.join("wetfish/#analysis.log"),
            "--- Log opened Sun Nov 01 22:00:00 2026\n\
             22:00 -!- rachel [rachel@hostR] has joined #analysis\n\
             22:00 -!- Irssi: #analysis: Total of 1 nicks [1 ops, 0 halfops, 0 voices, 0 normal]\n\
             22:00 -!- Irssi: Join to #analysis was synced in 0 secs\n\
             22:10 -!- quokka [quokkawet@hostQ] has joined #analysis\n\
             22:11 <quokka> hello 100%\n\
             22:12 -!- quokka is now known as quokka_away\n\
             22:13 -!- marlowe [marlowewet@hostM] has joined #analysis\n\
             22:14 -!- marlowe is now known as quokka\n\
             22:15 <quokka> not the same person\n\
             --- Day changed Mon Nov 02 2026\n\
             00:01  * quokka_away waves\n\
             00:02 <@rachel> hi\n\
             --- Log closed Mon Nov 02 01:00:00 2026\n",
        )
        .unwrap();
        fs::write(
            root.0.join("wetfish/quokka.log"),
            "--- Log opened Mon Nov 02 03:00:00 2026\n03:00 <quokka> private\n",
        )
        .unwrap();
        let mut conn = db::open(&root.0.join("wot.sqlite")).unwrap();
        let inventory = discover::discover(&[root.0.clone()]).unwrap();
        db::import(&mut conn, &inventory.files);
        identity::rebuild(&mut conn).unwrap();
        let id = |nick: &str| -> String {
            conn.query_row(
                "SELECT identity_id FROM identity_keys WHERE nick = ?1",
                params![nick],
                |row| row.get(0),
            )
            .unwrap()
        };
        let quokka = vec![id("quokka_away")];
        let marlowe = vec![id("marlowe")];
        assert_ne!(quokka, marlowe);

        // marlowe took the nick quokka after quokka left it, and quokka's
        // private message is left out.
        let (lines, more) = lines(&conn, &quokka, &Filter::default()).unwrap();
        assert!(!more);
        let said: Vec<(&str, &str, bool, &str)> = lines
            .iter()
            .map(|l| (l.date.as_str(), l.nick.as_str(), l.action, l.text.as_str()))
            .collect();
        assert_eq!(
            said,
            [
                ("2026-11-02", "quokka_away", true, "waves"),
                ("2026-11-01", "quokka", false, "hello 100%"),
            ]
        );
        let (theirs, _) = self::lines(&conn, &marlowe, &Filter::default()).unwrap();
        assert_eq!(theirs.len(), 1);
        assert_eq!(theirs[0].text, "not the same person");

        // Filters, with % searched for as itself.
        let only = |filter: Filter| self::lines(&conn, &quokka, &filter).unwrap().0.len();
        let day = Some("2026-11-01".to_string());
        assert_eq!(
            only(Filter {
                day,
                ..Filter::default()
            }),
            1
        );
        let text = Some("100%".to_string());
        assert_eq!(
            only(Filter {
                text,
                ..Filter::default()
            }),
            1
        );
        let text = Some("0%x".to_string());
        assert_eq!(
            only(Filter {
                text,
                ..Filter::default()
            }),
            0
        );
        let channel = Some("#ANALYSIS".to_string());
        assert_eq!(
            only(Filter {
                channel,
                ..Filter::default()
            }),
            2
        );
        assert_eq!(
            only(Filter {
                page: 1,
                ..Filter::default()
            }),
            0
        );

        // Present on both days, and spoke on both.
        let days = days(&conn, &quokka).unwrap();
        let summary: Vec<(String, i64)> = days
            .iter()
            .map(|(date, day)| (date.to_string(), day.spoken))
            .collect();
        assert_eq!(
            summary,
            [("2026-11-01".to_string(), 1), ("2026-11-02".to_string(), 1)]
        );
        assert!(days.values().all(|day| day.channels.contains("#analysis")));
    }

    #[test]
    fn escapes_like_patterns() {
        assert_eq!(escape_like(r"50%_off\now"), r"50\%\_off\\now");
    }
}
