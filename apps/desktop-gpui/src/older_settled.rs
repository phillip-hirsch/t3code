//! Older settled threads (`packages/client-runtime/src/state/olderSettledThreads.ts`). Shells
//! that omit them (`omitOlderSettled`) name a cut and count, per project, what they left out;
//! the Settled shelf reads them in pages below that cut, newest settle first.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::model::ThreadShell;
use crate::timestamp::parse_ms;

/// `OLDER_SETTLED_PAGE_SIZE`: one "Show more" step of the Settled shelf.
pub const PAGE_SIZE: usize = 25;

/// `OrchestrationV2OlderSettledThreads` as a snapshot sends it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutWire {
    before: String,
    as_of_sequence: u64,
    #[serde(default)]
    counts: Value,
}

/// Where an environment's shell cut settled threads, and how many are older per project.
#[derive(Clone, Debug, PartialEq)]
pub struct Cut {
    pub before: String,
    pub before_ms: i64,
    pub as_of_sequence: u64,
    counts: HashMap<String, usize>,
    /// The [`count_key`] the counts are exact for; `None` until a read says.
    pub counts_key: Option<u64>,
}

impl Cut {
    pub fn from_wire(wire: CutWire) -> Option<Self> {
        Some(Self {
            before_ms: parse_ms(&wire.before)?,
            before: wire.before,
            as_of_sequence: wire.as_of_sequence,
            counts: counts_from(&wire.counts),
            counts_key: None,
        })
    }

    /// Older settled threads of these projects (every project for `None`), loaded or not.
    pub fn count(&self, projects: Option<&[String]>) -> usize {
        match projects {
            None => self.counts.values().sum(),
            Some(projects) => projects.iter().filter_map(|id| self.counts.get(id)).sum(),
        }
    }

    pub fn set_counts(&mut self, counts: HashMap<String, usize>, key: u64) {
        self.counts = counts;
        self.counts_key = Some(key);
    }
}

/// `OrchestrationV2OlderSettledCount[]`, skipping malformed entries.
pub fn counts_from(value: &Value) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for entry in value.as_array().into_iter().flatten() {
        let (Some(project), Some(count)) = (entry["projectId"].as_str(), entry["count"].as_u64()) else { continue };
        *counts.entry(project.to_owned()).or_default() += usize::try_from(count).unwrap_or(usize::MAX);
    }
    counts
}

/// `isOlderSettledThreadShell`: a settled top-level thread settled before `before_ms`.
pub fn is_older_settled(thread: &ThreadShell, before_ms: i64) -> bool {
    thread.is_listed()
        && thread.settled_override.as_deref() == Some("settled")
        && thread.settled_at.as_deref().and_then(parse_ms).is_some_and(|at| at < before_ms)
}

/// `olderSettledCountKey`: changes whenever the older counts can. Any removal (by its largest
/// sequence), an older row that arrives or changes, or an old row outside the set (unsettled).
/// Rows hash independently of order, so it costs one pass and no sort.
pub fn count_key<'a>(
    threads: impl Iterator<Item = &'a ThreadShell>,
    cut: &Cut,
    removed_at: &HashMap<String, u64>,
) -> u64 {
    let last_removal = removed_at.values().copied().max().unwrap_or(0);
    let mut rows = 0_u64;
    for thread in threads.filter(|t| t.is_listed()) {
        let older = is_older_settled(thread, cut.before_ms);
        // Both are the server's ISO strings, which order as their times do.
        if older || thread.created_at.as_str() < cut.before.as_str() {
            let mut hasher = DefaultHasher::new();
            (&thread.id, older).hash(&mut hasher);
            rows ^= hasher.finish();
        }
    }
    let mut hasher = DefaultHasher::new();
    (last_removal, rows).hash(&mut hasher);
    hasher.finish()
}

/// `OrchestrationV2OlderSettledCursor`: below this settle time, then after this id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    settled_at: String,
    settled_ms: i64,
    thread_id: Option<String>,
}

impl Cursor {
    /// The cut itself; a cursor without an id is strict.
    pub fn of_cut(cut: &Cut) -> Self {
        Self { settled_at: cut.before.clone(), settled_ms: cut.before_ms, thread_id: None }
    }

