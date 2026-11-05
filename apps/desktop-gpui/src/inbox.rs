//! The sidebar's inbox organization and row labels.
//!
//! Ports `partitionSidebarThreads` (`apps/web/src/components/Sidebar.tsx`),
//! `client-runtime/src/state/{threadInbox,threadSort,threadSettled,models}.ts`,
//! `Sidebar.logic.ts` and `projectIdentity.ts`. One environment only, so the
//! web's environment-id tiebreaks and cross-environment grouping drop out.

use std::cmp::Reverse;
use std::collections::HashMap;

use crate::model::ThreadShell;

/// Milliseconds since the Unix epoch for an ISO-8601 timestamp (`Date.parse`).
pub fn parse_ms(iso: &str) -> Option<i64> {
    let bytes = iso.as_bytes();
    let num = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = iso.get(range)?;
        part.bytes().all(|b| b.is_ascii_digit()).then(|| part.parse().ok())?
    };
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = &iso[19..];
    let mut millis = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        let padded = format!("{:0<3}", &fraction[..digits.min(3)]);
        millis = padded.parse::<i64>().ok()?;
        rest = &fraction[digits..];
    }
    let offset_minutes = match rest {
        "Z" | "z" => 0,
        _ if rest.len() == 6 && (rest.starts_with('+') || rest.starts_with('-')) => {
            let sign = if rest.starts_with('-') { -1 } else { 1 };
            sign * (rest[1..3].parse::<i64>().ok()? * 60 + rest[4..6].parse::<i64>().ok()?)
        }
        _ => return None,
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    // Days from civil, Howard Hinnant's algorithm.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + hour) * 60 + minute - offset_minutes) * 60_000 + second * 1000 + millis)
}

fn ms(value: Option<&str>) -> Option<i64> {
    value.and_then(parse_ms)
}

/// `compactSidebarTimeLabel(formatRelativeTimeLabel(...))`.
pub fn format_relative(elapsed_ms: i64) -> String {
    let minutes = elapsed_ms.max(0) / 60_000;
    match minutes {
        0 => "now".into(),
        1..60 => format!("{minutes}m"),
        60..1440 => format!("{}h", minutes / 60),
        _ => format!("{}d", minutes / 1440),
    }
}

/// Milliseconds until [`format_relative`] shows a different label.
pub fn relative_changes_in(elapsed_ms: i64) -> i64 {
    let elapsed = elapsed_ms.max(0);
    let unit = match elapsed / 60_000 {
        0..60 => 60_000,
        60..1440 => 3_600_000,
        _ => 86_400_000,
    };
    unit - elapsed % unit
}

/// Milliseconds until [`format_duration`] shows a different label: every
/// second for the first minute, then every minute.
pub fn duration_changes_in(elapsed_ms: i64) -> i64 {
    let elapsed = elapsed_ms.max(0);
    let unit = if elapsed < 60_000 { 1000 } else { 60_000 };
    unit - elapsed % unit
}

/// Milliseconds until [`format_wake`] changes, or the wake itself; `None` once woken.
pub fn wake_changes_in(remaining_ms: i64) -> Option<i64> {
    const HOUR: i64 = 3_600_000;
    let unit = match remaining_ms {
        ..=0 => return None,
        1..HOUR => 60_000,
        HOUR..86_400_000 => HOUR,
        _ => 86_400_000,
    };
    Some(match remaining_ms % unit {
        0 => unit,
        rest => rest,
    })
}

/// `formatWorkingDurationLabel` in `client-runtime/src/state/models.ts`.
pub fn format_duration(elapsed_ms: i64) -> String {
    let seconds = elapsed_ms.max(0) / 1000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    format!("{}h {}m", minutes / 60, minutes % 60)
}

/// `snoozeWakeLabel`: time left until a snoozed thread wakes.
pub fn format_wake(remaining_ms: i64) -> String {
    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 24 * HOUR;
    let ceil = |unit: i64| (remaining_ms + unit - 1) / unit;
    match remaining_ms {
        ..=0 => "now".into(),
        1..HOUR => format!("{}m", ceil(60_000).max(1)),
        HOUR..DAY => format!("{}h", ceil(HOUR)),
        _ => format!("{}d", ceil(DAY)),
    }
}

