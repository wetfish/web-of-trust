//! Trust: your own rating of people put together with how active they have
//! been, and checking a channel's current users, pasted from irssi's `/who`,
//! against what the logs and your decisions say about them.
//!
//! - **Activity score**, 0 to 100, from the channel activity of a person's
//!   identities: consistency (months with spoken lines), tenure (months from
//!   first to last seen) and participation (days with spoken lines).
//! - **Trust score**, 0 to 99: the rating sets a band of 20 points, from
//!   distrusted at 0 to 19 up to known at 80 to 99, and the activity score
//!   places the person within it. The bands never overlap, so a known friend
//!   who rarely talks still ranks above the busiest stranger.

use std::collections::HashMap;

use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use crate::activity::ALL_CHANNELS;
use crate::db::Result;
use crate::lookup;
use crate::stats::{Kind, Rating};

/// Days present in a channel without ever saying a line there that flag
/// someone as a silent watcher, untrusted unless rated vouched or known.
pub const SILENT_DAYS: i64 = 30;

/// Whether days present with no spoken lines make a silent watcher.
pub fn silent(days_present: i64, spoken: i64) -> bool {
    spoken == 0 && days_present >= SILENT_DAYS
}

/// How long a channel review stays valid.
pub const REVIEW_VALID_MONTHS: i64 = 6;
/// Channels are ranked by their activity over this many months, the current
/// one included.
pub const RECENT_MONTHS: i64 = 6;

/// Points for each part of the activity score. They add up to 100.
const CONSISTENCY_POINTS: f64 = 40.0;
const TENURE_POINTS: f64 = 20.0;
const PARTICIPATION_POINTS: f64 = 40.0;
/// Months with spoken lines that earn all the consistency points.
const FULL_ACTIVE_MONTHS: f64 = 24.0;
/// Months from first to last seen that earn all the tenure points.
const FULL_TENURE_MONTHS: f64 = 36.0;
/// Days with spoken lines that earn all the participation points. Points
/// grow with the logarithm of the days, so the first weeks count the most.
const FULL_ACTIVE_DAYS: f64 = 365.0;

/// The activity score and what it was made from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActivityScore {
    /// 0 to 100.
    pub score: i64,
    /// Months with at least one spoken line in a channel.
    pub active_months: i64,
    /// Months from the first to the last month present, both included.
    pub tenure_months: i64,
    /// Days with spoken lines. Days on which two identities spoke count
    /// twice, up to the number of days in each month.
    pub active_days: i64,
    pub spoken: i64,
}

/// `YYYY-MM` as a number of months, for counting months between two.
fn month_number(month: &str) -> Option<i64> {
    let (year, month) = month.split_once('-')?;
    let (year, month): (i64, i64) = (year.parse().ok()?, month.parse().ok()?);
    (1..=12).contains(&month).then_some(year * 12 + month - 1)
}

fn days_in_month(number: i64) -> i64 {
    let first = |number: i64| {
        NaiveDate::from_ymd_opt(
            number.div_euclid(12) as i32,
            number.rem_euclid(12) as u32 + 1,
            1,
        )
    };
    match (first(number), first(number + 1)) {
        (Some(start), Some(end)) => (end - start).num_days(),
        _ => 31,
    }
}

