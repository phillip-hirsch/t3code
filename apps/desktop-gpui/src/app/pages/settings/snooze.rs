//! Snooze menu options: `resolveSnoozePreset(s)` (`client-runtime/src/state/threadSettled.ts`)
//! and the editor helpers of `SnoozePresetsSettings.tsx` and `SnoozePresetsSettings.logic.ts`.

use std::collections::HashSet;

use chrono::{DateTime, Datelike as _, Duration, NaiveTime, TimeZone, Timelike as _};
use serde::{Deserialize, Serialize};

pub(super) const MAX_PRESETS: usize = 8;
pub(super) const MAX_MINUTES: i64 = crate::inbox::MAX_SNOOZE_PRESET_MINUTES;

/// `SnoozePresetSetting`: a duration, or a local time on a day.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(super) enum Preset {
    After { minutes: i64 },
    At { day: String, time: String },
}

impl Preset {
    /// Whether the contract accepts it: a duration is 1 minute to [`MAX_MINUTES`].
    pub(super) fn in_range(&self) -> bool {
        match self {
            Preset::After { minutes } => (1..=MAX_MINUTES).contains(minutes),
            Preset::At { .. } => true,
        }
    }
}

pub(super) fn defaults() -> Vec<Preset> {
    vec![after(60), after(180), at("today", "18:00"), at("tomorrow", "09:00"), at("monday", "09:00")]
}

fn after(minutes: i64) -> Preset {
    Preset::After { minutes }
}

fn at(day: &str, time: &str) -> Preset {
    Preset::At { day: day.into(), time: time.into() }
}

/// The "When" choices, in menu order.
pub(super) const WHEN: [(&str, &str); 10] = [
    ("after", "In"),
    ("today", "Today"),
    ("tomorrow", "Tomorrow"),
    ("monday", "Monday"),
    ("tuesday", "Tuesday"),
    ("wednesday", "Wednesday"),
    ("thursday", "Thursday"),
    ("friday", "Friday"),
    ("saturday", "Saturday"),
    ("sunday", "Sunday"),
];

const WEEKDAYS: [&str; 7] = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unit {
    Minutes,
    Hours,
    Days,
}

impl Unit {
    pub(super) const ALL: [Unit; 3] = [Unit::Minutes, Unit::Hours, Unit::Days];

    pub(super) fn minutes(self) -> i64 {
        match self {
            Unit::Minutes => 1,
            Unit::Hours => 60,
            Unit::Days => 24 * 60,
        }
    }

    pub(super) fn id(self) -> &'static str {
        match self {
            Unit::Minutes => "minutes",
            Unit::Hours => "hours",
            Unit::Days => "days",
        }
    }

    pub(super) fn label(self, amount: i64) -> &'static str {
        let label = self.id();
        if amount == 1 { &label[..label.len() - 1] } else { label }
    }
}

pub(super) fn id(preset: &Preset) -> String {
    match preset {
        Preset::After { minutes } => format!("after:{minutes}"),
        Preset::At { day, time } => format!("at:{day}:{time}"),
    }
}

/// The largest unit that divides `minutes` evenly.
pub(super) fn split_minutes(minutes: i64) -> (i64, Unit) {
    let unit =
        [Unit::Days, Unit::Hours].into_iter().find(|unit| minutes % unit.minutes() == 0).unwrap_or(Unit::Minutes);
    (minutes / unit.minutes(), unit)
}

/// The next option "Add option" offers: the first candidate not in the list.
pub(super) fn next_candidate(presets: &[Preset]) -> Preset {
    let used: HashSet<String> = presets.iter().map(id).collect();
    defaults()
        .into_iter()
        .chain([after(30), at("saturday", "09:00"), after(24 * 60)])
        .find(|candidate| !used.contains(&id(candidate)))
        .unwrap_or(after(60))
}

/// The other kind's last value, so switching "When" back restores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct KindMemory {
    pub minutes: i64,
    pub time: String,
}

impl Default for KindMemory {
    fn default() -> Self {
        Self { minutes: 60, time: "09:00".into() }
    }
}

