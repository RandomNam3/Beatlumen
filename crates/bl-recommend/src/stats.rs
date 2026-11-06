//! Listening statistics from a list of plays, the same for a device and for
//! a server's view of all devices of a login.

use bl_json::Value;
use std::collections::{HashMap, HashSet};

const DAY: i64 = 86_400;
/// Entries per top list.
const TOP: usize = 10;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatSong {
    pub id: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    pub album_id: String,
    pub genre: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StatPlay {
    /// Index into the songs; `None`: a song no longer known.
    pub song: Option<u32>,
    /// When it started (seconds since the Unix epoch).
    pub t: u64,
    pub ms: u64,
    pub skip: bool,
}

/// The period looked at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Range {
    Week,
    Month,
    Year,
    All,
}

impl Range {
    pub fn parse(s: &str) -> Range {
        match s {
            "week" => Range::Week,
            "month" => Range::Month,
            "year" => Range::Year,
            _ => Range::All,
        }
    }
    fn days(self) -> Option<i64> {
        match self {
            Range::Week => Some(7),
            Range::Month => Some(30),
            Range::Year => Some(365),
            Range::All => None,
        }
    }
}

#[derive(Default)]
struct Tally {
    plays: u64,
    seconds: f64,
}

fn tally_json(t: &Tally, v: Value) -> Value {
    v.with("plays", t.plays).with("seconds", t.seconds.round())
}

fn top<K>(map: HashMap<K, Tally>) -> Vec<(K, Tally)> {
    let mut v: Vec<(K, Tally)> = map.into_iter().collect();
    v.sort_by(|a, b| {
        b.1.plays
            .cmp(&a.1.plays)
            .then(b.1.seconds.total_cmp(&a.1.seconds))
    });
    v.truncate(TOP);
    v
}

