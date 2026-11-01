# web-of-trust

`wot` is a set of command-line tools for IRC community maintainers. Its first
component reads irssi chat logs and reports how active users are, which channels
they are active in, and who they talk to, to help spot accounts that join
channels to collect data rather than to take part.

## Status

Early development. `wot analyze` currently finds the log files in one or more
irssi log folders, imports every line into a SQLite database, and prints an
inventory grouped by network tag and channel, or the day by day activity of one
channel. The reports still read the log files directly; moving them onto the
database comes next, followed by identity tracking and the social graph.

## Requirements

- Rust 1.89 or newer (edition 2024)
- A C compiler. SQLite is compiled into `wot` through rusqlite's `bundled`
  feature, so no system SQLite library is needed.

## Building

```bash
cargo build --release
```

The binary is written to `target/release/wot`. To install it on your `PATH`:

```bash
cargo install --path .
```

## Usage

Pass one log folder per irssi instance:

```bash
wot analyze ~/irclogs
wot analyze /archive/laptop/irclogs /archive/server/irclogs
```

The report is plain text on standard output, so it can be saved or paged. A
short summary of the database import goes to standard error, so it stays out
of saved reports:

```bash
wot analyze ~/irclogs > inventory.txt
wot analyze ~/irclogs | less
```

To see one channel's activity day by day instead of the inventory, name it
with `--channel`. Quote the name, because the shell treats an unquoted `#` at
the start of a word as a comment:

```bash
wot analyze ~/irclogs --channel '#wetfish'
```

Each run imports the logs into the database first, see [Database](#database).
To use a different database file:

```bash
wot analyze ~/irclogs --db /path/to/other.sqlite
```

Run `wot --help` or `wot analyze --help` for the full list of options.

## Expected log layout

Each folder passed on the command line should be laid out the way irssi's
default `autolog_path` writes logs:

```ini
autolog_path = ~/irclogs/$tag/$0.log
```

That gives one folder per network tag, with one `.log` file per channel or
private message target:

```text
irclogs/
  wetfish/
    #analysis.log
    rachel.log
```

Files whose names start with `#`, `&`, `!` or `+` are treated as channels, and
all other `.log` files as private messages. Anything that does not fit this
layout (files directly in the log folder, nested folders, files without a `.log`
extension, unreadable files) is listed under "Skipped" at the end of the report
rather than read.

## Expected log format

The parser expects irssi's default theme and these log settings, which are
also irssi's defaults:

```ini
log_timestamp = %H:%M
log_open_string = --- Log opened %a %b %d %H:%M:%S %Y
log_close_string = --- Log closed %a %b %d %H:%M:%S %Y
log_day_changed = --- Day changed %a %b %d %Y
```

It recognizes messages, actions, notices, server notices (`!server text`),
joins, parts, quits, kicks, nick changes, topic changes, mode changes,
netsplits and irssi's own `Irssi:` lines.
Any other line is counted as unparsed, so new formats show up in the report
instead of being silently ignored.

Timestamped lines only carry the time. The date comes from the most recent
`Log opened`, `Log closed` or `Day changed` line above them. irssi only writes
a `Day changed` line when the day of the month differs from the last line it
wrote, so a log that stays silent until the same day of a later month keeps the
old date until the next `Log opened` or `Log closed` line.

## Inventory report

For each network tag, the report lists every channel with:

- **In roots:** the numbered log folders at the top of the report that contain
  the channel, which shows where several irssi instances logged it
- **Lines:** all lines in the channel's log files
- **Spoken:** messages and actions
- **Joins:** join lines
- **First day** and **Last day:** the first and last dates with a timestamped
  line
- **Unparsed:** lines the parser did not recognize

Private message logs are summarized in one line per network tag. After the
totals, the report lists:

- **Read problems:** files that could not be read to the end, and files with
  timestamped lines before the first line giving a date
- **Unparsed lines:** up to 50 examples, at most 3 per file, as
  `[root] tag/target.log:line: text`. A line that repeats an earlier example,
  apart from its timestamp, is not shown again.
- **Skipped:** paths that do not fit the expected layout

## Daily activity report

`wot analyze --channel <name>` reads every log file whose name matches the
channel, ignoring case, in any network tag and log folder. It lists each day
with at least one spoken line:

- **Spoken:** messages and actions that day
- **Active users:** distinct nicks that spoke that day, ignoring case. A person
  who changes nick counts once for each nick, until identity tracking exists.

Days without spoken lines are left out. Below the table are the number of days,
the date range, the total spoken lines and the distinct nicks over the whole
range. Spoken lines before the first line giving a date are counted separately.

When several files match, for example the same channel logged by two irssi
instances, their counts are added together, so a day logged in both is counted
twice. The report lists the files it read so this is easy to spot.

## Database

`wot analyze` imports every log file into a SQLite database before it reports.
By default this is `wot.sqlite` in the repository folder, which `.gitignore`
excludes. `--db FILE` uses another file. The database is created when it does
not exist. You can open it with any SQLite client, such as `sqlite3 wot.sqlite`.

### Importing

The first run imports everything. Later runs only read what changed:

- A file seen before that has grown is read from where the last import
  stopped. The date in effect at that point is remembered, so lines after a
  `Day changed` line keep the right date across runs.
- A file that shrank, or whose first 1024 bytes changed, was replaced. Its old
  rows are deleted and it is imported again from the start.
- Only complete lines are imported. A last line without a newline, which irssi
  may still be writing, waits for the next run.

Each file is imported in its own transaction, so an interrupted run leaves the
database consistent and the next run carries on. A summary of the import is
printed to standard error.

The database records its schema version. If a newer version of `wot` changes
the schema, it refuses to use the old database; delete it and run again to
rebuild it from the logs.

### Tables

`files` has one row per log file:

| Column | Contents |
| --- | --- |
| `path` | Absolute path of the log file |
| `tag`, `target` | Network tag folder and channel or nick, from the path |
| `kind` | `channel` or `query` |
| `bytes_read`, `lines_read` | How far the file has been imported |
| `date` | The date in effect after the last imported line |
| `head` | The file's first 1024 bytes, to notice a replaced file |
| `imported_at` | When the file was last imported, in UTC |

`events` has one row per line, apart from blank lines and `Day changed` lines.
Every row has `file_id`, `line` (the line number), `date` (`YYYY-MM-DD`, empty
before the first line giving a date), `time` and `kind`. The other columns
depend on the kind:

| Kind | `nick` | `ident`, `host` | `other_nick` | `text` | `detail` |
| --- | --- | --- | --- | --- | --- |
| `message` | speaker | | | message | channel status, such as `@` |
| `action` | speaker | | | action | |
| `notice` | sender | | | notice | |
| `server_notice` | | | | notice | server |
| `join` | nick | yes | | | channel |
| `part` | nick | yes | | reason | channel |
| `quit` | nick | yes | | reason | |
| `kick` | kicked nick | | kicked by | reason | channel |
| `nick_change` | old nick | | new nick | | |
| `own_nick_change` | | | new nick | | |
| `topic` | nick | | | topic | channel |
| `topic_unset` | nick | | | | channel |
| `mode`, `server_mode` | set by | | | modes | channel |
| `netsplit` | | | | nicks | servers |
| `netsplit_over` | | | | nicks | |
| `client` | | | | irssi's message | |
| `log_opened`, `log_closed` | | | | | |
| `unparsed` | | | | the whole line | |

`time` is `HH:MM` for events, `HH:MM:SS` for `log_opened` and `log_closed`, and
empty for `unparsed` lines.
