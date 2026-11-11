//! T3 patch: pauses a window's vsync-driven frame requests while GPUI has nothing to draw, as
//! `MetalFrames` does on macOS, so an idle window doesn't wake the UI thread on every vblank.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::Thread;
use std::time::{Duration, Instant};

/// The running `VSyncProvider` thread. It parks while every window is paused.
static VSYNC_THREAD: Mutex<Option<Thread>> = Mutex::new(None);

/// How often a parked vsync thread still wakes to check for a lost device or a stop request.
pub(crate) const PARKED_VSYNC_RECHECK: Duration = Duration::from_secs(1);

/// How long a touchpad contact keeps its window's frames running before Direct Manipulation
/// reports the gesture: the gesture only advances on the per-frame `Update` calls.
pub(crate) const CONTACT_GRACE: Duration = Duration::from_millis(500);

pub(crate) fn set_vsync_thread(thread: Thread) {
    *VSYNC_THREAD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(thread);
}

pub(crate) fn unpark_vsync_thread() {
    if let Some(thread) = VSYNC_THREAD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
    {
        thread.unpark();
    }
}

/// Whether the vsync thread can park: no tracked window wants frames.
pub(crate) fn all_paused<'a>(mut paused: impl Iterator<Item = &'a AtomicBool>) -> bool {
    paused.all(|paused| paused.load(Ordering::Acquire))
}

/// One window's frame demand. The UI thread pauses and wakes it; the vsync thread reads
/// [`Self::paused_flag`] to skip the window, and parks once every window is paused.
#[derive(Default)]
pub(crate) struct WindowFrames {
    paused: Arc<AtomicBool>,
    /// Whether GPUI asked for a frame since the current paint began.
    woken: Cell<bool>,
    /// Whether the current paint drew or presented.
    drew: Cell<bool>,
    /// Until when a touchpad contact keeps frames running.
    contact_until: Cell<Option<Instant>>,
}

impl WindowFrames {
    pub(crate) fn paused_flag(&self) -> Arc<AtomicBool> {
        self.paused.clone()
    }

    pub(crate) fn wake(&self) {
        self.woken.set(true);
        if self.paused.swap(false, Ordering::AcqRel) {
            unpark_vsync_thread();
        }
    }

    /// A touchpad contact started: keep frames running until Direct Manipulation takes over.
    pub(crate) fn contact(&self, now: Instant) {
        self.contact_until.set(Some(now + CONTACT_GRACE));
        self.wake();
    }

    pub(crate) fn begin_frame(&self) {
        self.woken.set(false);
        self.drew.set(false);
    }

    pub(crate) fn note_drawn(&self) {
        self.drew.set(true);
    }

    /// Ends a paint, pausing the window's frames unless GPUI drew, asked for another frame, or a
    /// touchpad gesture (`gesture`) or recent contact still needs per-frame updates. Returns
    /// whether it paused.
    pub(crate) fn end_frame(&self, gesture: bool, now: Instant) -> bool {
        let contact = self.contact_until.get().is_some_and(|until| now < until);
        let pause = !self.drew.get() && !self.woken.get() && !gesture && !contact;
        if pause {
            self.paused.store(true, Ordering::Release);
        }
        pause
    }

    pub(crate) fn waker(self: &Rc<Self>) -> Rc<dyn Fn()> {
        let frames = Rc::downgrade(self);
        Rc::new(move || {
            if let Some(frames) = frames.upgrade() {
                frames.wake();
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_paused(frames: &WindowFrames) -> bool {
        frames.paused_flag().load(Ordering::Acquire)
    }

    #[test]
    fn frames_pause_only_when_gpui_neither_draws_nor_asks_again() {
        let frames = WindowFrames::default();
        let now = Instant::now();

        frames.begin_frame();
        frames.note_drawn();
        assert!(
            !frames.end_frame(false, now),
            "a drawn frame keeps frames running"
        );

        frames.begin_frame();
        frames.wake();
        assert!(
            !frames.end_frame(false, now),
            "a wake during the paint, like a throttled frame's retry, keeps them running"
        );

        frames.begin_frame();
        assert!(
            !frames.end_frame(true, now),
            "a running touchpad gesture keeps them running"
        );

        frames.begin_frame();
        assert!(frames.end_frame(false, now), "an idle paint pauses");
        assert!(is_paused(&frames));

        frames.wake();
        assert!(!is_paused(&frames), "a wake resumes a paused window");
    }

    #[test]
    fn a_wake_before_the_paint_was_served_by_it() {
        let frames = WindowFrames::default();
        frames.wake();
        frames.begin_frame();
        assert!(frames.end_frame(false, Instant::now()));
    }

    #[test]
    fn a_touchpad_contact_keeps_frames_running_until_its_grace_ends() {
        let frames = WindowFrames::default();
        let now = Instant::now();
        frames.contact(now);

        frames.begin_frame();
        assert!(!frames.end_frame(false, now + CONTACT_GRACE / 2));

        frames.begin_frame();
        assert!(frames.end_frame(false, now + CONTACT_GRACE));
    }

    #[test]
    fn the_waker_resumes_its_window_and_outlives_it_harmlessly() {
        let frames = Rc::new(WindowFrames::default());
        let waker = frames.waker();
        frames.begin_frame();
        frames.end_frame(false, Instant::now());
        waker();
        assert!(!is_paused(&frames));
        drop(frames);
        waker();
    }

    #[test]
    fn the_vsync_thread_parks_only_when_every_window_is_paused() {
        let windows = [AtomicBool::new(true), AtomicBool::new(false)];
        assert!(!all_paused(windows.iter()));
        windows[1].store(true, Ordering::Release);
        assert!(all_paused(windows.iter()));
        assert!(
            all_paused(std::iter::empty()),
            "with no windows there is nothing to request frames for"
        );
    }
}
