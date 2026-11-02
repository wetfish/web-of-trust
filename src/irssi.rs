//! Parser for irssi log lines.
//!
//! Expects irssi's default theme and these log settings, which are also
//! irssi's defaults:
//!
//! ```text
//! log_timestamp = %H:%M
//! log_open_string = --- Log opened %a %b %d %H:%M:%S %Y
//! log_close_string = --- Log closed %a %b %d %H:%M:%S %Y
//! log_day_changed = --- Day changed %a %b %d %Y
//! ```
//!
//! The event formats follow irssi's `themes/default.theme` and the format
//! tables in `src/fe-common/core/module-formats.c` and
//! `src/fe-common/irc/module-formats.c`.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

const LOG_OPENED: &str = "--- Log opened ";
const LOG_CLOSED: &str = "--- Log closed ";
const DAY_CHANGED: &str = "--- Day changed ";
const DATETIME_FORMAT: &str = "%a %b %d %H:%M:%S %Y";
const DATE_FORMAT: &str = "%a %b %d %Y";

/// Characters irssi shows before a nick for its channel status, such as
/// `@` for operators. A space means no status.
const NICK_MODES: [char; 6] = ['@', '+', '%', '~', '&', '!'];

/// One line of an irssi log file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line<'a> {
    LogOpened(NaiveDateTime),
    LogClosed(NaiveDateTime),
    DayChanged(NaiveDate),
    /// A timestamped line. Its date comes from the most recent open, close
    /// or day changed line; see [`DateTracker`].
    Event(NaiveTime, Event<'a>),
    Blank,
    Unrecognized,
}

