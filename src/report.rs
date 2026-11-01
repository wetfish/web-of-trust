//! Human-readable text reports.

use std::collections::{BTreeMap, HashSet};
use std::io::{self, Write};
use std::path::Path;

use chrono::NaiveDate;

use crate::db::ImportSummary;
use crate::discover::{Inventory, LogFile, TargetKind};
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
    writeln!(out)
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

fn heading(out: &mut impl Write, title: &str, underline: char) -> io::Result<()> {
    writeln!(out, "{title}")?;
    writeln!(
        out,
        "{}",
        underline.to_string().repeat(title.chars().count())
    )
}

/// Writes an aligned table, indented two spaces. `numeric` columns are
/// right-aligned.
fn table(
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

fn count_u64(n: u64, noun: &str) -> String {
    let suffix = if n == 1 { "" } else { "s" };
    format!("{} {noun}{suffix}", thousands(n))
}

fn thousands(n: u64) -> String {
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
    }
}
