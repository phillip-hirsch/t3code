//! Toasts for each keybindings reload the server reports (`KeybindingsUpdateToast.tsx`,
//! `.logic.ts`, `KeybindingsConfigWarning.tsx`).

use std::time::{Duration, Instant};

use gpui_kit::component::button::Button;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _};
use gpui_kit::{Context, Window};
use serde_json::{Value, json};

use crate::app::Workspace;

/// `KEYBINDINGS_TOAST_COOLDOWN_MS`: a save and the file watcher publish the same snapshot.
const COOLDOWN: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Decision {
    Success,
    Invalid(String),
}

#[derive(Default)]
pub(in crate::app) struct KeybindingToasts {
    last: Option<(String, Instant)>,
    pending: Vec<Decision>,
}

impl KeybindingToasts {
    /// `createKeybindingsUpdateToastController().handle`.
    fn decide(&mut self, event: &Value, now: Instant) -> Option<Decision> {
        if event["type"] != "keybindingsUpdated" {
            return None;
        }
        let issue = event["payload"]["issues"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|issue| issue["kind"].as_str().is_some_and(|kind| kind.starts_with("keybindings.")));
        let decision = match issue {
            Some(issue) => Decision::Invalid(issue["message"].as_str().unwrap_or_default().to_owned()),
            None => Decision::Success,
        };
        let key = match &decision {
            Decision::Success => "success".to_owned(),
            Decision::Invalid(message) => format!("invalid:{message}"),
        };
        if self.last.as_ref().is_some_and(|(last, at)| *last == key && now.duration_since(*at) < COOLDOWN) {
            return None;
        }
        self.last = Some((key, now));
        Some(decision)
    }
}

impl Workspace {
    /// Notes a server config event; the toasts show after the batch, where there is a window.
    pub(in crate::app) fn note_keybindings_event(&mut self, event: &Value) {
        let toasts = &mut self.settings_ui.keybinding_toasts;
        if let Some(decision) = toasts.decide(event, Instant::now()) {
            toasts.pending.push(decision);
        }
    }

    pub(in crate::app) fn show_keybindings_toasts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for decision in std::mem::take(&mut self.settings_ui.keybinding_toasts.pending) {
            let toast = match decision {
                Decision::Success => Notification::success("Keybindings configuration reloaded successfully.")
                    .title("Keybindings updated"),
                Decision::Invalid(message) => {
                    let can_open = self.server_info.keybindings_config_path.is_some()
                        && !self.server_info.available_editors.is_empty();
                    let workspace = cx.weak_entity();
                    Notification::warning(message).title("Invalid keybindings configuration").action(move |_, _, _| {
                        let workspace = workspace.clone();
                        Button::new("open-keybindings-file")
                            .outline()
                            .xsmall()
                            .label("Open keybindings.json")
                            .disabled(!can_open)
                            .on_click(move |_, window, cx| {
                                let _ = workspace.update(cx, |this, cx| this.open_keybindings_file(window, cx));
                            })
                    })
                }
            };
            window.push_notification(toast, cx);
        }
    }

    /// `KeybindingsConfigWarning`'s button: the file in the preferred editor.
    fn open_keybindings_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let available = &self.server_info.available_editors;
        let editor = self
            .preferred_editor
            .clone()
            .filter(|editor| available.contains(editor))
            .or_else(|| available.first().cloned());
        let (Some(client), Some(path), Some(editor)) =
            (self.client(), self.server_info.keybindings_config_path.clone(), editor)
        else {
            return;
        };
        cx.spawn_in(window, async move |_, cx| {
            if let Err(error) = client.request("shell.openInEditor", json!({ "cwd": path, "editor": editor })).await {
                let message = match &error {
                    crate::rpc::RpcError::Failure(value) => value["message"].as_str().map(str::to_owned),
                    _ => None,
                }
                .unwrap_or_else(|| "Unknown error opening file.".to_owned());
                let _ = cx.update(|window, cx| {
                    window.push_notification(Notification::error(message).title("Unable to open keybindings file"), cx)
                });
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reloads_toast_once_per_change() {
        let mut toasts = KeybindingToasts::default();
        let start = Instant::now();
        let ok = json!({"type": "keybindingsUpdated", "payload": {"issues": []}});
        let bad = json!({"type": "keybindingsUpdated", "payload": {"issues": [
            {"kind": "provider.status", "message": "ignored"},
            {"kind": "keybindings.invalid-json", "message": "Unexpected token"},
        ]}});
        assert_eq!(toasts.decide(&json!({"type": "snapshot"}), start), None);
        assert_eq!(toasts.decide(&ok, start), Some(Decision::Success));
        assert_eq!(toasts.decide(&ok, start + Duration::from_millis(500)), None);
        assert_eq!(
            toasts.decide(&bad, start + Duration::from_millis(600)),
            Some(Decision::Invalid("Unexpected token".into()))
        );
        assert_eq!(toasts.decide(&ok, start + Duration::from_millis(700)), Some(Decision::Success));
        assert_eq!(toasts.decide(&ok, start + Duration::from_secs(3)), Some(Decision::Success));
    }
}
