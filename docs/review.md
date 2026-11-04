# Reviewing in a browser

```bash
wot review
wot review --port 8080 --db /path/to/other.sqlite
```

`wot review` serves the review pages at <http://127.0.0.1:7171/> until you
press Ctrl+C. It only listens on 127.0.0.1, so only this computer can open
it, and it has no login. Pages are plain HTML, in your system's light or
dark mode. The only script is a small one for copy buttons, such as the one
beside a [lurker line](trust.md#lines-for-lurker); without it, the text can
still be selected and copied by hand.

Decisions are made with ordinary forms and do the same as the matching
command. Reloading a page never repeats one. Decisions that change
identities rebuild them, which the terminal shows step by step.

## Pages

- **Overview:** each network with its identities, people and bots, and its
  channels with their lines, spoken lines and how many of their identities
  never spoke. **Analyze now** reads new lines.
- **Analyze:** the log folders the imported files came from, and
  **Analyze now**, which does what `wot analyze` does with them. It runs in
  the background, and the page reloads itself with a progress bar for each
  step until it is done. Other pages keep working meanwhile, showing the data
  from before it, with a banner; make decisions once it is done. New network
  folders inside the log folders are found too, but a log folder never
  imported needs `wot analyze FOLDER` once.
- **Channels:** every channel, busiest first, with whether it was reviewed.
- **Channel:** paste a `/who` list to sort the channel's users into trusted,
  inactive, untrusted and undecided, see
  [Checking a channel's users](trust.md#checking-a-channels-users); then the
  channel's spoken lines and active nicks by day.
- **Activity:** identities ranked by their activity, in one channel or all,
  least or most active first, with silent watchers flagged.
- **Identities:** search by nick, ident, host or person. Tick results to
  make them a person or bot, or add them to one. A search by host also lists
  the matching hosts, to group them.
- **Identity:** one identity: who they are, the line marking them for
  lurker, their activity by month, their nicks, hosts, channels and nick
  changes, where a [cut](identities.md#cuts) can be made. An identity that
  belongs to a person gets the person's line.
- **People and bots:** everyone you recorded, most trusted first.
- **Person:** who they are across all their identities, when they were last
  active, the line marking them for lurker, the last lines they said, their
  activity by year and month with all their identities together, their trust
  score, rating, notes and identities.
- **History:** everything an identity or person said in channels, newest
  first, with filters for a channel, a day and text, and the days they were
  present.
- **Networks:** link network tags into networks and mark bouncers, see
  [Networks](logs.md#networks) and [Bouncers](logs.md#bouncers).
- **Host groups:** your [host groups](identities.md#host-groups), and the
  shared hosts not in one yet.
- **Cuts:** every cut, and whether it still applies.

## Code

The pages are in `src/review/`: `mod.rs` sets up the server, `layout.rs` has
the page frame, `copy.js` is the copy button script, and each area has its
own module. They read data through `src/stats.rs`, `src/lookup.rs`,
`src/activity.rs`, `src/trust.rs` and `src/history.rs`, and make decisions
through `src/decide.rs`, the same as the commands. Lurker lines are built in
`src/lurker.rs`.
