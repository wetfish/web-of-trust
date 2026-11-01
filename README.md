# web-of-trust

`wot` is a set of command-line tools for IRC community maintainers. Its first
component reads irssi chat logs and reports how active users are, which channels
they are active in, and who they talk to, to help spot accounts that join
channels to collect data rather than to take part.

## Status

Early development. `wot analyze` currently finds the log files in one or more
irssi log folders and prints an inventory of them, grouped by network tag and
channel. Log parsing, activity statistics, identity tracking and the social
graph come next.

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

## Inventory report

For each network tag, the report lists every channel with the log folders it
appears in, its total size and its total line count. Private message logs are
summarized as a count. The "In roots" column refers to the numbered log folders
at the top of the report, which shows where several irssi instances logged the
same channel.