/// What happened on a timestamped line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event<'a> {
    /// `<@nick> text` in a channel, or `<nick> text` in a query.
    Message {
        mode: Option<char>,
        nick: &'a str,
        text: &'a str,
    },
    /// ` * nick text`, or ` (*) nick text` for a private action shown
    /// outside a query window.
    Action { nick: &'a str, text: &'a str },
    /// `-nick- text`
    Notice { nick: &'a str, text: &'a str },
    /// `-!- nick [ident@host] has joined #channel`
    Join {
        nick: &'a str,
        ident: &'a str,
        host: &'a str,
        channel: &'a str,
    },
    /// `-!- nick [ident@host] has left #channel [reason]`
    Part {
        nick: &'a str,
        ident: &'a str,
        host: &'a str,
        channel: &'a str,
        reason: &'a str,
    },
    /// `-!- nick [ident@host] has quit [reason]`
    Quit {
        nick: &'a str,
        ident: &'a str,
        host: &'a str,
        reason: &'a str,
    },
    /// `-!- nick was kicked from #channel by op [reason]`
    Kick {
        nick: &'a str,
        channel: &'a str,
        by: &'a str,
        reason: &'a str,
    },
    /// `-!- old is now known as new`
    NickChange { old: &'a str, new: &'a str },
    /// `-!- You're now known as new`, the logging client's own nick.
    OwnNickChange { new: &'a str },
    /// `-!- nick changed the topic of #channel to: topic`
    Topic {
        nick: &'a str,
        channel: &'a str,
        topic: &'a str,
    },
    /// `-!- Topic unset by nick on #channel`
    TopicUnset { nick: &'a str, channel: &'a str },
    /// `-!- mode/#channel [+o nick] by op`, or `ServerMode/#channel ...`
    /// when a server set it.
    Mode {
        channel: &'a str,
        modes: &'a str,
        by: &'a str,
        server: bool,
    },
    /// `-!- Netsplit a <-> b quits: nick1, nick2`. irssi logs this instead
    /// of a quit line for each nick.
    Netsplit { servers: &'a str, nicks: &'a str },
    /// `-!- Netsplit over, joins: nick1, nick2`
    NetsplitOver { nicks: &'a str },
    /// `-!- Irssi: ...`, a message from the client itself.
    Client { text: &'a str },
    /// `!server text`, a notice from a server, such as connection messages
    /// or, for operators, notices about other users.
    ServerNotice { server: &'a str, text: &'a str },
}

/// Parses one line, with or without its trailing newline.
pub fn parse_line(line: &str) -> Line<'_> {
    let line = line.trim_end_matches(['\n', '\r']);
    if line.is_empty() {
        return Line::Blank;
    }
    if let Some(rest) = line.strip_prefix(LOG_OPENED) {
        return NaiveDateTime::parse_from_str(rest, DATETIME_FORMAT)
            .map_or(Line::Unrecognized, Line::LogOpened);
    }
    if let Some(rest) = line.strip_prefix(LOG_CLOSED) {
        return NaiveDateTime::parse_from_str(rest, DATETIME_FORMAT)
            .map_or(Line::Unrecognized, Line::LogClosed);
    }
    if let Some(rest) = line.strip_prefix(DAY_CHANGED) {
        return NaiveDate::parse_from_str(rest, DATE_FORMAT)
            .map_or(Line::Unrecognized, Line::DayChanged);
    }

    let Some((time, rest)) = split_timestamp(line) else {
        return Line::Unrecognized;
    };
    match parse_event(rest) {
        Some(event) => Line::Event(time, event),
        None => Line::Unrecognized,
    }
}

/// Tracks the current date while a log file is read from the top.
///
/// irssi writes a day changed line just before the first line of a new day.
/// It only compares the day of the month, so a log that stays silent until
/// the same day of a later month gets no day changed line, and its lines keep
/// the old date until the next open or close line.
#[derive(Default)]
pub struct DateTracker {
    date: Option<NaiveDate>,
}

impl DateTracker {
    /// Starts from a date already known, such as when resuming partway
    /// through a file.
    pub fn starting_at(date: Option<NaiveDate>) -> Self {
        DateTracker { date }
    }

    pub fn update(&mut self, line: &Line<'_>) {
        match line {
            Line::LogOpened(at) | Line::LogClosed(at) => self.date = Some(at.date()),
            Line::DayChanged(date) => self.date = Some(*date),
            _ => {}
        }
    }

    /// The date of the next timestamped line, or `None` before the first
    /// line that gives a date.
    pub fn date(&self) -> Option<NaiveDate> {
        self.date
    }
}

/// A line without its leading `HH:MM ` timestamp, if it has one.
pub fn without_timestamp(line: &str) -> &str {
    split_timestamp(line).map_or(line, |(_, rest)| rest)
}

/// Splits `HH:MM ` from the start of a line.
fn split_timestamp(line: &str) -> Option<(NaiveTime, &str)> {
    let bytes = line.as_bytes();
    if bytes.len() < 6 || bytes[2] != b':' || bytes[5] != b' ' {
        return None;
    }
    let hour = two_digits(&bytes[0..2])?;
    let minute = two_digits(&bytes[3..5])?;
    // The first six bytes are ASCII, so this is a character boundary.
    Some((NaiveTime::from_hms_opt(hour, minute, 0)?, &line[6..]))
}

fn two_digits(bytes: &[u8]) -> Option<u32> {
    match bytes {
        [tens @ b'0'..=b'9', ones @ b'0'..=b'9'] => {
            Some(u32::from(tens - b'0') * 10 + u32::from(ones - b'0'))
        }
        _ => None,
    }
}

fn parse_event(rest: &str) -> Option<Event<'_>> {
    if let Some(rest) = rest.strip_prefix("-!- ") {
        return parse_status(rest);
    }
    if let Some(rest) = rest.strip_prefix('<') {
        let (prefix, text) = rest.split_once('>')?;
        let text = text.strip_prefix(' ').unwrap_or(text);
        let (mode, nick) = split_mode(prefix);
        return valid_nick(nick).then_some(Event::Message { mode, nick, text });
    }
    if let Some(rest) = rest
        .strip_prefix(" * ")
        .or_else(|| rest.strip_prefix(" (*) "))
    {
        let (nick, text) = rest.split_once(' ').unwrap_or((rest, ""));
        return valid_nick(nick).then_some(Event::Action { nick, text });
    }
    if let Some(rest) = rest.strip_prefix('-') {
        let (prefix, text) = rest.split_once("- ")?;
        let (_, nick) = split_mode(prefix);
        return valid_nick(nick).then_some(Event::Notice { nick, text });
    }
    if let Some(rest) = rest.strip_prefix('!') {
        let (server, text) = rest.split_once(' ').unwrap_or((rest, ""));
        return (!server.is_empty()).then_some(Event::ServerNotice { server, text });
    }
    None
}

