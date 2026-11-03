//! Channel activity of each identity: how long it was present, how much was
//! said while it was there, and how much it said itself, by channel and by
//! month. It is rebuilt with the identities and read by the activity pages.
//!
//! Only channels count. Private messages only show one side of a
//! conversation, the logging client's, so they are kept apart and only shown
//! next to quiet identities. The logging client's own sessions are left out.
//!
//! - **Days present:** days on which the identity had a session in the
//!   channel, however short.
//! - **Minutes:** time present. Overlapping sessions of one identity, such as
//!   two clients, count once.
//! - **Heard:** messages and actions in the channel while the identity was
//!   present, its own included.
//! - **Spoken:** its own messages and actions.
//! - **Active days:** days on which it spoke at least once.
//!
//! Every identity also has rows for all channels together, under the channel
//! `*`, where days and minutes are not counted twice for being in two
//! channels at once.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::{Connection, params};

use crate::db::Result;
use crate::lookup;

/// The channel of the rows for all channels together. Channel names never
/// are `*`.
pub const ALL_CHANNELS: &str = "*";

/// One channel session of an identity, as activity is built from it.
pub struct Presence<'a> {
    pub identity: &'a str,
    pub tag: &'a str,
    pub channel: &'a str,
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    /// The file's count of spoken lines when the session started and ended.
    pub heard_from: i64,
    pub heard_to: i64,
    /// The file's count of spoken lines at the start of each day.
    pub day_marks: &'a [(NaiveDate, i64)],
    /// The session's own messages and actions by day.
    pub spoken_days: &'a BTreeMap<NaiveDate, i64>,
}

/// Activity added up over some months.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub days_present: i64,
    pub active_days: i64,
    pub minutes: i64,
    pub heard: i64,
    pub spoken: i64,
}

impl Totals {
    /// The identity's share of what was said while it was present, from 0 to
    /// 1, or `None` when nothing was said.
    pub fn share(&self) -> Option<f64> {
        (self.heard > 0).then(|| self.spoken as f64 / self.heard as f64)
    }

    fn add(&mut self, other: &Totals) {
        self.days_present += other.days_present;
        self.active_days += other.active_days;
        self.minutes += other.minutes;
        self.heard += other.heard;
        self.spoken += other.spoken;
    }
}

/// Everything one identity did in one channel, or in all of them.
#[derive(Default)]
struct Tally {
    intervals: Vec<(NaiveDateTime, NaiveDateTime)>,
    heard: BTreeMap<NaiveDate, i64>,
    spoken: BTreeMap<NaiveDate, i64>,
}

impl Tally {
    fn add(&mut self, presence: &Presence) {
        self.intervals
            .push((presence.start, presence.end.max(presence.start)));
        for (day, heard) in heard_by_day(presence.heard_from, presence.heard_to, presence.day_marks)
        {
            *self.heard.entry(day).or_default() += heard;
        }
        for (day, spoken) in presence.spoken_days {
            *self.spoken.entry(*day).or_default() += spoken;
        }
    }

    /// The tally by month, as `YYYY-MM`.
    fn months(&self) -> BTreeMap<String, Totals> {
        let mut months: BTreeMap<String, Totals> = BTreeMap::new();
        let mut present: BTreeMap<NaiveDate, i64> = BTreeMap::new();
        for (start, end) in merge(&self.intervals) {
            for (day, minutes) in minutes_by_day(start, end) {
                *present.entry(day).or_default() += minutes;
            }
        }
        for (day, minutes) in present {
            let month = months.entry(month_of(day)).or_default();
            month.days_present += 1;
            month.minutes += minutes;
        }
        for (day, heard) in &self.heard {
            months.entry(month_of(*day)).or_default().heard += heard;
        }
        for (day, spoken) in &self.spoken {
            let month = months.entry(month_of(*day)).or_default();
            month.spoken += spoken;
            if *spoken > 0 {
                month.active_days += 1;
            }
        }
        months
    }
}

fn month_of(day: NaiveDate) -> String {
    day.format("%Y-%m").to_string()
}

/// Splits the spoken lines numbered `from` to `to` (not included) over the
/// days of `marks`, which hold the count at the start of each day.
fn heard_by_day(from: i64, to: i64, marks: &[(NaiveDate, i64)]) -> Vec<(NaiveDate, i64)> {
    let mut days = Vec::new();
    if to <= from || marks.is_empty() {
        return days;
    }
    // The last day that started at or before line `from`.
    let first = marks
        .partition_point(|&(_, count)| count <= from)
        .saturating_sub(1);
    for (i, &(day, start)) in marks.iter().enumerate().skip(first) {
        if start >= to {
            break;
        }
        let end = marks.get(i + 1).map_or(i64::MAX, |&(_, next)| next);
        let heard = end.min(to) - start.max(from);
        if heard > 0 {
            days.push((day, heard));
        }
    }
    days
}