/// `withWhen`: `preset` moved to a new "When" choice.
pub(super) fn with_when(preset: &Preset, when: &str, memory: &KindMemory) -> Option<Preset> {
    if when == "after" {
        return Some(match preset {
            Preset::After { .. } => preset.clone(),
            Preset::At { .. } => after(memory.minutes),
        });
    }
    if !WHEN.iter().any(|(id, _)| *id == when) {
        return None;
    }
    let time = match preset {
        Preset::At { time, .. } => time.clone(),
        Preset::After { .. } => memory.time.clone(),
    };
    Some(Preset::At { day: when.into(), time })
}

/// A valid `HH:MM` 24-hour time.
pub(super) fn parse_time(time: &str) -> Option<NaiveTime> {
    (time.len() == 5 && time.as_bytes()[2] == b':').then(|| NaiveTime::parse_from_str(time, "%H:%M").ok())?
}

/// `snoozeTimeOfDayLabel`: "6:00 PM".
pub(super) fn format_time<Tz: TimeZone>(date: &DateTime<Tz>) -> String {
    let (pm, hour) = date.hour12();
    format!("{hour}:{:02} {}", date.minute(), if pm { "PM" } else { "AM" })
}

pub(super) struct Resolved<Tz: TimeZone> {
    pub id: String,
    pub label: String,
    pub when_label: String,
    pub wake: DateTime<Tz>,
}