/// The activity score of a set of identities, from their channel activity.
pub fn activity_score(conn: &Connection, identities: &[String]) -> Result<ActivityScore> {
    if identities.is_empty() {
        return Ok(ActivityScore::default());
    }
    let placeholders: Vec<String> = (2..identities.len() + 2).map(|n| format!("?{n}")).collect();
    let mut stmt = conn.prepare(&format!(
        "SELECT month, sum(days_present), sum(active_days), sum(spoken) FROM activity
         WHERE channel = ?1 AND identity_id IN ({}) GROUP BY month ORDER BY month",
        placeholders.join(", ")
    ))?;
    let values = std::iter::once(ALL_CHANNELS).chain(identities.iter().map(String::as_str));
    let months: Vec<(String, i64, i64, i64)> = stmt
        .query_map(params_from_iter(values), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut score = ActivityScore::default();
    let (mut first, mut last) = (None, None);
    for (month, present, active, spoken) in months {
        let Some(number) = month_number(&month) else {
            continue;
        };
        if present > 0 {
            first = Some(first.map_or(number, |first: i64| first.min(number)));
            last = Some(last.map_or(number, |last: i64| last.max(number)));
        }
        if active > 0 {
            score.active_months += 1;
        }
        score.active_days += active.min(days_in_month(number));
        score.spoken += spoken;
    }
    if let (Some(first), Some(last)) = (first, last) {
        score.tenure_months = last - first + 1;
    }
    let part = |value: f64, full: f64| (value / full).min(1.0);
    let points = CONSISTENCY_POINTS * part(score.active_months as f64, FULL_ACTIVE_MONTHS)
        + TENURE_POINTS * part(score.tenure_months as f64, FULL_TENURE_MONTHS)
        + PARTICIPATION_POINTS
            * part(
                (1.0 + score.active_days as f64).ln(),
                (1.0 + FULL_ACTIVE_DAYS).ln(),
            );
    score.score = points.round() as i64;
    Ok(score)
}

/// The trust score, 0 to 99: the rating's band of 20 points, and the
/// activity score, scaled to 0 to 19, placing the person within it.
pub fn trust_score(rating: Rating, activity: i64) -> i64 {
    let band = Rating::ALL.iter().position(|r| *r == rating).unwrap_or(2) as i64;
    band * 20 + activity.clamp(0, 100) * 19 / 100
}

/// A person or bot, as far as trust goes.
#[derive(Clone, Debug)]
pub struct Record {
    pub name: String,
    pub kind: Kind,
    pub rating: Rating,
    /// Current ids.
    pub identities: Vec<String>,
}

/// Every person and bot, and which of them each current identity belongs to.
pub struct Records {
    by_name: HashMap<String, Record>,
    of_identity: HashMap<String, String>,
}

impl Records {
    pub fn of_identity(&self, id: &str) -> Option<&Record> {
        self.of_identity
            .get(id)
            .and_then(|name| self.by_name.get(name))
    }
}

pub fn records(conn: &Connection) -> Result<Records> {
    let merges = lookup::merges(conn)?;
    let mut stmt = conn.prepare(
        "SELECT p.name, p.kind, p.rating, pi.identity_id
         FROM people p LEFT JOIN person_identities pi ON pi.person_id = p.id
         ORDER BY p.name, pi.identity_id",
    )?;
    let rows: Vec<(String, String, String, Option<String>)> = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut records = Records {
        by_name: HashMap::new(),
        of_identity: HashMap::new(),
    };
    for (name, kind, rating, identity) in rows {
        let kind = Kind::parse(&kind).unwrap_or(Kind::Person);
        let record = records
            .by_name
            .entry(name.clone())
            .or_insert_with(|| Record {
                name: name.clone(),
                kind,
                rating: match kind {
                    Kind::Person => Rating::parse(&rating).unwrap_or(Rating::Neutral),
                    Kind::Bot => Rating::Neutral,
                },
                identities: Vec::new(),
            });
        if let Some(identity) = identity {
            let current = lookup::current_id(&merges, &identity);
            if !record.identities.contains(&current) {
                record.identities.push(current.clone());
            }
            // An identity in two records is a conflict the identity page
            // shows; the first record by name is used here.
            records.of_identity.entry(current).or_insert(name);
        }
    }
    Ok(records)
}

// Who

/// One user from irssi's `/who` output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhoEntry {
    pub nick: String,
    pub ident: String,
    pub host: String,
    /// `H` when here or `G` when away, then `*` for an IRC operator, channel
    /// modes such as `@` and `+`, and any flags the server adds.
    pub status: String,
    pub realname: String,
}

/// What a pasted `/who` output held.
#[derive(Debug, Default)]
pub struct ParsedWho {
    /// The channel's users, each nick once.
    pub entries: Vec<WhoEntry>,
    /// The channels other replies were for, as shown.
    pub other_channels: Vec<String>,
    /// Lines that were not `/who` replies, such as "End of /WHO list".
    pub skipped: usize,
}

