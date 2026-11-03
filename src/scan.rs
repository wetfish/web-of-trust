//! Reads every discovered log file through the irssi parser and collects
//! counts for each file, and day by day activity for one chosen channel.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{self, BufRead, BufReader};

use chrono::NaiveDate;

use crate::discover::{Inventory, LogFile};
use crate::irssi::{self, DateTracker, Event, Line};
use crate::progress::{Progress, Unit};
use crate::report::thousands;

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

/// Spoken lines and the nicks that spoke them on one day.
#[derive(Default)]
pub struct Day {
    pub spoken: u64,
    /// Lowercased, since IRC nicks are case-insensitive.
    pub nicks: HashSet<String>,
}

/// Day by day activity for one channel, from every file logging it.
#[derive(Default)]
pub struct ChannelActivity {
    /// Indexes into `Inventory::files` of the files read for the channel.
    pub files: Vec<usize>,
    /// Only days with at least one spoken line.
    pub days: BTreeMap<NaiveDate, Day>,
    /// Spoken lines that came before any line giving the date.
    pub undated_spoken: u64,
}

impl ChannelActivity {
    fn record_spoken(&mut self, date: Option<NaiveDate>, nick: &str) {
        let Some(date) = date else {
            self.undated_spoken += 1;
            return;
        };
        let day = self.days.entry(date).or_default();
        day.spoken += 1;
        day.nicks.insert(nick.to_lowercase());
    }
}

pub struct Scan {
    /// One entry for each of `Inventory::files`, in the same order.
    pub files: Vec<FileStats>,
    pub samples: Vec<Sample>,
    /// Set when a channel was chosen.
    pub activity: Option<ChannelActivity>,
}

/// Scans quietly, for tests.
#[cfg(test)]
pub fn scan(inventory: &Inventory, channel: Option<&str>) -> Scan {
    scan_with_progress(inventory, channel, &mut Progress::quiet())
}

/// Lines between progress reports while a file is read.
const PROGRESS_LINES: u64 = 50_000;

/// Reads every file, reporting progress by bytes as it goes. When `channel`
/// is given, also collects day by day activity from the files whose target
/// matches it, ignoring case.
pub fn scan_with_progress(
    inventory: &Inventory,
    channel: Option<&str>,
    progress: &mut Progress,
) -> Scan {
    let total: u64 = inventory.files.iter().map(|file| file.bytes).sum();
    progress.stage("Reading log files for the report", total, Unit::Bytes);
    let mut finished: u64 = 0;
    let mut scan = Scan {
        files: Vec::with_capacity(inventory.files.len()),
        samples: Vec::new(),
        activity: channel.map(|_| ChannelActivity::default()),
    };
    for (index, file) in inventory.files.iter().enumerate() {
        let activity = match (&mut scan.activity, channel) {
            (Some(activity), Some(channel)) if file.target.eq_ignore_ascii_case(channel) => {
                activity.files.push(index);
                Some(activity)
            }
            _ => None,
        };

        let mut stats = FileStats::default();
        let count = inventory.files.len() as u64;
        let mut tick = |read: u64| {
            progress.update(finished + read, || {
                format!(
                    "file {} of {}",
                    thousands(index as u64 + 1),
                    thousands(count)
                )
            });
        };
        if let Err(err) = scan_file(
            file,
            index,
            &mut stats,
            &mut scan.samples,
            activity,
            &mut tick,
        ) {
            stats.error = Some(err.to_string());
        }
        finished += file.bytes;
        scan.files.push(stats);
    }
    progress.done(&format!(
        "{} files",
        thousands(inventory.files.len() as u64)
    ));
    scan
}

fn scan_file(
    file: &LogFile,
    index: usize,
    stats: &mut FileStats,
    samples: &mut Vec<Sample>,
    mut activity: Option<&mut ChannelActivity>,
    tick: &mut dyn FnMut(u64),
) -> io::Result<()> {
    let mut reader = BufReader::with_capacity(64 * 1024, File::open(&file.path)?);
    let mut buf = Vec::new();
    let mut read: u64 = 0;
    let mut dates = DateTracker::default();
    let mut file_samples = 0;

    loop {
        buf.clear();
        let bytes = reader.read_until(b'\n', &mut buf)?;
        if bytes == 0 {
            break;
        }
        read += bytes as u64;
        stats.lines += 1;
        if stats.lines % PROGRESS_LINES == 0 {
            tick(read);
        }

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
                    Event::Message { nick, .. } | Event::Action { nick, .. } => {
                        stats.spoken += 1;
                        if let Some(activity) = activity.as_deref_mut() {
                            activity.record_spoken(dates.date(), nick);
                        }
                    }
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
        let scan = scan(&inventory, None);
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
        assert!(scan.activity.is_none());
    }

    #[test]
    fn collects_daily_activity_for_one_channel() {
        let root = TempDir::new("activity");
        fs::create_dir_all(root.0.join("wetfish")).unwrap();
        fs::write(
            root.0.join("wetfish/#analysis.log"),
            "00:01 <rachel> before any date\n\
             --- Log opened Sun Nov 01 01:16:13 2026\n\
             01:16 -!- rachel [rachel@NETADMIN.VHOST] has joined #analysis\n\
             01:16 <@rachel> Hello World.\n\
             01:20  * Rachel loves sample data\n\
             01:21 < tester> Helllo\n\
             --- Day changed Mon Nov 02 2026\n\
             --- Day changed Tue Nov 03 2026\n\
             00:05 < tester> morning\n",
        )
        .unwrap();
        fs::write(
            root.0.join("wetfish/#other.log"),
            "--- Log opened Sun Nov 01 01:16:13 2026\n\
             01:16 <someone> not counted\n",
        )
        .unwrap();

        let inventory = discover::discover(&[root.0.clone()]).unwrap();
        let scan = scan(&inventory, Some("#ANALYSIS"));
        let activity = scan.activity.unwrap();

        assert_eq!(activity.files, [0]);
        assert_eq!(activity.undated_spoken, 1);

        let days: Vec<(NaiveDate, u64, usize)> = activity
            .days
            .iter()
            .map(|(date, day)| (*date, day.spoken, day.nicks.len()))
            .collect();
        assert_eq!(
            days,
            [
                (NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(), 3, 2),
                (NaiveDate::from_ymd_opt(2026, 11, 3).unwrap(), 1, 1),
            ]
        );
    }
}
