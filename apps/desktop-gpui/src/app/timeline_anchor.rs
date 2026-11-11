//! A newly sent turn's message anchors near the top while its reply streams in below it
//! (`useTimelineFollow.ts`, `resolveChatListAnchoredEndSpace` in `chatList.ts`,
//! `observeTimelineRun`): end space under the last row lets the list hold the message up there
//! even when little follows it. Only the thread's first user message anchors, as on the web; a
//! later one just returns the timeline to its end.
//!
//! While anchored the list is pinned to the message instead of following its tail, so a reply
//! that grows below it moves nothing, and end space measured a frame late only changes the blank
//! space under the reply.

use gpui_kit::{ListOffset, Pixels, SharedString, px};

use crate::model::{ThreadProjection, is_active_run_status};
use crate::timeline::Row;

/// `CHAT_TIMELINE_ANCHOR_OFFSET`: the message's distance below the viewport's top.
const OFFSET: f32 = 24.;
/// Space within this of the last is the same, so the list settles instead of laying out again.
const EPSILON: f32 = 0.5;
/// Extra space that keeps the pinned list just short of its end, where the list would go back
/// to following its tail.
const HOLD: f32 = 2.;
/// Frames to wait for the message's row to be measured (`positionAnchor(12)`).
const ATTEMPTS: u8 = 12;

/// The anchor for one visit to a thread; a switch starts over.
#[derive(Default)]
pub(super) struct Anchor {
    /// The sent message's id (`timelineAnchor.messageId`).
    message: Option<SharedString>,
    end_space: f32,
    /// Where the list was pinned, once the message was placed.
    pinned: Option<ListOffset>,
    attempts: u8,
    observed: Option<RunObservation>,
}

/// Where the anchored message is among the rows.
#[derive(Debug, PartialEq)]
pub(super) enum AnchorRow {
    /// Not shown yet: its thread is still loading.
    Missing,
    /// It is the first user message, at this index.
    First(usize),
    /// Another user message comes first, so no end space applies.
    NotFirst,
}

/// What the list measured at its last layout, in window pixels.
pub(super) struct Layout {
    pub index: usize,
    pub rows: usize,
    pub top: ListOffset,
    pub viewport_height: Pixels,
    pub anchor_top: Option<Pixels>,
    pub previous_height: Option<Pixels>,
    pub last_bottom: Option<Pixels>,
}

/// What the timeline does after [`Anchor::settle`].
#[derive(Debug, PartialEq)]
pub(super) enum Settle {
    /// Nothing measured yet; look again next frame.
    Wait,
    /// No anchor holds the list.
    Idle,
    /// The anchor let go; `to_end` resumes following the tail.
    Released { to_end: bool },
    /// The end space (if `remeasure`) and the pin (once) to apply.
    Placed { remeasure: bool, pin: Option<ListOffset> },
}

impl Anchor {
    /// `anchorNewTurn`: a message just sent, which the timeline follows from the end.
    pub(super) fn arm(&mut self, message: SharedString) {
        *self = Self { message: Some(message), observed: self.observed.take(), ..Self::default() };
    }

    pub(super) fn release(&mut self) {
        *self = Self { observed: self.observed.take(), ..Self::default() };
    }

    pub(super) fn message(&self) -> Option<&SharedString> {
        self.message.as_ref()
    }

    pub(super) fn end_space(&self) -> f32 {
        self.end_space
    }

    pub(super) fn is_pinned(&self) -> bool {
        self.pinned.is_some()
    }

