//! Progress on standard error while long work runs, so a large import never
//! looks stuck. In a terminal, one line is redrawn in place a few times a
//! second; otherwise, such as when output goes to a file, a line is printed
//! every few seconds. Each stage ends with a line saying what it did and how
//! long it took. The same can be kept in a shared report, for the review
//! page of an analysis started in the browser.

use std::io::{self, IsTerminal, Write};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::report::thousands;

/// How often the line is redrawn in a terminal.
const TERMINAL_INTERVAL: Duration = Duration::from_millis(200);
/// How often a line is printed when standard error is not a terminal.
const LOG_INTERVAL: Duration = Duration::from_secs(10);
/// How often a shared report is brought up to date.
const SHARED_INTERVAL: Duration = Duration::from_millis(250);

/// Progress as another thread reads it: the stages that ended, the one
/// under way, and the notes in between.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub done: Vec<Done>,
    pub current: Option<Current>,
    /// Lines outside any stage, such as how many log files were found.
    pub notes: Vec<String>,
}

pub type SharedReport = Arc<Mutex<Report>>;

/// A stage that ended: what it was, what it did and how long it took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Done {
    pub label: String,
    pub summary: String,
    pub took: String,
}

/// The stage under way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Current {
    pub label: String,
    /// How far it has come out of `total`, or with no total when 0.
    pub done: u64,
    pub total: u64,
    /// The line a terminal shows, such as `Reading events: 25% (...)`.
    pub line: String,
}

/// What a stage counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Bytes,
    /// Things, by their plural name, such as `events`.
    Items(&'static str),
}

struct Stage {
    label: String,
    total: u64,
    unit: Unit,
    started: Instant,
    /// When the line was last shown.
    shown: Option<Instant>,
}

/// Reports stages of work and their progress.
pub struct Progress {
    /// Whether to write to standard error.
    enabled: bool,
    terminal: bool,
    stage: Option<Stage>,
    /// Whether a redrawn line is on screen and has to be cleared.
    drawn: bool,
    /// A report kept up to date for another thread, and when it last was.
    shared: Option<SharedReport>,
    shared_at: Option<Instant>,
}

impl Progress {
    /// Reports on standard error.
    pub fn to_stderr() -> Self {
        Progress {
            enabled: true,
            terminal: io::stderr().is_terminal(),
            stage: None,
            drawn: false,
            shared: None,
            shared_at: None,
        }
    }

    /// Reports on standard error, and in `report` for another thread to show.
    pub fn to_stderr_and(report: SharedReport) -> Self {
        let mut progress = Progress::to_stderr();
        progress.shared = Some(report);
        progress
    }

    /// Reports nothing, for tests.
    #[cfg(test)]
    pub fn quiet() -> Self {
        Progress {
            enabled: false,
            terminal: false,
            stage: None,
            drawn: false,
            shared: None,
            shared_at: None,
        }
    }

    /// Reports only in `report`, for tests.
    #[cfg(test)]
    pub fn quiet_and(report: SharedReport) -> Self {
        let mut progress = Progress::quiet();
        progress.shared = Some(report);
        progress
    }

    /// Changes the shared report, if there is one.
    fn share(&self, change: impl FnOnce(&mut Report)) {
        if let Some(shared) = &self.shared {
            change(&mut shared.lock().unwrap_or_else(PoisonError::into_inner));
        }
    }

    /// Starts a stage counting up to `total`, or with no total when 0.
    pub fn stage(&mut self, label: &str, total: u64, unit: Unit) {
        self.clear();
        self.stage = Some(Stage {
            label: label.to_string(),
            total,
            unit,
            started: Instant::now(),
            shown: None,
        });
        self.shared_at = None;
        self.share(|report| {
            report.current = Some(Current {
                label: label.to_string(),
                done: 0,
                total,
                line: format!("{label}..."),
            });
        });
        if !self.enabled {
            return;
        }
        if self.terminal {
            // Shown until the first update replaces it, so a stage with no
            // updates still says what is happening.
            let mut err = io::stderr().lock();
            let _ = write!(err, "\r\x1b[2K{label}...");
            let _ = err.flush();
            self.drawn = true;
        } else {
            eprintln!("{label}...");
        }
    }

    /// Reports how far the stage has come. `detail` is only built when the
    /// line is shown, so calling this often costs little.
    pub fn update(&mut self, done: u64, detail: impl FnOnce() -> String) {
        let Some(stage) = self.stage.as_mut() else {
            return;
        };
        let now = Instant::now();
        let interval = if self.terminal {
            TERMINAL_INTERVAL
        } else {
            LOG_INTERVAL
        };
        let print = self.enabled
            && match stage.shown {
                Some(shown) => now.duration_since(shown) >= interval,
                // The first line waits a moment, so quick stages stay quiet.
                None => now.duration_since(stage.started) >= interval,
            };
        let share = self.shared.is_some()
            && self
                .shared_at
                .is_none_or(|at| now.duration_since(at) >= SHARED_INTERVAL);
        if !print && !share {
            return;
        }
        if print {
            stage.shown = Some(now);
        }
        let line = progress_line(stage, done, &detail(), now);
        let (label, total) = (stage.label.clone(), stage.total);
        if share {
            self.shared_at = Some(now);
            self.share(|report| {
                report.current = Some(Current {
                    label,
                    done,
                    total,
                    line: line.clone(),
                });
            });
        }
        if !print {
            return;
        }
        let mut err = io::stderr().lock();
        if self.terminal {
            // Back to the start of the line, and clear it.
            let _ = write!(err, "\r\x1b[2K{line}");
            self.drawn = true;
        } else {
            let _ = writeln!(err, "{line}");
        }
        let _ = err.flush();
    }

