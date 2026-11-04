mod activity;
mod db;
mod decide;
mod discover;
mod duplicates;
mod history;
mod identity;
mod irssi;
mod lines;
mod lookup;
mod lurker;
mod progress;
mod report;
mod review;
mod scan;
mod stats;
mod trust;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use rusqlite::Connection;

use crate::lookup::Filter;
use crate::progress::Progress;
use crate::stats::{Kind, Rating};

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
        #[arg(long, conflicts_with_all = ["nick", "ident", "host", "person"])]
        channel: Option<String>,

        /// Show the identities that used this nick instead of the inventory.
        /// `*` matches any characters; quote it in the shell: --nick 'rachel*'
        #[arg(long)]
        nick: Option<String>,

        /// Show the identities that connected with this ident. Combines with
        /// --nick, --host and --person, and `*` matches any characters
        #[arg(long)]
        ident: Option<String>,

        /// Show the identities that connected from this host. Combines with
        /// --nick, --ident and --person, and `*` matches any characters
        #[arg(long)]
        host: Option<String>,

        /// Show the identities that belong to this person
        #[arg(long)]
        person: Option<String>,

        /// SQLite database to import into. It is created if it does not exist.
        #[arg(long, value_name = "FILE", default_value = DEFAULT_DB)]
        db: PathBuf,

        /// Rebuild sessions and identities even when no log file changed,
        /// for example after upgrading wot to a version with new rules
        #[arg(long)]
        rebuild: bool,
    },

    /// Group hosts that are one bridge or bouncer
    Hosts {
        #[command(subcommand)]
        action: HostsAction,

        /// Network tag the hosts belong to. Needed only when logs from more
        /// than one tag are imported
        #[arg(long, global = true)]
        tag: Option<String>,

        /// SQLite database to use
        #[arg(long, value_name = "FILE", default_value = DEFAULT_DB, global = true)]
        db: PathBuf,
    },

    /// Say which identities belong to which person or bot
    People {
        #[command(subcommand)]
        action: PeopleAction,

        /// SQLite database to use
        #[arg(long, value_name = "FILE", default_value = DEFAULT_DB, global = true)]
        db: PathBuf,
    },

    /// Mark nick changes made by someone else at the keyboard
    Cuts {
        #[command(subcommand)]
        action: CutsAction,

        /// SQLite database to use
        #[arg(long, value_name = "FILE", default_value = DEFAULT_DB, global = true)]
        db: PathBuf,
    },

    /// Link network tags that are one network, such as a server's old and new
    /// hostnames, or a bouncer's and a client's
    Networks {
        #[command(subcommand)]
        action: NetworksAction,

        /// SQLite database to use
        #[arg(long, value_name = "FILE", default_value = DEFAULT_DB, global = true)]
        db: PathBuf,
    },

    /// Show imported lines of a network tag: unparsed ones, ones a bouncer
    /// replayed, or ones left out as logged twice
    Lines {
        /// A network tag or network, as wot tags list shows them
        tag: String,

        /// Only lines of this channel or private message log
        #[arg(long)]
        channel: Option<String>,

        /// Only lines irssi's format did not match
        #[arg(long, group = "show")]
        unparsed: bool,

        /// Only lines a bouncer replayed, between its playback markers, in
        /// tags marked with wot tags bouncer
        #[arg(long, group = "show")]
        replayed: bool,

        /// Only lines left out as logged twice, each with the copy kept
        #[arg(long, group = "show")]
        duplicates: bool,

        /// Only lines containing this text, ignoring case for ASCII letters
        #[arg(long)]
        contains: Option<String>,

        /// Show at most this many lines
        #[arg(long, default_value_t = 50)]
        limit: usize,

        /// SQLite database to read
        #[arg(long, value_name = "FILE", default_value = DEFAULT_DB)]
        db: PathBuf,
    },

    /// List network tags, or leave one out of imports and delete its lines
    Tags {
        #[command(subcommand)]
        action: TagsAction,

        /// SQLite database to use
        #[arg(long, value_name = "FILE", default_value = DEFAULT_DB, global = true)]
        db: PathBuf,
    },

    /// Shrink the database file after deleting lines, by rewriting it
    Compact {
        /// SQLite database to compact
        #[arg(long, value_name = "FILE", default_value = DEFAULT_DB)]
        db: PathBuf,
    },

    /// Review the imported data in a web browser, on this computer only
    Review {
        /// Port to serve on, at http://127.0.0.1:PORT/
        #[arg(long, default_value_t = 7171)]
        port: u16,

        /// SQLite database to review
        #[arg(long, value_name = "FILE", default_value = DEFAULT_DB)]
        db: PathBuf,
    },
}

