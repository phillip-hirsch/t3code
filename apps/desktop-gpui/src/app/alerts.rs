//! Thread alerts as sounds, in-app toasts and OS notifications, presented the
//! way `ThreadNotificationCoordinator.tsx` does.

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
use crate::notifications::{Alert, Kind, Mode, ThreadAlerts};

/// In-app thread toasts. Each has its own id, so several stack as the web's do.
struct ThreadToast;
/// OS notifications, one per thread.
struct ThreadSystemNotification;

#[derive(Default)]
pub(super) struct AlertState {
    /// Watches the shell; `None` while both notification settings are off.
    threads: Option<ThreadAlerts>,
    mode: Mode,
    /// OS notifications may be on screen; focusing the window clears them.
    pending: bool,
    toasts: usize,
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
        if !self.shell.live {
            return;
        }
        for alert in self.alerts.threads.get_or_insert_default().observe(&self.shell.threads) {
            self.present_alert(alert, mode, window, cx);
        }
    }

    fn present_alert(&mut self, alert: Alert, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.shell.threads.iter().find(|t| t.id == alert.thread_id) else { return };
        let body = SharedString::from(thread.title.clone());
        if mode.sound() {
            play_sound(alert.kind);
        }
        let workspace = cx.weak_entity();
        let thread_id = alert.thread_id.clone();
        let open_thread: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |window, cx| {
            let _ = workspace.update(cx, |this, cx| {
                this.close_settings(cx);
                this.select_thread(thread_id.clone(), window, cx);
            });
        });
        let focused = window.is_window_active();
        // Settings and pages are routes of their own on the web, so they never count as the open thread.
        let on_screen = self.page.is_none()
            && self.settings.is_none()
            && self.target.as_ref().is_some_and(|t| matches!(t, Target::Thread(id) if *id == alert.thread_id));
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
            let toast = Notification::new()
                .id1::<ThreadToast>(self.alerts.toasts)
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
                .autohide(true);
            window.push_notification(toast, cx);
            return;
        }
        if !mode.desktop() || focused {
            return;
        }
        self.alerts.pending = true;
        let notification = Notification::new()
            .id1::<ThreadSystemNotification>(SharedString::from(alert.thread_id.clone()))
            .title(alert.title())
            .message(body)
            .system()
            .on_click(move |_, window, cx| open_thread(window, cx));
        window.push_notification(notification, cx);
    }

    /// Retracts the OS notifications, as focusing the web app's window does.
    pub(super) fn clear_system_alerts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.alerts.pending) {
            window.remove_notification::<ThreadSystemNotification>(cx);
        }
    }
}

/// Plays an alert's sound off the UI thread; while one plays, the same sound again is dropped.
fn play_sound(kind: Kind) {
    static PLAYING_UNTIL: Mutex<[Option<Instant>; 2]> = Mutex::new([None, None]);
    let (slot, bytes): (usize, &'static [u8]) = match kind {
        Kind::Completion => (0, include_bytes!("../../assets/sounds/notification-completion.mp3")),
        Kind::Input => (1, include_bytes!("../../assets/sounds/notification-input.mp3")),
    };
    std::thread::spawn(move || {
        let Ok(source) = rodio::Decoder::try_from(Cursor::new(bytes)) else { return };
        let length = source.total_duration().unwrap_or(Duration::from_secs(1));
        {
            let mut playing_until = PLAYING_UNTIL.lock().unwrap_or_else(PoisonError::into_inner);
            let now = Instant::now();
            if playing_until[slot].is_some_and(|until| now < until) {
                return;
            }
            playing_until[slot] = Some(now + length);
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
