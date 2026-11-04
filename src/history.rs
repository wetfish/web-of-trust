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
         WHERE s.identity_id IN ({ids}) AND f.kind = 'channel'"
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

/// The lines of channel logs said by a session while it held the nick, the
/// logging client's included, apart from the lines left out: second copies
/// of lines logged twice, and a bouncer's own lines.
///
/// The spoken lines index finds a span's lines by file and nick, so only the
/// session's own lines are read, not every line it heard. Naming the index
/// keeps SQLite from reading the span through the line index instead, which
/// it does without statistics. The span's last line is always given, open
/// spans running to the largest line number: with `to_line IS NULL OR ...`,
/// SQLite could not use it as a bound and read every span to the end of its
/// file. Nicks compare ignoring case, as `lower()` does.
const SPOKEN_FROM: &str = "
    FROM sessions s
    JOIN session_nicks n ON n.session_id = s.id
    JOIN files f ON f.id = s.file_id AND f.kind = 'channel'
    JOIN events e INDEXED BY events_spoken
         ON e.file_id = s.file_id AND e.nick = n.nick COLLATE NOCASE
        AND e.line >= n.from_line AND e.line <= coalesce(n.to_line, 9223372036854775807)
        AND e.kind IN ('message', 'action')
        AND NOT EXISTS (SELECT 1 FROM left_out_lines d WHERE d.event_id = e.id)";

/// A span's lines, newest first, through the spoken lines index, with an
/// optional day and text: the same lines `SPOKEN_FROM` finds.
const SPAN_LINES: &str = "
    SELECT coalesce(e.date, ''), coalesce(e.time, ''), e.nick, e.kind, coalesce(e.text, ''),
           e.line
    FROM events e INDEXED BY events_spoken
    WHERE e.file_id = ?1 AND e.nick = ?2 COLLATE NOCASE AND e.line BETWEEN ?3 AND ?4
      AND e.kind IN ('message', 'action')
      AND NOT EXISTS (SELECT 1 FROM left_out_lines d WHERE d.event_id = e.id)
      AND (?6 IS NULL OR e.date = ?6)
      AND (?7 IS NULL OR e.text LIKE ?7 ESCAPE '\\')
    ORDER BY e.line DESC
    LIMIT ?5";

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

/// A session in a channel that spoke, for finding its lines.
struct Spoke {
    id: i64,
    file_id: i64,
    /// `YYYY-MM-DD HH:MM`: none of its lines is later.
    end_at: String,
    channel: String,
}

/// A line, with the file and line number that order lines of the same
/// minute.
struct Found {
    line: Line,
    file_id: i64,
    number: i64,
}

impl Found {
    /// When it was said, as `YYYY-MM-DD HH:MM`, comparable with a session's
    /// end.
    fn at(&self) -> String {
        format!("{} {}", self.line.date, self.line.time)
    }
}

/// Most recent first: by day and time, then by file and line, the latest
/// line of a file first.
fn newest_first(found: &mut [Found]) {
    found.sort_by(|a, b| {
        b.line
            .date
            .cmp(&a.line.date)
            .then_with(|| b.line.time.cmp(&a.line.time))
            .then(a.file_id.cmp(&b.file_id))
            .then(b.number.cmp(&a.number))
    });
}