fn is_active_run(status: &str) -> bool {
    matches!(status, "preparing" | "queued" | "starting" | "running" | "waiting")
}

fn is_terminal_run(status: &str) -> bool {
    matches!(status, "completed" | "interrupted" | "failed" | "cancelled" | "rolled_back")
}

/// What a subagent or side chat under a thread asks of the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildRequest {
    Approval,
    Input,
}

/// `pendingSubagentRequestsByThread` in `client-runtime/src/state/threadShell.ts`.
pub fn child_requests(threads: &[ThreadShell]) -> HashMap<&str, ChildRequest> {
    let mut requests: HashMap<&str, ChildRequest> = HashMap::new();
    let by_id: HashMap<&str, &ThreadShell> = threads.iter().map(|t| (t.id.as_str(), t)).collect();
    for child in threads {
        let Some(kind) = child.pending_runtime_request.as_ref().map(|r| r.kind.as_str()) else { continue };
        let Some(parent_id) = child.lineage.parent_thread_id.as_deref() else { continue };
        if kind == "auth_refresh" {
            continue;
        }
        if child.lineage.side_chat == Some(true) {
            let request = if kind == "user_input" { ChildRequest::Input } else { ChildRequest::Approval };
            if requests.get(parent_id) != Some(&ChildRequest::Approval) {
                requests.insert(parent_id, request);
            }
            continue;
        }
        // The coordinator answers its subagents' questions; only approvals climb.
        if child.lineage.relationship_to_parent.as_deref() != Some("subagent") || kind == "user_input" {
            continue;
        }
        let mut seen = vec![child.id.as_str()];
        let mut next = Some(parent_id);
        while let Some(id) = next.filter(|id| !seen.contains(id)) {
            seen.push(id);
            requests.insert(id, ChildRequest::Approval);
            next = by_id.get(id).and_then(|parent| {
                let chained = parent.lineage.relationship_to_parent.as_deref() == Some("subagent")
                    || parent.lineage.side_chat == Some(true);
                chained.then_some(parent.lineage.parent_thread_id.as_deref()).flatten()
            });
        }
    }
    requests
}

/// The facts the sidebar derives from one shell (`presentThreadShell`).
#[derive(Clone, Copy)]
pub struct Facts<'a> {
    pub thread: &'a ThreadShell,
    pub child_request: Option<ChildRequest>,
}

impl<'a> Facts<'a> {
    pub fn new(thread: &'a ThreadShell, requests: &HashMap<&str, ChildRequest>) -> Self {
        Self { thread, child_request: requests.get(thread.id.as_str()).copied() }
    }

