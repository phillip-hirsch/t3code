//! Server timestamps: the one ISO-8601 parser, and timeline labels as
//! `apps/web/src/timestampFormat.ts` writes them for en-US: "3:04 PM",
//! "yesterday at 3:04 PM", "11/3 3:04 PM", or "15:04" on a 24-hour clock
//! (`timestampFormat`; "locale" follows the system's clock).

use std::fmt::Display;
use std::sync::OnceLock;

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

/// Whether `timestampFormat` reads as a 12-hour clock (`resolveHour12`); "locale" follows the system.
pub fn hour12(timestamp_format: &str) -> bool {
    match timestamp_format {
        "24-hour" => false,
        "12-hour" => true,
        _ => system_hour12(),
    }
}

/// The system locale's clock, read once like the web's `timestampLocale`.
fn system_hour12() -> bool {
    static HOUR12: OnceLock<bool> = OnceLock::new();
    *HOUR12.get_or_init(|| {
        let (tag, forced) = system_locale();
        forced.unwrap_or_else(|| locale_hour12(&tag))
    })
}

/// `ElectronApp.systemLocale`: the locale, and macOS's forced clock (`AppleICUForce24HourTime`
/// or `AppleICUForce12HourTime`, as `hourCycle` h23 or h12).
#[cfg(target_os = "macos")]
fn system_locale() -> (String, Option<bool>) {
    use objc2_foundation::{NSLocale, NSUserDefaults, ns_string};
    let defaults = NSUserDefaults::standardUserDefaults();
    let forced = if defaults.boolForKey(ns_string!("AppleICUForce24HourTime")) {
        Some(false)
    } else if defaults.boolForKey(ns_string!("AppleICUForce12HourTime")) {
        Some(true)
    } else {
        None
    };
    (NSLocale::currentLocale().localeIdentifier().to_string(), forced)
}

/// Chromium's system locale on Windows: the user's regional format.
#[cfg(windows)]
fn system_locale() -> (String, Option<bool>) {
    use winsafe::{HKEY, RegistryValue, co};
    let name =
        HKEY::CURRENT_USER.RegGetValue(Some("Control Panel\\International"), Some("LocaleName"), co::RRF::RT_REG_SZ);
    match name {
        Ok(RegistryValue::Sz(name)) => (name, None),
        _ => ("en-US".to_owned(), None),
    }
}

/// Linux: the time locale from `LC_ALL`, `LC_TIME` or `LANG`, as Electron reads it.
#[cfg(all(unix, not(target_os = "macos")))]
fn system_locale() -> (String, Option<bool>) {
    let var = |name| std::env::var(name).ok().filter(|value: &String| !value.is_empty());
    let tag = var("LC_ALL").or_else(|| var("LC_TIME")).or_else(|| var("LANG"));
    let tag = tag.map(|tag| tag.split('.').next().unwrap_or_default().to_owned());
    (tag.filter(|tag| !tag.is_empty() && tag != "C" && tag != "POSIX").unwrap_or_else(|| "en-US".to_owned()), None)
}

/// Whether ICU's `Intl.DateTimeFormat` gives a locale a 12-hour clock for `{ hour: "numeric" }`.
/// `tag` is BCP 47 or POSIX ("en_US.UTF-8"); macOS's region override (`en_US@rg=gbzzzz`)
/// replaces the region, as `toSystemLocaleTag` does.
fn locale_hour12(tag: &str) -> bool {
    let (base, keywords) = tag.split_once('@').unwrap_or((tag, ""));
    let mut subtags = base.split('.').next().unwrap_or_default().split(['-', '_']);
    let language = subtags.next().unwrap_or_default().to_ascii_lowercase();
    let (mut script, mut region) = (None, None);
    for subtag in subtags {
        let alpha = subtag.bytes().all(|b| b.is_ascii_alphabetic());
        match subtag.len() {
            4 if alpha && script.is_none() => script = Some(subtag.to_ascii_lowercase()),
            2 if alpha => region = Some(subtag.to_ascii_uppercase()),
            3 if subtag.bytes().all(|b| b.is_ascii_digit()) => region = Some(subtag.to_owned()),
            _ => {}
        }
        if region.is_some() || !(subtag.len() == 4 && alpha) {
            break;
        }
    }
    let override_region = keywords.split(';').find_map(|keyword| {
        let rg = keyword.strip_prefix("rg=")?.to_ascii_uppercase();
        (rg.len() == 6 && rg.ends_with("ZZZZ")).then(|| rg[..2].to_owned())
    });
    let region = override_region.or(region).or_else(|| {
        // Traditional Chinese without a region is Taiwan's (`zh-Hant` → `zh-Hant-TW`).
        (language == "zh" && script.as_deref() == Some("hant")).then(|| "TW".to_owned())
    });
    let default = TWELVE_HOUR_LANGUAGES.contains(&language.as_str());
    let flipped = region.is_some_and(|region| FLIPPED_LOCALES.contains(&format!("{language}-{region}").as_str()));
    default != flipped
}

