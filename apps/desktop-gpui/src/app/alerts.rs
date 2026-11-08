//! Thread alerts as sounds, in-app toasts and OS notifications, presented the
//! way `ThreadNotificationCoordinator.tsx` does.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::rc::Rc;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::Button;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Icon, WindowExt as _};
use gpui_kit::{App, Context, SharedString, Styled as _, Window};
use rodio::Source as _;

use super::{Target, Workspace};
use crate::inbox::Status;
use crate::model::ThreadRef;
use crate::notifications::{Alert, Kind, Mode, ThreadAlerts, ThreadKey};

/// A click that needs the window, shared by the toast and the system notification.
type Callback = Rc<dyn Fn(&mut Window, &mut App)>;

/// Each thread's toast ids that may still be on screen.
type OpenToasts = Rc<RefCell<HashMap<ThreadKey, Vec<usize>>>>;

/// In-app thread toasts. Each has its own id, so several stack as the web's do.
struct ThreadToast;
/// OS notifications, one per thread, keyed by [`system_key`].
struct ThreadSystemNotification;

#[derive(Default)]
pub(super) struct AlertState {
    /// Watches the shell; `None` while both notification settings are off.
    threads: Option<ThreadAlerts>,
    mode: Mode,
    /// Threads whose OS notifications may be on screen, counted by the taskbar badge;
    /// focusing the window clears them.
    pending: HashSet<ThreadKey>,
    toasts: usize,
    /// So hiding a thread can close its toasts.
    open_toasts: OpenToasts,
}

impl Workspace {
    /// Presents the shell's new transitions; runs after each shell batch.
    pub(super) fn observe_alerts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mode = Mode::parse(self.client_settings.values.get("notificationMode"));
        if mode != self.alerts.mode {
            self.alerts.mode = mode;
            self.clear_system_alerts(window, cx);
        }
        if mode == Mode::Off && !self.client_settings.values.bool("inAppNotificationsEnabled") {
            self.alerts.threads = None;
            return;
        }
        if !self.shell.live() {
            return;
        }
        let alerts = self.alerts.threads.get_or_insert_default().observe(&self.shell.threads);
        self.retract_hidden_alerts(window, cx);
        if alerts.is_empty() {
            return;
        }
        let hidden_tags = self.hidden_thread_tags();
        let hidden = crate::model::hidden_threads(&self.shell.threads, &hidden_tags);
        let alerts: Vec<Alert> = alerts
            .into_iter()
            .filter(|alert| !hidden.contains(&(alert.environment_id.as_ref(), alert.thread_id.as_str())))
            .collect();
        for alert in alerts {
            self.present_alert(alert, mode, window, cx);
        }
    }

    /// The tags each environment's thread tag filter hides, for the environments that hide any.
    fn hidden_thread_tags(&self) -> HashMap<&str, Vec<String>> {
        self.sessions
            .iter()
            .filter_map(|s| Some((s.id.as_deref()?, crate::model::hidden_thread_tags(&s.server_info.settings)?)))
            .collect()
    }

    /// Closes the toasts and OS notifications of threads the tag filter now hides. While some
    /// environment hides tags, one whose thread left the list (archived) can't be judged, so it
    /// closes too, as does an OS notification whose environment was removed.
    pub(super) fn retract_hidden_alerts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.alerts.pending.is_empty() && self.alerts.open_toasts.borrow().is_empty() {
            return;
        }
        let hidden_tags = self.hidden_thread_tags();
        let hiding = !hidden_tags.is_empty();
        let hidden = crate::model::hidden_threads(&self.shell.threads, &hidden_tags);
        let listed: HashSet<ThreadRef> =
            self.shell.threads.iter().map(|t| (t.environment_id.as_ref(), t.id.as_str())).collect();
        let closes = |(env, id): &ThreadKey| {
            let key = (env.as_ref(), id.as_str());
            hidden.contains(&key) || (hiding && !listed.contains(&key))
        };
        let toasts: Vec<usize> = {
            let mut open = self.alerts.open_toasts.borrow_mut();
            let closing: Vec<ThreadKey> = open.keys().filter(|key| closes(key)).cloned().collect();
            closing.iter().filter_map(|key| open.remove(key)).flatten().collect()
        };
        let removed = |env: &SharedString| !self.sessions.iter().any(|s| s.id.as_ref() == Some(env));
        let closing: Vec<ThreadKey> =
            self.alerts.pending.iter().filter(|key| closes(key) || removed(&key.0)).cloned().collect();
        for toast in toasts {
            window.remove_notification1::<ThreadToast>(toast, cx);
        }
        if closing.is_empty() {
            return;
        }
        for key in closing {
            window.remove_notification1::<ThreadSystemNotification>(system_key(&key), cx);
            self.alerts.pending.remove(&key);
        }
        set_badge(window, self.alerts.pending.len());
    }

    fn present_alert(&mut self, alert: Alert, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        let key: ThreadKey = (alert.environment_id.clone(), alert.thread_id.clone());
        let Some(thread) = self.shell.threads.iter().find(|t| t.environment_id == key.0 && t.id == key.1) else {
            return;
        };
        let body = SharedString::from(thread.title.clone());
        if mode.sound() {
            play_sound(alert.kind);
        }
        let workspace = cx.weak_entity();
        let (env, thread_id) = key.clone();
        let open_thread: Callback = Rc::new(move |window, cx| {
            let _ = workspace.update(cx, |this, cx| {
                this.close_settings(window, cx);
                this.select_thread_in(env.clone(), thread_id.clone(), window, cx);
            });
        });
        let focused = window.is_window_active();
        // Settings and pages are routes of their own on the web, so they never count as the open thread.
        let on_screen = self.page.is_none()
            && self.settings.is_none()
            && self
                .target
                .as_ref()
                .is_some_and(|t| matches!(t, Target::Thread { env, id } if *env == key.0 && *id == key.1));
        if focused && !on_screen && self.client_settings.values.bool("inAppNotificationsEnabled") {
            let t = *crate::theme::tokens(cx);
            let (icon, color) = match (alert.kind, alert.status) {
                (Kind::Completion, _) => (Lucide::CircleCheck, t.success_foreground),
                (_, Status::Approval) => (Lucide::ShieldQuestionMark, t.warning_foreground),
                (_, Status::Limited) => (Lucide::CircleAlert, t.warning_foreground),
                (_, Status::Failed) => (Lucide::CircleAlert, t.error_foreground),
                _ => (Lucide::MessageCircleQuestionMark, t.info_foreground),
            };
            self.alerts.toasts += 1;
            let toast_id = self.alerts.toasts;
            let open_toasts = self.alerts.open_toasts.clone();
            open_toasts.borrow_mut().entry(key.clone()).or_default().push(toast_id);
            let toast = Notification::new()
                .id1::<ThreadToast>(toast_id)
                .icon(Icon::new(icon).text_color(color))
                .title(alert.title())
                .message(body)
                .action(move |_, _, cx| {
                    let open_thread = open_thread.clone();
                    Button::new("open-thread").label("Open thread").on_click(cx.listener(
                        move |toast, _, window, cx| {
                            toast.dismiss(window, cx);
                            open_thread(window, cx);
                        },
                    ))
                })
                // Base UI toasts time out with an action too.
                .autohide(true)
                .on_close(move |_, _| forget_toast(&open_toasts, &key, toast_id));
            window.push_notification(toast, cx);
            return;
        }
        if !mode.desktop() || focused {
            return;
        }
        let notification = Notification::new()
            .id1::<ThreadSystemNotification>(system_key(&key))
            .title(alert.title())
            .message(body)
            .system()
            .on_click(move |_, window, cx| open_thread(window, cx));
        window.push_notification(notification, cx);
        self.alerts.pending.insert(key);
        set_badge(window, self.alerts.pending.len());
    }

    /// Retracts the OS notifications, as focusing the web app's window does.
    pub(super) fn clear_system_alerts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.alerts.pending.is_empty() {
            self.alerts.pending.clear();
            window.remove_notification::<ThreadSystemNotification>(cx);
            set_badge(window, 0);
        }
    }
}

