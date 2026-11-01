//! Reads every discovered log file through the irssi parser and collects
//! counts for each file.

use std::fs::File;
use std::io::{self, BufRead, BufReader};

use chrono::NaiveDate;

use crate::discover::{Inventory, LogFile};
use crate::irssi::{self, DateTracker, Event, Line};

/// The most unrecognized lines kept as examples, in total and per file.
const MAX_SAMPLES: usize = 50;
const MAX_SAMPLES_PER_FILE: usize = 3;

#[derive(Default)]
pub struct FileStats {
    pub lines: u64,
    /// Messages and actions.
    pub spoken: u64,
    pub joins: u64,
    /// The first and last dates with a timestamped line.
    pub first_date: Option<NaiveDate>,
    pub last_date: Option<NaiveDate>,
    pub unrecognized: u64,
    /// Timestamped lines that came before any line giving the date.
    pub undated: u64,
    /// Set when the file could not be read to the end.
    pub error: Option<String>,
}

/// An unrecognized line, kept as an example.
pub struct Sample {
    /// Index into `Inventory::files`.
    pub file: usize,
    pub line_number: u64,
    pub text: String,
}

pub struct Scan {
    /// One entry for each of `Inventory::files`, in the same order.
    pub files: Vec<FileStats>,
    pub samples: Vec<Sample>,
}

pub fn scan(inventory: &Inventory) -> Scan {
    let mut scan = Scan {
        files: Vec::with_capacity(inventory.files.len()),
        samples: Vec::new(),
    };
    for (index, file) in inventory.files.iter().enumerate() {
        let mut stats = FileStats::default();
        if let Err(err) = scan_file(file, index, &mut stats, &mut scan.samples) {
            stats.error = Some(err.to_string());
        }
        scan.files.push(stats);
    }
    scan
}

fn scan_file(
    file: &LogFile,
    index: usize,
    stats: &mut FileStats,
    samples: &mut Vec<Sample>,
) -> io::Result<()> {
    let mut reader = BufReader::with_capacity(64 * 1024, File::open(&file.path)?);
    let mut buf = Vec::new();
    let mut dates = DateTracker::default();
    let mut file_samples = 0;

    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        stats.lines += 1;

        // Old logs may not be valid UTF-8.
        let text = String::from_utf8_lossy(&buf);
        let line = irssi::parse_line(&text);
        dates.update(&line);

        match &line {
            Line::Event(_, event) => {
                match dates.date() {
                    Some(date) => {
                        stats.first_date = Some(stats.first_date.map_or(date, |d| d.min(date)));
                        stats.last_date = Some(stats.last_date.map_or(date, |d| d.max(date)));
                    }
                    None => stats.undated += 1,
                }
                match event {
                    Event::Message { .. } | Event::Action { .. } => stats.spoken += 1,
                    Event::Join { .. } => stats.joins += 1,
                    _ => {}
                }
            }
            Line::Unrecognized => {
                stats.unrecognized += 1;
                let text = text.trim_end();
                if file_samples < MAX_SAMPLES_PER_FILE
                    && samples.len() < MAX_SAMPLES
                    && !already_sampled(samples, text)
                {
                    file_samples += 1;
                    samples.push(Sample {
                        file: index,
                        line_number: stats.lines,
                        text: text.to_string(),
                    });
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether an example with the same text, ignoring the timestamp, has
/// already been kept, so repeated lines do not crowd out other examples.
fn already_sampled(samples: &[Sample], text: &str) -> bool {
    let text = irssi::without_timestamp(text);
    samples
        .iter()
        .any(|sample| irssi::without_timestamp(&sample.text) == text)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::discover::{self, tests::TempDir};

    #[test]
    fn counts_a_sample_log() {
        let root = TempDir::new("scan");
        fs::create_dir_all(root.0.join("wetfish")).unwrap();
        fs::write(
            root.0.join("wetfish/#analysis.log"),
            "--- Log opened Sun Nov 01 01:16:13 2026\n\
             01:16 -!- rachel [rachel@NETADMIN.VHOST] has joined #analysis\n\
             01:16 -!- Irssi: Join to #analysis was synced in 0 secs\n\
             01:16 <@rachel> Hello World.\n\
             01:20  * rachel loves sample data\n\
             01:21 -!- Something new happened\n\
             01:22 -!- Something new happened\n\
             01:23 !irc.wetfish.net *** user1 invited user2 into the channel\n\
             --- Day changed Mon Nov 02 2026\n\
             00:05 < tester> morning\n",
        )
        .unwrap();

        let inventory = discover::discover(&[root.0.clone()]).unwrap();
        let scan = scan(&inventory);
        let stats = &scan.files[0];

        assert_eq!(stats.lines, 10);
        assert_eq!(stats.spoken, 3);
        assert_eq!(stats.joins, 1);
        assert_eq!(stats.unrecognized, 2);
        assert_eq!(stats.undated, 0);
        assert_eq!(stats.first_date, NaiveDate::from_ymd_opt(2026, 11, 1));
        assert_eq!(stats.last_date, NaiveDate::from_ymd_opt(2026, 11, 2));
        assert!(stats.error.is_none());

        assert_eq!(scan.samples.len(), 1);
        assert_eq!(scan.samples[0].line_number, 6);
        assert_eq!(scan.samples[0].text, "01:21 -!- Something new happened");
    }
}
