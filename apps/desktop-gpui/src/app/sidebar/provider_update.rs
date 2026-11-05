//! The footer pill that reports provider updates (`SidebarProviderUpdatePill.tsx`,
//! `getProviderUpdateSidebarPillView` in `ProviderUpdateLaunchNotification.logic.ts`):
//! "Updating Codex", then the outcome until it is dismissed, or for 3 s after a success.
//! The web's exit slide and countdown bar are left out, as repainting animations.

use std::collections::HashSet;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, h_flex};
use gpui_kit::{
    AnyElement, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, div, prelude::FluentBuilder as _, px, rems,
};

use super::tooltip;
use crate::app::Workspace;
use crate::model::Provider;
use crate::theme::Tokens;

const SUCCESS_VISIBLE: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    Loading,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct PillView {
    pub(super) key: String,
    pub(super) tone: Tone,
    pub(super) title: String,
    pub(super) description: String,
    pub(super) dismissible: bool,
}

/// What the pill remembers: when it started listening, what was dismissed, the success timer.
#[derive(Default)]
pub(super) struct PillState {
    visible_after: Option<String>,
    dismissed: HashSet<String>,
    timer_key: Option<String>,
    _timer: Option<Task<()>>,
}

/// `PROVIDER_DISPLAY_NAMES`.
fn display_name(driver: &str) -> &str {
    match driver {
        "antigravity" => "Antigravity",
        "codex" => "Codex",
        "claudeAgent" => "Claude",
        "cursor" => "Cursor",
        "grok" => "Grok",
        "fx" => "fx",
        "kiro" => "Kiro",
        "muse" => "Muse Code",
        "acpRegistry" => "ACP Registry",
        "pi" => "Pi",
        "opencode" => "OpenCode",
        other => other,
    }
}

fn update_field<'a>(provider: &'a Provider, key: &str) -> Option<&'a str> {
    provider.update_state.as_ref().and_then(|state| state[key].as_str())
}

fn status(provider: &Provider) -> Option<&str> {
    update_field(provider, "status")
}

fn finished_at(provider: &Provider) -> Option<&str> {
    update_field(provider, "finishedAt")
}

fn format_version(version: &str) -> String {
    if version.starts_with('v') { version.to_owned() } else { format!("v{version}") }
}

/// `formatProviderList`: "A", "A and B", "A, B, and C".
fn provider_list(providers: &[&Provider]) -> String {
    let names: Vec<&str> = providers.iter().map(|p| display_name(&p.driver)).collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

/// `dedupeProvidersByDriver`: per driver, the default instance, else the latest checked.
fn dedupe_by_driver<'a>(providers: impl Iterator<Item = &'a Provider>) -> Vec<&'a Provider> {
    let mut by_driver: Vec<&Provider> = Vec::new();
    for provider in providers {
        match by_driver.iter_mut().find(|p| p.driver == provider.driver) {
            None => by_driver.push(provider),
            Some(current) => {
                let keep_current = current.instance_id == current.driver
                    || (provider.instance_id != provider.driver
                        && provider.checked_at.as_deref().unwrap_or_default()
                            < current.checked_at.as_deref().unwrap_or_default());
                if !keep_current {
                    *current = provider;
                }
            }
        }
    }
    by_driver
}

/// `latestTerminalProvidersByDriver`: per driver, the instance that finished last.
fn latest_terminal_by_driver<'a>(providers: impl Iterator<Item = &'a Provider>) -> Vec<&'a Provider> {
    let mut by_driver: Vec<&Provider> = Vec::new();
    for provider in providers {
        match by_driver.iter_mut().find(|p| p.driver == provider.driver) {
            None => by_driver.push(provider),
            Some(current) => {
                if finished_at(provider).unwrap_or_default() >= finished_at(current).unwrap_or_default() {
                    *current = provider;
                }
            }
        }
    }
    by_driver
}

fn view_key(prefix: &str, providers: &[&Provider]) -> String {
    let mut parts: Vec<String> = providers
        .iter()
        .map(|p| {
            let message = update_field(p, "message").unwrap_or_default();
            format!("{}:{}:{message}", p.driver, finished_at(p).unwrap_or("pending"))
        })
        .collect();
    parts.sort();
    format!("{prefix}:{}", parts.join("|"))
}