#[derive(Subcommand)]
enum HostsAction {
    /// Create a host group
    AddGroup {
        name: String,

        #[arg(long, value_enum)]
        kind: GroupKind,

        /// Do not treat the ident as the person on these hosts, for a
        /// bouncer that gives everyone the same ident
        #[arg(long)]
        no_ident_user: bool,

        #[arg(long)]
        note: Option<String>,
    },

    /// Change a host group's kind, ident setting or note
    EditGroup {
        name: String,

        #[arg(long, value_enum)]
        kind: Option<GroupKind>,

        /// Whether the ident identifies the person on these hosts
        #[arg(long, value_name = "true|false")]
        ident_is_user: Option<bool>,

        #[arg(long)]
        note: Option<String>,
    },

    /// Add hosts to a group, exactly as they appear in the logs
    Add {
        group: String,

        #[arg(required = true)]
        hosts: Vec<String>,

        #[arg(long)]
        note: Option<String>,
    },

    /// Take hosts out of their group
    Remove {
        #[arg(required = true)]
        hosts: Vec<String>,
    },

    /// Delete a host group and take all its hosts out of it
    RemoveGroup { name: String },

    /// List host groups and their hosts
    List,
}

#[derive(Clone, Copy, ValueEnum)]
enum GroupKind {
    Bridge,
    Bouncer,
}

impl GroupKind {
    fn as_str(self) -> &'static str {
        match self {
            GroupKind::Bridge => "bridge",
            GroupKind::Bouncer => "bouncer",
        }
    }
}

#[derive(Subcommand)]
enum PeopleAction {
    /// Create a person, or a bot with --bot, optionally with identities
    Create {
        name: String,

        /// Identity ids, as shown by wot analyze --nick, --ident or --host
        ids: Vec<String>,

        /// Create a bot, which activity rankings leave out
        #[arg(long)]
        bot: bool,

        #[arg(long)]
        note: Option<String>,
    },

    /// Make a person a bot, or a bot a person, keeping its identities
    Kind { name: String, kind: RecordKind },

    /// Rate how well you know a person
    Rate { name: String, rating: RatingArg },

    /// Add a note to a person or bot, such as how you know them
    Note { name: String, text: String },

    /// Delete a note, by the number wot people list shows
    DeleteNote { name: String, id: i64 },

    /// Record who invited or vouches for a person; leave BY out to clear it
    InvitedBy { name: String, by: Option<String> },

    /// Add identities to a person or bot
    Add {
        name: String,

        #[arg(required = true)]
        ids: Vec<String>,
    },

    /// Take identities out of whichever person or bot they belong to
    Remove {
        #[arg(required = true)]
        ids: Vec<String>,
    },

    /// Delete a person or bot; their identities are kept and belong to no one
    Delete { name: String },

    /// List people and bots with their identities
    List,
}

#[derive(Clone, Copy, ValueEnum)]
enum RecordKind {
    Person,
    Bot,
}

impl RecordKind {
    fn kind(self) -> Kind {
        match self {
            RecordKind::Person => Kind::Person,
            RecordKind::Bot => Kind::Bot,
        }
    }
}