// ICU 78's answers (Node's `Intl`): languages whose bare tag reads 12-hour (those without
// data fall back to en-US), and the language-region pairs that read the other way.
const TWELVE_HOUR_LANGUAGES: &[&str] = &[
    "aa", "ab", "ae", "ak", "am", "an", "ar", "as", "av", "ay", "bh", "bi", "bn", "ceb", "ch", "ckb", "co", "cr", "cu",
    "dv", "dz", "ee", "el", "en", "fil", "fj", "gn", "gu", "haw", "hi", "ho", "ht", "hz", "ik", "io", "iu", "kg", "kj",
    "km", "kn", "ko", "kr", "ks", "kv", "la", "li", "mh", "mi", "ml", "mr", "ms", "na", "ng", "nr", "nv", "ny", "oj",
    "om", "or", "pa", "pi", "qu", "sa", "sd", "sm", "so", "sq", "ss", "ta", "te", "ti", "tl", "to", "ts", "tw", "ty",
    "ur", "ve", "vo", "wa", "yue",
];
const FLIPPED_LOCALES: &[&str] = &[
    "af-NA", "ar-IL", "ar-KM", "ar-MA", "bo-IN", "ckb-IR", "ee-TG", "en-AI", "en-AT", "en-BE", "en-BI", "en-BW",
    "en-BZ", "en-CC", "en-CH", "en-CK", "en-CM", "en-CX", "en-CZ", "en-DD", "en-DE", "en-DG", "en-DK", "en-EE",
    "en-ES", "en-FI", "en-FK", "en-FR", "en-FX", "en-GB", "en-GE", "en-GG", "en-GI", "en-GS", "en-HU", "en-ID",
    "en-IE", "en-IL", "en-IM", "en-IO", "en-IT", "en-JE", "en-JP", "en-KE", "en-LT", "en-LV", "en-MG", "en-MS",
    "en-MT", "en-MU", "en-MV", "en-NF", "en-NG", "en-NL", "en-NO", "en-NR", "en-NU", "en-PL", "en-PN", "en-PT",
    "en-RH", "en-RO", "en-RW", "en-SC", "en-SE", "en-SH", "en-SI", "en-SK", "en-SX", "en-TK", "en-TV", "en-TZ",
    "en-UA", "en-UG", "en-UK", "en-ZA", "en-ZW", "es-AR", "es-BO", "es-CL", "es-CO", "es-CR", "es-CU", "es-DO",
    "es-EC", "es-GT", "es-HN", "es-MX", "es-NI", "es-PA", "es-PE", "es-PH", "es-PR", "es-PY", "es-SV", "es-US",
    "es-UY", "es-VE", "ff-GH", "ff-GM", "ff-LR", "ff-MR", "ff-SL", "fr-DJ", "fr-DZ", "fr-MR", "fr-NH", "fr-SY",
    "fr-TD", "fr-TN", "fr-VU", "ha-GH", "ko-CN", "ku-IQ", "ms-ID", "ne-IN", "om-KE", "ps-PK", "pt-MO", "so-KE",
    "sq-MK", "sq-XK", "st-LS", "ta-LK", "tr-CY", "yue-CN", "zh-HK", "zh-MO", "zh-MY", "zh-SG", "zh-TW",
];

/// "3:04 PM", or "15:04" on a 24-hour clock.
pub fn clock<Tz: TimeZone>(time: &DateTime<Tz>, hour12: bool) -> String {
    let hour = time.hour();
    if !hour12 {
        return format!("{hour:02}:{:02}", time.minute());
    }
    let twelve = if hour.is_multiple_of(12) { 12 } else { hour % 12 };
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
    fn the_system_clock_follows_the_locale_like_icu() {
        for (tag, twelve) in [
            ("en-US", true),
            ("en_US", true),
            ("en", true),
            ("en-GB", false),
            ("en_GB.UTF-8", false),
            ("de_DE", false),
            ("de-US", false),
            ("ja_JP", false),
            ("ko-KR", true),
            ("es-ES", false),
            ("es-MX", true),
            ("el-GR", true),
            ("zh-CN", false),
            ("zh-Hant", true),
            ("zh-Hant-HK", true),
            ("zh_Hans_SG", true),
            ("pt-BR", false),
            // macOS's region override wins over the language's region.
            ("en_US@rg=gbzzzz", false),
            ("en_GB@rg=uszzzz", true),
            ("gsw-CH", false),
        ] {
            assert_eq!(locale_hour12(tag), twelve, "{tag}");
        }
        assert!(!hour12("24-hour"));
        assert!(hour12("12-hour"));
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
