//! Where each timeline was left (`rememberTimelinePosition`, `timelineListStateFor`): the row
//! and offset at the top of the viewport, whether it followed the end, and its open folds and
//! entries, so a thread opens again as it was.

use std::collections::{HashSet, VecDeque};

use gpui_kit::{Context, ListOffset, SharedString};

use super::{Target, Workspace};

/// The web keeps this many threads' positions.
const LIMIT: usize = 100;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Remembered {
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
        let position = Remembered {
            top: scroller.logical_scroll_top(),
            at_end: scroller.is_following_tail(),
            expanded: self.expanded.clone(),
        };
        self.positions.remember(target, position);
    }

    /// The open target's folds as it was left, and where to scroll once its rows arrive.
    pub(super) fn recall_position(&mut self) {
        let remembered = self.target.as_ref().and_then(|target| self.positions.get(target)).cloned();
        self.pending_position = remembered.as_ref().filter(|position| !position.at_end).map(|position| position.top);
        if let Some(remembered) = remembered {
            self.expanded = remembered.expanded;
        }
    }

    /// Scrolls a timeline that just got its rows to where it was left.
    pub(super) fn restore_position(&mut self, cx: &mut Context<Self>) {
        let Some(top) = self.pending_position.take() else { return };
        let Some(last) = self.rows.len().checked_sub(1) else { return };
        let top = restored_top(top, last);
        self.scroller.update(cx, |scroller, cx| scroller.scroll_to_offset(top, cx));
    }
}

/// A row past the end (the timeline lost rows meanwhile) clamps to its last row's top.
fn restored_top(top: ListOffset, last: usize) -> ListOffset {
    if top.item_ix <= last { top } else { ListOffset { item_ix: last, offset_in_item: gpui_kit::px(0.) } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::px;

    fn thread(id: &str) -> Target {
        Target::Thread { env: "local".into(), id: id.to_owned() }
    }

    fn at(row: usize, offset: f32, at_end: bool) -> Remembered {
        Remembered { top: ListOffset { item_ix: row, offset_in_item: px(offset) }, at_end, expanded: HashSet::new() }
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
    fn a_row_past_the_end_restores_to_the_last_row() {
        let top = ListOffset { item_ix: 9, offset_in_item: px(30.) };
        assert_eq!(restored_top(top, 12), top);
        assert_eq!(restored_top(top, 4), ListOffset { item_ix: 4, offset_in_item: px(0.) });
    }
}
