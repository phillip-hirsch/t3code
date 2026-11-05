//! One thread's terminal drawer: its terminals, how they group into split
//! panes, and which one is active. Ports `apps/web/src/terminalUiStateStore.ts`.

use std::collections::HashSet;

pub(super) const DEFAULT_HEIGHT: f32 = 280.;
pub(super) const MAX_PER_GROUP: usize = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Split {
    /// Side by side.
    #[default]
    Horizontal,
    /// Stacked.
    Vertical,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Group {
    pub id: String,
    pub terminal_ids: Vec<String>,
    pub split: Split,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Place {
    New,
    Split(Split),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct DrawerLayout {
    pub open: bool,
    pub height: f32,
    pub terminal_ids: Vec<String>,
    pub active_id: String,
    pub groups: Vec<Group>,
    pub active_group_id: String,
    /// Closed here; a lagging server list must not bring them back.
    suppressed: HashSet<String>,
}

impl Default for DrawerLayout {
    fn default() -> Self {
        Self {
            open: false,
            height: DEFAULT_HEIGHT,
            terminal_ids: Vec::new(),
            active_id: String::new(),
            groups: Vec::new(),
            active_group_id: String::new(),
            suppressed: HashSet::new(),
        }
    }
}

fn group_id_for(terminal_id: &str, groups: &[Group]) -> String {
    let base = format!("group-{terminal_id}");
    let mut candidate = base.clone();
    let mut index = 2;
    while groups.iter().any(|group| group.id == candidate) {
        candidate = format!("{base}-{index}");
        index += 1;
    }
    candidate
}

impl DrawerLayout {
    pub fn active_group(&self) -> Option<&Group> {
        self.groups
            .iter()
            .find(|group| group.id == self.active_group_id)
            .or_else(|| self.groups.iter().find(|group| group.terminal_ids.contains(&self.active_id)))
    }

    pub fn split_limit_reached(&self) -> bool {
        self.active_group().is_some_and(|group| group.terminal_ids.len() >= MAX_PER_GROUP)
    }

    /// Groups in the order of their first terminal, as the drawer's sidebar lists them.
    pub fn sorted_groups(&self) -> Vec<&Group> {
        let rank = |group: &Group| {
            group
                .terminal_ids
                .iter()
                .filter_map(|id| self.terminal_ids.iter().position(|other| other == id))
                .min()
                .unwrap_or(usize::MAX)
        };
        let mut groups: Vec<&Group> = self.groups.iter().collect();
        groups.sort_by_key(|group| rank(group));
        groups
    }

    /// Adds `terminal_id` as a new group or as a split of the active group, and
    /// makes it active. A split into a full group is refused.
    pub fn place(&mut self, terminal_id: &str, place: Place) -> bool {
        let terminal_id = terminal_id.trim();
        if terminal_id.is_empty() {
            return false;
        }
        self.suppressed.remove(terminal_id);
        let place = if self.terminal_ids.is_empty() { Place::New } else { place };
        let is_new = !self.terminal_ids.iter().any(|id| id == terminal_id);
        if let Place::Split(_) = place {
            if is_new
                && self.active_group().is_some_and(|group| {
                    group.terminal_ids.len() >= MAX_PER_GROUP && !group.terminal_ids.iter().any(|id| id == terminal_id)
                })
            {
                return false;
            }
        }
        if is_new {
            self.terminal_ids.push(terminal_id.to_owned());
        }
        let anchor_group = self.active_group().map(|group| group.id.clone());
        for group in &mut self.groups {
            group.terminal_ids.retain(|id| id != terminal_id);
        }
        self.groups.retain(|group| !group.terminal_ids.is_empty());
        match place {
            Place::New => {
                let id = group_id_for(terminal_id, &self.groups);
                self.groups.push(Group {
                    id: id.clone(),
                    terminal_ids: vec![terminal_id.to_owned()],
                    split: Split::Horizontal,
                });
                self.active_group_id = id;
            }
            Place::Split(split) => {
                let index = match anchor_group.and_then(|id| self.groups.iter().position(|group| group.id == id)) {
                    Some(index) => index,
                    None => {
                        let id = group_id_for(&self.active_id, &self.groups);
                        self.groups.push(Group {
                            id,
                            terminal_ids: vec![self.active_id.clone()],
                            split: Split::Horizontal,
                        });
                        self.groups.len() - 1
                    }
                };
                let group = &mut self.groups[index];
                let at = group
                    .terminal_ids
                    .iter()
                    .position(|id| *id == self.active_id)
                    .map_or(group.terminal_ids.len(), |i| i + 1);
                group.terminal_ids.insert(at, terminal_id.to_owned());
                group.split = split;
                self.active_group_id = group.id.clone();
            }
        }
        self.active_id = terminal_id.to_owned();
        self.open = true;
        true
    }

    /// Shows `terminal_id`, adding it as its own group if this drawer lacks it.
    pub fn ensure(&mut self, terminal_id: &str) {
        if !self.terminal_ids.iter().any(|id| id == terminal_id) {
            self.place(terminal_id, Place::New);
        }
        self.set_active(terminal_id);
        self.open = true;
    }

    pub fn set_active(&mut self, terminal_id: &str) {
        let Some(group) = self.groups.iter().find(|group| group.terminal_ids.iter().any(|id| id == terminal_id)) else {
            return;
        };
        self.active_group_id = group.id.clone();
        self.active_id = terminal_id.to_owned();
    }

    pub fn close(&mut self, terminal_id: &str) {
        self.suppressed.insert(terminal_id.to_owned());
        let Some(index) = self.terminal_ids.iter().position(|id| id == terminal_id) else { return };
        self.terminal_ids.remove(index);
        if self.terminal_ids.is_empty() {
            *self = Self { height: self.height, suppressed: std::mem::take(&mut self.suppressed), ..Self::default() };
            return;
        }
        if self.active_id == terminal_id {
            let siblings = self
                .groups
                .iter()
                .find(|group| group.terminal_ids.iter().any(|id| id == terminal_id))
                .map(|group| group.terminal_ids.clone())
                .unwrap_or_default();
            let at = siblings.iter().position(|id| id == terminal_id).unwrap_or(0);
            self.active_id = siblings
                .get(at + 1)
                .or_else(|| at.checked_sub(1).and_then(|i| siblings.get(i)))
                .or_else(|| self.terminal_ids.get(index.min(self.terminal_ids.len() - 1)))
                .cloned()
                .unwrap_or_default();
        }
        for group in &mut self.groups {
            group.terminal_ids.retain(|id| id != terminal_id);
        }
        self.groups.retain(|group| !group.terminal_ids.is_empty());
        self.fix_active_group();
    }

    /// Adopts the server's terminal list. A list that only lags this drawer's
    /// (a strict subset) is ignored, so a split still opening is not dropped.
    pub fn reconcile(&mut self, server_ids: &[String]) {
        let mut sorted_server: Vec<&String> = server_ids.iter().collect();
        let mut sorted_client: Vec<&String> = self.terminal_ids.iter().collect();
        sorted_server.sort();
        sorted_client.sort();
        if sorted_server == sorted_client {
            return;
        }
        if server_ids.len() < self.terminal_ids.len() && server_ids.iter().all(|id| self.terminal_ids.contains(id)) {
            return;
        }
        let next: Vec<String> = server_ids.iter().filter(|id| !self.suppressed.contains(*id)).cloned().collect();
        if next == self.terminal_ids {
            return;
        }
        for group in &mut self.groups {
            group.terminal_ids.retain(|id| next.contains(id));
        }
        self.groups.retain(|group| !group.terminal_ids.is_empty());
        for id in &next {
            if !self.groups.iter().any(|group| group.terminal_ids.contains(id)) {
                let group_id = group_id_for(id, &self.groups);
                self.groups.push(Group { id: group_id, terminal_ids: vec![id.clone()], split: Split::Horizontal });
            }
        }
        if !next.contains(&self.active_id) {
            self.active_id = next.first().cloned().unwrap_or_default();
        }
        self.terminal_ids = next;
        self.fix_active_group();
    }

    fn fix_active_group(&mut self) {
        if let Some(group) = self.groups.iter().find(|group| group.terminal_ids.contains(&self.active_id)) {
            self.active_group_id = group.id.clone();
        } else {
            self.active_group_id = self.groups.first().map(|group| group.id.clone()).unwrap_or_default();
        }
    }
}

/// N in `term-N` / `terminal-N`, optionally followed by a UUID suffix
/// (`packages/shared/src/terminalLabels.ts`).
fn terminal_number(terminal_id: &str) -> Option<&str> {
    let rest = terminal_id.strip_prefix("terminal-").or_else(|| terminal_id.strip_prefix("term-"))?;
    let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let (number, suffix) = rest.split_at(digits);
    let uuid = suffix.strip_prefix('-').is_some_and(|s| uuid::Uuid::try_parse(s).is_ok() && s.len() == 36);
    (!number.is_empty() && (suffix.is_empty() || uuid)).then_some(number)
}

/// `getTerminalLabel`, under a summary's own `label` when it has one.
pub(super) fn terminal_label(terminal_id: &str, summary_label: Option<&str>) -> String {
    if let Some(label) = summary_label.map(str::trim).filter(|label| !label.is_empty()) {
        return label.to_owned();
    }
    match terminal_number(terminal_id) {
        Some(number) => format!("Terminal {number}"),
        None => terminal_id.to_owned(),
    }
}

/// `nextTerminalId`: the lowest free `term-N`. Without the server's list a UUID
/// suffix keeps it from joining a session this client cannot see.
pub(super) fn next_terminal_id<'a>(existing: impl IntoIterator<Item = &'a str>, unique_suffix: Option<&str>) -> String {
    let used: HashSet<&str> = existing.into_iter().filter_map(terminal_number).collect();
    let next = (1..).find(|n: &usize| !used.contains(n.to_string().as_str())).unwrap_or(1);
    match unique_suffix {
        Some(suffix) => format!("term-{next}-{suffix}"),
        None => format!("term-{next}"),
    }
}

/// `Intl.Collator(undefined, {numeric: true})` for terminal ids: `term-2` before `term-10`.
pub(super) fn numeric_order(left: &str, right: &str) -> std::cmp::Ordering {
    fn chunks(id: &str) -> Vec<Result<u64, &str>> {
        let mut out = Vec::new();
        let mut rest = id;
        while let Some(first) = rest.chars().next() {
            let digit = first.is_ascii_digit();
            let end = rest.find(|c: char| c.is_ascii_digit() != digit).unwrap_or(rest.len());
            let (chunk, tail) = rest.split_at(end);
            out.push(if digit { chunk.parse().map_err(|_| chunk) } else { Err(chunk) });
            rest = tail;
        }
        out
    }
    chunks(left).cmp(&chunks(right))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_ids_and_labels() {
        assert_eq!(terminal_label("term-3", None), "Terminal 3");
        assert_eq!(terminal_label("term-3", Some("  node  ")), "node");
        assert_eq!(terminal_label("term-2-0b6f0d9e-3c43-4a61-9f6e-0e4c41f0a1b2", Some("")), "Terminal 2");
        assert_eq!(terminal_label("setup", None), "setup");
        assert_eq!(next_terminal_id(["term-1", "term-3"], None), "term-2");
        assert_eq!(next_terminal_id([], Some("abc")), "term-1-abc");
        let mut ids = vec!["term-10", "term-2", "term-1"];
        ids.sort_by(|a, b| numeric_order(a, b));
        assert_eq!(ids, ["term-1", "term-2", "term-10"]);
    }

    fn ids(layout: &DrawerLayout) -> Vec<Vec<&str>> {
        layout.groups.iter().map(|group| group.terminal_ids.iter().map(String::as_str).collect()).collect()
    }

    #[test]
    fn splits_after_the_active_terminal_and_stops_at_four_per_group() {
        let mut layout = DrawerLayout::default();
        assert!(layout.place("term-1", Place::Split(Split::Vertical)));
        assert_eq!(ids(&layout), [["term-1"]]);
        assert!(layout.open);
        layout.place("term-2", Place::Split(Split::Vertical));
        layout.set_active("term-1");
        layout.place("term-3", Place::Split(Split::Vertical));
        assert_eq!(ids(&layout), [["term-1", "term-3", "term-2"]]);
        assert_eq!(layout.groups[0].split, Split::Vertical);
        layout.place("term-4", Place::Split(Split::Horizontal));
        assert_eq!(layout.groups[0].split, Split::Horizontal);
        assert!(layout.split_limit_reached());
        assert!(!layout.place("term-5", Place::Split(Split::Horizontal)));
        assert!(layout.place("term-5", Place::New));
        assert_eq!(layout.groups.len(), 2);
        assert_eq!(layout.active_group_id, "group-term-5");
    }

    #[test]
    fn close_activates_the_next_sibling_then_resets_when_empty() {
        let mut layout = DrawerLayout { height: 400., ..DrawerLayout::default() };
        layout.place("term-1", Place::New);
        layout.place("term-2", Place::Split(Split::Horizontal));
        layout.place("term-3", Place::New);
        layout.set_active("term-1");
        layout.close("term-1");
        assert_eq!(layout.active_id, "term-2");
        assert_eq!(ids(&layout), [vec!["term-2"], vec!["term-3"]]);
        layout.close("term-2");
        layout.close("term-3");
        assert!(!layout.open);
        assert!(layout.terminal_ids.is_empty());
        assert_eq!(layout.height, 400.);
    }

    #[test]
    fn reconcile_adopts_server_terminals_but_not_a_lagging_subset_or_closed_ones() {
        let mut layout = DrawerLayout::default();
        layout.place("term-1", Place::New);
        layout.place("term-2", Place::Split(Split::Horizontal));
        // The server has not seen term-2 open yet.
        layout.reconcile(&["term-1".into()]);
        assert_eq!(ids(&layout), [["term-1", "term-2"]]);
        layout.close("term-2");
        layout.reconcile(&["term-1".into(), "term-2".into(), "term-3".into()]);
        assert_eq!(layout.terminal_ids, ["term-1", "term-3"]);
        assert_eq!(ids(&layout), [vec!["term-1"], vec!["term-3"]]);
        // Opening a closed id again lifts its suppression.
        layout.place("term-2", Place::New);
        layout.reconcile(&["term-1".into(), "term-2".into(), "term-3".into()]);
        assert_eq!(layout.terminal_ids, ["term-1", "term-3", "term-2"]);
    }

    #[test]
    fn sidebar_orders_groups_by_their_first_terminal() {
        let mut layout = DrawerLayout::default();
        layout.place("term-1", Place::New);
        layout.place("term-2", Place::New);
        layout.place("term-3", Place::New);
        // The server lists term-1 last, and its order wins.
        layout.reconcile(&["term-2".into(), "term-3".into(), "term-1".into(), "term-4".into()]);
        let order: Vec<&str> = layout.sorted_groups().iter().map(|group| group.id.as_str()).collect();
        assert_eq!(order, ["group-term-2", "group-term-3", "group-term-1", "group-term-4"]);
    }
}
