mod discover;
mod irssi;
mod report;
mod scan;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Web of trust tools for IRC communities.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read irssi log folders and report on them
    Analyze {
        /// Log folders to read, one per irssi instance, each laid out as
        /// <folder>/<network tag>/<channel or nick>.log
        #[arg(required = true, value_name = "LOG_ROOT")]
        roots: Vec<PathBuf>,
    },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Analyze { roots } => analyze(&roots),
    }
}

fn analyze(roots: &[PathBuf]) -> ExitCode {
    let inventory = match discover::discover(roots) {
        Ok(inventory) => inventory,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };

    let scan = scan::scan(&inventory);

    let mut out = io::BufWriter::new(io::stdout().lock());
    match report::inventory(&mut out, &inventory, &scan).and_then(|()| out.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        // The reader went away, for example `| head`. Not an error.
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: writing report: {err}");
            ExitCode::FAILURE
        }
    }
}
