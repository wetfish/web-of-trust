# Identities

The logs only show nicks, and anyone can take a nick. `wot` groups lines into
sessions and sessions into identities, one per person as far as the logs can
tell, and you correct it where it can't.

## Sessions

A session is one stretch of a nick being in one channel:

- It starts at a join. A nick that speaks or leaves without a join was already
  there when logging started.
- It ends at a part, quit or kick, when the log closes, when the nick joins
  again, or at the end of the file.
- A nick change continues the same session under the new nick.
- Netsplits do not end sessions.

Only join, part and quit lines show `ident@host`, so a session learns its
host from those.

The **logging client**, irssi itself, is recognized by the lines irssi writes
after its own join. It is someone in the channel too, usually you, and counts
like anyone else.

## Linking sessions into identities

Sessions are linked into one identity when they:

- **share a nick and `ident@host`**, or are the same session across a nick
  change, or
- **never showed a host** and used the same nick as an identity seen nearby
  in time. Such sessions are marked **inferred**. Private messages never show
  hosts, so they are always inferred.

A nick that never appears with a host gets an identity of its own. Nothing
else links identities: the same nick from a different host stays separate,
and so do different nicks from one `ident@host`. Joining them is your call,
by adding them to a [person](trust.md#people).

Identity ids are 12 hexadecimal characters, such as `1a2b3c4d5e6f`, and stay
the same between rebuilds. When new lines show that two identities are one,
the older id is kept and the other leads to it. Identities are separate for
each network, since hosts on different servers are not comparable.

## Host groups

Some hosts are one service: a Matrix bridge that moved between servers, or a
bouncer. A host group makes its hosts count as one:

```bash
wot hosts add-group matrix --kind bridge
wot hosts add matrix bridge.example.net old-bridge.example.net
```

Bridges and bouncers usually give each person their own ident, so by default
the ident identifies the person within a group: sessions with the same ident
are linked whatever their nick. Use `--no-ident-user` for a group that gives
everyone the same ident.

Hosts are added exactly as the logs show them, never with wildcards, so every
link is a decision you made. The Host groups page lists shared hosts that are
not in a group yet.

## Cuts

A cut marks a nick change made by someone else at the keyboard, such as a
friend using your computer. The session ends there, and the new nick starts a
session linked to nothing, so the two people stay apart:

```bash
wot cuts add 'wetfish/#dryfish.log:2050' --note "a friend on my computer"
```

The line is given as `folder/channel.log:LINE`, as the identity pages show
it, and must be a nick change. `wot` never assumes someone else was at the
keyboard; only a cut says so.

## Looking up identities

The Identities page searches by nick, ident, host and person, and an
identity's page shows everything about it. On the command line:

```bash
wot analyze ~/irclogs --nick rachel
wot analyze ~/irclogs --host 'Fish-*.chat'
wot analyze ~/irclogs --ident quokkawet --host bridge.example.net
wot analyze ~/irclogs --person rachel
```

Matching ignores case, and `*` matches any characters. Each identity is
listed with its person, nicks, hosts, sessions, time in channels, spoken
lines and when it was first and last seen, and with up to 10 matches, each
in full.

Every change to host groups, cuts or networks rebuilds identities straight
away.
