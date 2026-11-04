# Logs and importing

## Log layout

`wot analyze` takes one log folder per irssi instance, laid out the way
irssi's default `autolog_path` writes logs:

```ini
autolog_path = ~/irclogs/$tag/$0.log
```

That gives a folder per network tag, with a `.log` file per channel or
private message:

```text
irclogs/
  wetfish/
    #analysis.log
    rachel.log
```

Files starting with `#`, `&`, `!` or `+` are channels; other `.log` files are
private messages. Anything else is listed as skipped in the report rather
than read.

## Log format

`wot` expects irssi's default theme and log settings:

```ini
log_timestamp = %H:%M
log_open_string = --- Log opened %a %b %d %H:%M:%S %Y
log_close_string = --- Log closed %a %b %d %H:%M:%S %Y
log_day_changed = --- Day changed %a %b %d %Y
```

Lines only carry the time; the date comes from the last `Log opened`,
`Log closed` or `Day changed` line above them. Lines `wot` does not recognize
are counted as unparsed, so new formats show up instead of being ignored.

## Importing

The first `wot analyze` imports every line into the database. Later runs, and
**Analyze now** in `wot review`, only read what changed:

- A file that grew is read from where the last import stopped.
- A file that shrank, or whose start changed, was replaced, and is imported
  again from the start.
- A last line without a newline, which irssi may still be writing, waits for
  the next run.

Each file is imported in its own transaction, so an interrupted run carries on
where it stopped next time. When anything changed, sessions and identities are
rebuilt; `--rebuild` rebuilds even when nothing did, for example after
upgrading `wot`. Each step shows its progress and how long it took:

```text
Importing log files: checked 1,234 files (2m 03s)
Reading events: 12,345,678 events in 1,234 files, 234,567 sessions (1m 10s)
Linking sessions into identities: 3,456 identities, 12 of them new (2.1s)
```

## Reports

`wot analyze` prints a report after importing:

- **By default**, an inventory: for each network tag, every channel with its
  log folders, lines, spoken lines, joins, first and last day and unparsed
  lines, then any files that could not be read and examples of unparsed
  lines.
- **`--channel '#name'`**: the channel's spoken lines and active nicks for
  each day. Quote the name, since the shell treats `#` as a comment.
- **`--nick`, `--ident`, `--host` or `--person`**: the identities behind them,
  see [Identities](identities.md#looking-up-identities).

## Ignoring network tags

Some folders hold networks the project does not need, such as a large public
network that only slows things down. `wot tags ignore TAG` deletes everything
imported from it and leaves it out of later imports; `wot tags unignore TAG`
brings it back on the next import. `wot compact` then shrinks the database
file, which needs free disk space about its size.

## Networks

Each folder irssi logs a server into is a network tag. A server can end up
under several: its hostname changed, or a bouncer and a client named it
differently. Link them into a network, with `wot networks create` or on the
Networks page, and they become one: identities are linked across them, and
channels of the same name become one channel. Logs are still named by their
folder, as in `wetfish-bouncer/#wetfish.log`.

## Lines logged twice

The same channel is often logged twice at once, by a client and a bouncer, or
by two irssi instances. Every copy is imported, and each rebuild marks the
second copy of each line, so everything counted from the logs counts it once.
Two lines are the same when they are both messages or both actions, by the
same nick, with the same text, at most a minute apart. The log files
themselves are never changed.

## Bouncers

Mark the tags logged through a bouncer such as ZNC, with
`wot tags bouncer TAG` or **Mark as bouncer** on the Networks page. In their
logs:

- Lines between ZNC's `Buffer Playback...` and `Playback Complete.` markers
  are **replayed**: said earlier, while the client was away. ZNC puts each
  line's original time in front of its text, which is what `wot` compares
  them by. A replayed line only counts for someone still there.
- The bouncer's own lines, from nicks starting with `*`, are left out.
- Of a line logged twice, the bouncer's copy is the one left out.

## Checking lines

`wot lines TAG` shows the imported lines of a tag or network, with their file
and line number, to check what `wot` made of them:

```bash
wot lines wetfish --unparsed
wot lines wetfish --replayed --channel '#wetfish'
wot lines wetfish --duplicates
wot lines wetfish --contains 'Playback Complete'
```

`--duplicates` shows each pair, the copy kept and then the copy left out, to
check them by eye. It shows 50 lines unless `--limit` says otherwise.
