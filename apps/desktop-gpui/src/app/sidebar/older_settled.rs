//! The Settled shelf's older threads (`apps/web/src/state/olderSettledShelf.ts`): pages read
//! below each environment's cut while the shelf lists fewer rows than it shows, the counts kept
//! current as rows change, and the family of an opened thread the shell left out.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui_kit::{Context, SharedString, Task};
use serde::Deserialize as _;
use serde_json::{Value, json};

use crate::app::Workspace;
use crate::model::{ScopedId, ShellState, ThreadRef, ThreadRows};
use crate::older_settled::{self, Cursor, PAGE_SIZE, Pages};

/// A burst of changes (a PR sync touching many old threads) costs one count read.
const COUNT_REREAD_DELAY: Duration = Duration::from_millis(500);

/// One environment and the sidebar filter's projects there, or `None` for every project.
pub(super) type Scope = (SharedString, Option<Vec<String>>);

#[derive(Default)]
pub(super) struct Shelf {
    pages: HashMap<SharedString, Pages>,
    /// The page read in flight per environment; replacing or dropping it cancels the read.
    reads: HashMap<SharedString, Task<()>>,
    /// The count read waiting or in flight per environment, with the key it reads for.
    counts: HashMap<SharedString, (u64, Task<()>)>,
}

/// The shelf's scope on each environment whose shell leaves older settled threads out: the
/// filter's projects there, skipping an environment it keeps none of.
pub(super) fn scopes(shell: &ShellState, scoped: Option<&HashSet<ThreadRef>>) -> Vec<Scope> {
    shell
        .older_settled_cuts()
        .filter_map(|(env, _)| {
            let Some(scoped) = scoped else { return Some((env.clone(), None)) };
            let mut projects: Vec<String> =
                scoped.iter().filter(|(e, _)| *e == env.as_ref()).map(|(_, id)| (*id).to_owned()).collect();
            projects.sort_unstable();
            (!projects.is_empty()).then(|| (env.clone(), Some(projects)))
        })
        .collect()
}

impl Shelf {
    /// Follows the scope and each environment's latest snapshot; a reset cancels its read.
    pub(super) fn follow(&mut self, shell: &ShellState, scopes: &[Scope]) {
        self.pages.retain(|env, _| scopes.iter().any(|(e, _)| e == env));
        for (env, scope) in scopes {
            let Some(cut) = shell.older_settled(env) else { continue };
            let generation = shell.generation(env);
            match self.pages.get_mut(env) {
                Some(pages) => {
                    if pages.follow(generation, scope.clone(), cut) {
                        self.reads.remove(env);
                    }
                }
                None => {
                    self.pages.insert(env.clone(), Pages::new(generation, scope.clone(), cut));
                }
            }
        }
        let pages = &self.pages;
        self.reads.retain(|env, _| pages.contains_key(env));
    }

    pub(super) fn frontier(&self) -> Option<&Cursor> {
        older_settled::shelf_frontier(self.pages.values())
    }

    /// Asks for one more page where the shelf lists fewer rows than it shows.
    pub(super) fn grow(&mut self, listed: usize, visible: usize) {
        older_settled::grow(self.pages.values_mut(), listed, visible);
    }

    pub(super) fn loading(&self) -> bool {
        self.pages.values().any(Pages::is_loading)
    }

    pub(super) fn failed(&self) -> bool {
        self.pages.values().any(Pages::is_failed)
    }

    /// Reads failed pages again (a no-op when none failed).
    pub(super) fn retry(&mut self) {
        self.pages.values_mut().for_each(Pages::retry);
    }

    /// The pages to read now, each marked pending.
    fn next_reads(&mut self) -> Vec<PageRead> {
        self.pages
            .iter_mut()
            .filter_map(|(env, pages)| {
                let from = pages.next_read()?;
                Some(PageRead {
                    env: env.clone(),
                    generation: pages.generation(),
                    projects: pages.scope().map(<[String]>::to_vec),
                    from,
                })
            })
            .collect()
    }
}

struct PageRead {
    env: SharedString,
    generation: u64,
    projects: Option<Vec<String>>,
    from: Cursor,
}

impl PageRead {
    /// `orchestration.getOlderSettledThreads` for one page below `from`.
    fn input(&self) -> Value {
        let mut input = json!({"before": self.from.to_json(), "limit": PAGE_SIZE});
        if let Some(projects) = &self.projects {
            input["projectIds"] = json!(projects);
        }
        input
    }
}

impl Workspace {
    /// Starts the page reads the shelf asked for during this render.
    pub(super) fn read_older_settled_pages(&mut self, cx: &mut Context<Self>) {
        for read in self.sidebar.older.next_reads() {
            let Some(client) = self.env_client(&read.env) else {
                if let Some(pages) = self.sidebar.older.pages.get_mut(&read.env) {
                    pages.fail(read.generation, &read.from);
                }
                continue;
            };
            let env = read.env.clone();
            let task = cx.spawn(async move |this, cx| {
                let result = client.request("orchestration.getOlderSettledThreads", read.input()).await;
                let _ = this.update(cx, |this, cx| this.land_older_settled_page(&read, result, cx));
            });
            self.sidebar.older.reads.insert(env, task);
        }
    }

    fn land_older_settled_page(
        &mut self,
        read: &PageRead,
        result: Result<Value, crate::rpc::RpcError>,
        cx: &mut Context<Self>,
    ) {
        let Some(pages) = self.sidebar.older.pages.get_mut(&read.env) else { return };
        match result {
            Ok(value) => {
                let rows = value["threads"].as_array();
                let last = rows.and_then(|rows| rows.last()).and_then(Cursor::after);
                if pages.land(read.generation, &read.from, rows.map_or(0, Vec::len), last) {
                    let rows = ThreadRows::deserialize(&value).unwrap_or_default();
                    self.shell.mount_rows(&read.env, read.generation, rows);
                }
            }
            Err(error) => {
                log::warn!("orchestration.getOlderSettledThreads: {error}");
                pages.fail(read.generation, &read.from);
            }
        }
        self.notify_sidebar(cx);
    }

