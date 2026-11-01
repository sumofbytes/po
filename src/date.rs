//! Days, and how people write them: the due dates of issues. A [`Date`] is a day with no time
//! of day, and [`parse_due`] reads `fri`, `+3d`, `nov 2` and `2026-11-02`.

/// A day, as days since 1970-01-01.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Date(i64);

const MONTHS: [&str; 12] = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];
/// From Sunday, as `Date::weekday` counts.
const WEEKDAYS: [&str; 7] = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"];

fn leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Date {
    /// The day, if there is one by that number in that month.
    pub fn from_ymd(year: i64, month: u32, day: u32) -> Option<Self> {
        if !(1..=9999).contains(&year) || !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
            return None;
        }
        // Howard Hinnant's days_from_civil.
        let y = if month <= 2 { year - 1 } else { year };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let m = i64::from(month);
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        Some(Self(era * 146097 + doe - 719468))
    }

    pub fn ymd(self) -> (i64, u32, u32) {
        let z = self.0 + 719468;
        let era = z.div_euclid(146097);
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        (yoe + era * 400 + i64::from(month <= 2), month, day)
    }

    /// 0 for Sunday through 6 for Saturday.
    pub fn weekday(self) -> usize {
        (self.0 + 4).rem_euclid(7) as usize
    }

    pub fn plus(self, days: i64) -> Self {
        Self(self.0 + days)
    }

    /// `2026-11-02`, as the file keeps it.
    pub fn iso(self) -> String {
        let (y, m, d) = self.ymd();
        format!("{y:04}-{m:02}-{d:02}")
    }

    pub fn from_iso(text: &str) -> Option<Self> {
        let mut parts = text.splitn(3, '-');
        let y = parts.next()?.parse().ok()?;
        let m = parts.next()?.parse().ok()?;
        let d = parts.next()?.parse().ok()?;
        Self::from_ymd(y, m, d)
    }
}

/// Today where the user is.
pub fn today() -> Date {
    #[cfg(unix)]
    {
        // SAFETY: `localtime_r` writes only into the `tm` it is given.
        let local = unsafe {
            let now = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = std::mem::zeroed();
            (!libc::localtime_r(&now, &mut tm).is_null()).then_some(tm)
        };
        if let Some(day) = local.and_then(|tm| Date::from_ymd(i64::from(tm.tm_year) + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32)) {
            return day;
        }
    }
    Date((crate::now_ms() / 86_400_000) as i64)
}

/// A month by its name or the first three letters of it.
fn month(word: &str) -> Option<u32> {
    (word.len() >= 3).then(|| MONTHS.iter().position(|name| name.starts_with(word))).flatten().map(|ix| ix as u32 + 1)
}

/// The first day after `today` that is `weekday`.
fn next_weekday(today: Date, weekday: usize) -> Date {
    let ahead = (weekday as i64 - today.weekday() as i64).rem_euclid(7);
    today.plus(if ahead == 0 { 7 } else { ahead })
}

/// A day of `month` with no year given: the first one from today on, which for Feb 29 may be years off.
fn coming(today: Date, month: u32, day: u32) -> Option<Date> {
    let year = today.ymd().0;
    (year..=year + 8).filter_map(|year| Date::from_ymd(year, month, day)).find(|date| *date >= today)
}

/// `number` days or weeks from today. A span of more than a century is a typo.
fn after(today: Date, number: &str, unit: &str) -> Option<Date> {
    let number: i64 = number.trim_start_matches('+').parse().ok().filter(|n| *n <= 36_500)?;
    match unit {
        "d" | "day" | "days" => Some(today.plus(number)),
        "w" | "wk" | "wks" | "week" | "weeks" => Some(today.plus(number * 7)),
        _ => None,
    }
}

fn is_span(unit: &str) -> bool {
    matches!(unit, "d" | "day" | "days" | "w" | "wk" | "wks" | "week" | "weeks")
}

/// What the user typed for a due date, read the way people say it: `today`, `tomorrow`, a weekday
/// (the next one), `next week` (its Monday), `+3d`, `2w` or `in 3 days` from today, `nov 2` or `2 nov`, or
/// `2026-11-02`. Nothing typed is no date, `Some(None)`; what can't be read is `None`.
pub fn parse_due(text: &str, today: Date) -> Option<Option<Date>> {
    let text = text.trim().to_lowercase();
    if text.is_empty() {
        return Some(None);
    }
    let words: Vec<&str> = text.split(|c: char| c.is_whitespace() || c == ',').filter(|w| !w.is_empty()).collect();
    let date = match words.as_slice() {
        ["today" | "tod"] => Some(today),
        ["tomorrow" | "tmr" | "tom"] => Some(today.plus(1)),
        ["next", "week"] => Some(next_weekday(today, 1)),
        [word] if word.len() >= 3 && WEEKDAYS.iter().any(|day| day.starts_with(word)) => WEEKDAYS.iter().position(|day| day.starts_with(word)).map(|ix| next_weekday(today, ix)),
        [word] if word.contains('-') => Date::from_iso(word),
        [word] => {
            let count = word.trim_start_matches('+');
            let (number, unit) = count.split_at(count.find(|c: char| !c.is_ascii_digit()).unwrap_or(count.len()));
            after(today, number, unit)
        }
        [number, unit] | ["in", number, unit] if is_span(unit) => after(today, number, unit),
        [first, second] | [first, second, _] => {
            let (name, day) = if first.chars().all(|c| c.is_ascii_digit()) { (second, first) } else { (first, second) };
            let (month, day) = (month(name)?, day.parse().ok()?);
            match words.get(2) {
                Some(year) => {
                    let year: i64 = year.parse().ok()?;
                    // Two digits are this century's.
                    Date::from_ymd(if year < 100 { 2000 + year } else { year }, month, day)
                }
                None => coming(today, month, day),
            }
        }
        _ => None,
    };
    date.map(Some)
}

