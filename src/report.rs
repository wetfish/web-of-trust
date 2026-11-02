//! Human-readable text reports.

use std::collections::{BTreeMap, HashSet};
use std::io::{self, Write};
use std::path::Path;

use chrono::NaiveDate;

use crate::db::ImportSummary;
use crate::discover::{Inventory, LogFile, TargetKind};
use crate::identity::RebuildSummary;
use crate::lookup::{Filter, IdentityView, SessionRow};
use crate::scan::{ChannelActivity, FileStats, Scan};

/// A log file with the counts read from it.
type Entry<'a> = (&'a LogFile, &'a FileStats);

/// Lists the log files found, grouped by network tag and channel, with what
/// the parser read from them.
pub fn inventory(out: &mut impl Write, inventory: &Inventory, scan: &Scan) -> io::Result<()> {
    heading(out, "Log inventory", '=')?;
    writeln!(out, "Log folders:")?;
    for (i, root) in inventory.roots.iter().enumerate() {
        writeln!(out, "  [{}] {}", i + 1, root.display())?;
    }

    let entries: Vec<Entry> = inventory.files.iter().zip(&scan.files).collect();

    let mut by_tag: BTreeMap<&str, Vec<Entry>> = BTreeMap::new();
    for &entry in &entries {
        by_tag.entry(entry.0.tag.as_str()).or_default().push(entry);
    }

    for (tag, entries) in &by_tag {
        writeln!(out)?;
        heading(out, &format!("Network tag: {tag}"), '-')?;

        let mut channels: BTreeMap<&str, Vec<Entry>> = BTreeMap::new();
        let mut queries: BTreeMap<&str, Vec<Entry>> = BTreeMap::new();
        for &entry in entries {
            let group = match entry.0.kind {
                TargetKind::Channel => &mut channels,
                TargetKind::Query => &mut queries,
            };
            group
                .entry(entry.0.target.as_str())
                .or_default()
                .push(entry);
        }

        writeln!(
            out,
            "{}, {}, {}, {}, {}",
            count(entries.len(), "file"),
            count(channels.len(), "channel"),
            count(queries.len(), "private message target"),
            human_bytes(total_bytes(entries)),
            count_u64(sum(entries, |s| s.lines), "line"),
        )?;

        if !channels.is_empty() {
            writeln!(out)?;
            let rows: Vec<Vec<String>> = channels
                .iter()
                .map(|(name, entries)| {
                    vec![
                        name.to_string(),
                        root_list(entries),
                        thousands(sum(entries, |s| s.lines)),
                        thousands(sum(entries, |s| s.spoken)),
                        thousands(sum(entries, |s| s.joins)),
                        date_or_dash(first_date(entries)),
                        date_or_dash(last_date(entries)),
                        thousands(sum(entries, |s| s.unrecognized)),
                    ]
                })
                .collect();
            table(
                out,
                &[
                    "Channel",
                    "In roots",
                    "Lines",
                    "Spoken",
                    "Joins",
                    "First day",
                    "Last day",
                    "Unparsed",
                ],
                &rows,
                &[false, false, true, true, true, false, false, true],
            )?;
        }

        if !queries.is_empty() {
            let query_entries: Vec<Entry> = queries.values().flatten().copied().collect();
            writeln!(out)?;
            writeln!(
                out,
                "Private messages: {} in {}, {}, {}, {} spoken, {} unparsed",
                count(queries.len(), "target"),
                count(query_entries.len(), "file"),
                human_bytes(total_bytes(&query_entries)),
                count_u64(sum(&query_entries, |s| s.lines), "line"),
                thousands(sum(&query_entries, |s| s.spoken)),
                thousands(sum(&query_entries, |s| s.unrecognized)),
            )?;
        }
    }

    writeln!(out)?;
    if entries.is_empty() {
        writeln!(out, "No .log files found.")?;
    } else {
        writeln!(
            out,
            "Total: {} in {}, {}, {}, {} spoken, {} unparsed",
            count(entries.len(), "file"),
            count(by_tag.len(), "network tag"),
            human_bytes(total_bytes(&entries)),
            count_u64(sum(&entries, |s| s.lines), "line"),
            thousands(sum(&entries, |s| s.spoken)),
            thousands(sum(&entries, |s| s.unrecognized)),
        )?;
    }

    let problems: Vec<String> = entries
        .iter()
        .flat_map(|&(file, stats)| {
            let mut found = Vec::new();
            if let Some(err) = &stats.error {
                found.push(format!("{}: stopped reading: {err}", file_label(file)));
            }
            if stats.undated > 0 {
                found.push(format!(
                    "{}: {} before the first line giving a date",
                    file_label(file),
                    count_u64(stats.undated, "timestamped line"),
                ));
            }
            found
        })
        .collect();
    if !problems.is_empty() {
        writeln!(out)?;
        heading(out, &format!("Read problems ({})", problems.len()), '-')?;
        for problem in &problems {
            writeln!(out, "{problem}")?;
        }
    }

    if !scan.samples.is_empty() {
        let total = sum(&entries, |s| s.unrecognized);
        writeln!(out)?;
        heading(
            out,
            &format!(
                "Unparsed lines ({} in total, showing {})",
                thousands(total),
                scan.samples.len()
            ),
            '-',
        )?;
        for sample in &scan.samples {
            writeln!(
                out,
                "{}:{}: {}",
                file_label(&inventory.files[sample.file]),
                sample.line_number,
                sample.text
            )?;
        }
    }

    if !inventory.skipped.is_empty() {
        writeln!(out)?;
        heading(out, &format!("Skipped ({})", inventory.skipped.len()), '-')?;
        for skipped in &inventory.skipped {
            writeln!(out, "{}: {}", skipped.path.display(), skipped.reason)?;
        }
    }

    Ok(())
}