/// `getProviderUpdateSidebarPillView`.
pub(super) fn pill_view(
    providers: &[Provider],
    visible_after: Option<&str>,
    dismissed: &HashSet<String>,
) -> Option<PillView> {
    let active = dedupe_by_driver(providers.iter().filter(|p| matches!(status(p), Some("queued" | "running"))));
    if let Some(first) = active.first() {
        let mut parts: Vec<String> =
            active.iter().map(|p| format!("{}:{}", p.driver, status(p).unwrap_or("idle"))).collect();
        parts.sort();
        let one = active.len() == 1;
        return Some(PillView {
            key: format!("loading:{}", parts.join("|")),
            tone: Tone::Loading,
            title: if one {
                format!("Updating {}", display_name(&first.driver))
            } else {
                format!("Updating {} providers", active.len())
            },
            description: if one {
                format!("{} update in progress.", provider_list(&active))
            } else {
                format!("{} updates are in progress.", provider_list(&active))
            },
            dismissible: false,
        });
    }
    let recent = latest_terminal_by_driver(providers.iter().filter(|p| {
        matches!(status(p), Some("failed" | "unchanged" | "succeeded"))
            && visible_after.is_none_or(|after| finished_at(p).is_some_and(|at| at >= after))
    }));
    let of =
        |wanted: &str| -> Vec<&Provider> { recent.iter().copied().filter(|p| status(p) == Some(wanted)).collect() };
    let (failed, unchanged, succeeded) = (of("failed"), of("unchanged"), of("succeeded"));
    // Each outcome with when its providers last finished; the newest shows first.
    let mut candidates: Vec<(PillView, String)> = Vec::new();
    let latest = |list: &[&Provider]| list.iter().filter_map(|p| finished_at(p)).max().unwrap_or_default().to_owned();
    if let Some(first) = failed.first() {
        let title = match failed.len() {
            1 => match first.version_advisory.as_ref().and_then(|a| a["latestVersion"].as_str()) {
                Some(version) => format!("{} {} update failed", display_name(&first.driver), format_version(version)),
                None => format!("{} update failed", display_name(&first.driver)),
            },
            n => format!("{n} provider updates failed"),
        };
        let description = match (failed.as_slice(), update_field(first, "message")) {
            ([_], Some(message)) if !message.is_empty() => message.to_owned(),
            _ => format!("{} failed to update. Check provider settings for details.", provider_list(&failed)),
        };
        let view =
            PillView { key: view_key("failed", &failed), tone: Tone::Error, title, description, dismissible: true };
        candidates.push((view, latest(&failed)));
    }
    if let Some(first) = unchanged.first() {
        let one = unchanged.len() == 1;
        let view = PillView {
            key: view_key("unchanged", &unchanged),
            tone: Tone::Warning,
            title: if one {
                format!("{} still needs an update", display_name(&first.driver))
            } else {
                format!("{} providers still need updates", unchanged.len())
            },
            description: format!(
                "{} {} outdated. Review provider settings for details.",
                provider_list(&unchanged),
                if one { "still appears" } else { "still appear" }
            ),
            dismissible: true,
        };
        candidates.push((view, latest(&unchanged)));
    }
    if let Some(first) = succeeded.first() {
        let one = succeeded.len() == 1;
        let title = match (one, first.version.as_deref()) {
            (true, Some(version)) => format!("{} updated: {}", display_name(&first.driver), format_version(version)),
            (true, None) => format!("{} updated", display_name(&first.driver)),
            (false, _) => format!("{} providers updated", succeeded.len()),
        };
        let description = if one {
            "New sessions will use the updated provider."
        } else {
            "New sessions will use the updated providers."
        };
        let view = PillView {
            key: view_key("succeeded", &succeeded),
            tone: Tone::Success,
            title,
            description: description.to_owned(),
            dismissible: false,
        };
        candidates.push((view, latest(&succeeded)));
    }
    candidates.sort_by(|(_, a), (_, b)| b.cmp(a));
    candidates.into_iter().map(|(view, _)| view).find(|view| !dismissed.contains(&view.key))
}

