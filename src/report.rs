//! Human-readable text reports.

use std::collections::BTreeMap;
use std::io::{self, Write};

use crate::discover::{Inventory, LogFile, TargetKind};

/// Lists the log files found, grouped by network tag and channel.
pub fn inventory(out: &mut impl Write, inventory: &Inventory) -> io::Result<()> {
    heading(out, "Log inventory", '=')?;
    writeln!(out, "Log folders:")?;
    for (i, root) in inventory.roots.iter().enumerate() {
        writeln!(out, "  [{}] {}", i + 1, root.display())?;
    }

    let mut by_tag: BTreeMap<&str, Vec<&LogFile>> = BTreeMap::new();
    for file in &inventory.files {
        by_tag.entry(file.tag.as_str()).or_default().push(file);
    }

    for (tag, files) in &by_tag {
        writeln!(out)?;
        heading(out, &format!("Network tag: {tag}"), '-')?;

        let mut channels: BTreeMap<&str, Vec<&LogFile>> = BTreeMap::new();
        let mut queries: BTreeMap<&str, Vec<&LogFile>> = BTreeMap::new();
        for &file in files {
            let group = match file.kind {
                TargetKind::Channel => &mut channels,
                TargetKind::Query => &mut queries,
            };
            group.entry(file.target.as_str()).or_default().push(file);
        }

        writeln!(
            out,
            "{}, {}, {}, {}, {}",
            count(files.len(), "file"),
            count(channels.len(), "channel"),
            count(queries.len(), "private message target"),
            human_bytes(total_bytes(files)),
            count_u64(total_lines(files), "line"),
        )?;

        if !channels.is_empty() {
            writeln!(out)?;
            let rows: Vec<Vec<String>> = channels
                .iter()
                .map(|(name, files)| {
                    vec![
                        name.to_string(),
                        root_list(files),
                        human_bytes(total_bytes(files)),
                        thousands(total_lines(files)),
                    ]
                })
                .collect();
            table(
                out,
                &["Channel", "In roots", "Size", "Lines"],
                &rows,
                &[false, false, true, true],
            )?;
        }

        if !queries.is_empty() {
            let query_files: Vec<&LogFile> = queries.values().flatten().copied().collect();
            writeln!(out)?;
            writeln!(
                out,
                "Private messages: {} in {}, {}, {}",
                count(queries.len(), "target"),
                count(query_files.len(), "file"),
                human_bytes(total_bytes(&query_files)),
                count_u64(total_lines(&query_files), "line"),
            )?;
        }
    }

    writeln!(out)?;
    if inventory.files.is_empty() {
        writeln!(out, "No .log files found.")?;
    } else {
        writeln!(
            out,
            "Total: {} in {}, {}, {}",
            count(inventory.files.len(), "file"),
            count(by_tag.len(), "network tag"),
            human_bytes(inventory.files.iter().map(|f| f.bytes).sum()),
            count_u64(inventory.files.iter().map(|f| f.lines).sum(), "line"),
        )?;
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

fn total_bytes(files: &[&LogFile]) -> u64 {
    files.iter().map(|f| f.bytes).sum()
}

fn total_lines(files: &[&LogFile]) -> u64 {
    files.iter().map(|f| f.lines).sum()
}

/// The 1-based log folder numbers a channel appears in, such as "1, 3".
fn root_list(files: &[&LogFile]) -> String {
    let mut roots: Vec<usize> = files.iter().map(|f| f.root + 1).collect();
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