/// Lists spoken lines and active users for each day one channel had spoken
/// lines. Active users are distinct nicks, ignoring case.
pub fn daily_activity(
    out: &mut impl Write,
    inventory: &Inventory,
    channel: &str,
    activity: &ChannelActivity,
) -> io::Result<()> {
    heading(out, &format!("Daily activity: {channel}"), '=')?;
    writeln!(out, "Log files:")?;
    for &index in &activity.files {
        writeln!(out, "  {}", file_label(&inventory.files[index]))?;
    }
    if activity.files.len() > 1 {
        writeln!(
            out,
            "Counts from these files are added together, so a day logged by more than one file is counted more than once."
        )?;
    }
    writeln!(out)?;

    let (Some(first), Some(last)) = (
        activity.days.keys().next(),
        activity.days.keys().next_back(),
    ) else {
        writeln!(out, "No spoken lines.")?;
        return undated_note(out, activity);
    };

    let rows: Vec<Vec<String>> = activity
        .days
        .iter()
        .map(|(date, day)| {
            vec![
                date.to_string(),
                thousands(day.spoken),
                thousands(day.nicks.len() as u64),
            ]
        })
        .collect();
    table(
        out,
        &["Day", "Spoken", "Active users"],
        &rows,
        &[false, true, true],
    )?;

    let spoken: u64 = activity.days.values().map(|day| day.spoken).sum();
    let nicks: HashSet<&str> = activity
        .days
        .values()
        .flat_map(|day| day.nicks.iter().map(String::as_str))
        .collect();
    writeln!(out)?;
    writeln!(
        out,
        "{} with spoken lines, from {first} to {last}",
        count(activity.days.len(), "day")
    )?;
    writeln!(
        out,
        "Total: {}, {}",
        count_u64(spoken, "spoken line"),
        count(nicks.len(), "distinct nick")
    )?;
    undated_note(out, activity)
}

/// Summarizes what an import added to the database.
pub fn import_summary(
    out: &mut impl Write,
    db_path: &Path,
    summary: &ImportSummary,
) -> io::Result<()> {
    writeln!(out, "Database: {}", db_path.display())?;
    writeln!(
        out,
        "Imported {}: {}, {} with new lines, {} replaced, {} unchanged",
        count_u64(summary.lines, "new line"),
        count(summary.new_files, "new file"),
        thousands(summary.grown_files as u64),
        thousands(summary.replaced_files as u64),
        thousands(summary.unchanged_files as u64),
    )?;
    for (path, err) in &summary.errors {
        writeln!(out, "Could not import {}: {err}", path.display())?;
    }
    Ok(())
}

