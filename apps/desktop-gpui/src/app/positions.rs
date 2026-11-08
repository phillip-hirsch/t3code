//! Where each timeline was left (`rememberTimelinePosition`, `timelineListStateFor`): the row
//! and offset at the top of the viewport, whether it followed the end, and its open folds and
//! entries, so a thread opens again as it was. The row is kept by identity, since a turn that
//! finishes in the background can add or fold rows above it.

use std::collections::{HashSet, VecDeque};

use gpui_kit::{Context, ListOffset, SharedString};

use super::{Target, Workspace};
use crate::timeline::Row;

/// The web keeps this many threads' positions.
const LIMIT: usize = 100;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Remembered {
    /// The top row's [`Row::key`].
    pub row: Option<String>,
    pub top: ListOffset,
    pub at_end: bool,
    pub expanded: HashSet<SharedString>,
}

/// Positions by timeline, the least recently left first.
#[derive(Default)]
pub(super) struct Positions(VecDeque<(Target, Remembered)>);

impl Positions {
    fn remember(&mut self, target: Target, position: Remembered) {
        self.0.retain(|(known, _)| *known != target);
        self.0.push_back((target, position));
        if self.0.len() > LIMIT {
            self.0.pop_front();
        }
    }

    fn get(&self, target: &Target) -> Option<&Remembered> {
        self.0.iter().find(|(known, _)| known == target).map(|(_, position)| position)
    }
}

impl Workspace {
    /// Remembers where the open timeline is, before another one replaces it.
    pub(super) fn remember_position(&mut self, cx: &Context<Self>) {
        let Some(target) = self.target.clone() else { return };
        if self.rows.is_empty() {
            return;
        }
        let scroller = self.scroller.read(cx);
        let top = scroller.logical_scroll_top();
        let position = Remembered {
            row: self.rows.get(top.item_ix).map(Row::key),
            top,
            at_end: scroller.is_following_tail(),
            expanded: self.expanded.clone(),
        };
        self.positions.remember(target, position);
    }

    /// The open target's folds as it was left, and where to scroll once its rows arrive.
    pub(super) fn recall_position(&mut self) {
        let remembered = self.target.as_ref().and_then(|target| self.positions.get(target)).cloned();
        if let Some(remembered) = remembered {
            self.expanded = remembered.expanded.clone();
            self.pending_position = (!remembered.at_end).then_some(remembered);
        } else {
            self.pending_position = None;
        }
    }

    /// Scrolls a timeline that just got its rows to where it was left.
    pub(super) fn restore_position(&mut self, cx: &mut Context<Self>) {
        let Some(remembered) = self.pending_position.take() else { return };
        let Some(top) = restored_top(&remembered, &self.rows) else { return };
        self.scroller.update(cx, |scroller, cx| scroller.scroll_to_offset(top, cx));
    }
}

/// The remembered row where it is now; a row that is gone falls back to its old index, and one
/// past the end (the timeline lost rows meanwhile) to the last row's top.
fn restored_top(remembered: &Remembered, rows: &[Row]) -> Option<ListOffset> {
    let last = rows.len().checked_sub(1)?;
    let found = remembered.row.as_ref().and_then(|key| rows.iter().position(|row| row.key() == *key));
    let top = remembered.top;
    Some(match found {
        Some(item_ix) => ListOffset { item_ix, ..top },
        None if top.item_ix <= last => top,
        None => ListOffset { item_ix: last, offset_in_item: gpui_kit::px(0.) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::px;

    fn thread(id: &str) -> Target {
        Target::Thread { env: "local".into(), id: id.to_owned() }
    }

    fn at(row: usize, offset: f32, at_end: bool) -> Remembered {
        Remembered {
            row: None,
            top: ListOffset { item_ix: row, offset_in_item: px(offset) },
            at_end,
            expanded: HashSet::new(),
        }
    }

    fn event(id: &str) -> Row {
        Row::Event { id: id.into(), label: id.into() }
    }

    #[test]
    fn positions_are_kept_per_timeline_and_the_oldest_drop() {
        let mut positions = Positions::default();
        positions.remember(thread("a"), at(3, 12., false));
        positions.remember(thread("b"), at(0, 0., true));
        positions.remember(thread("a"), at(5, 4., false));
        assert_eq!(positions.get(&thread("a")), Some(&at(5, 4., false)));
        assert_eq!(positions.get(&thread("b")), Some(&at(0, 0., true)));
        assert_eq!(positions.get(&Target::Thread { env: "mac".into(), id: "a".into() }), None);
        for index in 0..LIMIT {
            positions.remember(thread(&format!("t{index}")), at(index, 0., false));
        }
        assert_eq!(positions.0.len(), LIMIT);
        assert_eq!(positions.get(&thread("a")), None);
        assert!(positions.get(&thread("t0")).is_some());
    }

    #[test]
    fn the_remembered_row_restores_where_it_is_now() {
        let rows: Vec<Row> = ["a", "b", "c"].map(event).into();
        let left = Remembered { row: Some(rows[1].key()), ..at(1, 30., false) };
        let grown: Vec<Row> = ["a", "work", "fold", "b", "c"].map(event).into();
        assert_eq!(restored_top(&left, &grown), Some(ListOffset { item_ix: 3, offset_in_item: px(30.) }));
        let folded: Vec<Row> = ["b", "c"].map(event).into();
        assert_eq!(restored_top(&left, &folded), Some(ListOffset { item_ix: 0, offset_in_item: px(30.) }));
        assert_eq!(restored_top(&left, &[]), None);
    }

    #[test]
    fn a_row_that_is_gone_restores_by_index_up_to_the_last_row() {
        let rows: Vec<Row> = (0..13).map(|index| event(&index.to_string())).collect();
        let left = Remembered { row: Some("event:gone".into()), ..at(9, 30., false) };
        assert_eq!(restored_top(&left, &rows), Some(left.top));
        assert_eq!(restored_top(&left, &rows[..5]), Some(ListOffset { item_ix: 4, offset_in_item: px(0.) }));
    }
}