/// How well you know a person, from the least trusted to the most.
#[derive(Clone, Copy, ValueEnum)]
enum RatingArg {
    Distrusted,
    Wary,
    Neutral,
    Vouched,
    Known,
}

impl RatingArg {
    fn rating(self) -> Rating {
        match self {
            RatingArg::Distrusted => Rating::Distrusted,
            RatingArg::Wary => Rating::Wary,
            RatingArg::Neutral => Rating::Neutral,
            RatingArg::Vouched => Rating::Vouched,
            RatingArg::Known => Rating::Known,
        }
    }
}

#[derive(Subcommand)]
enum NetworksAction {
    /// Create a network from network tags, as wot tags list shows them
    Create {
        name: String,

        #[arg(required = true)]
        tags: Vec<String>,

        #[arg(long)]
        note: Option<String>,
    },

    /// Link more network tags into a network
    Add {
        name: String,

        #[arg(required = true)]
        tags: Vec<String>,
    },

    /// Take network tags out of their network
    Remove {
        #[arg(required = true)]
        tags: Vec<String>,
    },

    /// Delete a network, taking its tags out of it first
    Delete { name: String },

    /// List networks and their tags
    List,
}

#[derive(Subcommand)]
enum TagsAction {
    /// List every network tag with its size, and the ignored ones
    List,

    /// Leave a network tag out of imports and delete everything imported
    /// from it, such as a large public network the project does not need
    Ignore {
        tag: String,

        #[arg(long)]
        note: Option<String>,
    },

    /// Import an ignored network tag again on the next wot analyze
    Unignore { tag: String },

    /// Mark a network tag as logged through a bouncer, such as ZNC: its
    /// playback is read for replayed lines, and its copies of lines logged
    /// twice are the ones left out
    Bouncer { tag: String },

    /// Take the bouncer mark off a network tag
    NoBouncer { tag: String },
}

#[derive(Subcommand)]
enum CutsAction {
    /// Cut a session at a nick change line, given as tag/target.log:LINE or
    /// the full path of the log file and :LINE. Quote it in the shell
    Add {
        at: String,

        #[arg(long)]
        note: Option<String>,
    },

    /// List cuts, and whether each still applies
    List,

    /// Delete a cut by its id from wot cuts list
    Remove { id: i64 },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Analyze {
            roots,
            channel,
            nick,
            ident,
            host,
            person,
            db,
            rebuild,
        } => {
            let filter = Filter {
                nick,
                ident,
                host,
                person,
            };
            analyze(&roots, channel.as_deref(), &filter, &db, rebuild)
        }
        Command::Hosts { action, tag, db } => finish(hosts(action, tag.as_deref(), &db)),
        Command::People { action, db } => finish(people(action, &db)),
        Command::Cuts { action, db } => finish(cuts(action, &db)),
        Command::Networks { action, db } => finish(networks(action, &db)),
        Command::Lines {
            tag,
            channel,
            unparsed,
            replayed,
            duplicates,
            contains,
            limit,
            db,
        } => {
            let show = if unparsed {
                lines::Show::Unparsed
            } else if replayed {
                lines::Show::Replayed
            } else if duplicates {
                lines::Show::Duplicates
            } else {
                lines::Show::All
            };
            let filter = lines::Filter {
                tag,
                channel,
                show,
                contains,
                limit,
            };
            finish(show_lines(&filter, &db))
        }
        Command::Tags { action, db } => finish(tags(action, &db)),
        Command::Compact { db } => finish(compact(&db)),
        Command::Review { port, db } => finish(review::run(&db, port)),
    }
}