/// Summarizes a rebuild of sessions and identities.
pub fn identity_summary(out: &mut impl Write, summary: &RebuildSummary) -> io::Result<()> {
    writeln!(
        out,
        "Identities: {} from {} ({} new, {} merged)",
        thousands(summary.identities as u64),
        count(summary.sessions, "session"),
        thousands(summary.new_identities as u64),
        thousands(summary.merged_identities as u64),
    )?;
    writeln!(
        out,
        "Sessions without a host: {} inferred from the same nick, {} known by nick only",
        thousands(summary.inferred_sessions as u64),
        thousands(summary.nick_only_sessions as u64),
    )?;
    if summary.cuts_applied + summary.cuts_stale > 0 {
        writeln!(
            out,
            "Cuts: {} applied, starting {}, {} stale (see wot cuts list)",
            thousands(summary.cuts_applied as u64),
            count(summary.cut_sessions, "session"),
            thousands(summary.cuts_stale as u64),
        )?;
    }
    Ok(())
}

/// Identities are shown in full when at most this many match.
const MAX_DETAILED: usize = 10;
/// Examples of nick changes and inferred sessions shown per identity.
const MAX_EXAMPLES: usize = 10;

/// Lists the identities matching a lookup, most recently seen first, and
/// shows each in full when only a few match.
pub fn identities(out: &mut impl Write, filter: &Filter, views: &[IdentityView]) -> io::Result<()> {
    heading(
        out,
        &format!("Identities matching {}", filter.describe()),
        '=',
    )?;
    writeln!(
        out,
        "{}, most recently seen first. Channel time adds up time in each channel, so time \
         in two channels at once counts twice. Private messages add no time.",
        count_plural(views.len() as u64, "identity", "identities")
    )?;
    writeln!(out)?;

    let rows: Vec<Vec<String>> = views
        .iter()
        .map(|view| {
            let sessions = &view.sessions;
            vec![
                view.id.clone(),
                person_label(&view.people),
                nick_list(sessions, 4),
                thousands(connections(sessions).len() as u64),
                thousands(sessions.len() as u64),
                duration(channel_minutes(sessions.iter())),
                thousands(spoken(sessions.iter())),
                day(&sessions[0].start_at).to_string(),
                day(view.last_seen()).to_string(),
            ]
        })
        .collect();
    table(
        out,
        &[
            "Identity",
            "Person",
            "Nicks",
            "Hosts",
            "Sessions",
            "Channel time",
            "Spoken",
            "First seen",
            "Last seen",
        ],
        &rows,
        &[false, false, false, true, true, true, true, false, false],
    )?;

    if views.len() > MAX_DETAILED {
        writeln!(out)?;
        writeln!(
            out,
            "Details are shown when {MAX_DETAILED} or fewer identities match. Narrow the \
             search, for example by adding --nick, --ident or --host."
        )?;
        return Ok(());
    }
    for view in views {
        writeln!(out)?;
        identity_details(out, view)?;
    }
    Ok(())
}

