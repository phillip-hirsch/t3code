//! Server timestamps: the one ISO-8601 parser, and timeline labels as
//! `apps/web/src/timestampFormat.ts` writes them for en-US: "3:04 PM",
//! "yesterday at 3:04 PM", "11/3 3:04 PM", or "15:04" on a 24-hour clock
//! (`timestampFormat`; "locale" is 12-hour in en-US).

use std::fmt::Display;

use chrono::{DateTime, Datelike as _, Local, NaiveDate, TimeZone, Timelike as _, Utc};

/// Milliseconds since the Unix epoch for an RFC 3339 timestamp, honoring its offset (`Date.parse`).
pub fn parse_ms(iso: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(iso).ok().map(|time| time.timestamp_millis())
}

/// Milliseconds since the Unix epoch, now.
pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// `Date.prototype.toISOString`: "2026-11-05T18:30:00.000Z"; `None` out of range.
pub fn iso(ms: i64) -> Option<String> {
    DateTime::from_timestamp_millis(ms).map(|t| t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

fn local<Tz: TimeZone>(iso: &str, zone: &Tz) -> Option<DateTime<Tz>> {
    Some(DateTime::parse_from_rfc3339(iso).ok()?.with_timezone(zone))
}

/// Whether `timestampFormat` reads as a 12-hour clock.
pub fn hour12(timestamp_format: &str) -> bool {
    timestamp_format != "24-hour"
}

fn clock<Tz: TimeZone>(time: &DateTime<Tz>, hour12: bool) -> String {
    let hour = time.hour();
    if !hour12 {
        return format!("{hour:02}:{:02}", time.minute());
    }
    let twelve = if hour % 12 == 0 { 12 } else { hour % 12 };
    format!("{twelve}:{:02} {}", time.minute(), if hour < 12 { "AM" } else { "PM" })
}

/// `formatDayAwareTimestamp`.
pub fn day_aware<Tz: TimeZone>(iso: &str, now: &DateTime<Tz>, hour12: bool) -> Option<String> {
    let time = local(iso, &now.timezone())?;
    let days = (now.date_naive() - time.date_naive()).num_days().max(0);
    let day = |date: NaiveDate| format!("{}/{}", date.month(), date.day());
    Some(match days {
        0 => clock(&time, hour12),
        1 => format!("yesterday at {}", clock(&time, hour12)),
        _ if time.year() == now.year() => format!("{} {}", day(time.date_naive()), clock(&time, hour12)),
        _ => format!("{}/{} {}", day(time.date_naive()), time.year(), clock(&time, hour12)),
    })
}

/// `formatUpcomingTimestamp`: "3:04 PM", "tomorrow at 3:04 PM", "11/9 3:04 PM"; past times read as `day_aware`.
pub fn upcoming<Tz: TimeZone>(iso: &str, now: &DateTime<Tz>, hour12: bool) -> Option<String> {
    let time = local(iso, &now.timezone())?;
    let days = (time.date_naive() - now.date_naive()).num_days();
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

/// [`upcoming`] in the system time zone.
pub fn upcoming_local(iso: &str, hour12: bool) -> Option<String> {
    upcoming(iso, &Local::now(), hour12)
}

/// `formatChatTimestampTooltip`: "3:04 PM, 5th November 2026".
pub fn tooltip<Tz: TimeZone>(iso: &str, zone: &Tz, hour12: bool) -> Option<String>
where
    Tz::Offset: Display,
{
    let time = local(iso, zone)?;
    let day = time.day();
    let suffix = match (day % 100, day % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    Some(format!("{}, {day}{suffix} {} {}", clock(&time, hour12), time.format("%B"), time.year()))
}

/// The day-aware text and its tooltip, in the system time zone.
pub fn now_local(iso: &str, hour12: bool) -> Option<(String, String)> {
    let now = Local::now();
    Some((day_aware(iso, &now, hour12)?, tooltip(iso, &Local, hour12)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn zone() -> FixedOffset {
        FixedOffset::west_opt(5 * 3600).unwrap()
    }

    fn at(iso: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(iso).unwrap().with_timezone(&zone())
    }

    #[test]
    fn parse_ms_reads_rfc3339_with_offsets_and_rejects_everything_else() {
        assert_eq!(parse_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(parse_ms("2026-11-04T10:15:00.000Z"), Some(1_793_787_300_000));
        assert_eq!(parse_ms("2026-11-04T10:15:00.5Z"), Some(1_793_787_300_500));
        assert_eq!(parse_ms("2026-11-04T10:15:00.123456789Z"), Some(1_793_787_300_123));
        assert_eq!(parse_ms("2026-11-04T12:15:00+02:00"), Some(1_793_787_300_000));
        assert_eq!(parse_ms("2026-11-04T12:15:00.5+02:00"), Some(1_793_787_300_500));
        assert_eq!(parse_ms("2026-11-04T05:15:00-05:00"), Some(1_793_787_300_000));
        assert_eq!(iso(1_793_787_300_500).as_deref(), Some("2026-11-04T10:15:00.500Z"));
        // Multibyte characters where digits, separators or offsets belong.
        for bad in [
            "2026-11-04",
            "garbage",
            "",
            "2026-11-04T10:15:00",
            "2026-02-30T10:15:00Z",
            "2026-13-04T10:15:00Z",
            "2026-11-04T10:15:00+0é00",
            "2026-11-04T10:15:00+é",
            "2026-11-04T10:15:00.éZ",
            "２０２６-11-04T10:15:00Z",
            "2026-11-04T10:15:00Zé",
            "\u{212A}026-11-04T10:15:00Z",
            "2026-11-04T10:15:00+99:99",
        ] {
            assert_eq!(parse_ms(bad), None, "{bad:?}");
        }
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
        let zone = zone();
        assert_eq!(tooltip("2026-11-05T20:04:00Z", &zone, true).unwrap(), "3:04 PM, 5th November 2026");
        assert_eq!(tooltip("2026-11-01T20:04:00Z", &zone, true).unwrap(), "3:04 PM, 1st November 2026");
        assert_eq!(tooltip("2026-11-12T20:04:00Z", &zone, true).unwrap(), "3:04 PM, 12th November 2026");
        assert_eq!(tooltip("2026-11-22T20:04:00Z", &zone, true).unwrap(), "3:04 PM, 22nd November 2026");
    }
}