/// Civil date of a day number (days since 1970-01-01).
fn civil(day: i64) -> (i64, u32, u32) {
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Works out the statistics. `utc_offset`: seconds the local time is ahead of
/// UTC, so that days and hours are the listener's.
pub fn stats(
    songs: &[StatSong],
    plays: &[StatPlay],
    range: Range,
    now: u64,
    utc_offset: i64,
) -> Value {
    let local_day = |t: u64| (t as i64 + utc_offset).div_euclid(DAY);
    let today = local_day(now);
    let from_day = range.days().map(|d| today - d + 1);

    let mut total = Tally::default();
    let mut skips = 0u64;
    let mut active = HashSet::new();
    let mut by_hour = [0f64; 24];
    let mut by_weekday = [0f64; 7];
    let mut by_song: HashMap<u32, Tally> = HashMap::new();
    let mut by_artist: HashMap<&str, Tally> = HashMap::new();
    let mut by_album: HashMap<&str, Tally> = HashMap::new();
    let mut by_genre: HashMap<String, Tally> = HashMap::new();
    let mut by_period: HashMap<i64, f64> = HashMap::new();
    // By day for short ranges, else by month.
    let by_days = matches!(range, Range::Week | Range::Month);
    let period_of = |day: i64| -> i64 {
        if by_days {
            day
        } else {
            let (y, m, _) = civil(day);
            y * 12 + i64::from(m) - 1
        }
    };
    let mut first: Option<u64> = None;
    for p in plays {
        let day = local_day(p.t);
        first = Some(first.map_or(p.t, |f: u64| f.min(p.t)));
        if from_day.is_some_and(|f| day < f) {
            continue;
        }
        let seconds = p.ms as f64 / 1000.0;
        total.seconds += seconds;
        if p.skip {
            skips += 1;
        } else {
            total.plays += 1;
            active.insert(day);
        }
        let secs_of_day = (p.t as i64 + utc_offset).rem_euclid(DAY);
        by_hour[(secs_of_day / 3600) as usize] += seconds;
        // 1970-01-01 was a Thursday; the week starts on Monday here.
        by_weekday[(day + 3).rem_euclid(7) as usize] += seconds;
        *by_period.entry(period_of(day)).or_default() += seconds;
        let Some(i) = p.song else {
            continue;
        };
        let Some(s) = songs.get(i as usize) else {
            continue;
        };
        if p.skip {
            continue;
        }
        let add = |t: &mut Tally| {
            t.plays += 1;
            t.seconds += seconds;
        };
        add(by_song.entry(i).or_default());
        for a in s.artists.iter().filter(|a| !a.is_empty()) {
            add(by_artist.entry(a.as_str()).or_default());
        }
        if !s.album_id.is_empty() {
            add(by_album.entry(s.album_id.as_str()).or_default());
        }
        for g in s
            .genre
            .split([';', ',', '/', '|'])
            .map(str::trim)
            .filter(|g| !g.is_empty())
        {
            add(by_genre.entry(g.to_string()).or_default());
        }
    }

    // Days in a row, whatever the range: up to today or yesterday.
    let days: HashSet<i64> = plays
        .iter()
        .filter(|p| !p.skip)
        .map(|p| local_day(p.t))
        .collect();
    let mut current = 0u64;
    let mut d = if days.contains(&today) {
        today
    } else {
        today - 1
    };
    while days.contains(&d) {
        current += 1;
        d -= 1;
    }
    let mut sorted: Vec<i64> = days.iter().copied().collect();
    sorted.sort_unstable();
    let (mut longest, mut run) = (0u64, 0u64);
    for (i, day) in sorted.iter().enumerate() {
        run = if i > 0 && sorted[i - 1] + 1 == *day {
            run + 1
        } else {
            1
        };
        longest = longest.max(run);
    }

    // The time line has no gaps.
    let timeline: Vec<Value> = if by_days {
        let start = from_day.unwrap_or(today);
        (start..=today)
            .map(|day| {
                let (y, m, d) = civil(day);
                Value::object()
                    .with("t", format!("{y:04}-{m:02}-{d:02}"))
                    .with(
                        "seconds",
                        by_period.get(&day).copied().unwrap_or(0.0).round(),
                    )
            })
            .collect()
    } else {
        let last = period_of(today);
        let first_period = match from_day {
            Some(f) => period_of(f),
            None => by_period
                .keys()
                .copied()
                .min()
                .unwrap_or(last)
                .max(last - 59),
        };
        (first_period..=last)
            .map(|p| {
                Value::object()
                    .with(
                        "t",
                        format!("{:04}-{:02}", p.div_euclid(12), p.rem_euclid(12) + 1),
                    )
                    .with("seconds", by_period.get(&p).copied().unwrap_or(0.0).round())
            })
            .collect()
    };

    let album_info: HashMap<&str, &StatSong> =
        songs.iter().map(|s| (s.album_id.as_str(), s)).collect();
    Value::object()
        .with("plays", total.plays)
        .with("skips", skips)
        .with("seconds", total.seconds.round())
        .with("activeDays", active.len())
        .with("streak", current)
        .with("longestStreak", longest)
        .with("since", first)
        .with(
            "topSongs",
            Value::Array(
                top(by_song)
                    .iter()
                    .map(|(i, t)| {
                        let s = &songs[*i as usize];
                        tally_json(
                            t,
                            Value::object()
                                .with("id", s.id.as_str())
                                .with("title", s.title.as_str())
                                .with("artist", s.artists.join(", ")),
                        )
                    })
                    .collect(),
            ),
        )
        .with(
            "topArtists",
            Value::Array(
                top(by_artist)
                    .iter()
                    .map(|(a, t)| tally_json(t, Value::object().with("name", *a)))
                    .collect(),
            ),
        )
        .with(
            "topAlbums",
            Value::Array(
                top(by_album)
                    .iter()
                    .map(|(a, t)| {
                        let s = album_info.get(a);
                        tally_json(
                            t,
                            Value::object()
                                .with("id", *a)
                                .with("title", s.map_or("", |s| s.album.as_str()))
                                .with("artist", s.map_or(String::new(), |s| s.artists.join(", "))),
                        )
                    })
                    .collect(),
            ),
        )
        .with(
            "topGenres",
            Value::Array(
                top(by_genre)
                    .iter()
                    .map(|(g, t)| tally_json(t, Value::object().with("name", g.as_str())))
                    .collect(),
            ),
        )
        .with(
            "byHour",
            Value::Array(by_hour.iter().map(|v| Value::from(v.round())).collect()),
        )
        .with(
            "byWeekday",
            Value::Array(by_weekday.iter().map(|v| Value::from(v.round())).collect()),
        )
        .with("timeline", Value::Array(timeline))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-11-05 12:00 UTC, a Thursday.
    const NOW: u64 = 1_793_880_000;
    const H: u64 = 3600;
    const D: u64 = 86_400;

    fn songs() -> Vec<StatSong> {
        vec![
            StatSong {
                id: "a".into(),
                title: "Song A".into(),
                artists: vec!["One".into()],
                album: "Album".into(),
                album_id: "al".into(),
                genre: "Rock; Pop".into(),
            },
            StatSong {
                id: "b".into(),
                title: "Song B".into(),
                artists: vec!["One".into(), "Two".into()],
                ..StatSong::default()
            },
        ]
    }

    fn play(song: u32, t: u64, secs: u64, skip: bool) -> StatPlay {
        StatPlay {
            song: Some(song),
            t,
            ms: secs * 1000,
            skip,
        }
    }

    #[test]
    fn totals_and_top_lists() {
        let plays = [
            play(0, NOW - H, 200, false),
            play(0, NOW - 2 * H, 200, false),
            play(1, NOW - 3 * H, 100, false),
            play(1, NOW - 4 * H, 5, true),
        ];
        let v = stats(&songs(), &plays, Range::All, NOW, 0);
        assert_eq!(v.get("plays").as_u64(), Some(3));
        assert_eq!(v.get("skips").as_u64(), Some(1));
        assert_eq!(v.get("seconds").as_f64(), Some(505.0));
        assert_eq!(v.get("topSongs").at(0).str_or("title", ""), "Song A");
        assert_eq!(v.get("topSongs").at(0).get("plays").as_u64(), Some(2));
        // Both artists of a song count; the skip counts for nobody.
        assert_eq!(v.get("topArtists").at(0).str_or("name", ""), "One");
        assert_eq!(v.get("topArtists").at(0).get("plays").as_u64(), Some(3));
        assert_eq!(v.get("topArtists").at(1).get("plays").as_u64(), Some(1));
        assert_eq!(v.get("topAlbums").at(0).str_or("title", ""), "Album");
        let genres = v.get("topGenres").as_array().unwrap();
        assert_eq!(genres.len(), 2);
    }

    #[test]
    fn range_cuts_off_but_streak_does_not() {
        let plays = [
            play(0, NOW - 40 * D, 100, false),
            play(0, NOW - 2 * D, 100, false),
            play(0, NOW - D, 100, false),
            play(0, NOW, 100, false),
        ];
        let week = stats(&songs(), &plays, Range::Week, NOW, 0);
        assert_eq!(week.get("plays").as_u64(), Some(3));
        assert_eq!(week.get("streak").as_u64(), Some(3));
        assert_eq!(week.get("longestStreak").as_u64(), Some(3));
        assert_eq!(week.get("activeDays").as_u64(), Some(3));
        assert_eq!(week.get("timeline").as_array().unwrap().len(), 7);
        let last = week.get("timeline").at(6).clone();
        assert_eq!(last.str_or("t", ""), "2026-11-05");
        assert_eq!(last.get("seconds").as_f64(), Some(100.0));
        let all = stats(&songs(), &plays, Range::All, NOW, 0);
        assert_eq!(all.get("plays").as_u64(), Some(4));
        // By month: September to November without gaps.
        let t = all.get("timeline").as_array().unwrap();
        assert_eq!(t[0].str_or("t", ""), "2026-09");
        assert_eq!(t[t.len() - 1].str_or("t", ""), "2026-11");
    }

    #[test]
    fn a_streak_may_end_yesterday_but_not_earlier() {
        let plays = [play(0, NOW - D, 100, false)];
        assert_eq!(
            stats(&songs(), &plays, Range::All, NOW, 0)
                .get("streak")
                .as_u64(),
            Some(1)
        );
        let plays = [play(0, NOW - 2 * D, 100, false)];
        let v = stats(&songs(), &plays, Range::All, NOW, 0);
        assert_eq!(v.get("streak").as_u64(), Some(0));
        assert_eq!(v.get("longestStreak").as_u64(), Some(1));
    }

    #[test]
    fn hours_and_weekdays_are_local() {
        // 12:00 UTC on a Thursday; at +13 h it is 01:00 on Friday.
        let plays = [play(0, NOW, 60, false)];
        let v = stats(&songs(), &plays, Range::All, NOW, 13 * 3600);
        assert_eq!(v.get("byHour").at(1).as_f64(), Some(60.0));
        assert_eq!(v.get("byWeekday").at(4).as_f64(), Some(60.0));
        let v = stats(&songs(), &plays, Range::All, NOW, 0);
        assert_eq!(v.get("byHour").at(12).as_f64(), Some(60.0));
        assert_eq!(v.get("byWeekday").at(3).as_f64(), Some(60.0));
    }

    #[test]
    fn unknown_songs_count_for_the_time_only() {
        let plays = [StatPlay {
            song: None,
            t: NOW,
            ms: 60_000,
            skip: false,
        }];
        let v = stats(&songs(), &plays, Range::All, NOW, 0);
        assert_eq!(v.get("plays").as_u64(), Some(1));
        assert!(v.get("topSongs").as_array().unwrap().is_empty());
    }

    #[test]
    fn dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_397), (2025, 11, 5));
        assert_eq!(civil(-1), (1969, 12, 31));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }
}