/// One `/who` reply line, as irssi's default theme shows it:
/// `#channel nick status hops ident@host [real name]`, after any timestamp
/// and `-!-`. irssi cuts the channel to 10 characters. Returns the channel
/// as shown and the user.
fn who_line(line: &str) -> Option<(&str, WhoEntry)> {
    let tokens: Vec<(usize, &str)> = line
        .split_whitespace()
        .map(|token| (token.as_ptr() as usize - line.as_ptr() as usize, token))
        .collect();
    for i in 0..tokens.len().saturating_sub(4) {
        let channel = tokens[i].1;
        let (nick, status, hops) = (tokens[i + 1].1, tokens[i + 2].1, tokens[i + 3].1);
        let (offset, mask) = tokens[i + 4];
        let fits = channel.starts_with(['#', '&', '!', '+'])
            && status.starts_with(['H', 'G'])
            && hops.bytes().all(|b| b.is_ascii_digit());
        let Some((ident, host)) = mask.split_once('@') else {
            continue;
        };
        if !fits || ident.is_empty() || host.is_empty() {
            continue;
        }
        let rest = line[offset + mask.len()..].trim();
        let realname = rest
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
            .unwrap_or(rest);
        return Some((
            channel,
            WhoEntry {
                nick: nick.to_string(),
                ident: ident.to_string(),
                host: host.to_string(),
                status: status.to_string(),
                realname: realname.to_string(),
            },
        ));
    }
    None
}

/// Whether a channel as `/who` showed it, cut to 10 characters, is this one.
fn same_channel(shown: &str, channel: &str) -> bool {
    let cut: String = channel.chars().take(10).collect();
    shown.eq_ignore_ascii_case(channel) || shown.eq_ignore_ascii_case(&cut)
}

/// Reads pasted `/who #channel` output, keeping the replies for `channel`.
pub fn parse_who(text: &str, channel: &str) -> ParsedWho {
    let mut parsed = ParsedWho::default();
    for line in text.lines() {
        match who_line(line) {
            Some((shown, entry)) if same_channel(shown, channel) => {
                let seen = parsed
                    .entries
                    .iter()
                    .any(|other| other.nick.eq_ignore_ascii_case(&entry.nick));
                if !seen {
                    parsed.entries.push(entry);
                }
            }
            Some((shown, _)) => {
                if !parsed.other_channels.iter().any(|other| other == shown) {
                    parsed.other_channels.push(shown.to_string());
                }
            }
            None if line.trim().is_empty() => {}
            None => parsed.skipped += 1,
        }
    }
    parsed
}

/// Stores a channel's users. Returns the list's id.
pub fn save_who(
    conn: &mut Connection,
    tag: &str,
    channel: &str,
    entries: &[WhoEntry],
) -> Result<i64> {
    let tx = conn.transaction()?;
    let id: i64 = tx.query_row(
        "INSERT INTO who_lists (tag, channel) VALUES (?1, ?2) RETURNING id",
        params![tag, channel],
        |row| row.get(0),
    )?;
    {
        let mut insert = tx.prepare(
            "INSERT INTO who_entries (list_id, nick, ident, host, status, realname)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for entry in entries {
            insert.execute(params![
                id,
                entry.nick,
                entry.ident,
                entry.host,
                entry.status,
                entry.realname
            ])?;
        }
    }
    tx.commit()?;
    Ok(id)
}

pub fn delete_who(conn: &Connection, id: i64) -> Result<()> {
    if conn.execute("DELETE FROM who_lists WHERE id = ?1", params![id])? == 0 {
        return Err(format!("there is no user list {id}").into());
    }
    Ok(())
}

/// A stored `/who` list.
pub struct WhoList {
    pub id: i64,
    /// UTC, as `YYYY-MM-DD HH:MM:SS`.
    pub pasted_at: String,
    pub entries: Vec<WhoEntry>,
    /// How many lists were pasted for the channel before this one.
    pub earlier: i64,
}