    /// Placed from a new layout: the end space that lets the message sit `OFFSET` below the top,
    /// and the pin there. A reader who scrolled the pin away, or a reply that fills the viewport,
    /// lets go.
    pub(super) fn settle(&mut self, row: AnchorRow, layout: Option<Layout>) -> Settle {
        if self.message.is_none() {
            return Settle::Idle;
        }
        let index = match row {
            AnchorRow::Missing => return Settle::Idle,
            AnchorRow::NotFirst => return self.let_go(true),
            AnchorRow::First(index) => index,
        };
        let Some(layout) = layout.filter(|layout| layout.index == index) else { return self.wait() };
        if let Some(pin) = self.pinned
            && layout.top != pin
        {
            return self.let_go(false);
        }
        let (Some(anchor_top), Some(last_bottom)) = (layout.anchor_top, layout.last_bottom) else {
            // Scrolled past the message, the rows after it already run beyond the viewport; a
            // top past the last row is a scroll to the end that has not laid out yet.
            if self.pinned.is_none() && index < layout.top.item_ix && layout.top.item_ix < layout.rows {
                return self.let_go(true);
            }
            return self.wait();
        };
        let needed = f32::from(anchor_top - last_bottom + layout.viewport_height) + self.end_space - OFFSET;
        if needed <= EPSILON {
            return self.let_go(true);
        }
        // Rows the reply appends wait for their own layout; only consecutive waits give up.
        self.attempts = 0;
        let space = needed + HOLD;
        let remeasure = (space - self.end_space).abs() > EPSILON;
        if remeasure {
            self.end_space = space;
        }
        let pin = self.pinned.is_none().then(|| pin_at(index, layout.previous_height));
        if pin.is_some() {
            self.pinned = pin;
        }
        Settle::Placed { remeasure, pin }
    }

    fn wait(&mut self) -> Settle {
        self.attempts += 1;
        if self.attempts > ATTEMPTS { self.let_go(true) } else { Settle::Wait }
    }

    fn let_go(&mut self, to_end: bool) -> Settle {
        self.release();
        Settle::Released { to_end }
    }

    /// The anchor's run finished, or did work: the reply now follows the end
    /// (`anchorRunSettled`, `shouldReleaseTimelineAnchorForToolActivity`).
    pub(super) fn run_moved_on(&self, projection: &ThreadProjection) -> bool {
        let Some(message) = &self.message else { return false };
        let Some(run) = projection.runs.iter().find(|run| run.user_message_id.as_deref() == Some(message)) else {
            return false;
        };
        !is_active_run_status(&run.status)
            || projection.visible_turn_items.iter().any(|row| {
                row.item.run_id.as_deref() == Some(&run.id)
                    && !matches!(row.item.item_type.as_str(), "user_message" | "assistant_message")
            })
    }

    /// A run that starts while the end is followed anchors its message, unless the last anchor
    /// is still being placed.
    pub(super) fn observe(
        &mut self,
        next: RunObservation,
        queued: bool,
        message: Option<&str>,
        following: bool,
    ) -> bool {
        let (observation, started) = observe_run(self.observed.as_ref(), next, queued, message);
        if self.observed.as_ref() == Some(&observation) {
            return false;
        }
        self.observed = Some(observation);
        let pending = self.message.is_some() && self.pinned.is_none();
        match started {
            Some(message) if following && !pending => {
                self.arm(message.into());
                true
            }
            _ => false,
        }
    }
}

/// `positionAnchor`'s `scrollToIndex(viewOffset)`: the message `OFFSET` below the top, which the
/// row before it supplies (the first row's own inset stands in for it).
fn pin_at(index: usize, previous_height: Option<Pixels>) -> ListOffset {
    match (index.checked_sub(1), previous_height) {
        (Some(previous), Some(height)) => ListOffset {
            item_ix: previous,
            offset_in_item: if height > px(OFFSET) { height - px(OFFSET) } else { px(0.) },
        },
        _ => ListOffset { item_ix: index, offset_in_item: px(0.) },
    }
}

/// `resolveChatListAnchoredEndSpace`: only the first user message can take end space.
pub(super) fn anchor_row(rows: &[Row], row_id: &str) -> AnchorRow {
    match rows.iter().enumerate().find(|(_, row)| matches!(row, Row::User { .. })) {
        Some((index, Row::User { id, .. })) if id == row_id => AnchorRow::First(index),
        Some(_) => AnchorRow::NotFirst,
        None => AnchorRow::Missing,
    }
}

/// The row a sent message shows as: its projected item, or the optimistic row until then.
pub(super) fn row_id(projection: Option<&ThreadProjection>, message: &str) -> String {
    projection
        .and_then(|projection| {
            projection
                .turn_items
                .iter()
                .find(|item| item.item_type == "user_message" && item.str("messageId") == Some(message))
        })
        .map_or_else(|| format!("optimistic:{message}"), |item| item.id.clone())
}

