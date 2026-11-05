//! "Enjoying T3 Code?" (`GitHubStarPrompt.tsx`): after more than five sent prompts, asks once per
//! session for a GitHub star until dismissed, unless the server's GitHub account already starred
//! the repository, and only once the user has been idle for a while.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Icon, WindowExt as _};
use gpui_kit::{Context, Styled as _, Window};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::app::Workspace;
use crate::rpc::RpcClient;

const REPOSITORY: &str = "pingdotgg/t3code";
const PROMPTS_BEFORE_ASKING: u32 = 5;
const IDLE: Duration = Duration::from_secs(30);

/// The web's two localStorage keys, kept on this device.
#[derive(Default, Serialize, Deserialize, PartialEq, Debug)]
#[serde(default)]
struct Stored {
    sent: u32,
    dismissed: bool,
}

struct StarPrompt;

fn path() -> Option<PathBuf> {
    let home = std::env::var_os("T3CODE_HOME").map(PathBuf::from).or_else(|| {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(|home| PathBuf::from(home).join(".t3-gpui"))
    })?;
    Some(home.join("github-star-prompt.json"))
}

fn read() -> Option<Stored> {
    let path = path()?;
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).ok(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(Stored::default()),
        Err(_) => None,
    }
}

fn write(stored: &Stored) -> Option<()> {
    let path = path()?;
    std::fs::create_dir_all(path.parent()?).ok()?;
    std::fs::write(path, serde_json::to_vec(stored).ok()?).ok()
}

fn dismiss() {
    if let Some(mut stored) = read() {
        stored.dismissed = true;
        let _ = write(&stored);
    }
}

/// Counts one sent prompt; true when this one should lead to asking.
fn count_prompt(stored: &mut Stored) -> bool {
    if stored.dismissed {
        return false;
    }
    stored.sent += 1;
    stored.sent > PROMPTS_BEFORE_ASKING
}

fn star_input() -> Value {
    json!({ "provider": "github", "host": "github.com", "repository": REPOSITORY })
}

impl Workspace {
    /// `recordPromptSent`, after a prompt was sent.
    pub(in crate::app) fn record_prompt_sent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_ui.star_prompt_shown {
            return;
        }
        // Storage that cannot be read or written could not keep a dismissal, so never ask.
        let Some(mut stored) = read() else { return };
        let ask = count_prompt(&mut stored);
        if write(&stored).is_none() || !ask {
            return;
        }
        let Some(client) = self.client() else { return };
        self.settings_ui.star_prompt_shown = true;
        // Input resets the idle clock; only keystrokes are observable app-wide here.
        let last_input = Rc::new(Cell::new(Instant::now()));
        let input_clock = last_input.clone();
        let keystrokes = cx.observe_keystrokes(move |_, _, _, _| input_clock.set(Instant::now()));
        cx.spawn_in(window, async move |_, cx| {
            let _keystrokes = keystrokes;
            let Ok(starred) = client.request("sourceControl.starred", star_input()).await else { return };
            if starred.as_bool() == Some(true) {
                return dismiss();
            }
            loop {
                let idle_for = last_input.get().elapsed();
                if idle_for < IDLE {
                    cx.background_executor().timer(IDLE - idle_for).await;
                    continue;
                }
                let Ok(active) = cx.update(|window, _| window.is_window_active()) else { return };
                if active {
                    break;
                }
                last_input.set(Instant::now());
            }
            // Another window of this app can dismiss while the check or the wait runs.
            if read().is_none_or(|stored| stored.dismissed) {
                return;
            }
            let _ = cx.update(|window, cx| show(client, window, cx));
        })
        .detach();
    }
}

fn show(client: RpcClient, window: &mut Window, cx: &mut gpui_kit::App) {
    let t = *crate::theme::tokens(cx);
    let toast = Notification::new()
        .id::<StarPrompt>()
        .icon(Icon::new(IconName::Star).text_color(t.warning_foreground))
        .title("Enjoying T3 Code?")
        .message("Give us a star on GitHub to let us know!")
        .autohide(false)
        .on_close(|_, _| dismiss())
        .action(move |_, _, cx| {
            let client = client.clone();
            Button::new("star-on-github").primary().label("Star on GitHub").on_click(cx.listener(
                move |toast, _, window, cx| {
                    dismiss();
                    toast.dismiss(window, cx);
                    let client = client.clone();
                    window
                        .spawn(cx, async move |cx| {
                            let starred = client.request("sourceControl.star", star_input()).await;
                            let _ = cx.update(|window, cx| {
                                if matches!(starred, Ok(Value::Bool(true))) {
                                    let thanks = Notification::success("It really helps T3 Code grow.")
                                        .title("Thanks for the star!");
                                    window.push_notification(thanks, cx);
                                } else {
                                    cx.open_url(&format!("https://github.com/{REPOSITORY}"));
                                }
                            });
                        })
                        .detach();
                },
            ))
        });
    window.push_notification(toast, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_after_the_fifth_prompt_until_dismissed() {
        let mut stored = Stored::default();
        let asks: Vec<bool> = (0..7).map(|_| count_prompt(&mut stored)).collect();
        assert_eq!(asks, [false, false, false, false, false, true, true]);
        stored.dismissed = true;
        assert!(!count_prompt(&mut stored));
        assert_eq!(stored.sent, 7);
    }
}