    /// After a page's last row, or `None` when it has no settle time.
    pub fn after(row: &Value) -> Option<Self> {
        let settled_at = row["settledAt"].as_str()?;
        Some(Self {
            settled_ms: parse_ms(settled_at)?,
            settled_at: settled_at.to_owned(),
            thread_id: Some(row["id"].as_str()?.to_owned()),
        })
    }

    pub fn to_json(&self) -> Value {
        match &self.thread_id {
            Some(id) => json!({"settledAt": self.settled_at, "threadId": id}),
            None => json!({"settledAt": self.settled_at}),
        }
    }

    /// Stops a merged list above `other`: a later settle time, on a tie a cursor with an id
    /// over one without, and a smaller id over a larger.
    fn is_above(&self, other: &Self) -> bool {
        self.settled_ms > other.settled_ms
            || (self.settled_ms == other.settled_ms
                && self.thread_id.as_ref().is_some_and(|id| other.thread_id.as_ref().is_none_or(|o| id < o)))
    }
}

/// `isListedSettledThread`: the shelf lists a row at or above its frontier (settle time desc,
/// id asc), so a row that turns older mid-session waits for the range that holds it.
pub fn is_listed(thread: &ThreadShell, frontier: Option<&Cursor>) -> bool {
    let Some(cursor) = frontier else { return true };
    let Some(at) = thread.settled_at.as_deref().and_then(parse_ms) else { return true };
    !(at < cursor.settled_ms
        || (at == cursor.settled_ms && cursor.thread_id.as_ref().is_some_and(|id| thread.id.as_str() > id.as_str())))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageStatus {
    Ready,
    Pending,
    Failed,
}

/// `OlderSettledPages`: one environment's pages below its cut, for one scope and shell
/// generation. Everything at or above `frontier` is held; `None` means nothing more loads.
#[derive(Clone, Debug, PartialEq)]
pub struct Pages {
    generation: u64,
    /// The scope's project ids, sorted, or `None` for every project.
    scope: Option<Vec<String>>,
    before: String,
    /// Pages the shelf asked for; `loaded` of them have arrived.
    requested: usize,
    loaded: usize,
    frontier: Option<Cursor>,
    status: PageStatus,
}

impl Pages {
    pub fn new(generation: u64, scope: Option<Vec<String>>, cut: &Cut) -> Self {
        Self {
            generation,
            scope,
            before: cut.before.clone(),
            requested: 0,
            loaded: 0,
            frontier: Some(Cursor::of_cut(cut)),
            status: PageStatus::Ready,
        }
    }

    /// Follows the shelf's scope and the environment's latest snapshot: a new scope starts
    /// over, a new snapshot reads the same pages again (its rows replaced the old reads).
    pub fn follow(&mut self, generation: u64, scope: Option<Vec<String>>, cut: &Cut) -> bool {
        if self.generation == generation && self.scope == scope && self.before == cut.before {
            return false;
        }
        let requested = if self.scope == scope { self.requested } else { 0 };
        *self = Self { requested, ..Self::new(generation, scope, cut) };
        true
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn scope(&self) -> Option<&[String]> {
        self.scope.as_deref()
    }

    /// Asked-for pages are loading.
    pub fn is_loading(&self) -> bool {
        self.requested > 0 && self.status == PageStatus::Pending
    }

    pub fn is_failed(&self) -> bool {
        self.requested > 0 && self.status == PageStatus::Failed
    }

    /// The next page to read, marking it pending: once per asked-for page, in order.
    pub fn next_read(&mut self) -> Option<Cursor> {
        if self.status != PageStatus::Ready || self.loaded >= self.requested {
            return None;
        }
        let from = self.frontier.clone()?;
        self.status = PageStatus::Pending;
        Some(from)
    }

    /// A page read from `from` arrived with `rows` rows, the last at `last`. False when it no
    /// longer answers this state (another snapshot or scope), so its rows must not merge.
    pub fn land(&mut self, generation: u64, from: &Cursor, rows: usize, last: Option<Cursor>) -> bool {
        if !self.answers(generation, from) {
            return false;
        }
        self.loaded += 1;
        // A short page ends the chain; counts can lag a backdated settle, so they do not gate.
        self.frontier = if rows < PAGE_SIZE { None } else { last };
        self.status = PageStatus::Ready;
        true
    }

    pub fn fail(&mut self, generation: u64, from: &Cursor) {
        if self.answers(generation, from) {
            self.status = PageStatus::Failed;
        }
    }

    /// Reads a failed page again; the pages after it follow.
    pub fn retry(&mut self) {
        if self.status == PageStatus::Failed {
            self.status = PageStatus::Ready;
        }
    }

    fn answers(&self, generation: u64, from: &Cursor) -> bool {
        self.generation == generation && self.status == PageStatus::Pending && self.frontier.as_ref() == Some(from)
    }
}

/// `settledShelfFrontier`: where a shelf over these environments stops listing, the highest
/// frontier of any that can still load. Rows below it could sort under rows not loaded yet. A
/// failed environment holds no one back; its Retry rejoins it.
pub fn shelf_frontier<'a>(pages: impl IntoIterator<Item = &'a Pages>) -> Option<&'a Cursor> {
    pages.into_iter().filter(|p| p.status != PageStatus::Failed).filter_map(|p| p.frontier.as_ref()).fold(
        None,
        |highest, frontier| match highest {
            Some(highest) if !frontier.is_above(highest) => Some(highest),
            _ => Some(frontier),
        },
    )
}

