//! Timeline timestamps, as `apps/web/src/timestampFormat.ts` writes them for
//! an en-US 12-hour clock: "3:04 PM", "yesterday at 3:04 PM", "11/3 3:04 PM".

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

fn local(iso: &str, zone: &TimeZone) -> Option<Zoned> {
    Some(iso.parse::<Timestamp>().ok()?.to_zoned(zone.clone()))
}

fn clock(time: &Zoned) -> String {
    let hour = time.hour();
    let twelve = if hour % 12 == 0 { 12 } else { hour % 12 };
    format!("{twelve}:{:02} {}", time.minute(), if hour < 12 { "AM" } else { "PM" })
}

/// `formatDayAwareTimestamp`.
pub fn day_aware(iso: &str, now: &Zoned) -> Option<String> {
    let time = local(iso, now.time_zone())?;
    let days = (now.date() - time.date()).get_days().max(0);
    let day = |date: Date| format!("{}/{}", date.month(), date.day());
    Some(match days {
        0 => clock(&time),
        1 => format!("yesterday at {}", clock(&time)),
        _ if time.year() == now.year() => format!("{} {}", day(time.date()), clock(&time)),
        _ => format!("{}/{} {}", day(time.date()), time.year(), clock(&time)),
    })
}

/// `formatChatTimestampTooltip`: "3:04 PM, 5th November 2026".
pub fn tooltip(iso: &str, zone: &TimeZone) -> Option<String> {
    let time = local(iso, zone)?;
    let day = time.day();
    let suffix = match (day % 100, day % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    Some(format!("{}, {day}{suffix} {} {}", clock(&time), time.strftime("%B"), time.year()))
}

/// The day-aware text and its tooltip, in the system time zone.
pub fn now_local(iso: &str) -> Option<(String, String)> {
    let now = Zoned::now();
    Some((day_aware(iso, &now)?, tooltip(iso, now.time_zone())?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(iso: &str) -> Zoned {
        iso.parse::<Timestamp>().unwrap().to_zoned(TimeZone::fixed(jiff::tz::offset(-5)))
    }

    #[test]
    fn day_aware_text_drops_the_date_for_today_and_names_yesterday() {
        let now = at("2026-11-05T20:00:00Z");
        assert_eq!(day_aware("2026-11-05T20:04:00Z", &now).unwrap(), "3:04 PM");
        assert_eq!(day_aware("2026-11-05T05:30:00Z", &now).unwrap(), "12:30 AM");
        assert_eq!(day_aware("2026-11-05T04:59:00Z", &now).unwrap(), "yesterday at 11:59 PM");
        assert_eq!(day_aware("2026-11-03T17:00:00Z", &now).unwrap(), "11/3 12:00 PM");
        assert_eq!(day_aware("2025-11-03T17:00:00Z", &now).unwrap(), "11/3/2025 12:00 PM");
        assert_eq!(day_aware("not a time", &now), None);
    }

    #[test]
    fn tooltip_spells_the_date_with_an_ordinal() {
        let zone = TimeZone::fixed(jiff::tz::offset(-5));
        assert_eq!(tooltip("2026-11-05T20:04:00Z", &zone).unwrap(), "3:04 PM, 5th November 2026");
        assert_eq!(tooltip("2026-11-01T20:04:00Z", &zone).unwrap(), "3:04 PM, 1st November 2026");
        assert_eq!(tooltip("2026-11-12T20:04:00Z", &zone).unwrap(), "3:04 PM, 12th November 2026");
        assert_eq!(tooltip("2026-11-22T20:04:00Z", &zone).unwrap(), "3:04 PM, 22nd November 2026");
    }
}