fn identity_details(out: &mut impl Write, view: &IdentityView) -> io::Result<()> {
    let sessions = &view.sessions;
    heading(out, &format!("Identity {} ({})", view.id, view.tag), '-')?;
    match view.people.as_slice() {
        [] => writeln!(out, "Person: not assigned")?,
        [person] => writeln!(out, "Person: {person}")?,
        people => writeln!(
            out,
            "People: {} (conflict: identities added to different people were merged since)",
            people.join(", ")
        )?,
    }

    let linked = |link: &str| sessions.iter().filter(|s| s.link == link).count() as u64;
    let logger = sessions.iter().filter(|s| s.logger).count() as u64;
    let mut line = format!(
        "{}: {} with a host, {} inferred, {} known by nick only",
        count_u64(sessions.len() as u64, "session"),
        thousands(linked("host")),
        thousands(linked("inferred")),
        thousands(linked("nick_only")),
    );
    let cut = linked("cut");
    if cut > 0 {
        line.push_str(&format!(", {} started by a cut", thousands(cut)));
    }
    if logger > 0 {
        line.push_str(&format!(", {} as the logging client", thousands(logger)));
    }
    writeln!(out, "{line}")?;
    writeln!(
        out,
        "Seen {} to {}, channel time {}, {}",
        day(&sessions[0].start_at),
        day(view.last_seen()),
        duration(channel_minutes(sessions.iter())),
        count_u64(spoken(sessions.iter()), "spoken line"),
    )?;

    // Nicks, by when they were last used.
    let mut nicks: BTreeMap<String, (String, u64, &str, &str)> = BTreeMap::new();
    for session in sessions {
        for nick in &session.nicks {
            let entry = nicks.entry(nick.to_lowercase()).or_insert((
                nick.clone(),
                0,
                session.start_at.as_str(),
                session.end_at.as_str(),
            ));
            entry.1 += 1;
            entry.2 = entry.2.min(session.start_at.as_str());
            entry.3 = entry.3.max(session.end_at.as_str());
        }
    }
    let mut nicks: Vec<_> = nicks.into_values().collect();
    nicks.sort_by(|a, b| b.3.cmp(a.3));
    let rows: Vec<Vec<String>> = nicks
        .iter()
        .map(|(nick, count, first, last)| {
            vec![
                nick.clone(),
                thousands(*count),
                day(first).to_string(),
                day(last).to_string(),
            ]
        })
        .collect();
    writeln!(out)?;
    table(
        out,
        &["Nick", "Sessions", "First seen", "Last seen"],
        &rows,
        &[false, true, false, false],
    )?;

    // Connections: ident@host and how the sessions were linked.
    let mut groups: BTreeMap<(String, &str), (u64, u64, &str, &str)> = BTreeMap::new();
    for session in sessions {
        let entry = groups
            .entry((connection_label(session), session.link.as_str()))
            .or_insert((0, 0, session.start_at.as_str(), session.end_at.as_str()));
        entry.0 += 1;
        entry.1 += session.spoken.max(0) as u64;
        entry.2 = entry.2.min(session.start_at.as_str());
        entry.3 = entry.3.max(session.end_at.as_str());
    }
    let mut groups: Vec<_> = groups.into_iter().collect();
    groups.sort_by(|a, b| b.1.3.cmp(a.1.3));
    let rows: Vec<Vec<String>> = groups
        .iter()
        .map(|((label, link), (count, spoken, first, last))| {
            vec![
                label.clone(),
                link_label(link).to_string(),
                thousands(*count),
                thousands(*spoken),
                day(first).to_string(),
                day(last).to_string(),
            ]
        })
        .collect();
    writeln!(out)?;
    table(
        out,
        &[
            "Connection",
            "Linked by",
            "Sessions",
            "Spoken",
            "First seen",
            "Last seen",
        ],
        &rows,
        &[false, false, true, true, false, false],
    )?;

    // Channels and private messages.
    let mut targets: BTreeMap<&str, (bool, u64, i64, u64, &str)> = BTreeMap::new();
    for session in sessions {
        let entry = targets.entry(session.target.as_str()).or_insert((
            session.channel,
            0,
            0,
            0,
            session.end_at.as_str(),
        ));
        entry.1 += 1;
        if session.channel {
            entry.2 += session.minutes;
        }
        entry.3 += session.spoken.max(0) as u64;
        entry.4 = entry.4.max(session.end_at.as_str());
    }
    let mut targets: Vec<_> = targets.into_iter().collect();
    targets.sort_by(|a, b| b.1.4.cmp(a.1.4));
    let rows: Vec<Vec<String>> = targets
        .iter()
        .map(|(target, (channel, count, minutes, spoken, last))| {
            vec![
                target.to_string(),
                if *channel { "channel" } else { "private" }.to_string(),
                thousands(*count),
                if *channel {
                    duration(*minutes)
                } else {
                    "-".to_string()
                },
                thousands(*spoken),
                day(last).to_string(),
            ]
        })
        .collect();
    writeln!(out)?;
    table(
        out,
        &[
            "Channel or nick",
            "Kind",
            "Sessions",
            "Time",
            "Spoken",
            "Last seen",
        ],
        &rows,
        &[false, false, true, true, true, false],
    )?;

    // Evidence: the nick changes that linked nicks, most recent first.
    let changes: Vec<_> = sessions
        .iter()
        .rev()
        .filter(|s| s.nicks.len() > 1)
        .collect();
    if !changes.is_empty() {
        writeln!(out)?;
        writeln!(out, "{}", examples_title("Nick changes", changes.len()))?;
        let rows: Vec<Vec<String>> = changes
            .iter()
            .take(MAX_EXAMPLES)
            .map(|session| {
                let end = session
                    .end_line
                    .map_or_else(|| "end".to_string(), |line| line.to_string());
                vec![
                    day(&session.start_at).to_string(),
                    format!("{}:{}-{end}", session.file, session.start_line),
                    session.nicks.join(" -> "),
                ]
            })
            .collect();
        table(
            out,
            &["Day", "Lines", "Nicks"],
            &rows,
            &[false, false, false],
        )?;
    }

    // Audit trail for sessions attributed by nick alone.
    let inferred: Vec<_> = sessions
        .iter()
        .rev()
        .filter(|s| s.link == "inferred")
        .collect();
    if !inferred.is_empty() {
        writeln!(out)?;
        writeln!(
            out,
            "{}",
            examples_title("Inferred sessions", inferred.len())
        )?;
        let rows: Vec<Vec<String>> = inferred
            .iter()
            .take(MAX_EXAMPLES)
            .map(|session| {
                vec![
                    day(&session.start_at).to_string(),
                    format!("{}:{}", session.file, session.start_line),
                    session.nicks.join(" -> "),
                    session.inferred_from.clone().unwrap_or_default(),
                ]
            })
            .collect();
        table(
            out,
            &["Day", "Line", "Nicks", "Inferred from"],
            &rows,
            &[false, false, false, false],
        )?;
    }
    Ok(())
}

