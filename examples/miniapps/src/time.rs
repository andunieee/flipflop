//! Dates and times the way people write and read them, in the device's own time zone. The host
//! puts the clock and the zone in every event (`ts`, `tz`); nothing here reads a system clock.

use std::sync::atomic::{AtomicI64, Ordering};

const DAY_MS: i64 = 86_400_000;

static NOW: AtomicI64 = AtomicI64::new(0);
static TZ: AtomicI64 = AtomicI64::new(0);

/// Remember the clock an event came with.
pub fn set_clock(ts: i64, tz_minutes: i64) {
    NOW.store(ts, Ordering::Relaxed);
    TZ.store(tz_minutes, Ordering::Relaxed);
}

/// The device's clock, in epoch milliseconds.
pub fn now() -> i64 {
    NOW.load(Ordering::Relaxed)
}

fn tz_ms() -> i64 {
    TZ.load(Ordering::Relaxed) * 60_000
}

/// Today as a number of days since 1970-01-01, here.
pub fn today() -> i64 {
    day_of(now())
}

/// The local day of a moment.
pub fn day_of(ms: i64) -> i64 {
    (ms + tz_ms()).div_euclid(DAY_MS)
}

/// The local minute of the day of a moment.
pub fn minute_of(ms: i64) -> i64 {
    (ms + tz_ms()).rem_euclid(DAY_MS) / 60_000
}

/// The moment of a local day and minute of the day.
pub fn moment(day: i64, minute: i64) -> i64 {
    day * DAY_MS + minute * 60_000 - tz_ms()
}

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// 0 for Monday.
fn weekday(day: i64) -> usize {
    (day + 3).rem_euclid(7) as usize
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + i64::from(m <= 2), m, d)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> Option<i64> {
    let year = if m <= 2 { y - 1 } else { y };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    // Feb 30 and the like do not round-trip.
    ((1..=12).contains(&m) && civil_from_days(days) == (y, m, d)).then_some(days)
}

/// "Today", "Tomorrow", "Sat 21 Nov" (with the year when it is not this one).
pub fn day_label(day: i64) -> String {
    let today = today();
    match day - today {
        0 => "Today".to_owned(),
        1 => "Tomorrow".to_owned(),
        -1 => "Yesterday".to_owned(),
        _ => {
            let (y, m, d) = civil_from_days(day);
            let year = if y == civil_from_days(today).0 {
                String::new()
            } else {
                format!(" {y}")
            };
            format!(
                "{} {d} {}{year}",
                WEEKDAYS[weekday(day)],
                MONTHS[m as usize - 1]
            )
        }
    }
}

/// "18:30".
pub fn clock(minute: i64) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

/// "Sat 21 Nov, 18:30".
pub fn when(ms: i64) -> String {
    format!("{}, {}", day_label(day_of(ms)), clock(minute_of(ms)))
}

/// "2026-11-21".
pub fn iso(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Year, month (1 to 12) and day of the month of a day number.
pub fn ymd(day: i64) -> (i64, i64, i64) {
    civil_from_days(day)
}

/// The day number of a calendar date, if there is such a date (not 31 Feb).
pub fn day_from_ymd(y: i64, m: i64, d: i64) -> Option<i64> {
    days_from_civil(y, m, d)
}

pub fn month_name(month: i64) -> &'static str {
    MONTHS[(month.clamp(1, 12) - 1) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip() {
        set_clock(0, 0);
        let day = day_from_ymd(2026, 11, 21).unwrap();
        assert_eq!(iso(day), "2026-11-21");
        assert_eq!(ymd(day), (2026, 11, 21));
        assert_eq!(WEEKDAYS[weekday(day)], "Sat");
        assert_eq!(day_from_ymd(2026, 2, 30), None);
        assert_eq!(day_from_ymd(2028, 2, 29).map(ymd), Some((2028, 2, 29)));
        assert_eq!(day_label(1), "Tomorrow");
    }

    #[test]
    fn zones_shift_the_day() {
        set_clock(23 * 3_600_000, 120);
        assert_eq!(today(), 1);
        assert_eq!(moment(1, 60), 86_400_000 - 3_600_000);
    }
}