/// Parses the text after `-!- `.
fn parse_status(rest: &str) -> Option<Event<'_>> {
    if let Some(text) = rest.strip_prefix("Irssi: ") {
        return Some(Event::Client { text });
    }
    if let Some(new) = rest.strip_prefix("You're now known as ") {
        return Some(Event::OwnNickChange { new });
    }
    if let Some(rest) = rest.strip_prefix("mode/") {
        return parse_mode(rest, false);
    }
    if let Some(rest) = rest.strip_prefix("ServerMode/") {
        return parse_mode(rest, true);
    }
    if let Some(nicks) = rest.strip_prefix("Netsplit over, joins: ") {
        return Some(Event::NetsplitOver { nicks });
    }
    if let Some(rest) = rest.strip_prefix("Netsplit ") {
        let (servers, nicks) = rest.split_once(" quits: ")?;
        return Some(Event::Netsplit { servers, nicks });
    }
    if let Some(rest) = rest.strip_prefix("Topic unset by ") {
        let (nick, channel) = rest.split_once(" on ")?;
        return Some(Event::TopicUnset { nick, channel });
    }

    // Everything else starts with a nick.
    let (nick, rest) = rest.split_once(' ')?;
    if !valid_nick(nick) {
        return None;
    }

    if let Some(rest) = rest.strip_prefix('[') {
        let (address, rest) = rest.split_once("] ")?;
        let (ident, host) = address.split_once('@').unwrap_or(("", address));
        if let Some(channel) = rest.strip_prefix("has joined ") {
            return Some(Event::Join {
                nick,
                ident,
                host,
                channel,
            });
        }
        if let Some(rest) = rest.strip_prefix("has left ") {
            let (channel, reason) = rest.split_once(' ').unwrap_or((rest, ""));
            return Some(Event::Part {
                nick,
                ident,
                host,
                channel,
                reason: unbracket(reason),
            });
        }
        if let Some(reason) = rest.strip_prefix("has quit ") {
            return Some(Event::Quit {
                nick,
                ident,
                host,
                reason: unbracket(reason),
            });
        }
        return None;
    }

    if let Some(rest) = rest.strip_prefix("was kicked from ") {
        let (channel, rest) = rest.split_once(" by ")?;
        let (by, reason) = rest.split_once(' ').unwrap_or((rest, ""));
        return Some(Event::Kick {
            nick,
            channel,
            by,
            reason: unbracket(reason),
        });
    }
    if let Some(new) = rest.strip_prefix("is now known as ") {
        return Some(Event::NickChange { old: nick, new });
    }
    if let Some(rest) = rest.strip_prefix("changed the topic of ") {
        let (channel, topic) = rest.split_once(" to:")?;
        return Some(Event::Topic {
            nick,
            channel,
            topic: topic.strip_prefix(' ').unwrap_or(topic),
        });
    }
    None
}

/// Parses `#channel [modes] by nick`.
fn parse_mode(rest: &str, server: bool) -> Option<Event<'_>> {
    let (channel, rest) = rest.split_once(" [")?;
    let (modes, by) = rest.rsplit_once("] by ")?;
    Some(Event::Mode {
        channel,
        modes,
        by,
        server,
    })
}