/// The people an identity belongs to, `-` for none, or the names with a
/// warning when merges put it in more than one.
fn person_label(people: &[String]) -> String {
    match people {
        [] => "-".to_string(),
        [person] => person.clone(),
        people => format!("conflict: {}", people.join(", ")),
    }
}

fn examples_title(what: &str, total: usize) -> String {
    if total > MAX_EXAMPLES {
        format!(
            "{what} ({}, showing the {MAX_EXAMPLES} most recent):",
            thousands(total as u64)
        )
    } else {
        format!("{what}:")
    }
}

/// The distinct nicks of some sessions, most recently used first, at most
/// `limit` of them followed by how many more there are.
fn nick_list(sessions: &[SessionRow], limit: usize) -> String {
    let mut seen = HashSet::new();
    let mut nicks = Vec::new();
    for session in sessions.iter().rev() {
        for nick in session.nicks.iter().rev() {
            if seen.insert(nick.to_lowercase()) {
                nicks.push(nick.as_str());
            }
        }
    }
    let mut list = nicks
        .iter()
        .take(limit)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    if nicks.len() > limit {
        list.push_str(&format!(" +{}", nicks.len() - limit));
    }
    list
}

/// The distinct `ident@host` combinations of some sessions.
fn connections(sessions: &[SessionRow]) -> HashSet<String> {
    sessions
        .iter()
        .filter(|session| session.host.is_some())
        .map(connection_label)
        .collect()
}

fn connection_label(session: &SessionRow) -> String {
    match &session.host {
        Some(host) => format!("{}@{host}", session.ident.as_deref().unwrap_or_default()),
        None => "(no host seen)".to_string(),
    }
}

fn link_label(link: &str) -> &str {
    match link {
        "host" => "nick and host",
        "inferred" => "inferred",
        "nick_only" => "nick only",
        "cut" => "cut",
        other => other,
    }
}

fn channel_minutes<'a>(sessions: impl Iterator<Item = &'a SessionRow>) -> i64 {
    sessions
        .filter(|session| session.channel)
        .map(|session| session.minutes)
        .sum()
}

fn spoken<'a>(sessions: impl Iterator<Item = &'a SessionRow>) -> u64 {
    sessions.map(|session| session.spoken.max(0) as u64).sum()
}

/// The `YYYY-MM-DD` part of a `YYYY-MM-DD HH:MM` timestamp.
pub(crate) fn day(at: &str) -> &str {
    at.get(..10).unwrap_or(at)
}

/// Minutes as `45m` below an hour, otherwise whole hours such as `1,234h`.
fn duration(minutes: i64) -> String {
    let minutes = minutes.max(0) as u64;
    if minutes < 60 {
        format!("{minutes}m")
    } else {
        format!("{}h", thousands(minutes / 60))
    }
}