    /// `shellRuntime(...)?.status`; `None` before the thread ever ran.
    pub fn runtime_status(&self) -> Option<&'a str> {
        let t = self.thread;
        let parked = t.status != "failed"
            && t.pending_background_tasks
                .iter()
                .any(|task| task.awaited == Some(true) || matches!(task.kind.as_str(), "subagent" | "background_task"));
        if t.latest_run_id.is_none() && t.active_provider_thread_id.is_none() && !parked {
            return None;
        }
        Some(if parked { "idle" } else { t.activity_run_status.as_deref().unwrap_or(&t.status) })
    }

    fn latest_run_status(&self) -> Option<&'a str> {
        self.thread.latest_run_id.as_ref()?;
        Some(if self.thread.status == "idle" { "completed" } else { &self.thread.status })
    }

    fn latest_run_requested_at(&self) -> Option<&'a str> {
        self.thread.latest_run_id.as_ref()?;
        self.thread.latest_run_requested_at.as_deref()
    }

    fn latest_run_started_at(&self) -> Option<&'a str> {
        self.thread.latest_run_id.as_ref()?;
        self.thread.latest_run_started_at.as_deref()
    }

    pub fn latest_run_completed_at(&self) -> Option<&'a str> {
        let t = self.thread;
        t.latest_run_id.as_ref()?;
        match &t.latest_run_completed_at {
            Some(value) => value.as_deref(),
            None if t.status == "idle" || is_terminal_run(&t.status) => Some(&t.updated_at),
            None => None,
        }
    }

    fn has_pending_approvals(&self) -> bool {
        self.thread
            .pending_runtime_request
            .as_ref()
            .is_some_and(|r| !matches!(r.kind.as_str(), "user_input" | "auth_refresh"))
    }

    fn has_pending_user_input(&self) -> bool {
        self.thread.pending_runtime_request.as_ref().is_some_and(|r| r.kind == "user_input")
    }

    /// `isThreadWorking` in `threadInbox.ts`.
    pub fn is_working(&self) -> bool {
        if self.has_pending_approvals() || self.has_pending_user_input() || self.child_request.is_some() {
            return false;
        }
        match self.runtime_status() {
            Some(status) if is_active_run(status) || status == "idle" => {}
            _ => return false,
        }
        let t = self.thread;
        let run_settled =
            self.latest_run_status().is_some_and(|status| !is_active_run(status)) && t.active_run_id != t.latest_run_id;
        !(t.interaction_mode.as_deref() == Some("plan") && t.has_actionable_proposed_plan && run_settled)
    }

    fn failure_edge(&self) -> Option<Option<&'a str>> {
        if !matches!(self.runtime_status(), Some("error" | "failed")) {
            return None;
        }
        if self.latest_run_status() == Some("failed") {
            return Some(self.latest_run_completed_at());
        }
        Some(Some(&self.thread.updated_at))
    }

    /// `threadRaisedHandWhileSnoozed` in `threadSettled.ts`.
    fn raised_hand_while_snoozed(&self) -> bool {
        if self.thread.pending_runtime_request.is_some()
            || self.child_request.is_some()
            || self.thread.pending_guest_message_count > 0
        {
            return true;
        }
        match self.failure_edge() {
            None => false,
            Some(failed_at) => match ms(self.thread.snoozed_at.as_deref()) {
                None => true,
                Some(snoozed_at) => ms(failed_at).is_some_and(|failed| failed > snoozed_at),
            },
        }
    }

    /// `effectiveSnoozed`.
    pub fn is_snoozed(&self, now: i64) -> bool {
        ms(self.thread.snoozed_until.as_deref()).is_some_and(|wake| wake > now) && !self.raised_hand_while_snoozed()
    }

    /// `threadWokeAt`.
    pub fn woke_at(&self, now: i64) -> Option<&'a str> {
        let t = self.thread;
        let wake = ms(t.snoozed_until.as_deref())?;
        if self.raised_hand_while_snoozed() {
            if let Some(Some(failed_at)) = self.failure_edge() {
                let newer = match ms(t.snoozed_at.as_deref()) {
                    None => true,
                    Some(snoozed_at) => ms(Some(failed_at)).is_some_and(|f| f > snoozed_at),
                };
                if newer {
                    return Some(failed_at);
                }
            }
            return if self.runtime_status().is_some() { Some(&t.updated_at) } else { t.snoozed_at.as_deref() };
        }
        (wake <= now).then_some(t.snoozed_until.as_deref()).flatten()
    }

    /// Woke and not visited since, the "Woke" top status.
    pub fn is_woke(&self, now: i64) -> bool {
        let Some(woke_at) = ms(self.woke_at(now)) else { return false };
        let visited_before = match &self.thread.last_visited_at {
            None => true,
            Some(visited) => parse_ms(visited).is_none_or(|v| v < woke_at),
        };
        visited_before && self.thread.settled_override.as_deref() != Some("settled")
    }

    /// `hasUnseenCompletion`.
    pub fn is_unread(&self) -> bool {
        let Some(completed) = ms(self.latest_run_completed_at()) else { return false };
        let Some(visited) = self.thread.last_visited_at.as_deref() else { return false };
        parse_ms(visited).is_none_or(|visited| completed > visited)
    }

    /// `resolveSidebarThreadStatus`.
    pub fn status(&self) -> Status {
        if self.has_pending_approvals() || self.child_request == Some(ChildRequest::Approval) {
            return Status::Approval;
        }
        if self.has_pending_user_input() || self.child_request == Some(ChildRequest::Input) {
            return Status::Input;
        }
        if self.thread.pending_guest_message_count > 0 {
            return Status::Request;
        }
        let runtime = self.runtime_status();
        match runtime {
            Some("running") if self.thread.blocked_on_thread_wait == Some(true) => Status::Waiting,
            Some("preparing") => Status::Preparing,
            Some("queued" | "starting" | "running" | "waiting") => Status::Working,
            Some("idle") => Status::Waiting,
            Some("failed") if self.thread.last_error_class.as_deref() == Some("usage_limit") => Status::Limited,
            Some("failed") => Status::Failed,
            _ => Status::Ready,
        }
    }

    /// `resolveThreadWorkingStartedAt`: when the running duration counts from.
    pub fn working_started_at(&self) -> Option<i64> {
        let t = self.thread;
        self.runtime_status()?;
        if let Some(started) = &t.activity_run_started_at {
            return ms(started.as_deref());
        }
        if self.latest_run_completed_at().is_none() && t.latest_run_id.is_some() && t.latest_run_id == t.active_run_id {
            return ms(self.latest_run_started_at()).or(ms(self.latest_run_requested_at()));
        }
        None
    }

    /// `resolveSettledThreadTimestamp`, in ms.
    pub fn settled_timestamp(&self) -> Option<i64> {
        if let Some(settled) = ms(self.thread.settled_at.as_deref()) {
            return Some(settled);
        }
        [
            self.thread.latest_user_message_at.as_deref(),
            self.latest_run_requested_at(),
            self.latest_run_started_at(),
            self.latest_run_completed_at(),
        ]
        .into_iter()
        .filter_map(ms)
        .max()
        .or_else(|| parse_ms(&self.thread.updated_at))
    }

    /// The compact time a card shows: last user message, else last update.
    pub fn activity_timestamp(&self) -> Option<i64> {
        parse_ms(self.thread.latest_user_message_at.as_deref().unwrap_or(&self.thread.updated_at))
    }

    fn return_timestamp(&self) -> i64 {
        [
            Some(self.thread.created_at.as_str()),
            self.thread.unsettled_at.as_deref(),
            self.latest_run_requested_at(),
            self.latest_run_completed_at(),
        ]
        .into_iter()
        .filter_map(ms)
        .fold(0, i64::max)
    }

    fn send_timestamp(&self) -> i64 {
        let sent = match &self.thread.latest_user_authored_message_at {
            Some(authored) => authored.as_deref(),
            None => self.latest_run_requested_at(),
        };
        ms(Some(&self.thread.created_at)).unwrap_or(0).max(ms(sent).unwrap_or(0))
    }
}

