# People and trust

Trust puts what you know about people together with what the logs show.

## People

A person is a human behind one or more identities, from any network. Each
identity belongs to at most one person. Link them on the Identities page, by
ticking identities and choosing **Create person** or adding them to someone,
or on the command line:

```bash
wot people create rachel 1a2b3c4d5e6f 9f8e7d6c5b4a
wot people add rachel 0a1b2c3d4e5f
```

A person's page sums them up across all their identities: their nicks and
hosts, when they were last seen and last spoke, how much they take part, and
the last lines they said.

## Bots

A bot is recorded like a person, with `--bot` or **Create bot**. Bots are not
rated, have no trust score, are never flagged, and are left out of activity
rankings unless asked for.

## Ratings, notes and invitations

Rate how well you know each person, with the slider on their page or
`wot people rate`:

| Rating | Meaning |
| --- | --- |
| Known | You know them personally, or have for years. |
| Vouched | Someone you trust invited them or vouches for them. |
| Neutral | No opinion yet. Everyone starts here. |
| Wary | Something about them seems off. |
| Distrusted | They are not who they claim to be, or should not be here. |

Add dated notes on how you know someone, and record who invited them, so
chains of invitations can be followed both ways:

```bash
wot people note quokka "met at a meetup in 2019"
wot people invited-by quokka rachel
```

## Activity

For each identity, channel and month, `wot` counts:

- **Days present:** days with a session in the channel.
- **Active days:** days they said anything.
- **Time:** time present, counting overlapping sessions once.
- **Heard:** lines said in the channel while they were there.
- **Spoken:** lines they said themselves.
- **Share:** spoken as a share of heard. Present for months with a share
  near zero means listening, not talking.

Private messages are left out, since they only show conversations with the
logging client.

## Activity score

The activity score, 0 to 100, sums up a person's channel activity, or an
identity's when it belongs to no one:

- **Consistency**, up to 40 points: months with spoken lines, all 40 at 24.
- **Tenure**, up to 20 points: months from first to last seen, all 20 at 36.
- **Participation**, up to 40 points: days with spoken lines, all 40 at 365,
  with the first weeks counting the most.

**Someone who stopped talking** has their score cut to a fifth once they have
said nothing for 6 months, counted back from the newest line in the logs, so
a pause in importing changes nothing. Someone seen for less than 6 months is
too new to judge.

## Trust score

The trust score, 0 to 99, is the rating's band of 20 points, distrusted from
0 to 19 up to known from 80 to 99, with the activity score placing them
within it. The rating always comes first: a known friend who rarely talks
ranks above the busiest stranger.

## Flags

- **⚑ Silent** (red): present on at least 30 days without saying a line, in
  the channel or in any channel. That is how an account collecting logs
  behaves. Someone who said a little, in fewer than 3 months, and has
  watched silently for 30 days or more since gets **⚑ Silent since** that
  month: a hello years ago doesn't outweigh the silence after it.
- **Inactive since** (amber): spoke in at least 3 months, then nothing for 6
  months or more. They probably used to take part.

Rating someone vouched or known clears the red flag, for someone you know
who only reads.

## Checking a channel's users

On a channel's page, paste what `/who #channel` prints in irssi. Each user is
matched against the logs:

1. **Exact:** the same nick from an `ident@host` the identity used.
2. **New nick:** an `ident@host` the identity used, with a new nick.
3. **Nick only:** the nick was seen, but never from this `ident@host`.
   Anyone can take a nick, so this gives no trust.
4. **Never seen.**

Then they are sorted into lists:

- **Trusted:** an exact or new nick match to someone rated vouched or known.
- **Inactive:** an exact or new nick match to someone rated neutral who has
  gone inactive. Probably legitimate, but not confirmed until you rate them.
- **Untrusted:** silent watchers, users never seen, and anyone rated wary or
  distrusted.
- **Undecided:** everyone else, including nick-only matches.
- **Bots.**

Each pasted list is kept, so real names from it show on identity and person
pages. **Mark reviewed** records that you went through a channel's users; a
review stays valid for six months.

## Lines for lurker

[lurker](https://github.com/wetfish/lurkers) is the Wetfish bot that moves
people who only watch into one of two rooms until they speak. The summary
at the top of each person's page, and of each identity's, has the command
that marks them, in a box to check and change, with a **Copy** button.
Paste it into irssi, which sends it to lurker in a private message, so
channels don't see who is being marked:

```text
/msg lurker untrusted quokka grace=0m silent=2024-03-11
/msg lurker trusted marlowe grace=0m silent=2025-08-04 activity=daily year=2024
```

- **Room:** `trusted`, for #purgatory, when rated vouched or known;
  `untrusted`, for #FBI_watchlist, for everyone else, including identities
  that belong to no one. Bots get no line. An identity that belongs to a
  person gets the person's line, from all their identities.
- **Nick:** the nick they used most recently.
- **grace:** always `0m`, so lurker moves them as soon as it has warned
  them. Change it before sending if they should have longer.
- **silent:** the last day they spoke in a channel, or the first day they
  were seen if they never spoke.
- **activity** and **year**, for trusted lurkers only: how often they
  chatted in the year they were most active.

Activity comes from the days they spoke, month by month, so two busy months
count even in an otherwise quiet year. A month is:

- **daily** with lines on at least half its days,
- **weekly** with lines in at least 3 of its weeks: days 1 to 7, 8 to 14,
  15 to 21, and 22 to the end,
- **monthly** with any line.

`activity` is the highest of these reached in at least 2 months of one
year, and `year` is the year with the most months at it or higher. A tie
goes to the year with more days spoken, then to the later year. Someone who
never spoke in 2 months of the same year gets neither.

Dates are in the logs' time zone, while lurker counts days in UTC, so
`silent` can be a day off from lurker's count.
