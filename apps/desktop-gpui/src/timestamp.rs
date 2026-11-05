//! Timeline timestamps, as `apps/web/src/timestampFormat.ts` writes them for
//! en-US: "3:04 PM", "yesterday at 3:04 PM", "11/3 3:04 PM", or "15:04" on a
//! 24-hour clock (`timestampFormat`; "locale" is 12-hour in en-US).

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

fn local(iso: &str, zone: &TimeZone) -> Option<Zoned> {
    Some(iso.parse::<Timestamp>().ok()?.to_zoned(zone.clone()))
}

/// Whether `timestampFormat` reads as a 12-hour clock.
pub fn hour12(timestamp_format: &str) -> bool {
    timestamp_format != "24-hour"
}

fn clock(time: &Zoned, hour12: bool) -> String {
    let hour = time.hour();
    if !hour12 {
        return format!("{hour:02}:{:02}", time.minute());
    }
    let twelve = if hour % 12 == 0 { 12 } else { hour % 12 };
    format!("{twelve}:{:02} {}", time.minute(), if hour < 12 { "AM" } else { "PM" })
}

/// `formatDayAwareTimestamp`.
pub fn day_aware(iso: &str, now: &Zoned, hour12: bool) -> Option<String> {
    let time = local(iso, now.time_zone())?;
    let days = (now.date() - time.date()).get_days().max(0);
    let day = |date: Date| format!("{}/{}", date.month(), date.day());
    Some(match days {
        0 => clock(&time, hour12),
        1 => format!("yesterday at {}", clock(&time, hour12)),
        _ if time.year() == now.year() => format!("{} {}", day(time.date()), clock(&time, hour12)),
        _ => format!("{}/{} {}", day(time.date()), time.year(), clock(&time, hour12)),
    })
}

/// `formatUpcomingTimestamp`: "3:04 PM", "tomorrow at 3:04 PM", "11/9 3:04 PM"; past times read as `day_aware`.
pub fn upcoming(iso: &str, now: &Zoned, hour12: bool) -> Option<String> {
    let time = local(iso, now.time_zone())?;
    let days = (time.date() - now.date()).get_days();
    if days < 0 {
        return day_aware(iso, now, hour12);
    }
    Some(match days {
        0 => clock(&time, hour12),
        1 => format!("tomorrow at {}", clock(&time, hour12)),
        _ if time.year() == now.year() => format!("{}/{} {}", time.month(), time.day(), clock(&time, hour12)),
        _ => format!("{}/{}/{} {}", time.month(), time.day(), time.year(), clock(&time, hour12)),
    })
}

/// `formatChatTimestampTooltip`: "3:04 PM, 5th November 2026".
pub fn tooltip(iso: &str, zone: &TimeZone, hour12: bool) -> Option<String> {
    let time = local(iso, zone)?;
    let day = time.day();
    let suffix = match (day % 100, day % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    Some(format!("{}, {day}{suffix} {} {}", clock(&time, hour12), time.strftime("%B"), time.year()))
}

/// The day-aware text and its tooltip, in the system time zone.
pub fn now_local(iso: &str, hour12: bool) -> Option<(String, String)> {
    let now = Zoned::now();
    Some((day_aware(iso, &now, hour12)?, tooltip(iso, now.time_zone(), hour12)?))
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
        assert_eq!(day_aware("2026-11-05T20:04:00Z", &now, true).unwrap(), "3:04 PM");
        assert_eq!(day_aware("2026-11-05T05:30:00Z", &now, true).unwrap(), "12:30 AM");
        assert_eq!(day_aware("2026-11-05T04:59:00Z", &now, true).unwrap(), "yesterday at 11:59 PM");
        assert_eq!(day_aware("2026-11-03T17:00:00Z", &now, true).unwrap(), "11/3 12:00 PM");
        assert_eq!(day_aware("2025-11-03T17:00:00Z", &now, true).unwrap(), "11/3/2025 12:00 PM");
        assert_eq!(day_aware("not a time", &now, true), None);
        assert_eq!(day_aware("2026-11-05T14:04:00Z", &now, hour12("24-hour")).unwrap(), "09:04");
        assert_eq!(day_aware("2026-11-04T20:04:00Z", &now, false).unwrap(), "yesterday at 15:04");
    }

    #[test]
    fn upcoming_text_names_tomorrow_and_dates_later_days() {
        let now = at("2026-11-05T20:00:00Z");
        assert_eq!(upcoming("2026-11-05T23:30:00Z", &now, true).unwrap(), "6:30 PM");
        assert_eq!(upcoming("2026-11-06T15:00:00Z", &now, true).unwrap(), "tomorrow at 10:00 AM");
        assert_eq!(upcoming("2026-11-09T15:00:00Z", &now, true).unwrap(), "11/9 10:00 AM");
        assert_eq!(upcoming("2027-01-09T15:00:00Z", &now, false).unwrap(), "1/9/2027 10:00");
        assert_eq!(upcoming("2026-11-04T20:04:00Z", &now, true).unwrap(), "yesterday at 3:04 PM");
    }

    #[test]
    fn tooltip_spells_the_date_with_an_ordinal() {
        let zone = TimeZone::fixed(jiff::tz::offset(-5));
        assert_eq!(tooltip("2026-11-05T20:04:00Z", &zone, true).unwrap(), "3:04 PM, 5th November 2026");
        assert_eq!(tooltip("2026-11-01T20:04:00Z", &zone, true).unwrap(), "3:04 PM, 1st November 2026");
        assert_eq!(tooltip("2026-11-12T20:04:00Z", &zone, true).unwrap(), "3:04 PM, 12th November 2026");
        assert_eq!(tooltip("2026-11-22T20:04:00Z", &zone, true).unwrap(), "3:04 PM, 22nd November 2026");
    }
}