/// `SidebarThreadStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Approval,
    Input,
    Request,
    Preparing,
    Working,
    Waiting,
    Failed,
    Limited,
    Ready,
}

/// `shouldRecedeSidebarThread`.
pub fn recedes(status: Status, unread: bool, woke: bool, active: bool) -> bool {
    match status {
        _ if active => false,
        Status::Input | Status::Request => false,
        Status::Preparing | Status::Working | Status::Waiting => true,
        Status::Ready | Status::Approval => !unread && !woke,
        Status::Failed | Status::Limited => false,
    }
}

#[derive(Default)]
pub struct Sections<'a> {
    pub pinned: Vec<Facts<'a>>,
    pub active: Vec<Facts<'a>>,
    pub working: Vec<Facts<'a>>,
    pub snoozed: Vec<Facts<'a>>,
    pub settled: Vec<Facts<'a>>,
}

/// `partitionSidebarThreads` with the default inbox settings (Working shelf on).
/// `working_shelf` is false while the environment is disconnected.
pub fn partition<'a>(
    threads: &'a [ThreadShell],
    now: i64,
    working_shelf: bool,
    include: impl Fn(&ThreadShell) -> bool,
) -> Sections<'a> {
    let requests = child_requests(threads);
    let mut sections = Sections::default();
    let visible =
        threads.iter().filter(|t| t.is_listed() && t.archived_at.is_none() && t.deleted_at.is_none() && include(t));
    for thread in visible {
        let facts = Facts::new(thread, &requests);
        let list = if facts.is_snoozed(now) {
            &mut sections.snoozed
        } else if thread.settled_override.as_deref() == Some("settled") {
            &mut sections.settled
        } else if thread.pinned_at.is_some() {
            &mut sections.pinned
        } else if working_shelf && facts.is_working() {
            &mut sections.working
        } else {
            &mut sections.active
        };
        list.push(facts);
    }
    let id = |f: &Facts<'a>| f.thread.id.as_str();
    // `sortPinnedThreadsByOrderKey`: keyed rows by key, then keyless newest first.
    sections.pinned.sort_by_cached_key(|f| {
        let key = f.thread.pin_order_key.as_deref();
        let created = if key.is_none() { ms(Some(&f.thread.created_at)).unwrap_or(0) } else { 0 };
        (key.is_none(), key, Reverse(created), id(f))
    });
    sections.active.sort_by_cached_key(|f| (Reverse(f.return_timestamp()), id(f)));
    sections.working.sort_by_cached_key(|f| (Reverse(f.send_timestamp()), id(f)));
    sections.snoozed.sort_by_cached_key(|f| ms(f.thread.snoozed_until.as_deref()).unwrap_or(i64::MAX));
    sections.settled.sort_by_cached_key(|f| (Reverse(f.settled_timestamp().unwrap_or(0)), id(f)));
    sections
}

