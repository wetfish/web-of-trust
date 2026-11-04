# Commands

Run `wot --help`, or `wot <command> --help`, for every option. Every command
takes `--db FILE` to use a database other than `wot.sqlite` in the
repository folder.

## Import and report

```bash
wot analyze ~/irclogs                          # import, then list every channel
wot analyze /laptop/irclogs /server/irclogs    # several irssi instances
wot analyze ~/irclogs --channel '#wetfish'     # one channel, day by day
wot analyze ~/irclogs --nick 'rachel*'         # identities behind a nick
wot analyze ~/irclogs --host bridge.example.net
wot analyze ~/irclogs --person rachel
wot analyze ~/irclogs --rebuild                # rebuild even if nothing changed
```

The report goes to standard output and progress to standard error, so
`wot analyze ~/irclogs > report.txt` saves just the report. See
[Logs and importing](logs.md).

## Review in a browser

```bash
wot review                 # http://127.0.0.1:7171/
wot review --port 8080
```

See [Reviewing in a browser](review.md).

## People and bots

```bash
wot people create rachel 1a2b3c4d5e6f 9f8e7d6c5b4a --note "the logs' owner"
wot people create relay 0a1b2c3d4e5f --bot
wot people add rachel 0a1b2c3d4e5f
wot people remove 0a1b2c3d4e5f
wot people kind relay person
wot people rate quokka vouched
wot people note quokka "met at a meetup in 2019"
wot people delete-note quokka 3
wot people invited-by quokka rachel
wot people delete rachel
wot people list
```

See [People and trust](trust.md).

## Host groups and cuts

```bash
wot hosts add-group matrix --kind bridge --note "Matrix bridge"
wot hosts add matrix bridge.example.net old-bridge.example.net
wot hosts edit-group matrix --ident-is-user false
wot hosts remove old-bridge.example.net
wot hosts remove-group matrix
wot hosts list

wot cuts add 'wetfish/#dryfish.log:2050' --note "a friend on my computer"
wot cuts remove 1
wot cuts list
```

With logs from several network tags, `wot hosts` needs `--tag`. See
[Identities](identities.md).

## Networks and tags

```bash
wot networks create example irc.example.net old.example.net
wot networks add example bouncer.example.net
wot networks remove old.example.net
wot networks delete example
wot networks list

wot tags list
wot tags bouncer wetfish-bouncer
wot tags no-bouncer wetfish-bouncer
wot tags ignore rizon --note "public network"
wot tags unignore rizon
wot compact                # shrink the database after ignoring a tag
```

See [Logs and importing](logs.md).

## Checking lines

```bash
wot lines wetfish --unparsed
wot lines wetfish --replayed --channel '#wetfish'
wot lines wetfish --duplicates --limit 200
wot lines wetfish --contains 'Playback Complete'
```

See [Checking lines](logs.md#checking-lines).