/// Joins overlapping or touching intervals.
fn merge(intervals: &[(NaiveDateTime, NaiveDateTime)]) -> Vec<(NaiveDateTime, NaiveDateTime)> {
    let mut sorted = intervals.to_vec();
    sorted.sort();
    let mut merged: Vec<(NaiveDateTime, NaiveDateTime)> = Vec::with_capacity(sorted.len());
    for (start, end) in sorted {
        match merged.last_mut() {
            Some((_, last_end)) if start <= *last_end => *last_end = (*last_end).max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// The days an interval touches, with the minutes it covers on each.
fn minutes_by_day(start: NaiveDateTime, end: NaiveDateTime) -> Vec<(NaiveDate, i64)> {
    let mut days = Vec::new();
    let mut day = start.date();
    loop {
        let next = day.succ_opt();
        let day_end = next.map(|next| next.and_time(chrono::NaiveTime::MIN));
        let from = start.max(day.and_time(chrono::NaiveTime::MIN));
        let to = day_end.map_or(end, |day_end| end.min(day_end));
        days.push((day, to.signed_duration_since(from).num_minutes().max(0)));
        match (next, day_end) {
            (Some(next), Some(day_end)) if end > day_end => day = next,
            _ => break,
        }
    }
    days
}

/// Replaces all activity with the activity of these sessions.
pub fn rebuild(conn: &Connection, presences: &[Presence]) -> Result<()> {
    let mut tallies: HashMap<(&str, &str, &str), Tally> = HashMap::new();
    for presence in presences {
        for channel in [presence.channel, ALL_CHANNELS] {
            tallies
                .entry((presence.identity, presence.tag, channel))
                .or_default()
                .add(presence);
        }
    }

    conn.execute("DELETE FROM activity", [])?;
    let mut insert = conn.prepare(
        "INSERT INTO activity (identity_id, tag, channel, month, days_present, active_days,
             minutes, heard, spoken)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for ((identity, tag, channel), tally) in &tallies {
        for (month, totals) in tally.months() {
            insert.execute(params![
                identity,
                tag,
                channel,
                month,
                totals.days_present,
                totals.active_days,
                totals.minutes,
                totals.heard,
                totals.spoken,
            ])?;
        }
    }
    Ok(())
}

/// The identities that belong to a bot, as they are now after any merges.
pub fn bots(conn: &Connection) -> Result<HashSet<String>> {
    let merges = lookup::merges(conn)?;
    let mut stmt = conn.prepare(
        "SELECT pi.identity_id FROM person_identities pi JOIN people p ON p.id = pi.person_id
         WHERE p.kind = 'bot'",
    )?;
    let ids: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids
        .iter()
        .map(|id| lookup::current_id(&merges, id))
        .collect())
}

/// Messages and actions an identity sent in private messages, apart from the
/// logging client's.
pub fn private_spoken(conn: &Connection, identity: &str) -> Result<i64> {
    let spoken = conn.query_row(
        "SELECT ifnull(sum(s.spoken), 0) FROM sessions s JOIN files f ON f.id = s.file_id
         WHERE s.identity_id = ?1 AND f.kind = 'query' AND s.logger = 0",
        params![identity],
        |row| row.get(0),
    )?;
    Ok(spoken)
}

/// How to order a ranking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    /// The smallest share of what was said while present first, then the
    /// most days present. Identities that heard nothing come last.
    LeastActive,
    /// The most spoken lines first.
    MostActive,
    /// The most days present first.
    LongestPresent,
}

pub struct RankFilter {
    pub tag: Option<String>,
    /// A channel, or all channels together when `None`.
    pub channel: Option<String>,
    /// A year as `YYYY`, or all time when `None`.
    pub year: Option<String>,
    /// Identities present on fewer days are left out.
    pub min_days: i64,
    pub include_bots: bool,
    pub order: Order,
    pub limit: usize,
}

pub struct RankRow {
    pub identity_id: String,
    pub tag: String,
    pub totals: Totals,
    /// `YYYY-MM`.
    pub first_month: String,
    pub last_month: String,
    pub nicks: Vec<String>,
    pub people: Vec<String>,
    pub bot: bool,
    /// Messages and actions sent in private messages, over all time.
    pub private_spoken: i64,
}

/// The ranked identities, at most `limit` of them, and how many matched.
pub fn ranking(conn: &Connection, filter: &RankFilter) -> Result<(Vec<RankRow>, usize)> {
    let channel = filter.channel.as_deref().unwrap_or(ALL_CHANNELS);
    let mut stmt = conn.prepare(
        "SELECT identity_id, tag, sum(days_present), sum(active_days), sum(minutes), sum(heard),
                sum(spoken), min(month), max(month)
         FROM activity
         WHERE channel = ?1 AND (?2 IS NULL OR tag = ?2)
           AND (?3 IS NULL OR substr(month, 1, 4) = ?3)
         GROUP BY identity_id, tag
         HAVING sum(days_present) >= ?4",
    )?;
    let bots = bots(conn)?;
    let mut rows: Vec<RankRow> = stmt
        .query_map(
            params![channel, filter.tag, filter.year, filter.min_days],
            |row| {
                Ok(RankRow {
                    identity_id: row.get(0)?,
                    tag: row.get(1)?,
                    totals: Totals {
                        days_present: row.get(2)?,
                        active_days: row.get(3)?,
                        minutes: row.get(4)?,
                        heard: row.get(5)?,
                        spoken: row.get(6)?,
                    },
                    first_month: row.get(7)?,
                    last_month: row.get(8)?,
                    nicks: Vec::new(),
                    people: Vec::new(),
                    bot: false,
                    private_spoken: 0,
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?;

    for row in &mut rows {
        row.bot = bots.contains(&row.identity_id);
    }
    if !filter.include_bots {
        rows.retain(|row| !row.bot);
    }
    sort(&mut rows, filter.order);
    let matched = rows.len();
    rows.truncate(filter.limit);

    let merges = lookup::merges(conn)?;
    let people = lookup::people_by_identity(conn, &merges)?;
    let mut nicks = conn
        .prepare("SELECT DISTINCT nick FROM identity_keys WHERE identity_id = ?1 ORDER BY nick")?;
    for row in &mut rows {
        row.nicks = nicks
            .query_map(params![row.identity_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        row.people = people.get(&row.identity_id).cloned().unwrap_or_default();
        row.private_spoken = private_spoken(conn, &row.identity_id)?;
    }
    Ok((rows, matched))
}

fn sort(rows: &mut [RankRow], order: Order) {
    match order {
        Order::LeastActive => rows.sort_by(|a, b| {
            // Identities that heard nothing say nothing about how quiet they
            // are, so they go last.
            let share = |row: &RankRow| row.totals.share().unwrap_or(f64::INFINITY);
            share(a)
                .total_cmp(&share(b))
                .then(b.totals.days_present.cmp(&a.totals.days_present))
                .then(a.identity_id.cmp(&b.identity_id))
        }),
        Order::MostActive => rows.sort_by(|a, b| {
            b.totals
                .spoken
                .cmp(&a.totals.spoken)
                .then(b.totals.active_days.cmp(&a.totals.active_days))
                .then(a.identity_id.cmp(&b.identity_id))
        }),
        Order::LongestPresent => rows.sort_by(|a, b| {
            b.totals
                .days_present
                .cmp(&a.totals.days_present)
                .then(a.identity_id.cmp(&b.identity_id))
        }),
    }
}

/// An identity's activity by month, in one channel or all of them, oldest
/// first.
pub fn months(
    conn: &Connection,
    identity: &str,
    channel: Option<&str>,
) -> Result<Vec<(String, Totals)>> {
    let mut stmt = conn.prepare(
        "SELECT month, days_present, active_days, minutes, heard, spoken FROM activity
         WHERE identity_id = ?1 AND channel = ?2 ORDER BY month",
    )?;
    let rows = stmt
        .query_map(params![identity, channel.unwrap_or(ALL_CHANNELS)], |row| {
            Ok((
                row.get(0)?,
                Totals {
                    days_present: row.get(1)?,
                    active_days: row.get(2)?,
                    minutes: row.get(3)?,
                    heard: row.get(4)?,
                    spoken: row.get(5)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Months added up by year, oldest first.
pub fn years(months: &[(String, Totals)]) -> Vec<(String, Totals)> {
    let mut years: BTreeMap<String, Totals> = BTreeMap::new();
    for (month, totals) in months {
        years
            .entry(month.get(..4).unwrap_or(month).to_string())
            .or_default()
            .add(totals);
    }
    years.into_iter().collect()
}

/// The years and the channels, by tag, that have any activity, for filters.
pub fn choices(conn: &Connection) -> Result<(Vec<String>, Vec<(String, String)>)> {
    let years = {
        let mut stmt =
            conn.prepare("SELECT DISTINCT substr(month, 1, 4) FROM activity ORDER BY 1 DESC")?;
        let rows = stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    let channels = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT tag, channel FROM activity WHERE channel <> ?1 ORDER BY 1, 2",
        )?;
        let rows = stmt
            .query_map(params![ALL_CHANNELS], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    Ok((years, channels))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::discover::{self, tests::TempDir};
    use crate::stats::Kind;
    use crate::{db, decide, identity};

    fn at(day: u32, hour: u32, minute: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, day)
            .unwrap()
            .and_hms_opt(hour, minute, 0)
            .unwrap()
    }

    fn day(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, day).unwrap()
    }

    #[test]
    fn splits_heard_lines_over_days() {
        let marks = [(day(1), 0), (day(2), 5), (day(3), 5), (day(4), 9)];
        assert_eq!(heard_by_day(2, 7, &marks), [(day(1), 3), (day(3), 2)]);
        assert_eq!(heard_by_day(9, 12, &marks), [(day(4), 3)]);
        assert!(heard_by_day(4, 4, &marks).is_empty());
    }

    #[test]
    fn splits_time_present_over_days_without_counting_overlaps_twice() {
        assert_eq!(
            minutes_by_day(at(1, 23, 0), at(2, 1, 30)),
            [(day(1), 60), (day(2), 90)]
        );
        assert_eq!(minutes_by_day(at(1, 10, 0), at(2, 0, 0)), [(day(1), 840)]);
        assert_eq!(
            merge(&[(at(1, 10, 0), at(1, 12, 0)), (at(1, 11, 0), at(1, 13, 0))]),
            [(at(1, 10, 0), at(1, 13, 0))]
        );
    }

    #[test]
    fn builds_and_ranks_channel_activity() {
        let root = TempDir::new("activity-ranking");
        fs::create_dir_all(root.0.join("wetfish")).unwrap();
        fs::write(
            root.0.join("wetfish/#analysis.log"),
            "--- Log opened Sun Nov 01 22:00:00 2026\n\
             22:00 -!- rachel [rachel@hostR] has joined #analysis\n\
             22:00 -!- Irssi: #analysis: Total of 1 nicks [1 ops, 0 halfops, 0 voices, 0 normal]\n\
             22:00 -!- Irssi: Join to #analysis was synced in 0 secs\n\
             22:10 -!- quokka [quokkawet@hostQ] has joined #analysis\n\
             22:20 -!- marlowe [marlowewet@hostM] has joined #analysis\n\
             22:30 <marlowe> hello\n\
             22:31 <@rachel> hi\n\
             --- Day changed Mon Nov 02 2026\n\
             00:30 <marlowe> still here\n\
             01:00 -!- marlowe [marlowewet@hostM] has quit [bye]\n\
             01:10 <@rachel> quiet now\n\
             --- Log closed Mon Nov 02 02:00:00 2026\n",
        )
        .unwrap();
        fs::write(
            root.0.join("wetfish/quokka.log"),
            "--- Log opened Mon Nov 02 03:00:00 2026\n03:00 <quokka> psst\n",
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
        let (quokka, marlowe) = (id("quokka"), id("marlowe"));

        // quokka heard all four lines over two days and said nothing.
        let months = months(&conn, &quokka, None).unwrap();
        assert_eq!(
            months,
            [(
                "2026-11".to_string(),
                Totals {
                    days_present: 2,
                    active_days: 0,
                    minutes: 230,
                    heard: 4,
                    spoken: 0,
                }
            )]
        );
        let in_channel = super::months(&conn, &marlowe, Some("#analysis")).unwrap();
        assert_eq!(
            in_channel[0].1,
            Totals {
                days_present: 2,
                active_days: 2,
                minutes: 160,
                heard: 3,
                spoken: 2,
            }
        );
        // The logging client has no activity.
        let logged: i64 = conn
            .query_row(
                "SELECT count(*) FROM activity WHERE identity_id = ?1",
                params![id("rachel")],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(logged, 0);

        let mut filter = RankFilter {
            tag: None,
            channel: None,
            year: None,
            min_days: 1,
            include_bots: false,
            order: Order::LeastActive,
            limit: 10,
        };
        let (rows, matched) = ranking(&conn, &filter).unwrap();
        assert_eq!(matched, 2);
        assert_eq!(rows[0].identity_id, quokka);
        assert_eq!(rows[0].nicks, ["quokka"]);
        assert_eq!(rows[0].private_spoken, 1);

        filter.order = Order::MostActive;
        assert_eq!(ranking(&conn, &filter).unwrap().0[0].identity_id, marlowe);

        // Bots are left out unless asked for.
        decide::create_person(&mut conn, "relay", Kind::Bot, &[marlowe.clone()], None).unwrap();
        assert_eq!(ranking(&conn, &filter).unwrap().1, 1);
        filter.include_bots = true;
        let (rows, _) = ranking(&conn, &filter).unwrap();
        assert!(rows[0].bot);

        filter.year = Some("2025".into());
        assert_eq!(ranking(&conn, &filter).unwrap().1, 0);

        let (years, channels) = choices(&conn).unwrap();
        assert_eq!(years, ["2026"]);
        assert_eq!(channels, [("wetfish".to_string(), "#analysis".to_string())]);
        assert_eq!(super::years(&months)[0].0, "2026");
    }
}