impl Workspace {
    /// The pill's view now, starting the success timer the first time a success shows.
    fn provider_pill_view(&mut self, cx: &mut Context<Self>) -> Option<PillView> {
        let providers = &self.server_info.providers;
        let state = &mut self.sidebar.provider_pill;
        // Outcomes from before this window opened don't show (`visibleAfterIso`).
        if state.visible_after.is_none() {
            state.visible_after = providers.iter().filter_map(|p| p.checked_at.clone()).max();
        }
        let view = pill_view(providers, state.visible_after.as_deref(), &state.dismissed)?;
        if view.tone == Tone::Success && state.timer_key.as_ref() != Some(&view.key) {
            let key = view.key.clone();
            state.timer_key = Some(key.clone());
            state._timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(SUCCESS_VISIBLE).await;
                let _ = this.update(cx, |this, cx| {
                    this.sidebar.provider_pill.dismissed.insert(key);
                    this.notify_sidebar(cx);
                });
            }));
        }
        Some(view)
    }

    pub(super) fn render_provider_pill(&mut self, tokens: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let view = self.provider_pill_view(cx)?;
        let sb = tokens.sidebar;
        let (bg, hover, fg): (Hsla, Hsla, Hsla) = match view.tone {
            Tone::Loading | Tone::Success => (tokens.secondary, sb.row_hover, sb.foreground),
            Tone::Warning => (tokens.warning.opacity(0.12), tokens.warning.opacity(0.18), tokens.warning),
            Tone::Error => (tokens.error.opacity(0.12), tokens.error.opacity(0.18), tokens.error),
        };
        let mark: AnyElement = match view.tone {
            Tone::Loading => Spinner::new().small().color(fg).into_any_element(),
            Tone::Success => Icon::new(IconName::CircleCheck).size(px(14.)).into_any_element(),
            Tone::Error => Icon::new(IconName::TriangleAlert).size(px(14.)).into_any_element(),
            Tone::Warning => Icon::new(IconName::Download).size(px(14.)).into_any_element(),
        };
        let description: SharedString = view.description.into();
        let key = view.key.clone();
        Some(
            h_flex()
                .min_h(px(28.))
                .w_full()
                .flex_none()
                .rounded(px(8.))
                .overflow_hidden()
                .text_size(rems(0.6875))
                .line_height(px(16.))
                .font_weight(FontWeight::MEDIUM)
                .bg(bg)
                .text_color(fg)
                .child(
                    h_flex()
                        .id("provider-update-pill")
                        .flex_1()
                        .min_w_0()
                        .gap(px(8.))
                        .px(px(8.))
                        .py(px(6.))
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .tooltip(move |window, cx| Tooltip::new(description.clone()).build(window, cx))
                        .on_click(cx.listener(|this, _, _, cx| this.open_settings("providers", cx)))
                        .child(div().flex_none().child(mark))
                        .child(div().min_w_0().child(view.title)),
                )
                .when_some(view.dismissible.then_some(key), |el, key| {
                    el.child(
                        div()
                            .id("provider-update-dismiss")
                            .mr(px(4.))
                            .size(px(20.))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .cursor_pointer()
                            .opacity(0.7)
                            .hover(|s| s.opacity(1.))
                            .tooltip(tooltip("Dismiss until provider status changes"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.sidebar.provider_pill.dismissed.insert(key.clone());
                                this.notify_sidebar(cx);
                            }))
                            .child(Icon::new(IconName::X).size(px(14.))),
                    )
                })
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provider(driver: &str, update: serde_json::Value) -> Provider {
        serde_json::from_value(json!({
            "instanceId": driver, "driver": driver, "enabled": true, "status": "ready",
            "version": "1.2.0", "checkedAt": "2026-11-05T10:00:00.000Z", "updateState": update,
        }))
        .unwrap()
    }

    #[test]
    fn a_running_update_shows_progress() {
        let providers = [provider("codex", json!({"status": "running"}))];
        let view = pill_view(&providers, None, &HashSet::new()).unwrap();
        assert_eq!(view.tone, Tone::Loading);
        assert_eq!(view.title, "Updating Codex");
        assert_eq!(view.description, "Codex update in progress.");
        let both =
            [provider("codex", json!({"status": "queued"})), provider("claudeAgent", json!({"status": "running"}))];
        let view = pill_view(&both, None, &HashSet::new()).unwrap();
        assert_eq!(
            (view.title.as_str(), view.description.as_str()),
            ("Updating 2 providers", "Codex and Claude updates are in progress.")
        );
    }

    #[test]
    fn the_latest_outcome_shows_until_dismissed_and_old_ones_stay_hidden() {
        let providers = [
            provider("codex", json!({"status": "succeeded", "finishedAt": "2026-11-05T10:05:00.000Z"})),
            provider(
                "claudeAgent",
                json!({"status": "failed", "finishedAt": "2026-11-05T10:04:00.000Z", "message": "npm failed"}),
            ),
        ];
        let view = pill_view(&providers, Some("2026-11-05T10:00:00.000Z"), &HashSet::new()).unwrap();
        assert_eq!((view.tone, view.title.as_str()), (Tone::Success, "Codex updated: v1.2.0"));
        let dismissed = HashSet::from([view.key]);
        let view = pill_view(&providers, Some("2026-11-05T10:00:00.000Z"), &dismissed).unwrap();
        assert_eq!(
            (view.tone, view.title.as_str(), view.description.as_str()),
            (Tone::Error, "Claude update failed", "npm failed")
        );
        assert!(view.dismissible);
        assert_eq!(pill_view(&providers, Some("2026-11-05T11:00:00.000Z"), &HashSet::new()), None);
    }
}