/// Prints a command's error, if it failed, and gives its exit code.
fn finish(result: db::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn open(db_path: &Path) -> db::Result<Connection> {
    db::open(db_path).map_err(|err| format!("opening database {}: {err}", db_path.display()).into())
}

fn networks(action: NetworksAction, db_path: &Path) -> db::Result<()> {
    let mut conn = open(db_path)?;
    match action {
        NetworksAction::Create { name, tags, note } => {
            decide::create_network(&mut conn, &name, &tags, note.as_deref())?;
            println!("Created network {name} from {}.", tags.join(", "));
        }
        NetworksAction::Add { name, tags } => {
            decide::add_to_network(&mut conn, &name, &tags)?;
            println!("Linked {} into {name}.", tags.join(", "));
        }
        NetworksAction::Remove { tags } => {
            decide::remove_from_network(&mut conn, &tags)?;
            println!("Took {} out of their network.", tags.join(", "));
        }
        NetworksAction::Delete { name } => {
            decide::delete_network(&mut conn, &name)?;
            println!("Deleted network {name}; its tags are separate again.");
        }
        NetworksAction::List => {
            let mut out = io::stdout().lock();
            decide::list_networks(&conn, &mut out)?;
            return Ok(());
        }
    }
    rebuild_identities(&mut conn)
}

fn show_lines(filter: &lines::Filter, db_path: &Path) -> db::Result<()> {
    let conn = open(db_path)?;
    let (found, more) = lines::find(&conn, filter)?;
    let mut out = io::BufWriter::new(io::stdout().lock());
    match lines::write(&mut out, &found, more, filter).and_then(|()| out.flush()) {
        Err(err) if err.kind() != io::ErrorKind::BrokenPipe => Err(err.into()),
        _ => Ok(()),
    }
}

fn tags(action: TagsAction, db_path: &Path) -> db::Result<()> {
    let mut conn = open(db_path)?;
    match action {
        TagsAction::List => {
            let mut out = io::stdout().lock();
            decide::list_tags(&conn, &mut out)?;
        }
        TagsAction::Ignore { tag, note } => {
            let mut progress = Progress::to_stderr();
            progress.stage(
                &format!("Deleting what was imported from {tag}"),
                0,
                progress::Unit::Bytes,
            );
            let deleted = decide::ignore_tag(&mut conn, &tag, note.as_deref())?;
            if deleted.files == 0 {
                progress.done("nothing was imported from it yet");
            } else {
                progress.done(&format!(
                    "{} files, {} lines",
                    report::thousands(deleted.files as u64),
                    report::thousands(deleted.lines as u64)
                ));
            }
            println!("{tag} is ignored: wot analyze leaves its files out from now on.");
            if deleted.files > 0 {
                rebuild_identities(&mut conn)?;
                println!(
                    "The database file keeps its size until it is compacted: run wot compact."
                );
            }
        }
        TagsAction::Unignore { tag } => {
            decide::unignore_tag(&conn, &tag)?;
            println!("{tag} is imported again by the next wot analyze.");
        }
        TagsAction::Bouncer { tag } => {
            decide::mark_bouncer(&conn, &tag)?;
            println!("{tag} is marked as logged through a bouncer.");
            rebuild_identities(&mut conn)?;
        }
        TagsAction::NoBouncer { tag } => {
            decide::unmark_bouncer(&conn, &tag)?;
            println!("{tag} is no longer marked as a bouncer.");
            rebuild_identities(&mut conn)?;
        }
    }
    Ok(())
}

/// Rewrites the database without the space deleted rows left behind.
fn compact(db_path: &Path) -> db::Result<()> {
    let conn = open(db_path)?;
    let size = || {
        std::fs::metadata(db_path)
            .map(|meta| meta.len())
            .unwrap_or(0)
    };
    let before = size();
    let mut progress = Progress::to_stderr();
    progress.stage("Compacting the database", 0, progress::Unit::Bytes);
    conn.execute_batch("VACUUM")?;
    progress.done(&format!(
        "{} down to {}",
        progress::bytes(before),
        progress::bytes(size())
    ));
    Ok(())
}

/// Rebuilds identities after a decision that changes them, with a summary on
/// standard error.
fn rebuild_identities(conn: &mut Connection) -> db::Result<()> {
    eprintln!("Building sessions and identities...");
    let summary = identity::rebuild_with_progress(conn, &mut Progress::to_stderr())?;
    report::identity_summary(&mut io::stderr().lock(), &summary)?;
    Ok(())
}

fn hosts(action: HostsAction, tag: Option<&str>, db_path: &Path) -> db::Result<()> {
    let mut conn = open(db_path)?;
    if let HostsAction::List = action {
        let mut out = io::stdout().lock();
        return decide::list_hosts(&conn, &mut out);
    }
    let tag = decide::resolve_tag(&conn, tag)?;
    match action {
        HostsAction::AddGroup {
            name,
            kind,
            no_ident_user,
            note,
        } => {
            decide::add_group(
                &conn,
                &tag,
                &name,
                kind.as_str(),
                !no_ident_user,
                note.as_deref(),
            )?;
            println!(
                "Created {} group {name} in {tag}. Add its hosts with: wot hosts add {name} HOST...",
                kind.as_str()
            );
            // An empty group changes nothing, so there is nothing to rebuild.
            return Ok(());
        }
        HostsAction::EditGroup {
            name,
            kind,
            ident_is_user,
            note,
        } => {
            decide::edit_group(
                &conn,
                &tag,
                &name,
                kind.map(GroupKind::as_str),
                ident_is_user,
                note.as_deref(),
            )?;
            println!("Updated host group {name}.");
        }
        HostsAction::Add { group, hosts, note } => {
            decide::add_hosts(&mut conn, &tag, &group, &hosts, note.as_deref())?;
            println!("Added {} to host group {group}.", hosts.join(", "));
        }
        HostsAction::Remove { hosts } => {
            decide::remove_hosts(&mut conn, &tag, &hosts)?;
            println!("Took {} out of their host group.", hosts.join(", "));
        }
        HostsAction::RemoveGroup { name } => {
            decide::remove_group(&conn, &tag, &name)?;
            println!("Deleted host group {name}.");
        }
        HostsAction::List => unreachable!("handled above"),
    }
    rebuild_identities(&mut conn)
}

fn people(action: PeopleAction, db_path: &Path) -> db::Result<()> {
    let mut conn = open(db_path)?;
    match action {
        PeopleAction::Create {
            name,
            ids,
            bot,
            note,
        } => {
            let kind = if bot { Kind::Bot } else { Kind::Person };
            decide::create_person(&mut conn, &name, kind, &ids, note.as_deref())?;
            println!(
                "Created the {} {name} with {} identities.",
                kind.as_str(),
                ids.len()
            );
        }
        PeopleAction::Kind { name, kind } => {
            decide::set_kind(&conn, &name, kind.kind())?;
            println!("{name} is a {} now.", kind.kind().as_str());
        }
        PeopleAction::Rate { name, rating } => {
            let rating = rating.rating();
            decide::set_rating(&conn, &name, rating)?;
            println!("Rated {name} {}: {}", rating.label(), rating.explanation());
        }
        PeopleAction::Note { name, text } => {
            decide::add_note(&conn, &name, &text)?;
            println!("Added a note to {name}.");
        }
        PeopleAction::DeleteNote { name, id } => {
            decide::delete_note(&conn, &name, id)?;
            println!("Deleted note {id} of {name}.");
        }
        PeopleAction::InvitedBy { name, by } => {
            decide::set_invited_by(&conn, &name, by.as_deref())?;
            match by {
                Some(by) => println!("{name} was invited by {by}."),
                None => println!("Cleared who invited {name}."),
            }
        }
        PeopleAction::Add { name, ids } => {
            decide::add_to_person(&mut conn, &name, &ids)?;
            println!("Added {} to {name}.", ids.join(", "));
        }
        PeopleAction::Remove { ids } => {
            decide::remove_from_person(&mut conn, &ids)?;
            println!("Took {} out of their person or bot.", ids.join(", "));
        }
        PeopleAction::Delete { name } => {
            decide::delete_person(&conn, &name)?;
            println!("Deleted {name}. Their identities now belong to no one.");
        }
        PeopleAction::List => {
            let mut out = io::stdout().lock();
            decide::list_people(&conn, &mut out)?;
        }
    }
    Ok(())
}

fn cuts(action: CutsAction, db_path: &Path) -> db::Result<()> {
    let mut conn = open(db_path)?;
    match action {
        CutsAction::Add { at, note } => {
            let change = decide::add_cut(&conn, &at, note.as_deref())?;
            println!("Cut {at}: {change}");
        }
        CutsAction::Remove { id } => {
            decide::remove_cut(&conn, id)?;
            println!("Deleted cut {id}.");
        }
        CutsAction::List => {
            let mut out = io::stdout().lock();
            return decide::list_cuts(&conn, &mut out);
        }
    }
    rebuild_identities(&mut conn)
}

fn analyze(
    roots: &[PathBuf],
    channel: Option<&str>,
    filter: &Filter,
    db_path: &Path,
    force: bool,
) -> ExitCode {
    let mut progress = Progress::to_stderr();
    let folders: Vec<String> = roots
        .iter()
        .map(|root| root.display().to_string())
        .collect();
    progress.note(&format!("Looking for log files in {}", folders.join(", ")));
    let mut inventory = match discover::discover(roots) {
        Ok(inventory) => inventory,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };
    progress.note(&format!(
        "Found {} log files, {} in all",
        report::thousands(inventory.files.len() as u64),
        progress::bytes(inventory.files.iter().map(|file| file.bytes).sum())
    ));

    let mut conn = match open(db_path) {
        Ok(conn) => conn,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };
    match decide::leave_out_ignored(&conn, &mut inventory.files) {
        Ok(left_out) => {
            for (tag, files) in left_out {
                progress.note(&format!(
                    "Leaving out {} files of the ignored tag {tag}",
                    report::thousands(files as u64)
                ));
            }
        }
        Err(err) => {
            eprintln!("error: reading ignored tags: {err}");
            return ExitCode::FAILURE;
        }
    }
    let summary = db::import_with_progress(&mut conn, &inventory.files, &mut progress);
    // Summaries go to standard error so the report stays clean.
    let _ = report::import_summary(&mut io::stderr().lock(), db_path, &summary);

    let changed = summary.new_files + summary.grown_files + summary.replaced_files > 0;
    let rebuild = match identity::is_built(&conn) {
        Ok(built) => force || changed || !built,
        Err(err) => {
            eprintln!("error: reading identities: {err}");
            return ExitCode::FAILURE;
        }
    };
    if rebuild {
        if let Err(err) = rebuild_identities(&mut conn) {
            eprintln!("error: building identities: {err}");
            return ExitCode::FAILURE;
        }
    }
    eprintln!();

    let mut out = io::BufWriter::new(io::stdout().lock());
    let written = if !filter.is_empty() {
        let views = match lookup::find(&conn, filter) {
            Ok(views) => views,
            Err(err) => {
                eprintln!("error: looking up identities: {err}");
                return ExitCode::FAILURE;
            }
        };
        if views.is_empty() {
            eprintln!("error: no identities match {}", filter.describe());
            return ExitCode::FAILURE;
        }
        report::identities(&mut out, filter, &views)
    } else {
        let scan = scan::scan_with_progress(&inventory, channel, &mut progress);
        match (channel, &scan.activity) {
            (Some(channel), Some(activity)) => {
                if activity.files.is_empty() {
                    eprintln!("error: no log files found for {channel}");
                    return ExitCode::FAILURE;
                }
                report::daily_activity(&mut out, &inventory, channel, activity)
            }
            _ => report::inventory(&mut out, &inventory, &scan),
        }
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