fn words(title: &str) -> Vec<Vec<char>> {
    let mut words = Vec::new();
    let mut current = Vec::new();
    for c in title.trim().chars() {
        if c.is_alphanumeric() {
            current.push(c);
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// `projectMonogram` in `apps/web/src/projectIdentity.ts`.
pub fn monogram(title: &str) -> String {
    let words = words(title);
    let Some(first_word) = words.first() else { return "PR".into() };
    let first = first_word[0];
    let second = first_word[1..]
        .iter()
        .find(|c| c.is_numeric())
        .or_else(|| if words.len() > 1 { words.last()?.first() } else { first_word.last() })
        .copied()
        .unwrap_or(first);
    format!("{first}{second}").to_uppercase().chars().take(2).collect()
}

/// `projectColor`: an index into [`PROJECT_COLORS`].
pub fn color_index(title: &str) -> usize {
    let seed = title.trim().to_lowercase();
    let seed = if seed.is_empty() { "project".to_owned() } else { seed };
    seed.chars().fold(0, |index, c| (index * 31 + c as usize) % PROJECT_COLORS.len())
}

/// `projectIconColors.ts`, as (name, light -600, dark -400).
pub const PROJECT_COLORS: [(&str, u32, u32); 18] = [
    ("gray", 0x4a5565, 0x99a1af),
    ("red", 0xe7000b, 0xff6467),
    ("orange", 0xf54900, 0xff8904),
    ("amber", 0xe17100, 0xffb900),
    ("yellow", 0xd08700, 0xfdc700),
    ("lime", 0x5ea500, 0x9ae600),
    ("green", 0x00a63e, 0x05df72),
    ("emerald", 0x009966, 0x00d492),
    ("teal", 0x009689, 0x00d5be),
    ("cyan", 0x0092b8, 0x00d3f2),
    ("sky", 0x0084d1, 0x00bcff),
    ("blue", 0x155dfc, 0x51a2ff),
    ("indigo", 0x4f39f6, 0x7c86ff),
    ("violet", 0x7f22fe, 0xa684ff),
    ("purple", 0x9810fa, 0xc27aff),
    ("fuchsia", 0xc800de, 0xed6aff),
    ("pink", 0xe60076, 0xfb64b6),
    ("rose", 0xec003f, 0xff637e),
];

#[cfg(test)]
mod tests {
    #[test]
    fn labels_report_when_they_next_change() {
        assert_eq!(relative_changes_in(0), 60_000);
        assert_eq!(relative_changes_in(61_000), 59_000);
        assert_eq!(relative_changes_in(2 * 3_600_000 + 5), 3_600_000 - 5);
        assert_eq!(duration_changes_in(1_500), 500);
        assert_eq!(duration_changes_in(90_000), 30_000);
        assert_eq!(wake_changes_in(0), None);
        assert_eq!(wake_changes_in(90_000), Some(30_000));
        assert_eq!(wake_changes_in(120_000), Some(60_000));
    }

    use super::*;
    use serde::Deserialize as _;
    use serde_json::{Value, json};

    fn thread(id: &str, extra: Value) -> ThreadShell {
        let mut value = json!({
            "id": id, "projectId": "p", "title": id, "status": "idle",
            "createdAt": "2026-11-01T00:00:00.000Z", "updatedAt": "2026-11-01T00:00:00.000Z",
            "lineage": {"rootThreadId": id},
        });
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        ThreadShell::deserialize(&value).unwrap()
    }

    fn ids(list: &[Facts]) -> Vec<String> {
        list.iter().map(|f| f.thread.id.clone()).collect()
    }

    const NOW: &str = "2026-11-04T12:00:00.000Z";

    fn now() -> i64 {
        parse_ms(NOW).unwrap()
    }

    #[test]
    fn parses_iso_timestamps() {
        assert_eq!(parse_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(parse_ms("2026-11-04T10:15:00.000Z"), Some(1_793_787_300_000));
        assert_eq!(parse_ms("2026-11-04T10:15:00.5Z"), Some(1_793_787_300_500));
        assert_eq!(parse_ms("2026-11-04T12:15:00+02:00"), Some(1_793_787_300_000));
        assert_eq!(parse_ms("2026-11-04"), None);
        assert_eq!(parse_ms("garbage"), None);
    }

    #[test]
    fn formats_relative_times_and_durations() {
        let cases = [
            (0, "now"),
            (59_999, "now"),
            (60_000, "1m"),
            (3_599_999, "59m"),
            (3_600_000, "1h"),
            (86_399_999, "23h"),
            (86_400_000, "1d"),
            (40 * 86_400_000, "40d"),
        ];
        for (elapsed, label) in cases {
            assert_eq!(format_relative(elapsed), label, "{elapsed}");
        }
        let cases = [
            (-5, "0s"),
            (999, "0s"),
            (35_000, "35s"),
            (60_000, "1m"),
            (35 * 60_000, "35m"),
            (3_600_000, "1h 0m"),
            (3_725_000, "1h 2m"),
        ];
        for (elapsed, label) in cases {
            assert_eq!(format_duration(elapsed), label, "{elapsed}");
        }
        let cases = [
            (0, "now"),
            (1, "1m"),
            (60_001, "2m"),
            (3_600_000, "1h"),
            (3_600_001, "2h"),
            (86_400_000, "1d"),
            (86_400_001, "2d"),
        ];
        for (remaining, label) in cases {
            assert_eq!(format_wake(remaining), label, "{remaining}");
        }
    }

    #[test]
    fn monograms_and_colors_match_the_web() {
        assert_eq!(monogram("japan-trip-december-2026"), "J2");
        assert_eq!(monogram("examples"), "ES");
        assert_eq!(monogram(".claude"), "CE");
        assert_eq!(monogram("tr3x"), "T3");
        assert_eq!(monogram("t3code"), "T3");
        assert_eq!(monogram("My Project"), "MP");
        assert_eq!(monogram("x"), "XX");
        assert_eq!(monogram("--"), "PR");
        assert_eq!(PROJECT_COLORS[color_index("japan-trip-december-2026")].0, "blue");
        assert_eq!(PROJECT_COLORS[color_index("examples")].0, "blue");
        assert_eq!(PROJECT_COLORS[color_index(".claude")].0, "purple");
    }

    #[test]
    fn sections_follow_snoozed_settled_pinned_working_active_precedence() {
        let threads = vec![
            thread("snoozed", json!({"snoozedUntil": "2026-11-05T00:00:00.000Z", "settledOverride": "settled"})),
            thread(
                "woke-early",
                json!({"snoozedUntil": "2026-11-05T00:00:00.000Z", "pendingRuntimeRequest": {"kind": "command"}}),
            ),
            thread("snooze-over", json!({"snoozedUntil": "2026-11-03T00:00:00.000Z"})),
            thread("settled", json!({"settledOverride": "settled", "pinnedAt": NOW})),
            thread("pinned", json!({"pinnedAt": NOW, "latestRunId": "r", "status": "running"})),
            thread("working", json!({"latestRunId": "r", "activeRunId": "r", "status": "running"})),
            thread(
                "asking",
                json!({"latestRunId": "r", "status": "running", "pendingRuntimeRequest": {"kind": "user_input"}}),
            ),
            thread(
                "sub",
                json!({"lineage": {"rootThreadId": "working", "parentThreadId": "working", "relationshipToParent": "subagent"}}),
            ),
            thread("archived", json!({"archivedAt": NOW})),
        ];
        let sections = partition(&threads, now(), true, |_| true);
        assert_eq!(ids(&sections.snoozed), ["snoozed"]);
        assert_eq!(ids(&sections.settled), ["settled"]);
        assert_eq!(ids(&sections.pinned), ["pinned"]);
        assert_eq!(ids(&sections.working), ["working"]);
        let mut active = ids(&sections.active);
        active.sort();
        assert_eq!(active, ["asking", "snooze-over", "woke-early"]);

        let offline = partition(&threads, now(), false, |_| true);
        assert!(offline.working.is_empty(), "a disconnected environment has no Working shelf");
    }

    #[test]
    fn subagent_approvals_pull_the_parent_out_of_working() {
        let threads = vec![
            thread("parent", json!({"latestRunId": "r", "status": "running"})),
            thread(
                "child",
                json!({"pendingRuntimeRequest": {"kind": "command"}, "lineage": {"rootThreadId": "parent", "parentThreadId": "parent", "relationshipToParent": "subagent"}}),
            ),
        ];
        let sections = partition(&threads, now(), true, |_| true);
        assert_eq!(ids(&sections.active), ["parent"]);
        assert_eq!(sections.active[0].status(), Status::Approval);
    }

    #[test]
    fn sort_orders_match_the_web() {
        let threads = vec![
            // Active: newest of createdAt, unsettledAt, run requested/completed.
            thread("a-old", json!({})),
            thread("a-unsettled", json!({"unsettledAt": "2026-11-03T00:00:00.000Z"})),
            thread(
                "a-run",
                json!({"latestRunId": "r", "status": "completed", "latestRunCompletedAt": "2026-11-02T00:00:00.000Z"}),
            ),
            // Pinned: keyed by key, then keyless newest first.
            thread("p-b", json!({"pinnedAt": NOW, "pinOrderKey": "b"})),
            thread("p-a", json!({"pinnedAt": NOW, "pinOrderKey": "a"})),
            thread("p-new", json!({"pinnedAt": NOW, "createdAt": "2026-11-03T00:00:00.000Z"})),
            thread("p-old", json!({"pinnedAt": NOW})),
            // Settled: settledAt, else latest activity, descending.
            thread("s-at", json!({"settledOverride": "settled", "settledAt": "2026-11-02T00:00:00.000Z"})),
            thread("s-msg", json!({"settledOverride": "settled", "latestUserMessageAt": "2026-11-03T00:00:00.000Z"})),
            // Snoozed: soonest wake first.
            thread("z-late", json!({"snoozedUntil": "2026-11-09T00:00:00.000Z"})),
            thread("z-soon", json!({"snoozedUntil": "2026-11-05T00:00:00.000Z"})),
            // Working: newest send first; an explicit null authored time beats the run time.
            thread(
                "w-run",
                json!({"latestRunId": "r", "status": "running", "latestRunRequestedAt": "2026-11-03T00:00:00.000Z"}),
            ),
            thread(
                "w-msg",
                json!({"latestRunId": "r", "status": "running", "latestUserAuthoredMessageAt": "2026-11-02T00:00:00.000Z", "latestRunRequestedAt": "2026-11-04T00:00:00.000Z"}),
            ),
        ];
        let sections = partition(&threads, now(), true, |_| true);
        assert_eq!(ids(&sections.active), ["a-unsettled", "a-run", "a-old"]);
        assert_eq!(ids(&sections.pinned), ["p-a", "p-b", "p-new", "p-old"]);
        assert_eq!(ids(&sections.settled), ["s-msg", "s-at"]);
        assert_eq!(ids(&sections.snoozed), ["z-soon", "z-late"]);
        assert_eq!(ids(&sections.working), ["w-run", "w-msg"]);
    }

    #[test]
    fn status_unread_and_recede() {
        let requests = HashMap::new();
        let read = thread(
            "read",
            json!({"latestRunId": "r", "status": "completed", "latestRunCompletedAt": "2026-11-02T00:00:00.000Z", "lastVisitedAt": "2026-11-03T00:00:00.000Z"}),
        );
        let unread = thread(
            "unread",
            json!({"latestRunId": "r", "status": "completed", "latestRunCompletedAt": "2026-11-03T00:00:00.000Z", "lastVisitedAt": "2026-11-02T00:00:00.000Z"}),
        );
        let never = thread(
            "never",
            json!({"latestRunId": "r", "status": "completed", "latestRunCompletedAt": "2026-11-03T00:00:00.000Z"}),
        );
        let waiting = thread("waiting", json!({"latestRunId": "r", "status": "running", "blockedOnThreadWait": true}));
        let limited =
            thread("limited", json!({"latestRunId": "r", "status": "failed", "lastErrorClass": "usage_limit"}));
        let woke = thread(
            "woke",
            json!({"snoozedUntil": "2026-11-04T11:00:00.000Z", "lastVisitedAt": "2026-11-04T10:00:00.000Z"}),
        );
        let [read, unread, never, waiting, limited, woke] =
            [&read, &unread, &never, &waiting, &limited, &woke].map(|t| Facts::new(t, &requests));
        assert_eq!(read.status(), Status::Ready);
        assert!(!read.is_unread() && unread.is_unread() && !never.is_unread());
        assert_eq!(waiting.status(), Status::Waiting);
        assert_eq!(limited.status(), Status::Limited);
        assert!(woke.is_woke(now()));
        assert!(recedes(Status::Ready, false, false, false));
        assert!(!recedes(Status::Ready, true, false, false));
        assert!(!recedes(Status::Ready, false, false, true));
        assert!(recedes(Status::Working, false, false, false));
        assert!(!recedes(Status::Failed, false, false, false));
    }

    #[test]
    fn working_duration_starts_at_the_activity_run() {
        let requests = HashMap::new();
        let activity = thread(
            "a",
            json!({"latestRunId": "r", "status": "running", "activityRunStartedAt": "2026-11-04T11:25:00.000Z"}),
        );
        let run = thread(
            "b",
            json!({"latestRunId": "r", "activeRunId": "r", "status": "running", "latestRunCompletedAt": null, "latestRunStartedAt": "2026-11-04T11:00:00.000Z"}),
        );
        assert_eq!(Facts::new(&activity, &requests).working_started_at(), parse_ms("2026-11-04T11:25:00.000Z"));
        assert_eq!(Facts::new(&run, &requests).working_started_at(), parse_ms("2026-11-04T11:00:00.000Z"));
    }
}