fn duration_label(minutes: i64) -> String {
    let (amount, unit) = split_minutes(minutes);
    format!("{amount} {}", unit.label(amount))
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn days_ahead<Tz: TimeZone>(day: &str, now: &DateTime<Tz>) -> i64 {
    match day {
        "today" => 0,
        "tomorrow" => 1,
        _ => {
            let target = WEEKDAYS.iter().position(|d| *d == day).unwrap_or(0) as i64;
            match (target - i64::from(now.weekday().num_days_from_sunday()) + 7) % 7 {
                0 => 7,
                ahead => ahead,
            }
        }
    }
}

/// Labels follow the actual day gap, so a Monday option on a Sunday reads "Tomorrow".
fn label(preset: &Preset, days_ahead: i64) -> String {
    match preset {
        Preset::After { minutes } => format!("In {}", duration_label(*minutes)),
        Preset::At { time, .. } if days_ahead == 0 => {
            let hour: u32 = time[..2].parse().unwrap_or(0);
            if hour < 12 {
                "This morning"
            } else if hour < 17 {
                "This afternoon"
            } else {
                "This evening"
            }
            .into()
        }
        Preset::At { .. } if days_ahead == 1 => "Tomorrow".into(),
        Preset::At { day, .. } if day == "monday" => "Next week".into(),
        Preset::At { day, .. } if days_ahead == 7 => format!("Next {}", capitalize(day)),
        Preset::At { day, .. } => capitalize(day),
    }
}

/// `resolveSnoozePreset`: when an option wakes from `now`, or `None` for a passed "Today".
pub(super) fn resolve<Tz: TimeZone>(preset: &Preset, now: &DateTime<Tz>) -> Option<Resolved<Tz>> {
    let (wake, ahead) = match preset {
        Preset::After { minutes } => (now.clone().checked_add_signed(Duration::try_minutes(*minutes)?)?, 0),
        Preset::At { day, time } => {
            let ahead = days_ahead(day, now);
            // Calendar days, so a DST day of 23 or 25 hours still lands on the right date.
            let date = now.date_naive().checked_add_days(chrono::Days::new(ahead as u64))?;
            let local = date.and_time(parse_time(time)?);
            let wake = now.timezone().from_local_datetime(&local).earliest()?;
            if wake <= *now {
                return None;
            }
            (wake, ahead)
        }
    };
    let label = label(preset, ahead);
    // Durations and "Next week" don't name the wake day, so a wake on another day shows it.
    let names_day = matches!(preset, Preset::At { .. }) && label != "Next week";
    let time = format_time(&wake);
    let when_label = if names_day || wake.date_naive() == now.date_naive() {
        time
    } else {
        format!("{} {time}", wake.naive_local().format("%a"))
    };
    Some(Resolved { id: id(preset), label, when_label, wake })
}

fn wake_minute<Tz: TimeZone>(resolved: &Resolved<Tz>) -> i64 {
    resolved.wake.timestamp().div_euclid(60)
}

/// `resolveSnoozePresets`: the menu, soonest first, one row per wake minute.
pub(super) fn resolve_all<Tz: TimeZone>(presets: &[Preset], now: &DateTime<Tz>) -> Vec<Resolved<Tz>> {
    let mut resolved: Vec<_> = presets.iter().filter_map(|preset| resolve(preset, now)).collect();
    resolved.sort_by_key(wake_minute);
    let mut seen = HashSet::new();
    resolved.retain(|preset| seen.insert(wake_minute(preset)));
    resolved
}

/// `sortSnoozeRows`: soonest wake first, options with no wake now last, ties in place.
pub(super) fn sort_rows<K, Tz: TimeZone>(rows: &mut [(K, Preset)], now: &DateTime<Tz>) {
    rows.sort_by_cached_key(|(_, preset)| resolve(preset, now).map_or(i64::MAX, |r| r.wake.timestamp_millis()));
}

/// `reconcileSnoozeDraft`: `None` adopts `saved`; otherwise the writes still in flight after
/// the one `saved` acknowledges.
pub(super) fn reconcile(pending: &[Vec<Preset>], saved: &[Preset]) -> Option<Vec<Vec<Preset>>> {
    let acknowledged = pending.iter().position(|snapshot| snapshot.as_slice() == saved)?;
    Some(pending[acknowledged + 1..].to_vec())
}

/// What each editor row previews on its right.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Preview {
    Menu { label: String, when: String },
    Muted(&'static str),
}

pub(super) fn previews<Tz: TimeZone>(presets: &[Preset], now: &DateTime<Tz>) -> Vec<Preview> {
    let (mut ids, mut wakes) = (HashSet::new(), HashSet::new());
    presets
        .iter()
        .map(|preset| match resolve(preset, now) {
            None => Preview::Muted("Passed for today"),
            Some(resolved) if ids.contains(&resolved.id) => Preview::Muted("Duplicate of an option above"),
            Some(resolved) if wakes.contains(&resolved.wake.timestamp_millis()) => {
                Preview::Muted("Same time as an option above")
            }
            Some(resolved) => {
                ids.insert(resolved.id);
                wakes.insert(resolved.wake.timestamp_millis());
                Preview::Menu { label: resolved.label, when: resolved.when_label }
            }
        })
        .collect()
}

/// "Menu now: …", naming the wake where two entries share a label.
pub(super) fn menu_order<Tz: TimeZone>(presets: &[Preset], now: &DateTime<Tz>) -> String {
    let menu = resolve_all(presets, now);
    if menu.is_empty() {
        return "Menus offer only Custom…".into();
    }
    let repeated = |label: &str| menu.iter().filter(|preset| preset.label == label).count() > 1;
    let names: Vec<String> = menu
        .iter()
        .map(|preset| {
            if repeated(&preset.label) {
                format!("{} {}", preset.label, preset.when_label)
            } else {
                preset.label.clone()
            }
        })
        .collect();
    format!("Menu now: {}", names.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, NaiveDate};

    /// Wednesday 2026-11-04 at `hh:mm`, UTC-8.
    fn wednesday(hour: u32, minute: u32) -> DateTime<FixedOffset> {
        let local = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap().and_hms_opt(hour, minute, 0).unwrap();
        FixedOffset::west_opt(8 * 3600).unwrap().from_local_datetime(&local).unwrap()
    }

    #[test]
    fn presets_resolve_to_menu_labels() {
        let now = wednesday(10, 15);
        let shown: Vec<(String, String)> =
            resolve_all(&defaults(), &now).into_iter().map(|r| (r.label, r.when_label)).collect();
        assert_eq!(
            shown,
            [
                ("In 1 hour", "11:15 AM"),
                ("In 3 hours", "1:15 PM"),
                ("This evening", "6:00 PM"),
                ("Tomorrow", "9:00 AM"),
                ("Next week", "Mon 9:00 AM"),
            ]
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
        );
        let late = wednesday(22, 30);
        assert!(resolve(&at("today", "18:00"), &late).is_none(), "a passed Today is left out");
        let day = resolve(&after(24 * 60), &late).unwrap();
        assert_eq!((day.label.as_str(), day.when_label.as_str()), ("In 1 day", "Thu 10:30 PM"));
        assert_eq!(resolve(&at("wednesday", "09:00"), &now).unwrap().label, "Next Wednesday");
        assert_eq!(resolve(&at("friday", "09:00"), &now).unwrap().label, "Friday");
        assert_eq!(resolve(&at("thursday", "13:00"), &now).unwrap().label, "Tomorrow");
        for minutes in [i64::MAX, i64::MIN, i64::MAX / 60_000, 200_000_000_000] {
            assert!(resolve(&after(minutes), &now).is_none(), "{minutes} minutes has no wake");
        }
        assert!(!after(MAX_MINUTES + 1).in_range() && !after(0).in_range() && after(MAX_MINUTES).in_range());
    }

    #[test]
    fn editor_rows_sort_by_wake_and_preview_duplicates() {
        let now = wednesday(19, 0);
        let mut rows: Vec<(u32, Preset)> =
            vec![(0, at("today", "18:00")), (1, at("tomorrow", "09:00")), (2, after(60)), (3, after(60))];
        sort_rows(&mut rows, &now);
        assert_eq!(rows.iter().map(|(key, _)| *key).collect::<Vec<_>>(), [2, 3, 1, 0], "passed options sort last");
        let presets: Vec<Preset> = rows.into_iter().map(|(_, preset)| preset).collect();
        assert_eq!(
            previews(&presets, &now)[1..],
            [
                Preview::Muted("Duplicate of an option above"),
                Preview::Menu { label: "Tomorrow".into(), when: "9:00 AM".into() },
                Preview::Muted("Passed for today"),
            ]
        );
        assert_eq!(
            previews(&[after(60 * 24), at("thursday", "19:00")], &now)[1],
            Preview::Muted("Same time as an option above")
        );
        assert_eq!(menu_order(&[at("today", "09:00")], &now), "Menus offer only Custom…");
        assert_eq!(menu_order(&[after(60), after(24 * 60)], &now), "Menu now: In 1 hour · In 1 day");
    }

    #[test]
    fn reconcile_keeps_drafts_until_an_outside_change() {
        let (a, b) = (vec![after(60)], vec![after(120)]);
        assert_eq!(reconcile(&[a.clone(), b.clone()], &a), Some(vec![b.clone()]), "our first write echoes back");
        assert_eq!(reconcile(&[a.clone(), b.clone()], &b), Some(vec![]));
        assert_eq!(reconcile(&[a], &defaults()), None, "someone else's change replaces the draft");
    }

    #[test]
    fn editor_helpers() {
        assert_eq!(split_minutes(90), (90, Unit::Minutes));
        assert_eq!(split_minutes(180), (3, Unit::Hours));
        assert_eq!(split_minutes(2 * 24 * 60), (2, Unit::Days));
        assert_eq!(next_candidate(&defaults()), after(30));
        assert_eq!(next_candidate(&[after(60)]), after(180));
        let memory = KindMemory { minutes: 45, time: "07:30".into() };
        assert_eq!(with_when(&after(60), "friday", &memory), Some(at("friday", "07:30")));
        assert_eq!(with_when(&at("today", "18:00"), "after", &memory), Some(after(45)));
        assert_eq!(with_when(&at("today", "18:00"), "monday", &memory), Some(at("monday", "18:00")));
        assert_eq!(parse_time("24:00"), None);
        assert!(parse_time("07:05").is_some() && parse_time("7:05").is_none());
        let decoded: Vec<Preset> = serde_json::from_value(
            serde_json::json!([{"kind": "after", "minutes": 60}, {"kind": "at", "day": "today", "time": "18:00"}]),
        )
        .unwrap();
        assert_eq!(decoded, [after(60), at("today", "18:00")]);
    }
}
