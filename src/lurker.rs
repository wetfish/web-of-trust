//! Lines for lurker, the Wetfish IRC bot that moves people who only watch
//! into one of two rooms until they speak: people you know who went quiet
//! into #purgatory as trusted lurkers, and people nobody vouches for into
//! #FBI_watchlist as untrusted ones.
//!
//! A line is a command for lurker, such as
//! `/msg lurker trusted marlowe grace=0m silent=2025-08-04 activity=daily year=2024`,
//! for you to check, change and paste into irssi, which sends it to lurker
//! in a private message, so channels don't see who is marked. Its fields are
//! lurker's own:
//!
//! - **Room:** trusted for someone rated vouched or known, untrusted for
//!   everyone else, including identities linked to no one.
//! - **grace:** always 0 minutes, so lurker acts as soon as it warns them,
//!   to change before sending when someone should have longer.
//! - **silent:** the last day they spoke in a channel, or the first day they
//!   were seen if they never spoke.
//! - **activity** and **year**, for trusted lurkers only: how often they
//!   chatted in the year they were most active, from the days they spoke
//!   month by month rather than an average over the year.

use std::collections::BTreeMap;
use std::fmt::Write;

use chrono::{Datelike, NaiveDate};

use crate::history::Day;
use crate::stats::Rating;

/// How a line starts: irssi's command for a private message to lurker, so
/// marking someone is not announced in a channel.
const PREFIX: &str = "/msg lurker";

/// The grace period every line gives, to change before sending.
const GRACE: &str = "0m";

/// Months of one year at a frequency or more that make it someone's
/// activity, so a single busy month is not enough.
const PEAK_MONTHS: usize = 2;

/// Weeks of a month with a spoken line that make it a weekly month: days 1
/// to 7, 8 to 14, 15 to 21, and 22 to the end of the month.
const WEEKLY_WEEKS: usize = 3;

/// The room lurker moves someone into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Room {
    /// #purgatory, for someone rated vouched or known.
    Trusted,
    /// #FBI_watchlist, for someone rated neutral or lower.
    Untrusted,
}

impl Room {
    pub fn of(rating: Rating) -> Room {
        if rating.trusted() {
            Room::Trusted
        } else {
            Room::Untrusted
        }
    }

    /// lurker's command for the room.
    pub fn as_str(self) -> &'static str {
        match self {
            Room::Trusted => "trusted",
            Room::Untrusted => "untrusted",
        }
    }
}

/// How often someone chatted in a month, from the least to the most.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Frequency {
    /// Spoke at least once.
    Monthly,
    /// Spoke in at least 3 of the month's 4 weeks.
    Weekly,
    /// Spoke on at least half the month's days.
    Daily,
}

impl Frequency {
    /// From the most to the least.
    const MOST_FIRST: [Frequency; 3] = [Frequency::Daily, Frequency::Weekly, Frequency::Monthly];

    /// The value lurker takes.
    pub fn as_str(self) -> &'static str {
        match self {
            Frequency::Daily => "daily",
            Frequency::Weekly => "weekly",
            Frequency::Monthly => "monthly",
        }
    }
}

/// The year someone was most active, and how often they chatted then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peak {
    pub year: i32,
    pub frequency: Frequency,
}

/// What a line tells lurker about someone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lurker {
    pub room: Room,
    pub nick: String,
    /// None for someone never seen in a channel.
    pub silent: Option<NaiveDate>,
    /// Only for trusted lurkers, since lurker refuses it for untrusted ones.
    pub peak: Option<Peak>,
}

impl Lurker {
    /// For a person rated `rating`, from the days of their channel history.
    /// An identity linked to no one is rated neutral. Bots are not lurkers.
    pub fn new(nick: &str, rating: Rating, days: &BTreeMap<NaiveDate, Day>) -> Lurker {
        let room = Room::of(rating);
        Lurker {
            room,
            nick: nick.to_string(),
            silent: silent(days),
            peak: match room {
                Room::Trusted => peak(days),
                Room::Untrusted => None,
            },
        }
    }

    /// The command to send, such as
    /// `/msg lurker untrusted quokka grace=0m silent=2024-03-11`.
    pub fn line(&self) -> String {
        let mut line = format!(
            "{PREFIX} {} {} grace={GRACE}",
            self.room.as_str(),
            self.nick
        );
        if let Some(silent) = self.silent {
            let _ = write!(line, " silent={}", silent.format("%Y-%m-%d"));
        }
        if let Some(peak) = self.peak {
            let _ = write!(
                line,
                " activity={} year={}",
                peak.frequency.as_str(),
                peak.year
            );
        }
        line
    }
}

/// When someone went silent: the last day they spoke in a channel, or the
/// first day they were seen in one if they never spoke.
fn silent(days: &BTreeMap<NaiveDate, Day>) -> Option<NaiveDate> {
    days.iter()
        .rev()
        .find(|(_, day)| day.spoken > 0)
        .map(|(&date, _)| date)
        .or_else(|| days.keys().next().copied())
}

