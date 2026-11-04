# web-of-trust

`wot` helps IRC community maintainers tell who takes part in their channels
and who only watches. It reads irssi chat logs and shows how active each user
is, which channels they are in, and how well you know them, to help spot
accounts that join channels to collect data rather than to take part.

Everything runs on your own computer. The logs go into a local SQLite
database, and `wot review` serves a web interface for reviewing it that only
your computer can open.

## Status

Early development, working as a proof of concept. It imports logs, groups
them into identities, tracks each identity's activity, records what you know
about people, and checks a channel's current users against all of it. Next is
the social graph of who talks to whom.

## Install

You need Rust 1.89 or newer and a C compiler. SQLite is built in.

```bash
cargo install --path .
```

This puts `wot` on your `PATH`. `cargo build --release` builds it to
`target/release/wot` instead.

## Quick start

Import your irssi log folders, one per irssi instance:

```bash
wot analyze ~/irclogs
```

The first run imports everything and can take a while for gigabytes of logs;
it shows each step as it goes. It then prints a summary of every channel.

Then review the data in your browser:

```bash
wot review
```

Open <http://127.0.0.1:7171/>. **Analyze now** on the Overview reads new
lines later, so the command line is only needed for the first import.

From there:

1. **Link the identities that are one person** into people on the
   Identities page, and rate how well you know them.
2. **Check a channel**: paste what irssi's `/who #channel` prints on the
   channel's page, and its users are sorted into trusted, inactive,
   untrusted and undecided.
3. **Look into the red flags**: silent watchers, present for weeks without
   saying a line.

## How it works

- **Sessions:** each stretch of a nick being in a channel, from join to part
  or quit, with how long it lasted and what it said.
- **Identities:** sessions linked by the same nick and `ident@host`, one per
  person as far as the logs can tell.
- **People:** identities you say are one human, with your rating, notes and
  who invited them. Bots are recorded the same way.
- **Activity:** for each identity, how many days it was present in each
  channel, how much it heard and how much it said.
- **Trust:** your rating together with an activity score, which flags
  accounts that watch without taking part.

Rebuilding never changes your decisions: people, ratings, notes, host groups,
networks and cuts are kept apart from what is rebuilt from the logs.

## Documentation

- [Logs and importing](docs/logs.md): the log layout `wot` expects, importing,
  networks, bouncers and lines logged twice.
- [Identities](docs/identities.md): how sessions and identities are built, and
  how to correct them with host groups and cuts.
- [People and trust](docs/trust.md): people and bots, ratings, the activity and
  trust scores, flags, and checking a channel's users.
- [Reviewing in a browser](docs/review.md): the pages of `wot review`.
- [Commands](docs/commands.md): every command, with examples.
- [Database](docs/database.md): the tables, for querying them or working on
  `wot`.

## License

MIT, see [LICENSE](LICENSE).