/// A page of lines, most recent first, and whether older lines follow.
///
/// Rather than sorting everything the identities ever said, which for
/// someone talkative is over a million lines, sessions are read the most
/// recently ended first, each newest line first. No line is later than the
/// end of its session, so once enough lines are found that are later than
/// the end of the next session, no older session can add one.
pub fn lines(
    conn: &Connection,
    identities: &[String],
    filter: &Filter,
) -> Result<(Vec<Line>, bool)> {
    if identities.is_empty() {
        return Ok((Vec::new(), false));
    }
    let mut values: Vec<Value> = identities.iter().cloned().map(Value::from).collect();
    let mut conditions = vec![
        format!("s.identity_id IN ({})", placeholders(1, identities.len())),
        "s.spoken > 0".to_string(),
    ];
    if let Some(day) = &filter.day {
        values.push(Value::from(day.clone()));
        let n = values.len();
        conditions.push(format!(
            "substr(s.start_at, 1, 10) <= ?{n} AND substr(s.end_at, 1, 10) >= ?{n}"
        ));
    }
    if let Some(channel) = &filter.channel {
        values.push(Value::from(channel.clone()));
        conditions.push(format!("f.target = ?{} COLLATE NOCASE", values.len()));
    }
    let sessions: Vec<Spoke> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT s.id, s.file_id, s.end_at, f.target FROM sessions s
             JOIN files f ON f.id = s.file_id AND f.kind = 'channel'
             WHERE {} ORDER BY s.end_at DESC, s.id",
            conditions.join(" AND ")
        ))?;
        let rows = stmt
            .query_map(params_from_iter(values.iter()), |row| {
                Ok(Spoke {
                    id: row.get(0)?,
                    file_id: row.get(1)?,
                    end_at: row.get(2)?,
                    channel: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };

    let mut spans = conn.prepare(
        "SELECT nick, from_line, coalesce(to_line, 9223372036854775807)
         FROM session_nicks WHERE session_id = ?1",
    )?;
    let mut said = conn.prepare(SPAN_LINES)?;
    let text = filter
        .text
        .as_ref()
        .map(|text| format!("%{}%", escape_like(text)));
    // This page and the one line after it, which says another page follows.
    let wanted = (filter.page + 1) * PAGE_SIZE + 1;
    let mut found: Vec<Found> = Vec::new();
    for session in &sessions {
        if found.len() >= wanted {
            newest_first(&mut found);
            found.truncate(wanted);
            if session.end_at < found[wanted - 1].at() {
                break;
            }
        }
        let nick_spans: Vec<(String, i64, i64)> = spans
            .query_map(params_from_iter([Value::from(session.id)]), |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        for (nick, from, to) in nick_spans {
            let params = [
                Value::from(session.file_id),
                Value::from(nick),
                Value::from(from),
                Value::from(to),
                Value::from(wanted as i64),
                filter.day.clone().map_or(Value::Null, Value::from),
                text.clone().map_or(Value::Null, Value::from),
            ];
            let rows = said.query_map(params_from_iter(params.iter()), |row| {
                Ok(Found {
                    line: Line {
                        date: row.get(0)?,
                        time: row.get(1)?,
                        channel: session.channel.clone(),
                        nick: row.get(2)?,
                        action: row.get::<_, String>(3)? == "action",
                        text: row.get(4)?,
                    },
                    file_id: session.file_id,
                    number: row.get(5)?,
                })
            })?;
            for row in rows {
                found.push(row?);
            }
        }
    }
    newest_first(&mut found);
    let start = filter.page * PAGE_SIZE;
    let more = found.len() > start + PAGE_SIZE;
    let lines = found
        .into_iter()
        .skip(start)
        .take(PAGE_SIZE)
        .map(|found| found.line)
        .collect();
    Ok((lines, more))
}

/// Escapes `%`, `_` and `\` for a LIKE pattern that uses `\` as its escape.
pub fn escape_like(text: &str) -> String {
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
        // The logging client's lines count like anyone else's.
        let rachel = vec![id("rachel")];
        let (own, _) = self::lines(&conn, &rachel, &Filter::default()).unwrap();
        assert_eq!(own.len(), 1);
        assert_eq!(own[0].text, "hi");
        assert_eq!(days(&conn, &rachel).unwrap().len(), 2);

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
    fn pages_lines_from_overlapping_sessions_newest_first() {
        let root = TempDir::new("history-pages");
        fs::create_dir_all(root.0.join("wetfish")).unwrap();
        // quokka in two channels at once, one line a minute, alternating,
        // and leaving #a and coming back halfway, for three sessions.
        let at = |minute: usize| format!("{:02}:{:02}", 10 + minute / 60, minute % 60);
        let (mut a, mut b) = (String::new(), String::new());
        for log in [&mut a, &mut b] {
            log.push_str("--- Log opened Sun Nov 01 09:00:00 2026\n");
        }
        a.push_str("09:59 -!- quokka [quokkawet@hostQ] has joined #a\n");
        b.push_str("09:59 -!- quokka [quokkawet@hostQ] has joined #b\n");
        for minute in 0..450 {
            let log = if minute % 2 == 0 { &mut a } else { &mut b };
            if minute == 180 {
                log.push_str(&format!(
                    "{} -!- quokka [quokkawet@hostQ] has quit [back soon]\n\
                     {} -!- quokka [quokkawet@hostQ] has joined #a\n",
                    at(minute),
                    at(minute)
                ));
            }
            log.push_str(&format!("{} <quokka> line {minute}\n", at(minute)));
        }
        fs::write(root.0.join("wetfish/#a.log"), a).unwrap();
        fs::write(root.0.join("wetfish/#b.log"), b).unwrap();
        let mut conn = db::open(&root.0.join("wot.sqlite")).unwrap();
        let inventory = discover::discover(&[root.0.clone()]).unwrap();
        db::import(&mut conn, &inventory.files);
        identity::rebuild(&mut conn).unwrap();
        let quokka: Vec<String> = vec![
            conn.query_row(
                "SELECT identity_id FROM identity_keys WHERE nick = 'quokka'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
        ];
        let sessions: i64 = conn
            .query_row(
                "SELECT count(*) FROM sessions WHERE identity_id = ?1",
                params![quokka[0]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sessions, 3);

        let page = |page: usize| {
            let filter = Filter {
                page,
                ..Filter::default()
            };
            lines(&conn, &quokka, &filter).unwrap()
        };
        let mut said = Vec::new();
        for number in 0..3 {
            let (lines, more) = page(number);
            assert_eq!(more, number < 2, "page {number}");
            said.extend(lines.into_iter().map(|line| line.text));
        }
        let expected: Vec<String> = (0..450).rev().map(|n| format!("line {n}")).collect();
        assert_eq!(said, expected);
        assert!(page(3).0.is_empty());

        // A channel and a day narrow the sessions read.
        let only_a = Filter {
            channel: Some("#A".into()),
            day: Some("2026-11-01".into()),
            ..Filter::default()
        };
        let (lines, _) = lines(&conn, &quokka, &only_a).unwrap();
        assert_eq!(lines[0].text, "line 448");
        assert!(lines.iter().all(|line| line.channel == "#a"));
    }

    #[test]
    fn escapes_like_patterns() {
        assert_eq!(escape_like(r"50%_off\now"), r"50\%\_off\\now");
    }
}