/// The days someone spoke in one month.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Month {
    /// Days with a spoken line.
    days: u32,
    /// Which of the month's weeks had a spoken line: days 1 to 7, 8 to 14,
    /// 15 to 21, and 22 to the end.
    weeks: [bool; 4],
    /// Days in the month.
    length: u32,
}

impl Month {
    fn frequency(&self) -> Frequency {
        let weeks = self.weeks.iter().filter(|&&spoke| spoke).count();
        if self.days * 2 >= self.length {
            Frequency::Daily
        } else if weeks >= WEEKLY_WEEKS {
            Frequency::Weekly
        } else {
            Frequency::Monthly
        }
    }
}

/// The months someone spoke in, by year and month.
fn months(days: &BTreeMap<NaiveDate, Day>) -> BTreeMap<(i32, u32), Month> {
    let mut months: BTreeMap<(i32, u32), Month> = BTreeMap::new();
    for (date, _) in days.iter().filter(|(_, day)| day.spoken > 0) {
        let month = months
            .entry((date.year(), date.month()))
            .or_insert_with(|| Month {
                length: u32::from(date.num_days_in_month()),
                ..Month::default()
            });
        month.days += 1;
        let week = ((date.day() - 1) / 7).min(3) as usize;
        month.weeks[week] = true;
    }
    months
}

/// The year someone was most active, and how often they chatted then: the
/// highest frequency they reached in at least 2 months of one year, and the
/// year with the most months at it or more. A tie goes to the year with
/// more days spoken, then to the later year. None when no year had 2 months
/// with a spoken line.
fn peak(days: &BTreeMap<NaiveDate, Day>) -> Option<Peak> {
    let mut years: BTreeMap<i32, Vec<Month>> = BTreeMap::new();
    for ((year, _), month) in months(days) {
        years.entry(year).or_default().push(month);
    }
    Frequency::MOST_FIRST.into_iter().find_map(|frequency| {
        years
            .iter()
            .map(|(&year, in_year)| {
                let reached = in_year
                    .iter()
                    .filter(|month| month.frequency() >= frequency)
                    .count();
                let spoke_on: u32 = in_year.iter().map(|month| month.days).sum();
                (reached, spoke_on, year)
            })
            .filter(|&(reached, _, _)| reached >= PEAK_MONTHS)
            .max()
            .map(|(_, _, year)| Peak { year, frequency })
    })
}

#[cfg(test)]
mod tests {
    use chrono::Days;

    use super::*;

    fn date(text: &str) -> NaiveDate {
        text.parse().unwrap()
    }

    /// `count` days in a row from `first`.
    fn run(first: &str, count: u64) -> Vec<NaiveDate> {
        (0..count).map(|n| date(first) + Days::new(n)).collect()
    }

    /// Days with one line said on each.
    fn spoke_on(dates: impl IntoIterator<Item = NaiveDate>) -> BTreeMap<NaiveDate, Day> {
        dates
            .into_iter()
            .map(|day| {
                (
                    day,
                    Day {
                        spoken: 1,
                        ..Day::default()
                    },
                )
            })
            .collect()
    }

    fn on(dates: &[&str]) -> Vec<NaiveDate> {
        dates.iter().map(|text| date(text)).collect()
    }

    fn frequency_of(dates: &[&str]) -> Frequency {
        let found = months(&spoke_on(on(dates)));
        assert_eq!(found.len(), 1, "one month");
        found.values().next().unwrap().frequency()
    }

    #[test]
    fn silent_is_the_last_day_spoken() {
        let mut history = spoke_on(on(&["2024-01-02", "2024-03-11"]));
        history.insert(date("2024-05-01"), Day::default());
        assert_eq!(silent(&history), Some(date("2024-03-11")));
    }

    #[test]
    fn silent_is_the_first_day_seen_without_a_line() {
        let mut history = BTreeMap::new();
        history.insert(date("2023-06-01"), Day::default());
        history.insert(date("2023-09-14"), Day::default());
        assert_eq!(silent(&history), Some(date("2023-06-01")));
        assert_eq!(silent(&BTreeMap::new()), None);
    }

    #[test]
    fn ratings_pick_the_room() {
        assert_eq!(Room::of(Rating::Known), Room::Trusted);
        assert_eq!(Room::of(Rating::Vouched), Room::Trusted);
        assert_eq!(Room::of(Rating::Neutral), Room::Untrusted);
        assert_eq!(Room::of(Rating::Wary), Room::Untrusted);
        assert_eq!(Room::of(Rating::Distrusted), Room::Untrusted);
    }