/// A due date as the list shows it, near ones by how near: `Today`, `Tomorrow`, `Fri`, then
/// `Nov 2`, with the year once it isn't this one.
pub fn due_label(due: Date, today: Date) -> String {
    let short = |name: &str| {
        let mut chars = name.chars();
        chars.next().map(|c| c.to_ascii_uppercase()).into_iter().chain(chars.take(2)).collect::<String>()
    };
    match due.0 - today.0 {
        0 => "Today".into(),
        1 => "Tomorrow".into(),
        -1 => "Yesterday".into(),
        2..=6 => short(WEEKDAYS[due.weekday()]),
        _ => {
            let (year, month, day) = due.ymd();
            let name = short(MONTHS[month as usize - 1]);
            if year == today.ymd().0 { format!("{name} {day}") } else { format!("{name} {day} {year}") }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i64, m: u32, d: u32) -> Date {
        Date::from_ymd(y, m, d).unwrap()
    }

    #[test]
    fn days_round_trip_through_the_calendar() {
        assert_eq!(day(1970, 1, 1), Date(0));
        assert_eq!(day(2026, 10, 26).ymd(), (2026, 10, 26));
        assert_eq!(day(2024, 2, 29).plus(1).ymd(), (2024, 3, 1));
        assert_eq!(Date::from_ymd(2026, 2, 29), None);
        // 2026-10-26 is a Monday.
        assert_eq!(day(2026, 10, 26).weekday(), 1);
    }

    #[test]
    fn due_dates_are_read_the_way_people_say_them() {
        let monday = day(2026, 10, 26);
        assert_eq!(parse_due("", monday), Some(None));
        assert_eq!(parse_due("today", monday), Some(Some(monday)));
        assert_eq!(parse_due("Tomorrow", monday), Some(Some(day(2026, 10, 27))));
        assert_eq!(parse_due("fri", monday), Some(Some(day(2026, 10, 30))));
        // The day it is now means next week's.
        assert_eq!(parse_due("monday", monday), Some(Some(day(2026, 11, 2))));
        assert_eq!(parse_due("next week", monday), Some(Some(day(2026, 11, 2))));
        assert_eq!(parse_due("+3d", monday), Some(Some(day(2026, 10, 29))));
        assert_eq!(parse_due("2w", monday), Some(Some(day(2026, 11, 9))));
        assert_eq!(parse_due("nov 2", monday), Some(Some(day(2026, 11, 2))));
        assert_eq!(parse_due("2 November", monday), Some(Some(day(2026, 11, 2))));
        // A day already gone this year is next year's.
        assert_eq!(parse_due("jan 5", monday), Some(Some(day(2027, 1, 5))));
        assert_eq!(parse_due("mar 1, 2028", monday), Some(Some(day(2028, 3, 1))));
        assert_eq!(parse_due("2026-12-31", monday), Some(Some(day(2026, 12, 31))));
        assert_eq!(parse_due("soon", monday), None);
        assert_eq!(parse_due("feb 30", monday), None);
    }

    #[test]
    fn spans_years_and_leap_days_are_read_sensibly() {
        let monday = day(2026, 10, 26);
        assert_eq!(parse_due("3 days", monday), Some(Some(day(2026, 10, 29))));
        assert_eq!(parse_due("in 2 weeks", monday), Some(Some(day(2026, 11, 9))));
        assert_eq!(parse_due("nov 2 27", monday), Some(Some(day(2027, 11, 2))));
        // Feb 29 waits for the next leap year, 2028.
        assert_eq!(parse_due("feb 29", monday), Some(Some(day(2028, 2, 29))));
        // A typo that would overflow is unreadable, not a panic.
        assert_eq!(parse_due("9000000000000000000d", monday), None);
        assert_eq!(parse_due("9000000000000000000w", monday), None);
        assert_eq!(parse_due("nov 2 99999999999999999", monday), None);
        assert_eq!(parse_due("99999999999999999-01-01", monday), None);
    }

    #[test]
    fn near_dates_are_labelled_by_how_near() {
        let monday = day(2026, 10, 26);
        assert_eq!(due_label(monday, monday), "Today");
        assert_eq!(due_label(monday.plus(1), monday), "Tomorrow");
        assert_eq!(due_label(monday.plus(-1), monday), "Yesterday");
        assert_eq!(due_label(monday.plus(4), monday), "Fri");
        assert_eq!(due_label(day(2026, 11, 12), monday), "Nov 12");
        assert_eq!(due_label(day(2027, 1, 5), monday), "Jan 5 2027");
    }
}