/// `threadNotificationTag`: one OS notification per environment and thread.
fn system_key((env, thread_id): &ThreadKey) -> SharedString {
    SharedString::from(format!("{env}\u{0}{thread_id}"))
}

/// `onRemove`: a toast closed by any means leaves its thread's list.
fn forget_toast(open_toasts: &RefCell<HashMap<ThreadKey, Vec<usize>>>, key: &ThreadKey, toast_id: usize) {
    let mut open = open_toasts.borrow_mut();
    if let Some(ids) = open.get_mut(key) {
        ids.retain(|id| *id != toast_id);
        if ids.is_empty() {
            open.remove(key);
        }
    }
}

/// `setNotificationBadge` through the desktop bridge.
fn set_badge(window: &Window, count: usize) {
    crate::desktop::badge::set(window, count);
}

/// Plays an alert's sound off the UI thread; while one plays, the same sound again is dropped.
fn play_sound(kind: Kind) {
    // Two independent timestamps, so a poisoned lock holds nothing half-updated.
    static PLAYING_UNTIL: Mutex<[Option<Instant>; 2]> = Mutex::new([None, None]);
    let playing_until = || PLAYING_UNTIL.lock().unwrap_or_else(PoisonError::into_inner);
    let (slot, bytes): (usize, &'static [u8]) = match kind {
        Kind::Completion => (0, include_bytes!("../../assets/sounds/notification-completion.mp3")),
        Kind::Input => (1, include_bytes!("../../assets/sounds/notification-input.mp3")),
    };
    let now = Instant::now();
    {
        let mut playing_until = playing_until();
        if playing_until[slot].is_some_and(|until| now < until) {
            return;
        }
        // Held for a second until the decoded length is known.
        playing_until[slot] = Some(now + Duration::from_secs(1));
    }
    std::thread::spawn(move || {
        let Ok(source) = rodio::Decoder::try_from(Cursor::new(bytes)) else { return };
        if let Some(length) = source.total_duration() {
            playing_until()[slot] = Some(now + length);
        }
        match rodio::DeviceSinkBuilder::open_default_sink() {
            Ok(mut sink) => {
                sink.log_on_drop(false);
                let player = rodio::Player::connect_new(sink.mixer());
                player.append(source);
                player.sleep_until_end();
            }
            Err(error) => log::warn!("could not play the notification sound: {error}"),
        }
    });
}