    #[test]
    fn daily_months_have_lines_on_half_their_days() {
        let frequency = |first: &str, count: u64| {
            let found = months(&spoke_on(run(first, count)));
            assert_eq!(found.len(), 1, "one month");
            found.values().next().unwrap().frequency()
        };
        // 31 days in March: 16 is half, rounded up. Days 1 to 15 are in
        // three weeks.
        assert_eq!(frequency("2024-03-01", 15), Frequency::Weekly);
        assert_eq!(frequency("2024-03-01", 16), Frequency::Daily);
        // 28 days in February 2023, 29 in February 2024.
        assert_eq!(frequency("2023-02-01", 14), Frequency::Daily);
        assert_eq!(frequency("2024-02-01", 14), Frequency::Monthly);
        assert_eq!(frequency("2024-02-01", 15), Frequency::Daily);
    }

    #[test]
    fn weekly_months_have_lines_in_3_of_4_weeks() {
        assert_eq!(
            frequency_of(&["2024-04-01", "2024-04-08", "2024-04-15"]),
            Frequency::Weekly
        );
        assert_eq!(
            frequency_of(&["2024-04-03", "2024-04-10", "2024-04-30"]),
            Frequency::Weekly
        );
        assert_eq!(
            frequency_of(&["2024-04-01", "2024-04-08"]),
            Frequency::Monthly
        );
        // Days 22 to 31 are one week.
        assert_eq!(
            frequency_of(&["2024-05-01", "2024-05-22", "2024-05-29", "2024-05-31"]),
            Frequency::Monthly
        );
        assert_eq!(frequency_of(&["2024-05-19"]), Frequency::Monthly);
    }

    #[test]
    fn two_daily_months_make_a_daily_year() {
        // A daily user for two months, and weekly for a whole year before.
        let mut dates = run("2024-01-01", 60);
        for month in 1..=12 {
            for day in [1, 8, 15] {
                dates.push(NaiveDate::from_ymd_opt(2023, month, day).unwrap());
            }
        }
        assert_eq!(
            peak(&spoke_on(dates)),
            Some(Peak {
                year: 2024,
                frequency: Frequency::Daily
            })
        );
    }

    #[test]
    fn one_daily_month_is_not_enough() {
        let mut dates = run("2023-01-01", 31);
        dates.extend(on(&["2023-02-01", "2023-02-08", "2023-02-15"]));
        dates.extend(on(&["2023-03-01", "2023-03-08", "2023-03-15"]));
        assert_eq!(
            peak(&spoke_on(dates)),
            Some(Peak {
                year: 2023,
                frequency: Frequency::Weekly
            })
        );
    }

    #[test]
    fn ties_go_to_more_days_then_the_later_year() {
        let weekly = |year: i32| {
            let mut dates = Vec::new();
            for month in [3, 4] {
                for day in [1, 8, 15] {
                    dates.push(NaiveDate::from_ymd_opt(year, month, day).unwrap());
                }
            }
            dates
        };
        let mut dates = weekly(2022);
        dates.extend(weekly(2023));
        assert_eq!(
            peak(&spoke_on(dates.clone())),
            Some(Peak {
                year: 2023,
                frequency: Frequency::Weekly
            })
        );
        dates.push(date("2022-03-22"));
        assert_eq!(
            peak(&spoke_on(dates)),
            Some(Peak {
                year: 2022,
                frequency: Frequency::Weekly
            })
        );
    }

    #[test]
    fn no_peak_without_two_months_in_a_year() {
        assert_eq!(peak(&spoke_on(run("2024-05-01", 31))), None);
        assert_eq!(peak(&spoke_on(on(&["2022-12-30", "2023-01-02"]))), None);
        assert_eq!(peak(&BTreeMap::new()), None);
    }

    #[test]
    fn lines_for_lurker() {
        let mut active = spoke_on(run("2024-01-01", 60));
        active.insert(date("2025-08-04"), Day::default());
        assert_eq!(
            Lurker::new("marlowe", Rating::Vouched, &active).line(),
            "/msg lurker trusted marlowe grace=0m silent=2024-02-29 activity=daily year=2024"
        );
        // Untrusted lurkers get no activity, which lurker refuses for them.
        assert_eq!(
            Lurker::new("quokka", Rating::Neutral, &active).line(),
            "/msg lurker untrusted quokka grace=0m silent=2024-02-29"
        );
        let quiet = spoke_on(on(&["2024-03-11"]));
        assert_eq!(
            Lurker::new("quokka", Rating::Wary, &quiet).line(),
            "/msg lurker untrusted quokka grace=0m silent=2024-03-11"
        );
        let mut never = BTreeMap::new();
        never.insert(date("2023-06-01"), Day::default());
        assert_eq!(
            Lurker::new("lindqvist", Rating::Known, &never).line(),
            "/msg lurker trusted lindqvist grace=0m silent=2023-06-01"
        );
        assert_eq!(
            Lurker::new("lindqvist", Rating::Distrusted, &BTreeMap::new()).line(),
            "/msg lurker untrusted lindqvist grace=0m"
        );
    }
}