/// `wantedOlderSettledPages`: while the shelf lists fewer than `visible` rows, one more page for
/// each environment whose frontier is the shelf's, once its pages have loaded.
pub fn grow<'a>(pages: impl IntoIterator<Item = &'a mut Pages>, listed: usize, visible: usize) {
    if listed >= visible {
        return;
    }
    let pages: Vec<&mut Pages> = pages.into_iter().collect();
    let Some(frontier) = shelf_frontier(pages.iter().map(|p| &**p)).cloned() else { return };
    for page in pages {
        if page.status == PageStatus::Ready
            && page.loaded == page.requested
            && page.frontier.as_ref() == Some(&frontier)
        {
            page.requested += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cut(before: &str, counts: Value) -> Cut {
        Cut::from_wire(CutWire { before: before.to_owned(), as_of_sequence: 5, counts }).unwrap()
    }

    fn thread(id: &str, settled_at: Option<&str>) -> ThreadShell {
        serde_json::from_value(json!({
            "id": id, "projectId": "p", "title": id, "status": "idle", "updatedAt": "2024-01-01T00:00:00.000Z",
            "createdAt": "2024-01-01T00:00:00.000Z", "lineage": {"rootThreadId": id},
            "settledOverride": settled_at.map(|_| "settled"), "settledAt": settled_at,
        }))
        .unwrap()
    }

    fn row(id: &str, settled_at: &str) -> Value {
        json!({"id": id, "settledAt": settled_at})
    }

    const CUT: &str = "2024-03-01T00:00:00.000Z";

    #[test]
    fn pages_read_in_order_until_a_short_page() {
        let cut = cut(CUT, json!([]));
        let mut pages = Pages::new(1, None, &cut);
        assert_eq!(pages.next_read(), None, "nothing asked for yet");
        grow([&mut pages], 0, 10);
        let first = pages.next_read().unwrap();
        assert_eq!(first.to_json(), json!({"settledAt": CUT}));
        assert_eq!(pages.next_read(), None, "one read at a time");
        assert!(pages.is_loading());
        grow([&mut pages], 0, 10);
        assert_eq!(pages.requested, 1, "a loading environment is not asked again");

        let last = Cursor::after(&row("t25", "2024-02-01T00:00:00.000Z"));
        assert!(pages.land(1, &first, PAGE_SIZE, last.clone()));
        assert_eq!(pages.frontier, last);
        grow([&mut pages], 30, 35);
        let second = pages.next_read().unwrap();
        assert_eq!(second.to_json(), json!({"settledAt": "2024-02-01T00:00:00.000Z", "threadId": "t25"}));
        assert!(pages.land(1, &second, 3, Cursor::after(&row("t28", "2024-01-15T00:00:00.000Z"))));
        assert_eq!((pages.frontier.as_ref(), pages.loaded), (None, 2), "a short page ends the chain");
        grow([&mut pages], 0, 100);
        assert_eq!(pages.next_read(), None);
    }

    #[test]
    fn a_failed_page_retries_and_a_stale_answer_is_dropped() {
        let cut = cut(CUT, json!([]));
        let mut pages = Pages::new(1, None, &cut);
        grow([&mut pages], 0, 10);
        let from = pages.next_read().unwrap();
        pages.fail(1, &from);
        assert!(pages.is_failed() && !pages.is_loading());
        assert_eq!(pages.next_read(), None, "a failure waits for Retry");
        pages.retry();
        assert_eq!(pages.next_read(), Some(from.clone()));

        // A new snapshot reads the same pages again; the old answer no longer lands.
        assert!(pages.follow(2, None, &cut));
        assert!(!pages.land(1, &from, PAGE_SIZE, None));
        assert_eq!((pages.requested, pages.loaded), (1, 0));
        assert_eq!(pages.next_read(), Some(from));
        // A new scope starts over.
        assert!(pages.follow(2, Some(vec!["p".into()]), &cut));
        assert_eq!((pages.requested, pages.status), (0, PageStatus::Ready));
        assert!(!pages.follow(2, Some(vec!["p".into()]), &cut));
    }

    #[test]
    fn the_shelf_lists_down_to_the_highest_frontier_that_can_still_load() {
        let a = cut(CUT, json!([]));
        let b = cut("2024-02-01T00:00:00.000Z", json!([]));
        let mut pa = Pages::new(1, None, &a);
        let mut pb = Pages::new(1, None, &b);
        assert_eq!(shelf_frontier([&pa, &pb]), Some(&Cursor::of_cut(&a)));
        // Only the environment at the shelf's frontier reads on.
        grow([&mut pa, &mut pb], 0, 10);
        assert_eq!((pa.requested, pb.requested), (1, 0));
        let from = pa.next_read().unwrap();
        pa.fail(1, &from);
        assert_eq!(shelf_frontier([&pa, &pb]), Some(&Cursor::of_cut(&b)), "a failure holds no one back");
        pa.retry();
        let from = pa.next_read().unwrap();
        assert!(pa.land(1, &from, 2, None));
        assert_eq!(shelf_frontier([&pa, &pb]), Some(&Cursor::of_cut(&b)));

        let frontier = Cursor::after(&row("m", "2024-01-10T00:00:00.000Z"));
        let listed = |id: &str, at: &str| is_listed(&thread(id, Some(at)), frontier.as_ref());
        assert!(listed("a", "2024-01-11T00:00:00.000Z"));
        assert!(listed("a", "2024-01-10T00:00:00.000Z"), "a tie with a smaller id sorts above");
        assert!(listed("m", "2024-01-10T00:00:00.000Z"));
        assert!(!listed("z", "2024-01-10T00:00:00.000Z"));
        assert!(!listed("a", "2024-01-09T00:00:00.000Z"));
        // On a tie, a cursor with an id stops above one without.
        let bare = Cursor { thread_id: None, ..frontier.clone().unwrap() };
        assert!(frontier.as_ref().unwrap().is_above(&bare) && !bare.is_above(frontier.as_ref().unwrap()));
    }

    #[test]
    fn counts_and_their_key_follow_the_rows_that_can_change_them() {
        let cut = cut(CUT, json!([{"projectId": "p", "count": 4}, {"projectId": "q", "count": 2}, {"bad": 1}]));
        assert_eq!((cut.count(None), cut.count(Some(&["q".into(), "x".into()]))), (6, 2));
        let older = thread("o", Some("2024-02-01T00:00:00.000Z"));
        let newer = thread("n", Some("2024-04-01T00:00:00.000Z"));
        assert!(is_older_settled(&older, cut.before_ms) && !is_older_settled(&newer, cut.before_ms));
        let removed = HashMap::new();
        let key = count_key([&older, &newer].into_iter(), &cut, &removed);
        assert_eq!(key, count_key([&newer, &older].into_iter(), &cut, &removed), "order does not matter");
        let unsettled = thread("o", None);
        assert_ne!(key, count_key([&unsettled, &newer].into_iter(), &cut, &removed));
        assert_ne!(key, count_key([&older, &newer].into_iter(), &cut, &HashMap::from([("x".into(), 9)])));
    }
}
