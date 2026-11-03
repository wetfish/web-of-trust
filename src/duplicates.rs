//! Lines left out of every count: second copies of lines logged twice, and a
//! bouncer's own lines. The logs and the imported lines are never changed;
//! each rebuild works out again which lines to leave out.
//!
//! The same channel is often logged more than once at the same time, such as
//! by a client and by a bouncer, or by two irssi instances. Two lines are the
//! same when they are in logs of the same channel in the same network, have
//! the same nick (ignoring case), are both messages or both actions with the
//! same text, and their times are at most a minute apart, since two machines'
//! clocks can differ. Lines pair off one to one, so someone who says the same
//! thing twice still said two lines, and a line only one log has is kept.
//! When one copy was logged through a bouncer, that copy is left out, so the
//! direct client's lines are the ones kept.
//!
//! Network tags logged through a bouncer are marked by hand. In those, ZNC
//! replays the lines missed while no client was attached between two lines
//! of its own, `<***> Buffer Playback...` and `<***> Playback Complete.`, and
//! by default puts each line's original time in front of its text, as in
//! `[14:02:11] hello`. Only the lines between those markers are replays:
//! for them, the time in front is the time compared, and the text after it.
//! Lines from ZNC's own nicks, which start with `*`, are left out.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use chrono::{NaiveDate, NaiveTime};
use rusqlite::{Connection, params};

use crate::db::Result;
use crate::progress::{Progress, Unit};
use crate::report::thousands;

/// How far apart, in minutes, two copies of a line can be.
pub const WINDOW_MINUTES: i64 = 1;
/// The nick ZNC writes its playback markers as.
const PLAYBACK_NICK: &str = "***";
/// The start of the line ZNC writes before replaying, and after.
const PLAYBACK_START: &str = "Buffer Playback";
const PLAYBACK_END: &str = "Playback Complete";

/// The lines to leave out, and the lines a bouncer replayed.
#[derive(Debug, Default)]
pub struct Duplicates {
    /// Second copies of lines logged twice: the copy left out, and the copy
    /// kept, as event ids.
    pub copies: HashMap<i64, i64>,
    /// A bouncer's own lines, such as its playback markers.
    pub bouncer: HashSet<i64>,
    /// Lines a bouncer replayed, between its playback markers.
    pub replayed: HashSet<i64>,
    /// Second copies left out, by network tag and lowercased channel or nick.
    pub by_target: HashMap<(String, String), i64>,
}

impl Duplicates {
    /// Whether a line counts for nothing.
    pub fn left_out(&self, id: i64) -> bool {
        self.copies.contains_key(&id) || self.bouncer.contains(&id)
    }
}

/// A time ZNC put in front of a replayed line, such as `[14:02]` or
/// `[14:02:11]`, and the text after it.
pub fn replay_time(text: &str) -> Option<(NaiveTime, &str)> {
    let rest = text.strip_prefix('[')?;
    let end = rest.find(']')?;
    let stamp = &rest[..end];
    let time = NaiveTime::parse_from_str(stamp, "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(stamp, "%H:%M"))
        .ok()?;
    // Only digits and colons, so `[12:30pm]` or `[1:2]` are left alone.
    let looks_right = stamp.len() >= 5
        && stamp.bytes().all(|b| b.is_ascii_digit() || b == b':')
        && stamp.as_bytes()[stamp.len() - 3] == b':';
    looks_right.then(|| (time, rest[end + 1..].trim_start()))
}

/// Minutes since 1970, for comparing times across days.
fn minute_of(date: NaiveDate, time: NaiveTime) -> i64 {
    date.and_time(time).and_utc().timestamp().div_euclid(60)
}

/// When a replayed line was said and what it said, from the time in front of
/// its text, or when it was logged when there is none.
fn replayed_moment(date: NaiveDate, logged: NaiveTime, text: &str) -> (i64, &str) {
    let logged_minute = minute_of(date, logged);
    match replay_time(text) {
        Some((said, rest)) => {
            let mut minute = minute_of(date, said);
            // A replay shortly after midnight of lines from before it.
            if minute > logged_minute + WINDOW_MINUTES {
                minute -= 24 * 60;
            }
            (minute, rest)
        }
        None => (logged_minute, text),
    }
}

fn key(nick: &str, kind: &str, text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    nick.to_lowercase().hash(&mut hasher);
    kind.hash(&mut hasher);
    text.hash(&mut hasher);
    hasher.finish()
}

/// One copy of a line, already counted.
struct Seen {
    id: i64,
    minute: i64,
    /// The last log whose copy was paired with it, so each other log pairs
    /// with it at most once.
    paired_with: usize,
}

/// The lowercased folder tags marked as logged through a bouncer.
pub fn bouncer_tags(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT lower(tag) FROM bouncer_tags")?;
    let tags = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(tags)
}

/// Reads a bouncer's log for its own lines and the lines it replayed.
fn read_bouncer_log(conn: &Connection, file: i64, found: &mut Duplicates) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, kind, nick, text FROM events
         WHERE file_id = ?1 AND kind IN ('message', 'action', 'log_opened', 'log_closed')
         ORDER BY line",
    )?;
    let mut rows = stmt.query(params![file])?;
    let mut in_playback = false;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let kind: String = row.get(1)?;
        let nick: Option<String> = row.get(2)?;
        let text: Option<String> = row.get(3)?;
        if kind == "log_opened" || kind == "log_closed" {
            // A playback never runs across a new log.
            in_playback = false;
            continue;
        }
        let nick = nick.unwrap_or_default();
        if nick.starts_with('*') {
            found.bouncer.insert(id);
            let text = text.unwrap_or_default();
            if nick == PLAYBACK_NICK && text.starts_with(PLAYBACK_START) {
                in_playback = true;
            } else if nick == PLAYBACK_NICK && text.starts_with(PLAYBACK_END) {
                in_playback = false;
            }
        } else if in_playback {
            found.replayed.insert(id);
        }
    }
    Ok(())
}

