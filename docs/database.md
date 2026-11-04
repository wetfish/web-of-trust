# Database

`wot` keeps everything in one SQLite file, `wot.sqlite` in the repository
folder by default, which `.gitignore` leaves out. Open it with any SQLite
client, such as `sqlite3 wot.sqlite`.

## Two kinds of tables

- **Rebuilt from the logs:** events, sessions, identities, activity and the
  totals. They are rebuilt whenever new lines are imported, and can always be
  rebuilt again.
- **Your decisions:** people, ratings, notes, host groups, networks, cuts,
  ignored tags, channel reviews and pasted user lists. Rebuilds never change
  them, so they last as long as the file. Deleting `wot.sqlite` deletes them
  too, so back it up.

## Upgrades

The file records its schema version. A newer `wot` upgrades an older file the
next time it opens it, which can take a few minutes for a large one, and
refuses a file from a newer `wot` than itself. Some upgrades clear the totals
so that the next `wot analyze`, or **Analyze now**, rebuilds with new rules;
until then, channel pages may show no activity.

## Write-ahead log

The file uses SQLite's write-ahead log, so pages keep showing the data from
before an analysis while it writes. While `wot` runs, `wot.sqlite-wal` and
`wot.sqlite-shm` sit next to it: back up all three, or only `wot.sqlite` once
`wot` has stopped. A write waits up to 5 seconds for another one to finish.

## Tables from the logs

| Table | One row per | Main columns |
| --- | --- | --- |
| `files` | log file | `path`, `folder_tag` (its folder), `tag` (its network), `target`, `kind` (`channel` or `query`), how far it was read |
| `events` | log line | `file_id`, `line`, `date`, `time`, `kind`, `nick`, `ident`, `host`, `other_nick`, `text`, `detail` |
| `sessions` | session | `file_id`, `identity_id`, `link`, `nicks`, `ident`, `host`, `start_at`, `end_at`, `minutes`, `spoken`, `heard`, `logger` |
| `session_nicks` | nick a session held | `session_id`, `nick`, `from_line`, `to_line`: the lines it held the nick over |
| `identities` | identity | `id`, `tag`, `created_at`, `merged_into` |
| `identity_keys` | nick and `ident@host` seen | `tag`, `nick`, `ident`, `host`, `identity_id` |
| `activity` | identity, channel and month | `days_present`, `active_days`, `minutes`, `heard`, `spoken`; channel `*` is all channels together |
| `left_out_lines` | line not counted | `event_id`, `reason` (`duplicate` or `bouncer`), `kept_id` |
| `replayed_lines` | line a bouncer replayed | `event_id` |
| `file_totals`, `channel_totals`, `channel_days` | file, channel, channel day | totals for the overview and channel pages |

Dates are `YYYY-MM-DD` and times `HH:MM`, in the logs' own time zone. Which
event columns are filled depends on its `kind`:

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

## Your decisions

| Table | Holds |
| --- | --- |
| `people` | people and bots: `name`, `kind`, `rating`, `invited_by`, `note` |
| `person_identities` | which identities belong to which person |
| `person_notes` | dated notes on people |
| `host_groups`, `host_group_members` | host groups and their hosts |
| `session_cuts` | cuts, by file path and line |
| `networks`, `network_tags` | networks and the tags linked into them |
| `bouncer_tags`, `ignored_tags` | tags marked as bouncers, and tags left out |
| `channel_reviews` | when each channel was last reviewed |
| `who_lists`, `who_entries` | pasted `/who` lists, with real names |

## Keeping pages fast

A large import holds tens of millions of lines, so the pages never read them
all:

- The totals tables are filled during each rebuild, so the Overview and
  channel pages read a row per channel or day.
- The `events_spoken` index finds the lines a session said by file and nick,
  without reading every line it heard. It takes about 20 bytes a message.
- A page of a person's history reads their most recent sessions first and
  stops as soon as the page is full.
- `identity_keys` is indexed by identity, and by host and ident, for the
  pages that look these up for every row and for `/who` checks.