/// Splits a channel status character, or the space irssi shows instead of
/// one, from the start of a nick.
fn split_mode(prefix: &str) -> (Option<char>, &str) {
    match prefix.chars().next() {
        Some(' ') => (None, &prefix[1..]),
        Some(mode) if NICK_MODES.contains(&mode) => (Some(mode), &prefix[mode.len_utf8()..]),
        _ => (None, prefix),
    }
}

fn valid_nick(nick: &str) -> bool {
    !nick.is_empty() && !nick.contains(char::is_whitespace)
}

/// Removes the brackets irssi puts around reasons: `[reason]`.
fn unbracket(text: &str) -> &str {
    text.strip_prefix('[')
        .and_then(|text| text.strip_suffix(']'))
        .unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(line: &str) -> Event<'_> {
        match parse_line(line) {
            Line::Event(_, event) => event,
            other => panic!("not an event: {other:?}"),
        }
    }

    fn date(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, day).unwrap()
    }

    #[test]
    fn parses_log_open_close_and_day_changed() {
        assert_eq!(
            parse_line("--- Log opened Sun Nov 01 01:16:13 2026\n"),
            Line::LogOpened(date(1).and_hms_opt(1, 16, 13).unwrap())
        );
        assert_eq!(
            parse_line("--- Log closed Sun Nov 01 01:21:27 2026"),
            Line::LogClosed(date(1).and_hms_opt(1, 21, 27).unwrap())
        );
        assert_eq!(
            parse_line("--- Day changed Mon Nov 02 2026"),
            Line::DayChanged(date(2))
        );
    }

    #[test]
    fn parses_the_timestamp() {
        assert_eq!(
            parse_line("01:16 <@rachel> Hello World."),
            Line::Event(
                NaiveTime::from_hms_opt(1, 16, 0).unwrap(),
                Event::Message {
                    mode: Some('@'),
                    nick: "rachel",
                    text: "Hello World."
                }
            )
        );
    }

    #[test]
    fn parses_messages_with_and_without_a_mode() {
        assert_eq!(
            event("01:21 < tester> Helllo"),
            Event::Message {
                mode: None,
                nick: "tester",
                text: "Helllo"
            }
        );
        assert_eq!(
            event("01:21 <rachel> hi from a query"),
            Event::Message {
                mode: None,
                nick: "rachel",
                text: "hi from a query"
            }
        );
        assert_eq!(
            event("01:21 <+voiced> a > b"),
            Event::Message {
                mode: Some('+'),
                nick: "voiced",
                text: "a > b"
            }
        );
    }

    #[test]
    fn parses_actions_and_notices() {
        assert_eq!(
            event("01:20  * rachel loves sample data"),
            Event::Action {
                nick: "rachel",
                text: "loves sample data"
            }
        );
        assert_eq!(
            event("01:20 -NickServ- This nickname is registered."),
            Event::Notice {
                nick: "NickServ",
                text: "This nickname is registered."
            }
        );
    }

    #[test]
    fn parses_joins_parts_and_quits() {
        assert_eq!(
            event("01:16 -!- rachel [rachel@NETADMIN.VHOST] has joined #analysis"),
            Event::Join {
                nick: "rachel",
                ident: "rachel",
                host: "NETADMIN.VHOST",
                channel: "#analysis"
            }
        );
        assert_eq!(
            event("01:21 -!- tester [rachel@Fish-67a.p8m.199.192.IP] has left #analysis []"),
            Event::Part {
                nick: "tester",
                ident: "rachel",
                host: "Fish-67a.p8m.199.192.IP",
                channel: "#analysis",
                reason: ""
            }
        );
        assert_eq!(
            event("02:00 -!- juniper [~juniper@Fish-3qz.k4m.10.20.IP] has quit [Quit: bye [now]]"),
            Event::Quit {
                nick: "juniper",
                ident: "~juniper",
                host: "Fish-3qz.k4m.10.20.IP",
                reason: "Quit: bye [now]"
            }
        );
    }

    #[test]
    fn parses_kicks_modes_and_nick_changes() {
        assert_eq!(
            event("01:22 -!- tester was kicked from #analysis by rachel [BYE]"),
            Event::Kick {
                nick: "tester",
                channel: "#analysis",
                by: "rachel",
                reason: "BYE"
            }
        );
        assert_eq!(
            event("01:21 -!- mode/#analysis [+b :tester!*@*] by rachel"),
            Event::Mode {
                channel: "#analysis",
                modes: "+b :tester!*@*",
                by: "rachel",
                server: false
            }
        );
        assert_eq!(
            event("01:23 -!- tester is now known as tester2"),
            Event::NickChange {
                old: "tester",
                new: "tester2"
            }
        );
        assert_eq!(
            event("01:23 -!- You're now known as tester2"),
            Event::OwnNickChange { new: "tester2" }
        );
    }

    #[test]
    fn parses_topics_netsplits_and_client_lines() {
        assert_eq!(
            event("02:01 -!- rachel changed the topic of #analysis to: Welcome!"),
            Event::Topic {
                nick: "rachel",
                channel: "#analysis",
                topic: "Welcome!"
            }
        );
        assert_eq!(
            event("02:02 -!- Topic unset by rachel on #analysis"),
            Event::TopicUnset {
                nick: "rachel",
                channel: "#analysis"
            }
        );
        assert_eq!(
            event("02:03 -!- Netsplit irc.a.net <-> irc.b.net quits: alice, bob"),
            Event::Netsplit {
                servers: "irc.a.net <-> irc.b.net",
                nicks: "alice, bob"
            }
        );
        assert_eq!(
            event("02:04 -!- Netsplit over, joins: alice, bob"),
            Event::NetsplitOver {
                nicks: "alice, bob"
            }
        );
        assert_eq!(
            event("01:16 -!- Irssi: Join to #analysis was synced in 0 secs"),
            Event::Client {
                text: "Join to #analysis was synced in 0 secs"
            }
        );
    }

    #[test]
    fn parses_server_notices() {
        assert_eq!(
            event("00:13 !cannonfodder.wetfish.net *** user1 invited user2 into the channel"),
            Event::ServerNotice {
                server: "cannonfodder.wetfish.net",
                text: "*** user1 invited user2 into the channel"
            }
        );
        assert_eq!(
            event("13:57 !denver.wetfish.net Welcome to FishNet!"),
            Event::ServerNotice {
                server: "denver.wetfish.net",
                text: "Welcome to FishNet!"
            }
        );
    }

    #[test]
    fn strips_the_timestamp() {
        assert_eq!(
            without_timestamp("01:23 -!- Something new"),
            "-!- Something new"
        );
        assert_eq!(without_timestamp("no timestamp"), "no timestamp");
    }

    #[test]
    fn flags_lines_it_does_not_know() {
        assert_eq!(parse_line("not a log line"), Line::Unrecognized);
        assert_eq!(
            parse_line("01:23 -!- Something new happened"),
            Line::Unrecognized
        );
        assert_eq!(parse_line("--- Log opened yesterday"), Line::Unrecognized);
        assert_eq!(parse_line("25:61 <rachel> hi"), Line::Unrecognized);
        assert_eq!(parse_line("\n"), Line::Blank);
    }

    #[test]
    fn tracks_the_date_through_a_file() {
        let mut dates = DateTracker::default();
        let mut seen = Vec::new();
        for text in [
            "01:00 <rachel> before any date",
            "--- Log opened Sun Nov 01 01:16:13 2026",
            "01:16 <@rachel> Hello World.",
            "--- Day changed Mon Nov 02 2026",
            "00:01 <@rachel> next day",
        ] {
            let line = parse_line(text);
            dates.update(&line);
            if matches!(line, Line::Event(..)) {
                seen.push(dates.date());
            }
        }
        assert_eq!(seen, [None, Some(date(1)), Some(date(2))]);
    }
}