/// `TimelineRunObservation`.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct RunObservation {
    pub thread: String,
    pub hydrated: bool,
    pub run: Option<String>,
}

/// `observeTimelineRun`: the observation to keep, and the message of a run that just started in
/// a thread that was already showing.
pub(super) fn observe_run(
    previous: Option<&RunObservation>,
    next: RunObservation,
    queued: bool,
    message: Option<&str>,
) -> (RunObservation, Option<String>) {
    let observation = RunObservation { run: if queued { None } else { next.run.clone() }, ..next.clone() };
    let Some(previous) = previous.filter(|previous| previous.thread == next.thread && previous.hydrated) else {
        return (observation, None);
    };
    match (next.hydrated, &next.run, message) {
        (true, Some(run), Some(message)) if !queued && previous.run.as_ref() != Some(run) => {
            (observation, Some(message.to_owned()))
        }
        _ => (previous.clone(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str) -> Row {
        Row::User {
            id: id.to_owned().into(),
            text: "hi".into(),
            steer: None,
            grouped_with_previous: false,
            grouped_with_next: false,
            continues_steer: false,
            created_at: SharedString::default(),
            revert: None,
            attribution: None,
            content: Default::default(),
        }
    }

    fn working() -> Row {
        Row::Working { started_at: None }
    }

    /// The message at row 2 (100px down), 300px of rows after it, in a 600px viewport.
    fn layout(top: ListOffset, end_space: f32) -> Layout {
        Layout {
            index: 2,
            rows: 5,
            top,
            viewport_height: px(600.),
            anchor_top: Some(px(100.)),
            previous_height: Some(px(60.)),
            last_bottom: Some(px(400. + end_space)),
        }
    }

    fn tail() -> ListOffset {
        ListOffset { item_ix: 0, offset_in_item: px(0.) }
    }

    #[test]
    fn only_the_first_user_message_anchors() {
        let rows = [working(), user("a"), user("b")];
        assert_eq!(anchor_row(&rows, "a"), AnchorRow::First(1));
        assert_eq!(anchor_row(&rows, "b"), AnchorRow::NotFirst);
        assert_eq!(anchor_row(&[working()], "a"), AnchorRow::Missing);
    }

    #[test]
    fn the_message_pins_below_the_top_with_end_space_that_settles() {
        let mut anchor = Anchor::default();
        anchor.arm("m".into());
        // 300px from the message's top down, 600 tall: 276px more lets it sit 24px under the top
        // (and 2px to hold it), pinned 24px from the end of the 60px row before it.
        let pin = ListOffset { item_ix: 1, offset_in_item: px(36.) };
        assert_eq!(
            anchor.settle(AnchorRow::First(2), Some(layout(tail(), 0.))),
            Settle::Placed { remeasure: true, pin: Some(pin) }
        );
        assert_eq!(anchor.end_space(), 278.);
        // The next layout carries the space: nothing changes, so nothing lays out again.
        assert_eq!(
            anchor.settle(AnchorRow::First(2), Some(layout(pin, 278.))),
            Settle::Placed { remeasure: false, pin: None }
        );
        // The reply grows 100px: the space shrinks, the pin stays.
        let mut grown = layout(pin, 278.);
        grown.last_bottom = Some(px(778.));
        assert_eq!(anchor.settle(AnchorRow::First(2), Some(grown)), Settle::Placed { remeasure: true, pin: None });
        assert_eq!(anchor.end_space(), 178.);
        assert!(anchor.is_pinned());
    }

    #[test]
    fn the_anchor_lets_go_when_the_reply_fills_the_view_or_the_reader_scrolls() {
        let pin = ListOffset { item_ix: 1, offset_in_item: px(36.) };
        let mut anchor = Anchor::default();
        anchor.arm("m".into());
        anchor.settle(AnchorRow::First(2), Some(layout(tail(), 0.)));
        let mut full = layout(pin, 278.);
        full.last_bottom = Some(px(1000.));
        assert_eq!(anchor.settle(AnchorRow::First(2), Some(full)), Settle::Released { to_end: true });
        assert_eq!((anchor.message(), anchor.end_space()), (None, 0.));

        anchor.arm("m".into());
        anchor.settle(AnchorRow::First(2), Some(layout(tail(), 0.)));
        let scrolled = ListOffset { item_ix: 0, offset_in_item: px(10.) };
        assert_eq!(
            anchor.settle(AnchorRow::First(2), Some(layout(scrolled, 278.))),
            Settle::Released { to_end: false }
        );

        // A later message returns the timeline to its end.
        anchor.arm("m".into());
        assert_eq!(anchor.settle(AnchorRow::NotFirst, None), Settle::Released { to_end: true });
    }

    #[test]
    fn an_unmeasured_message_waits_a_few_frames_then_follows_the_end() {
        let mut anchor = Anchor::default();
        anchor.arm("m".into());
        assert_eq!(anchor.settle(AnchorRow::Missing, None), Settle::Idle);
        for _ in 0..ATTEMPTS {
            assert_eq!(anchor.settle(AnchorRow::First(2), None), Settle::Wait);
        }
        assert_eq!(anchor.settle(AnchorRow::First(2), None), Settle::Released { to_end: true });

        let unmeasured =
            |item_ix| Layout { anchor_top: None, ..layout(ListOffset { item_ix, offset_in_item: px(0.) }, 0.) };
        anchor.arm("m".into());
        assert_eq!(anchor.settle(AnchorRow::First(2), Some(unmeasured(5))), Settle::Wait, "the end is still settling");
        assert_eq!(
            anchor.settle(AnchorRow::First(2), Some(unmeasured(3))),
            Settle::Released { to_end: true },
            "the rows after it already overflow"
        );
    }

    #[test]
    fn the_anchor_lets_go_once_its_run_works_or_finishes() {
        use serde_json::json;
        let projection = |status: &str, items: Vec<(&str, &str)>| -> ThreadProjection {
            let rows: Vec<_> = items
                .into_iter()
                .map(|(id, kind)| {
                    let item = json!({"id": id, "type": kind, "threadId": "t", "runId": "r1", "messageId": "m"});
                    json!({"visibility": "visible", "sourceThreadId": "t", "sourceItemId": id, "item": item})
                })
                .collect();
            serde_json::from_value(json!({
                "thread": {"id": "t", "title": "T"},
                "runs": [{"id": "r1", "status": status, "userMessageId": "m"}],
                "turnItems": [],
                "visibleTurnItems": rows,
            }))
            .unwrap()
        };
        let mut anchor = Anchor::default();
        anchor.arm("m".into());
        let replying = vec![("u", "user_message"), ("a", "assistant_message")];
        assert!(!anchor.run_moved_on(&projection("running", replying.clone())));
        assert!(anchor.run_moved_on(&projection("running", vec![("u", "user_message"), ("w", "reasoning")])));
        assert!(anchor.run_moved_on(&projection("completed", replying)));
        anchor.arm("other".into());
        assert!(!anchor.run_moved_on(&projection("completed", vec![])), "its run has not started");
    }

    #[test]
    fn a_run_anchors_when_it_starts_in_a_thread_already_showing() {
        let seen =
            |run: Option<&str>, hydrated| RunObservation { thread: "t".into(), hydrated, run: run.map(Into::into) };
        let mut anchor = Anchor::default();
        assert!(!anchor.observe(seen(None, false), false, None, true), "loading");
        assert!(
            !anchor.observe(seen(Some("r1"), true), false, Some("m1"), true),
            "a run already running when it showed"
        );
        assert!(!anchor.observe(seen(Some("r2"), true), true, Some("m2"), true), "a queued run waits");
        assert!(anchor.observe(seen(Some("r2"), true), false, Some("m2"), true));
        assert_eq!(anchor.message().map(SharedString::as_ref), Some("m2"));
        assert!(!anchor.observe(seen(Some("r3"), true), false, Some("m3"), true), "the last anchor is still placing");
        anchor.release();
        assert!(!anchor.observe(seen(Some("r4"), true), false, Some("m4"), false), "the reader scrolled away");
        let other = RunObservation { thread: "u".into(), hydrated: true, run: Some("r5".into()) };
        assert!(!anchor.observe(other, false, Some("m5"), true), "another thread's first look");
    }
}