fn undated_note(out: &mut impl Write, activity: &ChannelActivity) -> io::Result<()> {
    if activity.undated_spoken > 0 {
        writeln!(
            out,
            "Not shown: {} before the first line giving a date",
            count_u64(activity.undated_spoken, "spoken line")
        )?;
    }
    Ok(())
}

pub(crate) fn heading(out: &mut impl Write, title: &str, underline: char) -> io::Result<()> {
    writeln!(out, "{title}")?;
    writeln!(
        out,
        "{}",
        underline.to_string().repeat(title.chars().count())
    )
}

/// Writes an aligned table, indented two spaces. `numeric` columns are
/// right-aligned.
pub(crate) fn table(
    out: &mut impl Write,
    header: &[&str],
    rows: &[Vec<String>],
    numeric: &[bool],
) -> io::Result<()> {
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }

    let header: Vec<String> = header.iter().map(|h| h.to_string()).collect();
    write_row(out, &header, &widths, numeric)?;
    for row in rows {
        write_row(out, row, &widths, numeric)?;
    }
    Ok(())
}

fn write_row(
    out: &mut impl Write,
    cells: &[String],
    widths: &[usize],
    numeric: &[bool],
) -> io::Result<()> {
    let mut line = String::from("  ");
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            line.push_str("   ");
        }
        let width = widths[i];
        if numeric[i] {
            line.push_str(&format!("{cell:>width$}"));
        } else {
            line.push_str(&format!("{cell:<width$}"));
        }
    }
    writeln!(out, "{}", line.trim_end())
}

/// A file as `[root] tag/target.log`, matching the numbered log folders at
/// the top of the report.
fn file_label(file: &LogFile) -> String {
    format!("[{}] {}/{}.log", file.root + 1, file.tag, file.target)
}

fn total_bytes(entries: &[Entry]) -> u64 {
    entries.iter().map(|&(file, _)| file.bytes).sum()
}

fn sum(entries: &[Entry], field: impl Fn(&FileStats) -> u64) -> u64 {
    entries.iter().map(|&(_, stats)| field(stats)).sum()
}

fn first_date(entries: &[Entry]) -> Option<NaiveDate> {
    entries
        .iter()
        .filter_map(|&(_, stats)| stats.first_date)
        .min()
}

fn last_date(entries: &[Entry]) -> Option<NaiveDate> {
    entries
        .iter()
        .filter_map(|&(_, stats)| stats.last_date)
        .max()
}

fn date_or_dash(date: Option<NaiveDate>) -> String {
    date.map_or_else(|| "-".to_string(), |date| date.to_string())
}

/// The 1-based log folder numbers a channel appears in, such as "1, 3".
fn root_list(entries: &[Entry]) -> String {
    let mut roots: Vec<usize> = entries.iter().map(|&(file, _)| file.root + 1).collect();
    roots.dedup();
    roots
        .iter()
        .map(|root| root.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn count(n: usize, noun: &str) -> String {
    count_u64(n as u64, noun)
}

fn count_plural(n: u64, one: &str, many: &str) -> String {
    let noun = if n == 1 { one } else { many };
    format!("{} {noun}", thousands(n))
}

fn count_u64(n: u64, noun: &str) -> String {
    let suffix = if n == 1 { "" } else { "s" };
    format!("{} {noun}{suffix}", thousands(n))
}

pub(crate) fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn formats_byte_sizes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1_536), "1.5 KiB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MiB");
    }

    #[test]
    fn counts_with_plurals() {
        assert_eq!(count(1, "file"), "1 file");
        assert_eq!(count(2, "file"), "2 files");
        assert_eq!(count_u64(12_000, "line"), "12,000 lines");
        assert_eq!(count_plural(1, "identity", "identities"), "1 identity");
        assert_eq!(count_plural(3, "identity", "identities"), "3 identities");
    }

    #[test]
    fn formats_durations_and_days() {
        assert_eq!(duration(45), "45m");
        assert_eq!(duration(90), "1h");
        assert_eq!(duration(60 * 1_500), "1,500h");
        assert_eq!(day("2026-11-01 19:32"), "2026-11-01");
    }
}