/// One log of a channel: its file, and whether it was logged through a
/// bouncer.
struct Log {
    file: i64,
    bouncer: bool,
}

/// Finds the lines to leave out in every channel: a bouncer's own lines, and
/// the second copies of lines logged more than once.
pub fn find(conn: &Connection, progress: &mut Progress) -> Result<Duplicates> {
    let bouncers = bouncer_tags(conn)?;
    let mut found = Duplicates::default();

    // Logs of the same target in the same network.
    let mut groups: HashMap<(String, String), Vec<Log>> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT id, tag, lower(target), lower(folder_tag) FROM files ORDER BY id")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let folder: String = row.get(3)?;
            groups
                .entry((row.get(1)?, row.get(2)?))
                .or_default()
                .push(Log {
                    file: row.get(0)?,
                    bouncer: bouncers.contains(&folder),
                });
        }
    }

    for log in groups.values().flatten().filter(|log| log.bouncer) {
        read_bouncer_log(conn, log.file, &mut found)?;
    }

    groups.retain(|_, logs| logs.len() > 1);
    let mut groups: Vec<((String, String), Vec<Log>)> = groups.into_iter().collect();
    groups.sort_by(|a, b| a.0.cmp(&b.0));

    progress.stage(
        "Finding lines logged twice",
        groups.len() as u64,
        Unit::Items("channels logged more than once"),
    );
    let mut range = conn.prepare(
        "SELECT min(date), max(date) FROM events WHERE file_id = ?1 AND date IS NOT NULL",
    )?;
    let mut lines = conn.prepare(
        "SELECT id, date, time, nick, kind, text FROM events
         WHERE file_id = ?1 AND date BETWEEN ?2 AND ?3
           AND kind IN ('message', 'action') AND time IS NOT NULL
         ORDER BY line",
    )?;
    for (done, (target, mut logs)) in groups.into_iter().enumerate() {
        progress.update(done as u64, || {
            format!(
                "{} lines logged twice",
                thousands(found.copies.len() as u64)
            )
        });
        // Direct clients first, so a bouncer's copy is the one left out.
        logs.sort_by_key(|log| (log.bouncer, log.file));
        let mut ranges: Vec<(i64, String, String)> = Vec::new();
        for log in &logs {
            let (first, last): (Option<String>, Option<String>) =
                range.query_row(params![log.file], |row| Ok((row.get(0)?, row.get(1)?)))?;
            if let (Some(first), Some(last)) = (first, last) {
                ranges.push((log.file, first, last));
            }
        }
        let mut seen: HashMap<u64, Vec<Seen>> = HashMap::new();
        let mut count = 0;
        for (index, (file, first, last)) in ranges.iter().enumerate() {
            // Only the days this log shares with another can hold copies.
            let shared = ranges
                .iter()
                .filter(|(other, ..)| other != file)
                .filter_map(|(_, other_first, other_last)| {
                    let from = first.max(other_first);
                    let to = last.min(other_last);
                    (from <= to).then(|| (from.clone(), to.clone()))
                })
                .reduce(|(a_from, a_to), (b_from, b_to)| (a_from.min(b_from), a_to.max(b_to)));
            let Some((from, to)) = shared else {
                continue;
            };
            let mut rows = lines.query(params![file, from, to])?;
            while let Some(row) = rows.next()? {
                let id: i64 = row.get(0)?;
                if found.bouncer.contains(&id) {
                    continue;
                }
                let date: String = row.get(1)?;
                let time: String = row.get(2)?;
                let nick: Option<String> = row.get(3)?;
                let kind: String = row.get(4)?;
                let text: Option<String> = row.get(5)?;
                let (Ok(date), Ok(time)) = (
                    NaiveDate::parse_from_str(&date, "%Y-%m-%d"),
                    NaiveTime::parse_from_str(&time, "%H:%M"),
                ) else {
                    continue;
                };
                let text = text.unwrap_or_default();
                let (minute, said) = if found.replayed.contains(&id) {
                    replayed_moment(date, time, &text)
                } else {
                    (minute_of(date, time), text.as_str())
                };
                let copies = seen
                    .entry(key(nick.as_deref().unwrap_or_default(), &kind, said))
                    .or_default();
                let earlier = copies.iter_mut().find(|copy| {
                    copy.paired_with != index && (copy.minute - minute).abs() <= WINDOW_MINUTES
                });
                match earlier {
                    Some(copy) => {
                        copy.paired_with = index;
                        found.copies.insert(id, copy.id);
                        count += 1;
                    }
                    None => copies.push(Seen {
                        id,
                        minute,
                        paired_with: index,
                    }),
                }
            }
        }
        if count > 0 {
            found.by_target.insert(target, count);
        }
    }
    progress.done(&format!(
        "{} lines logged twice, {} replayed by a bouncer",
        thousands(found.copies.len() as u64),
        thousands(found.replayed.len() as u64)
    ));
    Ok(found)
}