/// The most recent user list pasted for a channel.
pub fn latest_who(conn: &Connection, tag: &str, channel: &str) -> Result<Option<WhoList>> {
    let lists: Vec<(i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, pasted_at FROM who_lists WHERE tag = ?1 AND channel = ?2
             ORDER BY pasted_at DESC, id DESC",
        )?;
        let rows = stmt
            .query_map(params![tag, channel], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    let Some((id, pasted_at)) = lists.first().cloned() else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(
        "SELECT nick, ident, host, status, realname FROM who_entries WHERE list_id = ?1
         ORDER BY nick COLLATE NOCASE",
    )?;
    let entries = stmt
        .query_map(params![id], |row| {
            Ok(WhoEntry {
                nick: row.get(0)?,
                ident: row.get(1)?,
                host: row.get(2)?,
                status: row.get(3)?,
                realname: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(WhoList {
        id,
        pasted_at,
        entries,
        earlier: lists.len() as i64 - 1,
    }))
}

/// How a user was matched to the logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Match {
    /// The same nick from an `ident@host` the identity used.
    Exact,
    /// An `ident@host` the identity used, with a nick it never used there.
    NewNick,
    /// A nick seen before, but never from this `ident@host`. Anyone can take
    /// a nick, so this gives no trust.
    NickOnly,
    /// Never seen in the logs.
    Unknown,
}

/// Which list a user goes in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Rated vouched or known.
    Trusted,
    /// Rated neutral, not linked to a person, or only matched by nick.
    Undecided,
    /// Rated wary or distrusted, or never seen.
    Untrusted,
    /// Belongs to a bot.
    Bot,
}

/// A user checked against the logs and your decisions.
pub struct Checked {
    pub entry: WhoEntry,
    pub how: Match,
    /// The matched identity, the most recently seen when several match.
    pub identity: Option<String>,
    /// How many identities matched, the chosen one included.
    pub candidates: usize,
    pub record: Option<Record>,
    /// Of the record's identities, or of the identity when it has no record.
    pub activity: ActivityScore,
    /// None for bots and users never seen.
    pub trust: Option<i64>,
    /// Days present in the channel without a spoken line there, when that
    /// makes them a silent watcher. Never set for bots or for people rated
    /// vouched or known.
    pub silent_days: Option<i64>,
    pub verdict: Verdict,
}

/// The hosts that count as this one: those of its host group, if any, and
/// whether the group's ident identifies the person.
fn host_group(conn: &Connection, tag: &str, host: &str) -> Result<(Vec<String>, Option<bool>)> {
    let group: Option<(i64, bool)> = conn
        .query_row(
            "SELECT g.id, g.ident_is_user FROM host_group_members m
             JOIN host_groups g ON g.id = m.group_id WHERE m.tag = ?1 AND m.host = ?2",
            params![tag, host],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((group, ident_is_user)) = group else {
        return Ok((vec![host.to_string()], None));
    };
    let mut stmt = conn.prepare("SELECT host FROM host_group_members WHERE group_id = ?1")?;
    let mut hosts: Vec<String> = stmt
        .query_map(params![group], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if !hosts.iter().any(|h| h.eq_ignore_ascii_case(host)) {
        hosts.push(host.to_string());
    }
    Ok((hosts, Some(ident_is_user)))
}

/// The identities with keys matching what is given, most recently seen first.
fn identities_matching(
    conn: &Connection,
    tag: &str,
    nick: Option<&str>,
    ident: Option<&str>,
    hosts: &[String],
) -> Result<Vec<String>> {
    let mut conditions = vec!["k.tag = ?1".to_string()];
    let mut values: Vec<String> = vec![tag.to_string()];
    if let Some(nick) = nick {
        values.push(nick.to_lowercase());
        conditions.push(format!("k.nick = ?{}", values.len()));
    }
    if let Some(ident) = ident {
        values.push(ident.to_string());
        conditions.push(format!("k.ident = ?{}", values.len()));
    }
    if !hosts.is_empty() {
        let mut any = Vec::new();
        for host in hosts {
            values.push(host.clone());
            any.push(format!("k.host = ?{} COLLATE NOCASE", values.len()));
        }
        conditions.push(format!("({})", any.join(" OR ")));
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT k.identity_id, max(s.end_at) AS seen FROM identity_keys k
         LEFT JOIN sessions s ON s.identity_id = k.identity_id
         WHERE {} GROUP BY k.identity_id ORDER BY seen DESC, k.identity_id",
        conditions.join(" AND ")
    ))?;
    let ids = stmt
        .query_map(params_from_iter(values), |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// Days present in a channel and lines said there, by a set of identities.
/// Days on which two of them were present count twice.
pub fn channel_presence(
    conn: &Connection,
    tag: &str,
    channel: &str,
    identities: &[String],
) -> Result<(i64, i64)> {
    if identities.is_empty() {
        return Ok((0, 0));
    }
    let placeholders: Vec<String> = (3..identities.len() + 3).map(|n| format!("?{n}")).collect();
    let values = [tag, channel]
        .into_iter()
        .chain(identities.iter().map(String::as_str));
    let presence = conn.query_row(
        &format!(
            "SELECT coalesce(sum(days_present), 0), coalesce(sum(spoken), 0) FROM activity
             WHERE tag = ?1 AND channel = ?2 COLLATE NOCASE AND identity_id IN ({})",
            placeholders.join(", ")
        ),
        params_from_iter(values),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(presence)
}

/// Checks each user of a channel in a network tag.
pub fn check(
    conn: &Connection,
    tag: &str,
    channel: &str,
    entries: &[WhoEntry],
) -> Result<Vec<Checked>> {
    let records = records(conn)?;
    let mut scores: HashMap<Vec<String>, ActivityScore> = HashMap::new();
    let mut checked = Vec::with_capacity(entries.len());
    for entry in entries {
        let (hosts, ident_is_user) = host_group(conn, tag, &entry.host)?;
        let exact = identities_matching(
            conn,
            tag,
            Some(entry.nick.as_str()),
            Some(entry.ident.as_str()),
            &hosts,
        )?;
        // A host group whose ident does not identify the person may give
        // many people the same ident, so the ident only counts with a nick.
        let new_nick = match ident_is_user {
            _ if !exact.is_empty() => Vec::new(),
            Some(false) => Vec::new(),
            Some(true) => identities_matching(conn, tag, None, Some(entry.ident.as_str()), &hosts)?,
            None => identities_matching(conn, tag, None, Some(entry.ident.as_str()), &hosts[..1])?,
        };
        let (how, ids) = if !exact.is_empty() {
            (Match::Exact, exact)
        } else if !new_nick.is_empty() {
            (Match::NewNick, new_nick)
        } else {
            let by_nick = identities_matching(conn, tag, Some(entry.nick.as_str()), None, &[])?;
            if by_nick.is_empty() {
                (Match::Unknown, by_nick)
            } else {
                (Match::NickOnly, by_nick)
            }
        };
        let identity = ids.first().cloned();
        let record = identity
            .as_deref()
            .and_then(|id| records.of_identity(id))
            .cloned();
        let activity_ids = match &record {
            Some(record) => record.identities.clone(),
            None => identity.iter().cloned().collect(),
        };
        let rating = record.as_ref().map_or(Rating::Neutral, |r| r.rating);
        let bot = record.as_ref().is_some_and(|r| r.kind == Kind::Bot);
        let (days_present, spoken) = channel_presence(conn, tag, channel, &activity_ids)?;
        let silent_days =
            (silent(days_present, spoken) && !bot && !rating.trusted()).then_some(days_present);
        let activity = match scores.get(&activity_ids) {
            Some(score) => *score,
            None => {
                let score = activity_score(conn, &activity_ids)?;
                scores.insert(activity_ids, score);
                score
            }
        };
        let verdict = match how {
            Match::Unknown => Verdict::Untrusted,
            // Watching for weeks without a word is a red flag of its own.
            _ if silent_days.is_some() => Verdict::Untrusted,
            Match::NickOnly if rating.untrusted() => Verdict::Untrusted,
            Match::NickOnly => Verdict::Undecided,
            _ if bot => Verdict::Bot,
            _ if rating.trusted() => Verdict::Trusted,
            _ if rating.untrusted() => Verdict::Untrusted,
            _ => Verdict::Undecided,
        };
        let trust = (how != Match::Unknown && !bot).then(|| trust_score(rating, activity.score));
        checked.push(Checked {
            entry: entry.clone(),
            how,
            identity,
            candidates: ids.len(),
            record,
            activity,
            trust,
            silent_days,
            verdict,
        });
    }
    Ok(checked)
}

/// The real names `/who` showed for an identity's nicks and `ident@host`,
/// most recently seen first, with when each was last seen.
pub fn real_names(conn: &Connection, identity: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT e.realname, max(l.pasted_at) FROM who_entries e
         JOIN who_lists l ON l.id = e.list_id
         JOIN identity_keys k ON k.tag = l.tag AND k.nick = lower(e.nick)
                             AND k.ident = e.ident AND k.host = e.host COLLATE NOCASE
         WHERE k.identity_id = ?1 AND e.realname <> ''
         GROUP BY e.realname ORDER BY 2 DESC",
    )?;
    let rows = stmt
        .query_map(params![identity], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

// Channels

/// When a channel's users were last reviewed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Review {
    /// UTC, as `YYYY-MM-DD HH:MM:SS`.
    pub reviewed_at: String,
    pub valid_until: String,
    pub valid: bool,
}

pub fn review(conn: &Connection, tag: &str, channel: &str) -> Result<Option<Review>> {
    let months = format!("+{REVIEW_VALID_MONTHS} months");
    let mut stmt = conn.prepare(
        "SELECT reviewed_at, datetime(reviewed_at, ?3), datetime(reviewed_at, ?3) > datetime('now')
         FROM channel_reviews WHERE tag = ?1 AND channel = ?2
         ORDER BY reviewed_at DESC, id DESC LIMIT 1",
    )?;
    let mut rows = stmt.query_map(params![tag, channel, months], |row| {
        Ok(Review {
            reviewed_at: row.get(0)?,
            valid_until: row.get(1)?,
            valid: row.get(2)?,
        })
    })?;
    Ok(rows.next().transpose()?)
}

/// A channel with its recent activity and review status.
pub struct ChannelStatus {
    pub tag: String,
    pub name: String,
    /// Spoken lines over the recent months.
    pub recent_spoken: i64,
    /// Identities present over the recent months, the logging client aside.
    pub recent_identities: i64,
    /// `YYYY-MM`, the last month anyone was present.
    pub last_month: String,
    pub spoken: i64,
    pub review: Option<Review>,
    /// When a user list was last pasted.
    pub last_who: Option<String>,
}

/// Every channel with activity, the most active recently first.
pub fn channels(conn: &Connection) -> Result<Vec<ChannelStatus>> {
    let since = format!("-{} months", RECENT_MONTHS - 1);
    let rows: Vec<(String, String, i64, i64, String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT tag, channel,
                    sum(CASE WHEN month >= ?2 THEN spoken ELSE 0 END),
                    count(DISTINCT CASE WHEN month >= ?2 AND days_present > 0
                                        THEN identity_id END),
                    max(month), sum(spoken)
             FROM activity WHERE channel <> ?1 GROUP BY tag, channel",
        )?;
        let start: String = conn.query_row(
            "SELECT strftime('%Y-%m', 'now', 'start of month', ?1)",
            params![since],
            |row| row.get(0),
        )?;
        let rows = stmt
            .query_map(params![ALL_CHANNELS, start], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    let mut pasted =
        conn.prepare("SELECT max(pasted_at) FROM who_lists WHERE tag = ?1 AND channel = ?2")?;
    let mut channels = Vec::with_capacity(rows.len());
    for (tag, name, recent_spoken, recent_identities, last_month, spoken) in rows {
        let review = review(conn, &tag, &name)?;
        let last_who: Option<String> = pasted.query_row(params![tag, name], |row| row.get(0))?;
        channels.push(ChannelStatus {
            tag,
            name,
            recent_spoken,
            recent_identities,
            last_month,
            spoken,
            review,
            last_who,
        });
    }
    channels.sort_by(|a, b| {
        b.recent_spoken
            .cmp(&a.recent_spoken)
            .then(b.spoken.cmp(&a.spoken))
            .then(a.name.cmp(&b.name))
    });
    Ok(channels)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::discover::{self, tests::TempDir};
    use crate::{db, decide, identity};

    #[test]
    fn reads_irssis_who_output() {
        let text = "\
00:52 -!- #analysis rachel    H@  0  rachel@NETADMIN.VHOST [Rachel]
00:52 -!- #analysis quokka    G   1  ~quokka@Fish-1a2b3c.example.net [a [bracketed] name]
00:52 -!- #analysis marlowe   H+  0  marlowe@host.example.org []
00:52 -!-     #other relay     H   0  relay@bots.example.org [Relay]
00:52 -!- #analysis RACHEL    H   0  rachel@elsewhere [duplicate nick]
00:52 -!- End of /WHO list
";
        let parsed = parse_who(text, "#analysis");
        assert_eq!(parsed.entries.len(), 3);
        assert_eq!(
            parsed.entries[0],
            WhoEntry {
                nick: "rachel".into(),
                ident: "rachel".into(),
                host: "NETADMIN.VHOST".into(),
                status: "H@".into(),
                realname: "Rachel".into(),
            }
        );
        assert_eq!(parsed.entries[1].ident, "~quokka");
        assert_eq!(parsed.entries[1].status, "G");
        assert_eq!(parsed.entries[1].realname, "a [bracketed] name");
        assert_eq!(parsed.entries[2].realname, "");
        assert_eq!(parsed.other_channels, ["#other"]);
        assert_eq!(parsed.skipped, 1);
    }

    #[test]
    fn matches_channels_irssi_cut_to_ten_characters() {
        let line = "-!- #wetfish-d quokka H 0 quokka@host [Quokka]";
        let parsed = parse_who(line, "#wetfish-dev-chat");
        assert_eq!(parsed.entries.len(), 1);
        assert!(parse_who(line, "#wetfish").entries.is_empty());
    }

    #[test]
    fn scores_activity_and_trust() {
        assert_eq!(month_number("2026-11"), Some(2026 * 12 + 10));
        assert_eq!(month_number("2026-13"), None);
        assert_eq!(days_in_month(month_number("2024-02").unwrap()), 29);
        assert_eq!(days_in_month(month_number("2026-12").unwrap()), 31);

        assert_eq!(trust_score(Rating::Distrusted, 100), 19);
        assert_eq!(trust_score(Rating::Wary, 0), 20);
        assert_eq!(trust_score(Rating::Neutral, 0), 40);
        assert_eq!(trust_score(Rating::Neutral, 50), 49);
        assert_eq!(trust_score(Rating::Known, 100), 99);
        // A known person who never talks outranks the busiest stranger.
        assert!(trust_score(Rating::Known, 0) > trust_score(Rating::Neutral, 100));
    }

    fn entry(nick: &str, mask: &str, realname: &str) -> WhoEntry {
        let (ident, host) = mask.split_once('@').unwrap();
        WhoEntry {
            nick: nick.into(),
            ident: ident.into(),
            host: host.into(),
            status: "H".into(),
            realname: realname.into(),
        }
    }

    #[test]
    fn checks_a_channels_users_against_the_logs_and_decisions() {
        let root = TempDir::new("trust-check");
        fs::create_dir_all(root.0.join("wetfish")).unwrap();
        fs::write(
            root.0.join("wetfish/#analysis.log"),
            "--- Log opened Sun Nov 01 22:00:00 2026\n\
             22:00 -!- rachel [rachel@hostR] has joined #analysis\n\
             22:00 -!- Irssi: #analysis: Total of 1 nicks [1 ops, 0 halfops, 0 voices, 0 normal]\n\
             22:00 -!- Irssi: Join to #analysis was synced in 0 secs\n\
             22:10 -!- quokka [quokkawet@hostQ] has joined #analysis\n\
             22:11 <quokka> hi\n\
             22:20 -!- marlowe [marlowewet@hostM] has joined #analysis\n\
             22:21 -!- relay [relay@bots.example.org] has joined #analysis\n\
             22:22 <relay> build passed\n\
             --- Log closed Sat Dec 05 23:00:00 2026\n",
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
        let (quokka, relay) = (id("quokka"), id("relay"));
        decide::create_person(&mut conn, "quokka", Kind::Person, &[quokka.clone()], None).unwrap();
        decide::set_rating(&conn, "quokka", Rating::Known).unwrap();
        decide::create_person(&mut conn, "relay", Kind::Bot, &[relay], None).unwrap();
        assert!(decide::set_rating(&conn, "relay", Rating::Known).is_err());

        let users = [
            entry("quokka", "quokkawet@hostQ", "Quokka"),
            entry("quokka_", "quokkawet@HOSTQ", ""),
            entry("marlowe", "marlowewet@elsewhere", ""),
            entry("relay", "relay@bots.example.org", "Relay"),
            entry("stranger", "x@nowhere", ""),
        ];
        let checked = check(&conn, "wetfish", "#analysis", &users).unwrap();
        let summary: Vec<(Match, Verdict)> = checked.iter().map(|c| (c.how, c.verdict)).collect();
        assert_eq!(
            summary,
            [
                (Match::Exact, Verdict::Trusted),
                (Match::NewNick, Verdict::Trusted),
                // marlowe sat in the channel for weeks without a word.
                (Match::NickOnly, Verdict::Untrusted),
                (Match::Exact, Verdict::Bot),
                (Match::Unknown, Verdict::Untrusted),
            ]
        );
        assert_eq!(checked[0].identity.as_deref(), Some(quokka.as_str()));
        assert_eq!(checked[0].activity.spoken, 1);
        assert_eq!(checked[0].activity.active_months, 1);
        assert_eq!(
            checked[0].trust,
            Some(trust_score(Rating::Known, checked[0].activity.score))
        );
        assert_eq!(checked[3].trust, None);
        assert!(checked[2].silent_days.unwrap() >= SILENT_DAYS);
        assert_eq!(checked[0].silent_days, None, "quokka spoke");
        assert_eq!(checked[3].silent_days, None, "bots are never flagged");

        // Rating a silent watcher vouched or known clears the flag.
        let marlowe = checked[2].identity.clone().unwrap();
        decide::create_person(&mut conn, "marlowe", Kind::Person, &[marlowe], None).unwrap();
        decide::set_rating(&conn, "marlowe", Rating::Vouched).unwrap();
        let rated = check(&conn, "wetfish", "#analysis", &users[2..3]).unwrap();
        assert_eq!(rated[0].silent_days, None);
        assert_eq!(
            rated[0].verdict,
            Verdict::Undecided,
            "still only a nick match"
        );
        assert!(silent(SILENT_DAYS, 0));
        assert!(!silent(SILENT_DAYS - 1, 0));
        assert!(!silent(365, 1));
        assert_eq!(checked[4].trust, None);

        // Someone using a trusted nick from an unknown host gets no trust.
        let impostor = check(
            &conn,
            "wetfish",
            "#analysis",
            &[entry("quokka", "evil@elsewhere", "")],
        )
        .unwrap();
        assert_eq!(
            (impostor[0].how, impostor[0].verdict),
            (Match::NickOnly, Verdict::Undecided)
        );
        // A distrusted nick stays untrusted from any host.
        decide::set_rating(&conn, "quokka", Rating::Distrusted).unwrap();
        let impostor = check(
            &conn,
            "wetfish",
            "#analysis",
            &[entry("quokka", "evil@elsewhere", "")],
        )
        .unwrap();
        assert_eq!(impostor[0].verdict, Verdict::Untrusted);

        // Pasted lists keep real names, which the logs never show.
        save_who(&mut conn, "wetfish", "#analysis", &users).unwrap();
        let latest = latest_who(&conn, "wetfish", "#ANALYSIS").unwrap().unwrap();
        assert_eq!((latest.entries.len(), latest.earlier), (5, 0));
        let names: Vec<String> = real_names(&conn, &quokka)
            .unwrap()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, ["Quokka"]);

        assert_eq!(review(&conn, "wetfish", "#analysis").unwrap(), None);
        decide::mark_reviewed(&conn, "wetfish", "#analysis").unwrap();
        assert!(decide::mark_reviewed(&conn, "wetfish", "#nowhere").is_err());
        let channels = channels(&conn).unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(
            (channels[0].name.as_str(), channels[0].spoken),
            ("#analysis", 2)
        );
        assert!(channels[0].review.as_ref().unwrap().valid);
        assert!(channels[0].last_who.is_some());

        delete_who(&conn, latest.id).unwrap();
        assert!(latest_who(&conn, "wetfish", "#analysis").unwrap().is_none());
    }
}
