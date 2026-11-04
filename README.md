# web-of-trust

`wot` is a set of command-line tools for IRC community maintainers. Its first
component reads irssi chat logs and reports how active users are, which channels
they are active in, and who they talk to, to help spot accounts that join
channels to collect data rather than to take part.

## Status

Early development. `wot analyze` currently finds the log files in one or more
irssi log folders, imports every line into a SQLite database, groups the lines
into sessions and identities, and prints an inventory grouped by network tag
and channel, the day by day activity of one channel, or the identities behind a
nick, ident, host or person. `wot hosts`, `wot people` and `wot cuts` record
decisions about who is who, people and bots alike. Every identity's
channel activity is tracked by channel and month: how long it was present, how
much was said while it was there, and how much it said itself. `wot review`
serves a web interface on this computer for reviewing all of it, ranking the
most and least active identities, and making the same decisions in a browser.
People get your own rating, notes and who invited them, which together with
their activity make a trust score, and a channel's current users, pasted from
irssi's `/who`, are sorted into trusted, inactive, untrusted and undecided.
Tags that are one server are linked into networks, and lines logged twice, by
a client and a bouncer, count once. The social graph of who talks to whom
comes next.

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

To see the identities behind a nick, ident, host or person, use `--nick`,
`--ident`, `--host` or `--person`, see [Identity views](#identity-views). `*`
matches any characters in the first three, so quote patterns that use it:

```bash
wot analyze ~/irclogs --nick rachel
wot analyze ~/irclogs --host 'Fish-*.chat'
wot analyze ~/irclogs --ident quokkawet --host bridge.example.net
wot analyze ~/irclogs --person rachel
```

Once the logs are imported, record who is who with `wot hosts`, `wot people`
and `wot cuts`, see [Decisions](#decisions). These work on the database
alone and need no log folders:

```bash
wot hosts add-group matrix --kind bridge
wot hosts add matrix bridge.example.net old-bridge.example.net
wot people create rachel 1a2b3c4d5e6f 9f8e7d6c5b4a
wot cuts add 'wetfish/#dryfish.log:2050' --note "rachel on a friend's computer"
```

Each run imports the logs into the database first, see [Database](#database).
To use a different database file:

```bash
wot analyze ~/irclogs --db /path/to/other.sqlite
```

Tags that are one network, such as a server's old and new hostnames or a
bouncer's and a client's logs, are linked into a network, see
[Networks](#networks), and tags logged through a bouncer are marked, see
[Bouncers](#bouncers). Lines both logs have count once, see
[Lines logged twice](#lines-logged-twice), and `wot lines` shows the lines of
a tag that need a closer look:

```bash
wot networks create example irc.example.net old.example.net
wot tags bouncer example-bouncer
wot lines example --replayed
wot lines example --unparsed --channel '#wetfish'
```

To leave a network tag out, such as a large public network the project does
not need, see [Ignoring network tags](#ignoring-network-tags):

```bash
wot tags list
wot tags ignore rizon --note "public network"
wot compact
```

To review the imported data in a web browser instead, see
[Reviewing in a browser](#reviewing-in-a-browser):

```bash
wot review
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

The database uses SQLite's write-ahead log, so `wot review` keeps showing pages
while an analysis writes for minutes, with what was there before until the
analysis is done. While it is open, `wot.sqlite-wal` and `wot.sqlite-shm` sit
next to it; copy all three to back it up while `wot` runs, or only
`wot.sqlite` once it has stopped. A write waits up to 5 seconds for another one
to finish before it fails.

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

Large imports take a while, so `wot analyze` says what it is doing on standard
error, stage by stage: finding the log files, importing them, reading events,
linking sessions into identities, saving sessions, building channel activity,
and reading the logs again for the report. In a terminal, one line updates in
place with the percentage done, the counts so far and the time taken; when
standard error is not a terminal, a line is printed every ten seconds instead.
Each stage ends with a line saying what it did and how long it took:

```text
Looking for log files in /home/rachel/irclogs
Found 1,234 log files, 4.1 GB in all
Importing log files: checked 1,234 files (2m 03s)
Building sessions and identities...
Reading events: 12,345,678 events in 1,234 files, 234,567 sessions (1m 10s)
Linking sessions into identities: 3,456 identities, 12 of them new (2.1s)
Saving sessions: 234,567 sessions (8.0s)
Building channel activity: from 120,000 channel sessions (5.0s)
Writing to the database: saved (1.2s)
```

Decisions that rebuild identities, from the command line or from `wot review`,
show the same stages; for `wot review`, in the terminal it runs in.

### Ignoring network tags

Some log folders hold networks the project does not need, such as large public
networks with millions of lines that only slow imports and reports down.
`wot tags list` lists every imported network tag with its files, channels,
lines and size, biggest first, and the tags already ignored.

`wot tags ignore TAG` deletes every file and line imported from the tag and
leaves its folders out of every later `wot analyze`, which says how many files
it left out. Identities are rebuilt straight away, since the deleted lines may
have linked sessions. Decisions about the tag, such as host groups, are kept.
Tags are matched ignoring case, and a tag can be ignored before it was ever
imported. An ignored tag leaves its network, if it was in one. `wot tags unignore TAG` imports it again on the next run, from the
start.

SQLite keeps the space deleted rows took, so the database file stays the same
size until `wot compact` rewrites it. That takes a while for a large database
and needs free disk space about the size of the database.

The database records its schema version. When a newer version of `wot` adds
tables, it adds them to an existing database the next time it opens it. A
database made by a newer version of `wot` than the one running is refused.
It holds your decisions and notes, so use a newer `wot` rather than deleting
it.

### Tables

`files` has one row per log file:

| Column | Contents |
| --- | --- |
| `path` | Absolute path of the log file |
| `folder_tag`, `target` | Network tag folder and channel or nick, from the path |
| `tag` | The network the file belongs to: its folder tag, or the [network](#networks) it was linked into |
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

`events` is indexed by file and line, by file and date, and, for messages and
actions only, by file, nick ignoring case, and line. The last one,
`events_spoken`, finds the lines a session said without reading every line it
heard, which is what makes the history and person pages quick for someone
present for years. It takes about 20 bytes a message; upgrading a database to
schema version 12 builds it, which takes a few minutes for gigabytes of logs.

`sessions`, `identities` and `identity_keys` are described under
[Identities](#identities).

## Identities

After importing, `wot analyze` groups the events into sessions and the
sessions into identities, one per person as far as the logs can tell. This is
rebuilt from the events whenever the import added or replaced anything, the
first time a database is used, and when `--rebuild` is given. Use `--rebuild`
after upgrading `wot`, since a new version may group sessions differently.
Progress and a summary go to standard error:

```text
Building sessions and identities...
Identities: 1,234 from 56,789 sessions (12 new, 3 merged)
Sessions without a host: 4,567 inferred from the same nick, 89 known by nick only
```

### Sessions

A session is one continuous stretch of a nick being present in one log file.
It records how long the person was present and how much they spoke, which is
what a trust score needs.

- It starts at a join line. A nick that speaks, parts, quits or is kicked
  without a join line since logging started was already present, so its
  session starts at the `Log opened` line.
- It ends at a part, quit or kick, at a `Log closed` line, when the log is
  opened again without having been closed (at the last line before that), when
  the nick joins again, when another nick changes into it, or at the end of the
  file.
- A nick change continues the same session under the new nick.
- Netsplits do not end sessions, since the same connections come back.
- The logging client's own session is recognized from two `Irssi:` lines that
  irssi writes after its own join:
  1. `#channel: Total of N nicks` on the very next line, within a minute, as
     soon as the server's names list arrives. irssi also writes this for
     `/names` at any time, so on its own it is not enough.
  2. `Join to #channel was synced in N secs`, once irssi has finished querying
     the channel. `/names` never writes this. The join must have been about N
     seconds before this line, within a minute.

  `You're now known as` lines rename the logger's session, and nothing else.
  irssi only writes the sync line while its `channel_sync` setting is on,
  which is the default. With it off, no session is marked as the logger's.

  The logging client is someone in the channel too, usually you, so its
  sessions count like anyone else's: in activity, scores, history and the
  overview. Only private messages tell it apart, see [Activity](#activity).

Only join, part and quit lines show `ident@host`, so a session learns its host
from its join line, or from its part or quit line when it had no join.

Sessions only cover time while irssi was logging. Someone already in a channel
when irssi joined, who never speaks, parts or quits before irssi stops logging,
does not appear in that stretch of the log at all.

### Linking sessions into identities

- **`host`:** sessions that showed `ident@host` are linked when they share a
  nick and `ident@host`. A nick change inside a session links its nicks too.
- **`inferred`:** a session that never showed a host is attributed to the
  identity of the nearest session in time that used the same nick with a host,
  preferring an earlier one. `inferred_from` records which session, for
  auditing. Private message sessions are always inferred, since queries never
  show hosts.
- **`nick_only`:** a nick that never appears with a host gets an identity of
  its own.
- **`cut`:** a session started by a [cut](#cuts). It keeps no host and is
  linked to nothing else.

[Host groups](#host-groups) change the `host` rule: hosts in one group count
as one host, and in a group whose ident identifies the person, sessions with
the same ident anywhere in the group are linked whatever their nick.

Nothing else links sessions. Otherwise the same nick from a different host,
and different nicks from the same `ident@host`, stay separate identities.
[People](#people) join identities without changing them.

Identity ids are 12 random hexadecimal characters. `identity_keys` records
which nick and `ident@host` combinations belong to which id, so ids stay the
same between rebuilds. When new evidence links two identities, the one created
first keeps its id, and the other records `merged_into`. An identity known only
by nick can later be absorbed when that nick shows a host; its old id is then
left without sessions.

Identities are separate for each network tag, since hosts on different servers
are not comparable.

### Identity views

`--nick`, `--ident` and `--host` print the identities matching them instead of
the inventory. Matching ignores case and `*` matches any characters. Given
together, they must all match the same nick and `ident@host` combination, so
`--ident quokkawet --host bridge.example.net` finds whoever used that ident on
that host. `--person` shows the identities of a person, and combines with the
others. None of them can be combined with `--channel`.

The report starts with a table of the matching identities, most recently seen
first: their person, nicks, how many `ident@host` combinations they used, sessions,
channel time, spoken lines, and first and last seen. Channel time is the time
present in any channel: time in two channels at once, or in one channel logged
twice, counts once, so it is never more than the time from the first session
to the last. Private messages add no time.

When 10 or fewer identities match, each one follows in full:

- **Person:** who the identity belongs to, if anyone. Two names mean
  identities added to different people were merged by a rebuild since, which
  needs sorting out.
- **Totals:** sessions by how they were linked, sessions as the logging
  client, and the date range, channel time and spoken lines.
- **Nicks:** each nick with its sessions and first and last seen.
- **Connections:** each `ident@host` with how its sessions were linked
  (`nick and host`), plus sessions without a host, as `inferred`,
  `nick only` or `cut`.
- **Channels and private messages:** sessions, time, spoken lines and last
  seen for each.
- **Nick changes:** the sessions that changed nick, which is what links
  different nicks, as `tag/target.log:start-end`, up to the 10 most recent.
- **Inferred sessions:** sessions attributed by nick alone, with the session
  they were inferred from, up to the 10 most recent.

Line numbers point into the log files, so `sed -n 'START,ENDp' FILE` shows
the lines behind any link.

### Identity tables

`identities`: `id`, `tag`, `created_at` (UTC) and `merged_into`.

`identity_keys`: `tag`, `nick` (lowercased), `ident` and `host` (both empty for
nick-only keys), and `identity_id`. Sessions started by a cut have an empty
`ident` and a `host` of `#cut:` and the cut's id. Besides its key, it is
indexed by identity, for the pages that list each identity's nicks, and by
tag, host ignoring case, and ident, for checking a channel's users and
counting the identities on a host.

`sessions`:

| Column | Contents |
| --- | --- |
| `file_id` | The log file |
| `identity_id` | The identity |
| `link` | `host`, `inferred`, `nick_only` or `cut` |
| `inferred_from` | For inferred sessions, the session they were attributed from |
| `nicks` | Nicks used, space separated, in order |
| `ident`, `host` | From the session's join, part or quit line, if any |
| `start_at`, `end_at` | `YYYY-MM-DD HH:MM` |
| `start_line` | The join line, or the line where the nick first appeared |
| `joined` | 1 when the session started with a join line |
| `end_line` | The line that ended the session, if one did |
| `end_reason` | `part`, `quit`, `kick`, `log_closed`, `log_reopened`, `rejoined`, `nick_taken`, `cut` or `end_of_log` |
| `minutes` | Time present |
| `spoken` | Messages and actions |
| `logger` | 1 for the logging client's own session |
| `heard` | Messages and actions in the channel while the session was present, its own included |

`session_nicks`: `session_id`, `nick` (lowercased), `from_line` and `to_line`
(empty when the session ran to the end of the file): the lines over which a
session held each of its nicks, for sessions that spoke, the logging
client's included. A message on these lines by that nick was the session's, so
a nick someone else took after a nick change is never mistaken for the
session's. The [History](#history) page finds lines this way.

`file_totals` (`file_id`, `spoken`, `joins`, `first_at`, `last_at`) and
`channel_totals` (`tag`, `channel`, `identities`, `speakers`, `duplicates`,
the second copies of lines logged twice, and `nicks`, the distinct nicks that
spoke, ignoring case) hold the overview's totals, rebuilt with the sessions,
so the overview reads a row per file and channel instead of adding up
millions of sessions. `channel_days` (`tag`, `channel`, `date`, `spoken`,
`nicks`) holds each channel's spoken lines and distinct nicks by day, counted
while the events are replayed, so a channel's page reads a row a day instead
of every line the channel ever had; lines left out are not counted. A
database upgraded since its last rebuild has no totals yet, and `wot analyze`
rebuilds on its next run. Upgrading to schema version 12 clears them, so run
`wot analyze` once before `wot review`.

## Reviewing in a browser

`wot review` serves a web interface for reviewing the imported data, then
keeps running until you press Ctrl+C. Open the address it prints, by default
<http://127.0.0.1:7171/>.

```bash
wot review
wot review --port 8080 --db /path/to/other.sqlite
```

It only listens on 127.0.0.1, so it can only be opened from the same
computer, and it has no login. The pages read the database; new lines are
read from the log files by **Analyze now**, on the Overview and the Analyze
page, or by `wot analyze`. The pages are built on the server as plain HTML
with no JavaScript, and follow the system's light or dark mode.

Decisions are made with ordinary forms. Each action changes the database the
same way as the matching `wot hosts`, `wot people` or `wot cuts` command, then
returns to the page with a notice or an error, so reloading never repeats it.
Changes to host groups and cuts rebuild identities, and the notice summarizes
the rebuild. Deleting a person or a host group needs a confirmation box
ticked.

### Pages

- **Overview** (`/`): for each network tag, the number of identities, people,
  bots, host groups and cuts, and a table of channels with their log files,
  lines, spoken lines and joins. Identities counts the identities with a
  session in the channel, the logging client included; Spoke counts those
  that said anything; Silent is the share that never did; Merged counts the
  lines logged twice that were counted once, see [Lines logged
  twice](#lines-logged-twice). Private messages are summarized in one line.
  **Analyze now** reads new lines, see the Analyze page below.
- **Analyze** (`/analyze`): the log folders the imported files came from,
  each the folder holding `<network tag>/<channel or nick>.log`, and
  **Analyze now**, which does what `wot analyze` does with them, with a box to
  rebuild even when no log file changed, as `--rebuild` does. New network tags
  inside the folders are found too, and a folder that is no longer there is
  skipped; a folder never imported needs `wot analyze FOLDER` once. The
  analysis runs in the background, one at a time, while the page reloads
  itself every 2 seconds with each stage that ended, what it did and how long
  it took, and the stage under way with a progress bar and the line the
  terminal shows. Then it says what the analysis did, or why it failed. Every
  page shows a banner while it runs; decisions made meanwhile wait up to 5
  seconds for it and then fail, so make them once it is done.
- **Channels** (`/channels`): every channel with activity, the busiest over
  the last six months first, with its recent spoken lines and identities,
  the last month anyone was present, its review status and when a user list
  was last pasted. See [Trust](#trust).
- **Channel** (`/channel?tag=TAG&name=CHANNEL`): the channel's review status
  with a button to mark it reviewed, a box to paste `/who` output into, and
  the users of the latest list in trusted, inactive, untrusted, undecided and
  bots, see [Checking a channel's users](#checking-a-channels-users). Below
  that, spoken lines and nicks for each day with spoken lines, most recent
  first, with a bar comparing each day with the busiest one. The log files
  read for the channel are listed at the end.
- **Identities** (`/identities`): a search by nick, ident, host and person,
  matched the same way as `wot analyze --nick`, `--ident`, `--host` and
  `--person`. Results show the identity, its person, nicks, number of
  `ident@host` combinations, sessions, channel time (overlapping sessions
  counted once), spoken lines, and first and last seen, most recently seen
  first, up to 500. Each result has a
  checkbox, and the **All** link at the top of the column loads the results
  again with every box ticked (**None** clears them): the ticked identities
  can be made into a new person or a new bot, added to an existing one, or
  taken out of their person or bot, all at once and without leaving the
  search. Identities that belong to a bot are labelled "bot". Adding an
  identity to the person or bot it already belongs to changes nothing, and
  taking out identities that belong to no one skips them. A search by host
  also lists the matching hosts, up to 50, each with its host group: an
  ungrouped host can be added to an existing group, a grouped one taken out,
  and a new group can be created for the ungrouped hosts. These actions come
  back to the same search, so the identities on a host can be checked before
  and after grouping it.
- **Identity** (`/identity?id=ID`): one identity in full, named after the nick
  it used most recently. It opens with who the identity is: its other nicks,
  real names from pasted user lists, the `ident@host` combinations it
  connected from most recently, the person or bot it belongs to with their
  rating, when it was seen with its activity score and any silent watcher or
  inactive flag, and its sessions. Links jump to each section below. Then, in
  order: its person or bot, which can be set here by adding it to an existing
  one, creating one, or removing it; its activity by year and month; its
  nicks; its connections with their host group; its channels and private
  messages; and, for fixing it, its nick changes, where a session can be cut,
  and its inferred sessions. Nicks, idents, hosts and channels link to their
  own searches and pages. Connections on hosts that are not in a group link to
  a search by that host, where it can be grouped. An id merged into another
  identity leads to that identity.
- **People and bots** (`/people`): people, most trusted first, with their
  rating, trust score, activity score, identities, nicks, last seen with any
  inactive or silent watcher flag, and latest note; then bots, most active
  first; and a form to create either, optionally with identity ids.
- **Person or bot** (`/person?name=NAME`): it opens with who they are across
  all their identities: their nicks and the `ident@host` combinations they
  connected from, most recent first, real names from pasted user lists, how
  many identities on which network tags, their trust score, rating and who
  invited them, when they were last seen and where, when they last spoke, how
  long ago each was, the days they were present from first to last with their
  time in channels, the days they spoke and their lines, their activity score
  with any silent watcher or inactive flag, and the channels they were in on
  the most days, each linked to the history of that channel. Links jump to
  each section below. Then, in order: the last 10 lines they said in channels,
  each day linked to that day in the history; their trust score and what it
  was made from, a slider to rate a person that explains each rating as it is
  chosen, who invited them and whom they invited; notes to add and delete;
  their identities with their totals, with checkboxes and an **All** link to
  remove several at once; and, under Manage, forms to add identities by id,
  make a person a bot or a bot a person, and delete it.
- **History** (`/history?id=ID` or `/history?person=NAME`): everything an
  identity, or all of a person's identities, said in channels, most recent
  first, 200 lines to a page, with filters for a channel, a day and text the
  line contains. Below, every day they were present, by month with the
  newest open, with the lines said and the channels; silent days included.
  Linked from identity and person pages. See [History](#history).
- **Networks** (`/networks`): every network tag folder with its network,
  files, channels, lines and size, with checkboxes: the ticked tags can be
  made into a new network, added to an existing one, taken out of their
  network, or marked as logged through a bouncer or not, and bouncers are
  labelled "bouncer". Then each network with its tags and a way to delete it,
  and the ignored tags. Every change rebuilds identities. See
  [Networks](#networks) and [Bouncers](#bouncers).
- **Host groups** (`/hosts`): each group with its hosts, how many identities
  used each, and forms to add and remove hosts, change the kind, ident
  setting and note, or delete the group; a form to create a group; and the
  shared hosts not in any group, used by the most identities first. Shared
  hosts with many identities and different idents are likely bridges or
  bouncers, and link to a search by that host, to review its identities and
  group it.
- **Cuts** (`/cuts`): every cut with its nick change and whether it still
  applies, with forms to remove one or add one by line.
- **Activity** (`/activity`): identities ranked by their channel activity, see
  [Activity](#activity). Filters choose a channel or all channels, a year or
  all time, the order (least active, most active or longest present first),
  the days present an identity needs to be ranked (7 unless chosen), and
  whether to include bots, which are labelled "bot". Identities present on 30
  days or more without a line get a red flag. Over all channels, each row
  shows the identity's network tag. Channel pages link to their own ranking.
  The identity page shows the identity's activity by year, with each month
  behind a fold, where a bar shows the month's days present in grey with the
  days it spoke filled in green, and notes private messages when it never
  spoke in a channel.

### Code

The code is in `src/review/`. `mod.rs` sets up the server, the routes and
what the pages share: errors, database access, notices and redirects.
`layout.rs` has the page frame and helpers, and `style.css` the styles. Each
area has its own module with its pages and actions: `pages.rs` for the
overview and channels, then `identities.rs`, `people.rs`, `hosts.rs`,
`cuts.rs`, `activity.rs`, `channels.rs` and `history.rs`. The data comes
from `src/stats.rs`, `src/lookup.rs`, `src/activity.rs`, `src/trust.rs` and
`src/history.rs`, and the actions call the same functions in `src/decide.rs`
as the commands.

## Activity

Activity is tracked for each identity, never merged into people, so the
numbers stay right however identities are linked later. It only counts
channels: private messages only show conversations with the logging client,
so they are left out. The logging client's own sessions in channels count
like anyone else's, since it is someone in the channel too, usually you.

Upgrading a database to schema version 11 clears the overview totals, so the
next `wot analyze` rebuilds activity, nick spans and totals with the logging
client counted. Until then, your own lines since you started logging with
the client you talk from are missing, and you may show as inactive.

For each identity, channel and month:

- **Days present:** days with a session in the channel, however short.
- **Active days:** days on which it said anything.
- **Time:** minutes present. Overlapping sessions of one identity count once.
- **Heard:** messages and actions in the channel while it was present, its
  own included.
- **Spoken:** its own messages and actions.
- **Share:** spoken as a share of heard. An identity present for a long time
  with a share near zero listens but does not talk; one that used to talk and
  went quiet shows a high share in earlier years.

Each identity also has rows for all channels together, where days and time
are not counted twice for being in two channels at once. Years and all time
are sums of months. Sessions are split over the days and months they cover,
so a session that lasted weeks through a bouncer counts on every day of it.

Activity is rebuilt with the identities. A session can only be seen while
irssi was logging: someone already in a channel when irssi joined, who never
speaks, parts or quits before irssi stops logging, leaves no trace.

The activity ranking leaves out identities that belong to a bot, see
[Bots](#bots), unless asked to include them. Next to an identity that never
spoke in a channel, the review pages show how many private messages it sent
the logging client, which tells a shy member from someone who never talks.

`activity` table: `identity_id`, `tag`, `channel` (`*` for all channels
together), `month` (`YYYY-MM`), `days_present`, `active_days`, `minutes`,
`heard` and `spoken`. An index on every column the ranking reads, in the order
it groups them, lets it rank millions of rows in about a second.

Upgrading a database to this version empties `sessions` so that the next
`wot analyze` rebuilds them with heard lines, activity and nick spans. Run it
once before `wot review`.

## History

The history of an identity, or of a person across all their identities, lists
what they said in channels and the days they were present. Private messages
are left out, as in activity; the logging client's own lines count.

Lines are found through each session's nick spans in `session_nicks`, kept
with the sessions: a line belongs to a session when its nick is the one the
session held on that line. Someone who takes a nick after its owner changed
away from it is never mistaken for the owner.

A page of lines reads the sessions that ended most recently first, each
newest line first, through the `events_spoken` index, and stops once no
older session can add a line to the page: no line is later than the end of
its session. So a page takes the same short time however much someone said,
while sorting everything a talkative person ever said would mean sorting
over a million lines. Searching for text reads every line until a page is
found, so a search for something rare takes longer.

A day counts as present when a session in a channel covered any part of it,
so a session through a bouncer counts on every day it lasted. A session
longer than a year counts only its first and last days, so one session left
open by a broken log cannot fill years.

## Lines logged twice

The same channel is often logged more than once at the same time: by a client
and by a bouncer, or by two irssi instances. Every copy is imported, and each
rebuild marks the second copies of lines, so everything counted from the logs
counts each line once: spoken lines, lines heard, activity, trust scores,
history and the channel pages' daily counts. Lines only one log has are kept,
so a channel's lines are everything any of its logs saw. The log files and the
imported lines are never changed.

Logs are of the same channel when they have the same channel name, ignoring
case, in the same network tag, from any log folder. Linking tags into a
[network](#networks) makes their channels the same channel too.

Two lines are the same when they have the same nick, ignoring case, are both
messages or both actions with the same text, and are at most a minute apart,
since two machines' clocks can differ. They pair off one to one, so someone
who said the same thing twice still said two lines. Only the days two logs
share are compared.

### Bouncers

Mark the network tags logged through a bouncer, by tag folder:

```bash
wot tags bouncer wetfish-bouncer
wot tags no-bouncer wetfish-bouncer
```

or with **Mark as bouncer** on the Networks page. Each change rebuilds
identities. In a bouncer's logs:

- **Replays** are read from ZNC's playback markers, and from nothing else.
  When a client attaches, ZNC replays what it buffered between two lines of
  its own, `< ***> Buffer Playback...` and `< ***> Playback Complete.`, and by
  default puts each line's original time in front of its text, as in
  `[14:02:11] hello`. irssi logs the replay when it arrives. A line between
  the markers is compared at the time in front of it, without it, so it
  matches the line as a direct client logged it; a replay that arrived after
  midnight of a line from before it is put on the day before. Anywhere else,
  including other tags, a time in square brackets is just part of the text,
  as when someone pastes from their own logs.
- **A replayed line only counts for someone still there** when it was
  replayed, since it says nothing about who was present then.
- **The bouncer's own lines**, from its nicks starting with `*` such as the
  playback markers, are left out of every count.
- **Its copy of a line logged twice is the one left out**, so the lines kept
  are the direct client's.

Joins and quits from both logs still build sessions; time present is merged
where an identity's sessions overlap, so it is not counted twice either.

`left_out_lines` holds the lines left out, rebuilt with the sessions:
`event_id`, `reason` (`duplicate` or `bouncer`) and, for a duplicate,
`kept_id`, the copy kept. `replayed_lines` holds the `event_id` of every line
replayed between a bouncer's markers, and `bouncer_tags` the tags marked as
bouncers. Upgrading a database to this version clears the overview totals, so
the next `wot analyze` rebuilds with these rules. The inventory report of
`wot analyze` reads the log files themselves, so it still counts every copy.

### Checking lines

`wot lines TAG` shows the imported lines of a network tag or network, with
their file and line number, to track down what the rules above or irssi's
format did not expect:

```bash
wot lines example --unparsed
wot lines example --replayed --channel '#wetfish'
wot lines example --duplicates --limit 200
wot lines example --contains 'Playback Complete'
```

Duplicates come out in pairs:

```text
kept      /logs/client/wetfish/#wetfish.log:1204  2026-11-01 22:20  <quokka> back now
left out  /logs/bouncer/wetfish-bouncer/#wetfish.log:877  2026-11-01 22:30  <quokka> [22:20:05] back now
```

- `--unparsed`: lines irssi's format did not match.
- `--replayed`: lines a bouncer replayed, between its playback markers, in
  tags marked as bouncers.
- `--duplicates`: lines left out as logged twice, each pair shown together:
  the copy kept, then the copy left out, both with their file and line, to
  check each match by eye.
- `--contains TEXT`: lines containing the text, ignoring case for ASCII
  letters. Combines with the others, and with `--channel`.

It shows 50 lines unless `--limit` says otherwise, and says when there are
more.

## Decisions

Identities are built from what the logs show. Decisions record what only a
person can know: which hosts are one bridge or bouncer, who is behind which
identities, and when someone else was at the keyboard. They live in their own
tables, which rebuilds and imports never change, so they last as long as the
database. Deleting `wot.sqlite` deletes them too.

Decisions refer to things the logs have shown, so import the logs first. Each
command takes `--db FILE` like `wot analyze`.

### Host groups

A host group says that several hosts are one bridge or bouncer, for example a
Matrix bridge that has moved between servers. Hosts are added exactly as they
appear in the logs, ignoring case, with no wildcards, and each host can be in
one group only.

```bash
wot hosts add-group matrix --kind bridge --note "Matrix bridge"
wot hosts add matrix bridge.example.net old-bridge.example.net
wot hosts add-group znc --kind bouncer --no-ident-user
wot hosts edit-group znc --ident-is-user true
wot hosts remove old-bridge.example.net
wot hosts remove-group znc
wot hosts list
```

The kind is `bridge` or `bouncer`. Bridges and bouncers usually give each
person their own ident, so by default the ident identifies the person on a
group's hosts: sessions with the same ident anywhere in the group are linked,
whatever their nick. `--no-ident-user`, or `edit-group --ident-is-user false`,
turns this off for a group whose idents are shared, such as a bouncer that
gives everyone the same ident. Its hosts still count as one host, so the same
nick and ident are linked across them.

`wot hosts add` refuses a host that has not been seen in the logs, to catch
typos. When logs from several network tags are imported, choose the tag with
`--tag`. Changes rebuild identities straight away.

### People

A person is a human behind one or more identities, named as you like. A
person can hold identities from any network tag, and each identity belongs to
at most one person. Identity ids come from the identity views of
`wot analyze`.

```bash
wot people create rachel 1a2b3c4d5e6f 9f8e7d6c5b4a --note "the logs' owner"
wot people add rachel 0a1b2c3d4e5f
wot people remove 0a1b2c3d4e5f
wot people delete rachel
wot people list
```

Adding an identity to the person it already belongs to changes nothing.
`wot people remove` with several ids skips those that belong to no one, and
only fails when none of them belong to a person. The same commands work for
bots.

Rate how well you know someone, note how you know them, and record who invited
or vouches for them, see [Trust](#trust):

```bash
wot people rate quokka vouched
wot people note quokka "met at a meetup in 2019"
wot people delete-note quokka 3
wot people invited-by quokka rachel
wot people invited-by quokka
```

Note numbers are shown by `wot people list`. `wot people invited-by` without
a second name clears it. Only people invite, and never in a circle.

People do not change identities, so nothing is rebuilt. When a rebuild merges
an identity into another, its person follows it. If that puts one identity in
two people, the identity views show both names as a conflict.

### Networks

Each folder irssi logs a server into is a network tag, named after the server
or its address. A server can end up under several: its hostname changed, a
bouncer and a client named it differently, or two irssi instances did.
Linking those tags into a network makes them one from then on:

```bash
wot networks create example irc.example.net old.example.net --note "renamed in 2024"
wot networks add example bouncer.example.net
wot networks remove old.example.net
wot networks delete example
wot networks list
```

Tags are given as `wot tags list` shows them, ignoring case, and each is in
at most one network. The network's name replaces the tags everywhere: the
overview, the channels, rankings and filters show it, identities are linked
across the tags, and channels of the same name become one channel, with
[lines logged twice](#lines-logged-twice) counted once. Host groups, channel
reviews and pasted user lists made under a tag move to the network. A tag that
has a host group or host of the same name as one already in the network
cannot join until one of them is renamed or removed.

Identities keep their ids: one person's identity under each tag becomes one
identity, under the older id, and a person or bot it belonged to follows it.
Taking a tag out of its network makes it separate again, and its identities
keep their ids where they can; decisions made while it was linked stay with
the network. Every change rebuilds identities.

Logs are still named by their folder: `wot cuts add` takes
`folder/target.log:LINE`, and identity pages name files that way.

`networks`: `id`, `name`, `note` and `created_at`. `network_tags`: `tag`,
`network_id` and `created_at`. Each file keeps the folder it came from in
`files.folder_tag`.

### Cuts

A cut marks a nick change made by someone else at the keyboard, such as
someone using a friend's computer. The session ends at the nick change, and
the new nick starts a session that keeps no host and is linked to nothing
else, so the two people stay apart. Add the session after the cut to a person
if you know who it was.

```bash
wot cuts add 'wetfish/#dryfish.log:2050' --note "rachel on a friend's computer"
wot cuts list
wot cuts remove 1
```

The line is given as `tag/target.log:LINE`, where `tag` is the network tag
folder the log is in even when it is linked into a network, or as the full
path of the log
file and `:LINE` when the same file name is in more than one log folder. Line
numbers are shown in the nick changes of the identity views. The line must be
a nick change. A cut stores the nick change it was made on; if the file is
replaced and that line no longer holds it, the cut is stale and not applied,
and `wot cuts list` says so. Changes rebuild identities straight away.

### Bots

A bot is a person record for a program instead of a human: its identities
are joined the same way, under one name, and activity rankings leave them out
unless asked to include bots. People and bots share names, so a bot cannot
take the name of a person.

```bash
wot people create relay 1a2b3c4d5e6f 9f8e7d6c5b4a --bot --note "announces builds"
wot people add relay 0a1b2c3d4e5f
wot people kind relay person
wot people kind relay bot
```

`wot people kind` turns a person into a bot or back, keeping its identities.
Bots are not rated and have no trust score.
`wot people list` lists people, then bots. In `wot review`, tick identities in
a search and choose **Create bot**, or add them to an existing bot.

### Decision tables

`host_groups`: `id`, `tag`, `name`, `kind` (`bridge` or `bouncer`),
`ident_is_user` (1 when the ident identifies the person), `note` and
`created_at`.

`host_group_members`: `group_id`, `tag`, `host`, `note` and `created_at`.

`people`: `id`, `name`, `note`, `created_at`, `kind` (`person` or `bot`),
`rating` (`distrusted`, `wary`, `neutral`, `vouched` or `known`) and
`invited_by` (the id of the person who invited them).

`person_notes`: `id`, `person_id`, `text` and `created_at`.

`channel_reviews`: `id`, `tag`, `channel` and `reviewed_at`.

`person_identities`: `identity_id`, `person_id`, `note` and `created_at`.

`session_cuts`: `id`, `path` (the log file), `line`, `expected` (the nick
change on that line), `note` and `created_at`.

`ignored_tags`: `tag`, `note` and `created_at`, see
[Ignoring network tags](#ignoring-network-tags).

`who_lists`: `id`, `tag`, `channel` and `pasted_at`.

`who_entries`: `list_id`, `nick`, `ident`, `host`, `status` (`H` here or `G`
away, `*` for an IRC operator, then channel modes and server flags) and
`realname`.

## Trust

Trust puts what you know about people together with what the logs show.
Ratings, notes and invitations are decisions like the others, kept in their
own tables. Pasted user lists are kept too, since they hold real names the
logs never show.

### Ratings

Each person has a rating, set on their page with a slider or with
`wot people rate`. Everyone starts neutral. Bots are not rated.

| Rating | Meaning |
|---|---|
| Known | You know them personally, or have for years. |
| Vouched | Someone you trust invited them or vouches for them. |
| Neutral | No opinion yet. Everyone starts here. |
| Wary | Something about them seems off. |
| Distrusted | You know they are not who they claim to be, or should not be here. |

Notes are dated and kept in order, for how you know someone, who introduced
them or anything worth remembering. Invited by links a person to the person
who invited or vouches for them, so chains of invitations can be followed
from either end.

### Scores

The **activity score**, 0 to 100, comes from the channel activity of all of
a person's identities, or of one identity not linked to anyone:

- **Consistency**, up to 40 points: months with spoken lines, all 40 at 24
  months.
- **Tenure**, up to 20 points: months from first to last seen in a channel,
  all 20 at 36 months.
- **Participation**, up to 40 points: days with spoken lines, on a log scale
  so the first weeks count the most, all 40 at 365 days.

Long-time regulars score high; accounts that joined recently or never talk
score low.

Someone with no spoken line in a channel in the 6 months before the newest
log line has their score **cut to a fifth**, so a regular who stopped talking
a year ago no longer ranks with those who still talk. The months are counted
back from the month of the newest line in the channel logs, not from today, so
a pause in importing does not change anyone's score, and in whole months, so
the last line can be up to 7 months old before the cut. Someone first seen
less than 6 months before that is too new to judge and keeps their score.
How they stopped decides what they are:

- **Inactive:** they spoke in at least 3 months before stopping, so they
  probably used to take part. They get an amber "Inactive since" flag with
  the month of their last line.
- **Silent watcher:** they spoke in fewer than 3 months, and have been present
  on at least 30 days since the month of their last line. A hello years ago
  does not outweigh the silence since, so they get the same red flag as
  someone who never spoke, as "Silent since" that month (see
  [Checking a channel's users](#checking-a-channels-users)).

Someone who spoke in fewer than 3 months and has barely been present since
gets the cut but no flag. The person and identity pages show what the score
was made from, and why it was cut.

The **trust score**, 0 to 99, is the rating's band of 20 points, from
distrusted at 0 to 19 up to known at 80 to 99, plus the activity score scaled
to 0 to 19. The bands never overlap, so the rating always comes first: a known
friend who rarely talks ranks above the busiest stranger, and among people
rated the same, activity decides. Bots have no trust score.

### Checking a channel's users

On a channel's page, run `/who #channel` in irssi and paste what it prints.
irssi shows each user as `#channel nick status hops ident@host [real name]`,
after any timestamp; other lines, such as `End of /WHO list`, are ignored,
and so are replies for other channels. irssi cuts the channel name to 10
characters, which is allowed for.

Each user is matched against the identity keys of the channel's network tag:

1. **Exact:** the same nick from an `ident@host` the identity used, counting
   the hosts of its host group as one.
2. **New nick:** an `ident@host` the identity used, with a nick it never used
   there. In a host group whose ident does not identify the person, the
   ident only counts together with the nick.
3. **Nick only:** the nick was seen, but never from this `ident@host`.
   Anyone can take a nick, so this gives no trust.
4. **Never seen.**

When several identities match, the most recently seen is used. Its person or
bot then decides the list:

- **Trusted:** an exact or new nick match to a person rated vouched or known,
  inactive or not.
- **Inactive:** an exact or new nick match to someone not rated vouched or
  known who is inactive (see [Scores](#scores)): probably legitimate, but
  not confirmed until you rate them, which moves them to another list. Rated
  wary or distrusted, they stay untrusted.
- **Untrusted:** a silent watcher (see below), never seen, or matched to
  someone rated wary or distrusted, including by nick only.
- **Undecided:** everyone else: rated neutral, not linked to a person, or
  matched by nick only, inactive or not, since anyone can take a nick.
- **Bots:** matched to a bot.

A **silent watcher** has been present in the channel on at least 30 days
without ever saying a line there, counting all of a person's identities, or
spoke in fewer than 3 months and has been present on 30 days or more since,
without a line in 6 months (see [Scores](#scores)). That is how an account
collecting logs behaves, so they get a red flag and are listed as untrusted,
the longest silent first, whatever else they matched as.
Rating them vouched or known clears the flag, for someone you know who only
reads; bots are never flagged. The same flag shows on the activity ranking and
on identity pages, for an identity present on 30 days or more without a line
in any channel.

Each list shows the nick and status, `ident@host`, real name, person or bot
and rating, trust and activity scores, the identity, any silent watcher or
inactive flag, and how it was matched.
Unknown users link to a search by their host. Each pasted list is kept, so
the real names it holds show on the identity pages; a list can be deleted
from the channel page.

### Channel reviews

**Mark reviewed** on a channel's page records that you went through its users.
A review stays valid for six months, after which the Channels page shows it
as expired.