    /// Ends the stage with what it did, and how long it took.
    pub fn done(&mut self, summary: &str) {
        self.clear();
        let Some(stage) = self.stage.take() else {
            return;
        };
        let took = elapsed(stage.started.elapsed());
        if self.enabled {
            eprintln!("{}: {summary} ({took})", stage.label);
        }
        self.share(|report| {
            report.current = None;
            report.done.push(Done {
                label: stage.label,
                summary: summary.to_string(),
                took,
            });
        });
    }

    /// Prints a line of its own, outside any stage's progress.
    pub fn note(&mut self, message: &str) {
        self.clear();
        if self.enabled {
            eprintln!("{message}");
        }
        self.share(|report| report.notes.push(message.to_string()));
    }

    /// Removes a redrawn line from the terminal.
    fn clear(&mut self) {
        if self.drawn {
            let mut err = io::stderr().lock();
            let _ = write!(err, "\r\x1b[2K");
            let _ = err.flush();
            self.drawn = false;
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.clear();
    }
}

fn progress_line(stage: &Stage, done: u64, detail: &str, now: Instant) -> String {
    let count = |n: u64| match stage.unit {
        Unit::Bytes => bytes(n),
        Unit::Items(_) => thousands(n),
    };
    let name = match stage.unit {
        Unit::Bytes => "",
        Unit::Items(name) => name,
    };
    let mut line = format!("{}: ", stage.label);
    if stage.total > 0 {
        let percent = (done as f64 * 100.0 / stage.total as f64).min(100.0);
        line.push_str(&format!(
            "{percent:.0}% ({} of {}{}{name})",
            count(done.min(stage.total)),
            count(stage.total),
            if name.is_empty() { "" } else { " " }
        ));
    } else {
        line.push_str(&format!(
            "{}{}{name}",
            count(done),
            if name.is_empty() { "" } else { " " }
        ));
    }
    if !detail.is_empty() {
        line.push_str(", ");
        line.push_str(detail);
    }
    line.push_str(&format!(", {}", elapsed(now.duration_since(stage.started))));
    line
}

/// A size such as `512 B`, `3.4 MB` or `1.2 GB`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut size = n as f64 / 1024.0;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

/// A duration such as `0.4s`, `12s` or `3m 05s`.
pub fn elapsed(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    if seconds < 10.0 {
        format!("{seconds:.1}s")
    } else if seconds < 60.0 {
        format!("{seconds:.0}s")
    } else {
        let whole = seconds as u64;
        format!("{}m {:02}s", whole / 60, whole % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes_counts_and_durations() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(3 * 1024 * 1024 + 400 * 1024), "3.4 MB");
        assert_eq!(bytes(5 * 1024 * 1024 * 1024), "5.0 GB");
        assert_eq!(elapsed(Duration::from_millis(400)), "0.4s");
        assert_eq!(elapsed(Duration::from_secs(12)), "12s");
        assert_eq!(elapsed(Duration::from_secs(185)), "3m 05s");
    }

    #[test]
    fn describes_progress_with_and_without_a_total() {
        let started = Instant::now();
        let stage = |total, unit| Stage {
            label: "Reading events".into(),
            total,
            unit,
            started,
            shown: None,
        };
        let line = progress_line(
            &stage(4_000, Unit::Items("events")),
            1_000,
            "12 sessions",
            started,
        );
        assert_eq!(
            line,
            "Reading events: 25% (1,000 of 4,000 events), 12 sessions, 0.0s"
        );
        let line = progress_line(&stage(0, Unit::Bytes), 2048, "", started);
        assert_eq!(line, "Reading events: 2.0 KB, 0.0s");
    }

    #[test]
    fn keeps_a_shared_report_for_another_thread() {
        let report: SharedReport = Arc::default();
        let mut progress = Progress::quiet_and(Arc::clone(&report));
        progress.note("Found 2 log files");
        progress.stage("Importing", 100, Unit::Bytes);
        assert_eq!(report.lock().unwrap().current.as_ref().unwrap().total, 100);
        progress.update(40, || "a.log".into());
        let current = report.lock().unwrap().current.clone().unwrap();
        assert_eq!((current.label.as_str(), current.done), ("Importing", 40));
        assert!(
            current
                .line
                .starts_with("Importing: 40% (40 B of 100 B), a.log")
        );
        // Updates in quick succession are not all shared.
        progress.update(50, || "b.log".into());
        assert_eq!(report.lock().unwrap().current.as_ref().unwrap().done, 40);
        progress.done("2 files");

        let report = report.lock().unwrap();
        assert_eq!(report.current, None);
        assert_eq!(report.notes, ["Found 2 log files"]);
        assert_eq!(report.done.len(), 1);
        assert_eq!(
            (
                report.done[0].label.as_str(),
                report.done[0].summary.as_str()
            ),
            ("Importing", "2 files")
        );
    }

    #[test]
    fn stays_quiet_when_asked() {
        let mut progress = Progress::quiet();
        progress.stage("Importing", 10, Unit::Bytes);
        progress.update(5, || panic!("a quiet progress never builds its line"));
        progress.done("nothing to say");
    }
}