/// Saves the lines left out, and the lines replayed, for the pages and
/// commands that read lines directly.
pub fn save(conn: &Connection, found: &Duplicates) -> Result<()> {
    conn.execute("DELETE FROM left_out_lines", [])?;
    conn.execute("DELETE FROM replayed_lines", [])?;
    let mut insert =
        conn.prepare("INSERT INTO left_out_lines (event_id, reason, kept_id) VALUES (?1, ?2, ?3)")?;
    for (id, kept) in &found.copies {
        insert.execute(params![id, "duplicate", kept])?;
    }
    for id in &found.bouncer {
        insert.execute(params![id, "bouncer", None::<i64>])?;
    }
    let mut insert = conn.prepare("INSERT INTO replayed_lines (event_id) VALUES (?1)")?;
    for id in &found.replayed {
        insert.execute(params![id])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::discover::{self, tests::TempDir};
    use crate::{db, identity};

    #[test]
    fn reads_the_time_a_bouncer_put_in_front_of_a_line() {
        let time = |h, m, s| NaiveTime::from_hms_opt(h, m, s).unwrap();
        assert_eq!(
            replay_time("[14:02:11] hello"),
            Some((time(14, 2, 11), "hello"))
        );
        assert_eq!(
            replay_time("[09:30] hi there"),
            Some((time(9, 30, 0), "hi there"))
        );
        assert_eq!(replay_time("[12:30pm] lunch"), None);
        assert_eq!(replay_time("[1:2] no"), None);
        assert_eq!(replay_time("[link] text"), None);
        assert_eq!(replay_time("hello [14:02]"), None);

        let day = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
        // Replayed at 00:05 from before midnight: the day before.
        let (minute, text) = replayed_moment(day, time(0, 5, 0), "[23:58:40] late");
        assert_eq!(text, "late");
        assert_eq!(minute, minute_of(day, time(23, 58, 0)) - 24 * 60);
    }

    #[test]
    fn counts_a_line_logged_by_a_client_and_a_bouncer_once() {
        let root = TempDir::new("duplicates");
        for folder in ["client/wetfish", "bouncer/wetfish-bouncer"] {
            fs::create_dir_all(root.0.join(folder)).unwrap();
        }
        // The client was there all evening. The bouncer's clock is a minute
        // ahead; it heard quokka say "same" twice, and replayed what it heard
        // while detached. A pasted log line in the client's log is not a
        // replay, and neither is one outside ZNC's markers.
        fs::write(
            root.0.join("client/wetfish/#analysis.log"),
            "--- Log opened Sun Nov 01 22:00:00 2026\n\
             22:00 -!- quokka [quokkawet@hostQ] has joined #analysis\n\
             22:01 <quokka> hello\n\
             22:02 <quokka> same\n\
             22:03 <quokka> same\n\
             22:04  * quokka waves\n\
             22:10 <quokka> [21:00] <marlowe> pasted from my logs\n\
             22:20 <quokka> said while the bouncer was detached\n\
             --- Log closed Sun Nov 01 23:00:00 2026\n",
        )
        .unwrap();
        fs::write(
            root.0.join("bouncer/wetfish-bouncer/#analysis.log"),
            "--- Log opened Sun Nov 01 22:00:00 2026\n\
             22:01 -!- quokka [quokkawet@hostQ] has joined #analysis\n\
             22:02 <quokka> hello\n\
             22:03 <quokka> same\n\
             22:04 <quokka> same\n\
             22:05  * quokka waves\n\
             22:11 <quokka> [21:00] <marlowe> pasted from my logs\n\
             22:30 < ***> Buffer Playback...\n\
             22:30 <quokka> [22:20:05] said while the bouncer was detached\n\
             22:30 <quokka> [22:25:00] only the bouncer heard this\n\
             22:30 < ***> Playback Complete.\n\
             22:31 <quokka> [22:20:05] not a replay, after the markers\n\
             --- Log closed Sun Nov 01 23:00:00 2026\n",
        )
        .unwrap();
        let roots = [root.0.join("client"), root.0.join("bouncer")];
        let mut conn = db::open(&root.0.join("wot.sqlite")).unwrap();
        let inventory = discover::discover(&roots).unwrap();
        db::import(&mut conn, &inventory.files);
        conn.execute(
            "INSERT INTO bouncer_tags (tag) VALUES ('WETFISH-bouncer')",
            [],
        )
        .unwrap();
        // Both tags linked into one network, as wot networks does.
        conn.execute(
            "UPDATE files SET tag = 'wetfish' WHERE folder_tag = 'wetfish-bouncer'",
            [],
        )
        .unwrap();

        let found = find(&conn, &mut Progress::quiet()).unwrap();
        let text = |id: &i64| -> String {
            conn.query_row(
                "SELECT text FROM events WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap()
        };
        let mut replayed: Vec<String> = found.replayed.iter().map(text).collect();
        replayed.sort();
        assert_eq!(
            replayed,
            [
                "[22:20:05] said while the bouncer was detached",
                "[22:25:00] only the bouncer heard this"
            ]
        );
        assert_eq!(found.bouncer.len(), 2, "ZNC's two markers");
        // hello, same twice, waves, the pasted line and the replayed line;
        // every copy left out is the bouncer's.
        assert_eq!(found.copies.len(), 6);
        for (left_out, kept) in &found.copies {
            let folder = |id: &i64| -> String {
                conn.query_row(
                    "SELECT f.folder_tag FROM events e JOIN files f ON f.id = e.file_id
                     WHERE e.id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .unwrap()
            };
            assert_eq!(folder(left_out), "wetfish-bouncer");
            assert_eq!(folder(kept), "wetfish");
            assert_eq!(text(left_out).trim_start_matches("[22:20:05] "), text(kept));
        }
        assert_eq!(
            found.by_target[&("wetfish".to_string(), "#analysis".to_string())],
            6
        );

        // Everything counted from the logs counts each line once, and ZNC's
        // own lines not at all.
        identity::rebuild(&mut conn).unwrap();
        let spoken: i64 = conn
            .query_row("SELECT sum(spoken) FROM sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(spoken, 8, "6 from the client, 2 only the bouncer has");
        let left_out: i64 = conn
            .query_row("SELECT count(*) FROM left_out_lines", [], |row| row.get(0))
            .unwrap();
        assert_eq!(left_out, 8);
        let nicks: i64 = conn
            .query_row(
                "SELECT count(*) FROM identity_keys WHERE nick = '***'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(nicks, 0, "ZNC is nobody");
    }
}
