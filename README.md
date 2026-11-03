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
irssi's `/who`, are sorted into trusted, untrusted and undecided. The social
graph of who talks to whom comes next.

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

The database records its schema version. When a newer version of `wot` adds
tables, it adds them to an existing database the next time it opens it. A
database made by a newer version of `wot` than the one running is refused;
delete it and run again to rebuild it from the logs.

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
channel time, spoken lines, and first and last seen. Channel time adds up the
time in each channel, so time spent in two channels at once counts twice.
Private messages add no time.

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
`ident` and a `host` of `#cut:` and the cut's id.

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

## Reviewing in a browser

`wot review` serves a web interface for reviewing the imported data, then
keeps running until you press Ctrl+C. Open the address it prints, by default
<http://127.0.0.1:7171/>.

```bash
wot review
wot review --port 8080 --db /path/to/other.sqlite
```

It only listens on 127.0.0.1, so it can only be opened from the same
computer, and it has no login. It reads the database and never the log files,
so import new lines with `wot analyze` first; reloading a page then shows
them. Importing stays on the command line. The pages are built on the server
as plain HTML with no JavaScript, and follow the system's light or dark mode.

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
  session in the channel, apart from the logging client; Spoke counts those
  that said anything; Silent is the share that never did. Private messages are
  summarized in one line.
- **Channels** (`/channels`): every channel with activity, the busiest over
  the last six months first, with its recent spoken lines and identities,
  the last month anyone was present, its review status and when a user list
  was last pasted. See [Trust](#trust).
- **Channel** (`/channel?tag=TAG&name=CHANNEL`): the channel's review status
  with a button to mark it reviewed, a box to paste `/who` output into, and
  the users of the latest list in trusted, untrusted, undecided and bots,
  see [Checking a channel's users](#checking-a-channels-users). Below that,
  spoken lines and nicks for each day with spoken lines, most recent first,
  with a bar comparing each day with the busiest one. The log files read for
  the channel are listed at the end.
- **Identities** (`/identities`): a search by nick, ident, host and person,
  matched the same way as `wot analyze --nick`, `--ident`, `--host` and
  `--person`. Results show the identity, its person, nicks, number of
  `ident@host` combinations, sessions, channel time, spoken lines, and first
  and last seen, most recently seen first, up to 500. Each result has a
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
- **Identity** (`/identity?id=ID`): one identity in full: its person, nicks,
  connections with their host group, channels and private messages, nick
  changes and inferred sessions. Nicks, idents, hosts and channels link to
  their own searches and pages. Its Activity section shows its activity score,
  and real names from pasted user lists are listed under its nicks. From here
  an identity can be added to an existing person or bot, to a new one, or
  removed from its person or bot, and a session can be cut at any of its nick
  changes. Connections on hosts that are not in a group link to a search by
  that host, where it can be grouped. An id merged into another identity leads
  to that identity.
- **People and bots** (`/people`): people, most trusted first, with their
  rating, trust score, activity score, identities, nicks, last seen and
  latest note; then bots, most active first; and a form to create either,
  optionally with identity ids.
- **Person or bot** (`/person?name=NAME`): its trust score and what it was
  made from, a slider to rate a person that explains each rating as it is
  chosen, who invited them and whom they invited, and notes to add and
  delete. Then its identities with their totals, with checkboxes and an
  **All** link to remove several at once, a button to make a person a bot or
  a bot a person, and forms to add identities by id or delete it.
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
  whether to include bots, which are labelled "bot". Over all channels, each
  row shows the identity's network tag. Channel pages link to their own
  ranking. The identity page shows the identity's activity by year, with each
  month behind a fold, and notes private messages when it never spoke in a
  channel.

### Code

The code is in `src/review/`. `mod.rs` sets up the server, the routes and
what the pages share: errors, database access, notices and redirects.
`layout.rs` has the page frame and helpers, and `style.css` the styles. Each
area has its own module with its pages and actions: `pages.rs` for the
overview and channels, then `identities.rs`, `people.rs`, `hosts.rs`,
`cuts.rs`, `activity.rs` and `channels.rs`. The data comes from
`src/stats.rs`, `src/lookup.rs`, `src/activity.rs` and `src/trust.rs`, and
the actions call the same functions in `src/decide.rs` as the commands.

## Activity

Activity is tracked for each identity, never merged into people, so the
numbers stay right however identities are linked later. It only counts
channels: private messages only show conversations with the logging client,
so they are left out, and so are the logging client's own sessions.

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
`heard` and `spoken`.

Upgrading a database to this version empties `sessions` so that the next
`wot analyze` rebuilds them with heard lines and activity. Run it once before
`wot review`.

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

The line is given as `tag/target.log:LINE`, or as the full path of the log
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
score low. The person and identity pages show what the score was made from.

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

- **Trusted:** an exact or new nick match to a person rated vouched or known.
- **Untrusted:** never seen, or matched to someone rated wary or distrusted,
  including by nick only.
- **Undecided:** everyone else: rated neutral, not linked to a person, or
  matched by nick only.
- **Bots:** matched to a bot.

Each list shows the nick and status, `ident@host`, real name, person or bot
and rating, trust and activity scores, the identity, and how it was matched.
Unknown users link to a search by their host. Each pasted list is kept, so
the real names it holds show on the identity pages; a list can be deleted
from the channel page.

### Channel reviews

**Mark reviewed** on a channel's page records that you went through its users.
A review stays valid for six months, after which the Channels page shows it
as expired.
