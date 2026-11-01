mod db;
mod discover;
mod identity;
mod irssi;
mod report;
mod scan;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// The default database, in the repository folder. `.gitignore` excludes it.
const DEFAULT_DB: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/wot.sqlite");

/// Web of trust tools for IRC communities.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Import irssi log folders into the database and report on them
    Analyze {
        /// Log folders to read, one per irssi instance, each laid out as
        /// <folder>/<network tag>/<channel or nick>.log
        #[arg(required = true, value_name = "LOG_ROOT")]
        roots: Vec<PathBuf>,

        /// Show day by day activity for this channel instead of the
        /// inventory. Quote it in the shell: --channel '#wetfish'
        #[arg(long)]
        channel: Option<String>,

        /// SQLite database to import into. It is created if it does not exist.
        #[arg(long, value_name = "FILE", default_value = DEFAULT_DB)]
        db: PathBuf,
    },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Analyze { roots, channel, db } => analyze(&roots, channel.as_deref(), &db),
    }
}

fn analyze(roots: &[PathBuf], channel: Option<&str>, db_path: &Path) -> ExitCode {
    let inventory = match discover::discover(roots) {
        Ok(inventory) => inventory,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };

    let mut conn = match db::open(db_path) {
        Ok(conn) => conn,
        Err(err) => {
            eprintln!("error: opening database {}: {err}", db_path.display());
            return ExitCode::FAILURE;
        }
    };
    let summary = db::import(&mut conn, &inventory.files);
    // Summaries go to standard error so the report stays clean.
    let _ = report::import_summary(&mut io::stderr().lock(), db_path, &summary);

    let changed = summary.new_files + summary.grown_files + summary.replaced_files > 0;
    let rebuild = match identity::is_built(&conn) {
        Ok(built) => changed || !built,
        Err(err) => {
            eprintln!("error: reading identities: {err}");
            return ExitCode::FAILURE;
        }
    };
    if rebuild {
        eprintln!("Building sessions and identities...");
        match identity::rebuild(&mut conn) {
            Ok(rebuilt) => {
                let _ = report::identity_summary(&mut io::stderr().lock(), &rebuilt);
            }
            Err(err) => {
                eprintln!("error: building identities: {err}");
                return ExitCode::FAILURE;
            }
        }
    }
    eprintln!();

    let scan = scan::scan(&inventory, channel);

    let mut out = io::BufWriter::new(io::stdout().lock());
    let written = match (channel, &scan.activity) {
        (Some(channel), Some(activity)) => {
            if activity.files.is_empty() {
                eprintln!("error: no log files found for {channel}");
                return ExitCode::FAILURE;
            }
            report::daily_activity(&mut out, &inventory, channel, activity)
        }
        _ => report::inventory(&mut out, &inventory, &scan),
    };

    match written.and_then(|()| out.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        // The reader went away, for example `| head`. Not an error.
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: writing report: {err}");
            ExitCode::FAILURE
        }
    }
}
