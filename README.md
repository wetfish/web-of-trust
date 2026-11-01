# web-of-trust

`wot` is a set of command-line tools for IRC community maintainers. Its first
component reads irssi chat logs and reports how active users are, which channels
they are active in, and who they talk to, to help spot accounts that join
channels to collect data rather than to take part.

## Status

Early development. `wot analyze` currently finds the log files in one or more
irssi log folders, parses every line, and prints an inventory grouped by network
tag and channel. Activity statistics, identity tracking and the social graph
come next.

## Requirements

- Rust 1.85 or newer (edition 2024)

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

The report is plain text on standard output, so it can be saved or paged:

```bash
wot analyze ~/irclogs > inventory.txt
wot analyze ~/irclogs | less
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