    /// "Show N more", or Retry after a failed page; nothing while a page loads.
    pub(super) fn show_more_settled(&mut self, cx: &mut Context<Self>) {
        if self.sidebar.older.loading() {
            return;
        }
        self.sidebar.older.retry();
        self.sidebar.settled_visible += super::SETTLED_PAGE;
        self.notify_sidebar(cx);
    }

    /// Reads an environment's older settled counts again once its rows changed in a way that
    /// can move them (`olderSettledCountKey`), after a short wait that a burst shares.
    pub(in crate::app) fn sync_older_settled_counts(&mut self, env: &SharedString, cx: &mut Context<Self>) {
        let key = self.shell.older_settled_count_key(env);
        let Some((key, cut)) = key.zip(self.shell.older_settled(env)) else {
            self.sidebar.older.counts.remove(env);
            return;
        };
        if cut.counts_key == Some(key) {
            self.sidebar.older.counts.remove(env);
            return;
        }
        if self.sidebar.older.counts.get(env).is_some_and(|(read, _)| *read == key) {
            return;
        }
        let Some(client) = self.env_client(env) else { return };
        let generation = self.shell.generation(env);
        let input = json!({"before": {"settledAt": cut.before}, "limit": 0});
        let id = env.clone();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COUNT_REREAD_DELAY).await;
            let result = client.request("orchestration.getOlderSettledThreads", input).await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(value) => {
                    let counts = older_settled::counts_from(&value["counts"]);
                    this.shell.set_older_settled_counts(&id, generation, key, counts);
                    this.notify_sidebar(cx);
                }
                Err(error) => log::warn!("orchestration.getOlderSettledThreads counts: {error}"),
            });
        });
        self.sidebar.older.counts.insert(env.clone(), (key, task));
    }

    /// An opened thread the shell left out (an older settled one) gets its row from its family
    /// (`threadFamilies.mount`), so its header and sidebar row know it.
    pub(in crate::app) fn mount_thread_family(&mut self, env: &SharedString, thread_id: &str, cx: &mut Context<Self>) {
        let key = ScopedId::new(env.clone(), thread_id);
        if self.shell.older_settled(env).is_none() || self.shell.thread(&key).is_some() {
            return;
        }
        let Some(client) = self.env_client(env) else { return };
        let generation = self.shell.generation(env);
        cx.spawn(async move |this, cx| {
            let result = client.request("orchestration.getThreadFamily", json!({"threadId": key.id})).await;
            let rows = result
                .map_err(|error| error.to_string())
                .and_then(|value| ThreadRows::deserialize(&value).map_err(|error| error.to_string()));
            let rows = match rows {
                Ok(rows) => rows,
                Err(error) => {
                    log::warn!("orchestration.getThreadFamily: {error}");
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                if this.shell.mount_rows(&key.env, generation, rows) {
                    this.notify_sidebar(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

/// The filter's projects by environment, sorted, for a search's `projectIds`.
pub(super) fn search_projects(scoped: &HashSet<ScopedId>) -> HashMap<SharedString, Vec<String>> {
    let mut projects: HashMap<SharedString, Vec<String>> = HashMap::new();
    for key in scoped {
        projects.entry(key.env.clone()).or_default().push(key.id.clone());
    }
    for ids in projects.values_mut() {
        ids.sort_unstable();
    }
    projects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shelf_scope_follows_the_filter_on_each_environment_with_a_cut() {
        let mut shell = ShellState::default();
        let cut = json!({"before": "2024-02-01T00:00:00.000Z", "asOfSequence": 1, "counts": []});
        for (env, older) in [("a", Some(cut.clone())), ("b", Some(cut)), ("c", None)] {
            let mut snapshot = json!({"snapshotSequence": 1, "projects": [], "threads": []});
            if let Some(older) = older {
                snapshot["olderSettled"] = older;
            }
            shell.apply(&env.into(), &json!({"kind": "snapshot", "snapshot": snapshot})).unwrap();
        }
        let mut all = scopes(&shell, None);
        all.sort_by_key(|(env, _)| env.to_string());
        let named = |env: &str, projects: Option<&[&str]>| -> Scope {
            (env.into(), projects.map(|ids| ids.iter().map(|id| (*id).to_owned()).collect()))
        };
        assert_eq!(all, [named("a", None), named("b", None)]);
        let scoped = HashSet::from([("a", "p2"), ("a", "p1"), ("c", "p3")]);
        assert_eq!(scopes(&shell, Some(&scoped)), [named("a", Some(&["p1", "p2"]))]);

        let mut shelf = Shelf::default();
        shelf.follow(&shell, &all);
        shelf.grow(0, 10);
        let reads: Vec<Value> = shelf.next_reads().iter().map(PageRead::input).collect();
        assert_eq!(reads, vec![json!({"before": {"settledAt": "2024-02-01T00:00:00.000Z"}, "limit": 25}); 2]);
        assert!(shelf.loading());
        // Narrowing the filter starts the remaining environment over and drops the other.
        shelf.follow(&shell, &scopes(&shell, Some(&scoped)));
        assert_eq!(shelf.pages.len(), 1);
        assert!(!shelf.loading());
        shelf.grow(0, 10);
        let input = shelf.next_reads().pop().map(|read| read.input());
        assert_eq!(input.unwrap()["projectIds"], json!(["p1", "p2"]));
    }
}
