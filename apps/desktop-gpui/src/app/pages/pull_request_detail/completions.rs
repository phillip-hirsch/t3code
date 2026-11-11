//! `PullRequestMarkdownTextarea`: the panel's composers complete `@login`, `#123` and GitLab
//! `!123` from the host's `pullRequests.filterOptions`, in a list under the textarea.

use std::collections::{HashMap, HashSet};

use gpui_kit::component::input::TextareaState;
use gpui_kit::{EntityId, Focusable as _, Keystroke, MouseButton, MouseDownEvent, anchored, deferred};

use super::avatars::avatar_or;
use super::*;
use crate::pull_request_completions::{self as logic, Completion, Kind, Support, Trigger};

/// `COMPLETION_SEARCH_DEBOUNCE_MS`.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(200);
/// `COMPLETION_SUGGESTION_LIMIT`.
const LIMIT: usize = 8;

/// The trigger open in one textarea and the host's answers for it.
pub(super) struct Completing {
    input: Entity<TextareaState>,
    trigger: Trigger,
    /// The highlighted row, for the query it was picked under.
    highlighted: (String, usize),
    /// The trigger's key once typing settles.
    settled: Option<String>,
    _debounce: Option<Task<()>>,
    answers: HashMap<String, Result<Value, String>>,
    in_flight: HashSet<String>,
    /// The latest answer, narrowed to what is typed while a newer search catches up.
    last: Option<Value>,
}

impl Completing {
    /// The `filterOptions` request the trigger stands for: its sigil and the settled search.
    fn request(&self) -> (String, Option<String>) {
        let sent = logic::debounced_search(self.trigger.sigil, self.settled.as_deref());
        (format!("{}{}", self.trigger.sigil, sent.as_deref().unwrap_or("")), sent)
    }

    fn view(&self) -> (Vec<Completion>, bool, bool) {
        let answer = self.answers.get(&self.request().0);
        let options = match answer {
            Some(Ok(value)) => Some(value),
            _ => self.last.as_ref(),
        };
        let pending = answer.is_none() || self.settled.as_deref() != Some(logic::key(&self.trigger).as_str());
        (logic::completions(&self.trigger, options, LIMIT), pending, matches!(answer, Some(Err(_))))
    }

    fn active(&self, count: usize) -> usize {
        if self.highlighted.0 == self.trigger.query { self.highlighted.1.min(count.saturating_sub(1)) } else { 0 }
    }
}

/// `EMPTY_STATE`: pending, failed and empty copy per kind.
fn empty_copy(kind: Kind, pending: bool, failed: bool) -> &'static str {
    match (kind, pending, failed) {
        (Kind::Mention, true, _) => "Searching users...",
        (Kind::Mention, _, true) => "Couldn't search users on the host",
        (Kind::Mention, ..) => "No matching users",
        (Kind::Reference, true, _) => "Searching...",
        (Kind::Reference, _, true) => "Couldn't search the host",
        (Kind::Reference, ..) => "No matching issues or change requests",
    }
}

/// `ReferenceStateIcon`.
fn reference_glyph(candidate: &Value, t: &Tokens) -> (Lucide, Hsla) {
    let open = tone(t, 0x009966, 0x5ee9b5, 0.9);
    let merged = tone(t, 0x7f22fe, 0xc4b4ff, 0.9);
    match (candidate["kind"].as_str(), candidate["state"].as_str()) {
        (Some("change-request"), Some("merged")) => (Lucide::GitMerge, merged),
        (Some("change-request"), Some("closed")) => (Lucide::GitPullRequestClosed, tone(t, 0xe7000b, 0xffa2a2, 0.9)),
        (Some("change-request"), _) => (Lucide::GitPullRequestArrow, open),
        (_, Some("open")) => (Lucide::CircleDot, open),
        _ => (Lucide::CircleCheck, merged),
    }
}

impl Workspace {
    fn pr_completion_support(&self) -> Option<Support> {
        let capabilities = &self.pull_requests.panel.as_ref()?.detail.as_ref()?.capabilities;
        Support {
            mentions: capabilities.mentions == Some(true),
            reference_sigils: capabilities.reference_sigils.clone(),
        }
        .any()
    }

    /// Re-reads the trigger at `input`'s caret, on every edit and caret move; `edited` clears an
    /// Escape that hid the list.
    pub(super) fn sync_pr_completion(
        &mut self,
        input: &Entity<TextareaState>,
        focused: bool,
        edited: bool,
        cx: &mut Context<Self>,
    ) {
        let support = self.pr_completion_support();
        let state = input.read(cx);
        let caret = state.selected_range();
        let trigger = support
            .filter(|_| focused && caret.is_empty())
            .and_then(|support| logic::detect(&state.value(), caret.end, &support));
        let id = input.entity_id();
        let Some(write) = self.pr_write() else { return };
        if edited {
            write.dismissed = None;
        }
        let trigger = trigger.filter(|t| write.dismissed != Some((id, t.range.start)));
        let held = write.completion.as_mut().filter(|c| c.input.entity_id() == id);
        let Some(trigger) = trigger else {
            if held.is_some() {
                write.completion = None;
                cx.notify();
            }
            return;
        };
        let key = logic::key(&trigger);
        match held {
            Some(held) if held.trigger == trigger => return,
            Some(held) if held.trigger.sigil == trigger.sigil => {
                let rekeyed = logic::key(&held.trigger) != key;
                held.trigger = trigger;
                if !rekeyed {
                    cx.notify();
                    return;
                }
            }
            _ => {
                write.completion = Some(Completing {
                    input: input.clone(),
                    trigger,
                    highlighted: (String::new(), 0),
                    settled: None,
                    _debounce: None,
                    answers: HashMap::new(),
                    in_flight: HashSet::new(),
                    last: None,
                });
            }
        }
        let Some(pr) = self.shown_pr_key() else { return };
        let owner = pr.clone();
        let debounce = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                let Some(completing) = this.pr_completion_of(&owner, id) else { return };
                completing.settled = Some(key);
                this.request_pr_completions(&owner, id, cx);
                cx.notify();
            });
        });
        if let Some(completing) = self.pr_completion_of(&pr, id) {
            completing._debounce = Some(debounce);
        }
        self.request_pr_completions(&pr, id, cx);
        cx.notify();
    }

    /// The completion open in `input` of the pull request `pr` names, shown or parked.
    fn pr_completion_of(&mut self, pr: &str, input: EntityId) -> Option<&mut Completing> {
        self.pr_panel(pr)?.write.completion.as_mut().filter(|c| c.input.entity_id() == input)
    }

    pub(super) fn close_pr_completion(&mut self, input: EntityId, cx: &mut Context<Self>) {
        let Some(write) = self.pr_write() else { return };
        if write.completion.as_ref().is_some_and(|c| c.input.entity_id() == input) {
            write.completion = None;
            cx.notify();
        }
    }

    /// Asks the host for the trigger's options, once per search; the answer lands on `pr`'s panel.
    fn request_pr_completions(&mut self, pr: &str, entity: EntityId, cx: &mut Context<Self>) {
        let Some(client) = self.pull_request_client_of(pr) else { return };
        let Some(panel) = self.pr_panel(pr) else { return };
        let project_id = panel.reference.project_id.clone();
        let Some(completing) = panel.write.completion.as_mut().filter(|c| c.input.entity_id() == entity) else {
            return;
        };
        let (key, sent) = completing.request();
        if completing.answers.contains_key(&key) || !completing.in_flight.insert(key.clone()) {
            return;
        }
        let mut input = match completing.trigger.kind {
            Kind::Mention => json!({"kind": "authors", "projectId": project_id}),
            Kind::Reference => {
                json!({"kind": "references", "projectId": project_id, "sigil": completing.trigger.sigil.to_string()})
            }
        };
        if let Some(sent) = sent {
            input["query"] = json!(sent);
        }
        let pr = pr.to_owned();
        cx.spawn(async move |this, cx| {
            let result = client.request("pullRequests.filterOptions", input).await;
            let _ = this.update(cx, |this, cx| {
                let Some(completing) = this.pr_completion_of(&pr, entity) else { return };
                completing.in_flight.remove(&key);
                if let Ok(value) = &result {
                    completing.last = Some(value.clone());
                }
                completing.answers.insert(key, result.map_err(|error| list_logic::error_message(&error)));
                cx.notify();
            });
        })
        .detach();
    }

    /// Escape, the arrows and a plain Enter or Tab while the list is open; `true` when handled.
    pub(in crate::app) fn pr_completion_key(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(write) = self.pull_requests.panel.as_mut().map(|p| &mut p.write) else { return false };
        let Some(completing) = write.completion.as_mut() else { return false };
        if !completing.input.focus_handle(cx).is_focused(window) {
            return false;
        }
        let m = keystroke.modifiers;
        if keystroke.key == "escape" {
            write.dismissed = Some((completing.input.entity_id(), completing.trigger.range.start));
            write.completion = None;
            cx.notify();
            return true;
        }
        let (list, ..) = completing.view();
        if list.is_empty() {
            return false;
        }
        let active = completing.active(list.len());
        match keystroke.key.as_str() {
            "up" | "down" if !m.control && !m.alt && !m.platform => {
                let step = if keystroke.key == "down" { 1 } else { list.len() - 1 };
                completing.highlighted = (completing.trigger.query.clone(), (active + step) % list.len());
                cx.notify();
                true
            }
            "enter" | "tab" if !m.shift && !m.control && !m.alt && !m.platform => {
                self.pick_pr_completion(&list[active].token, window, cx);
                true
            }
            _ => false,
        }
    }

    fn pick_pr_completion(&mut self, token: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(completing) = self.pr_write().and_then(|w| w.completion.take()) else { return };
        let trigger = completing.trigger;
        completing.input.update(cx, |state, cx| {
            let text = state.value().to_string();
            let (_, caret) = logic::insert(&text, &trigger, token);
            let spaced = text[trigger.range.end..].starts_with([' ', '\t']);
            state.set_selected_range(trigger.range.clone(), cx);
            state.replace(if spaced { token.to_owned() } else { format!("{token} ") }, window, cx);
            state.set_selected_range(caret..caret, cx);
        });
        cx.notify();
    }

    /// The suggestions under `input`, when its caret sits in a trigger.
    pub(super) fn render_pr_completions(
        &self,
        input: &Entity<TextareaState>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let completing = self.pull_requests.panel.as_ref()?.write.completion.as_ref()?;
        if completing.input.entity_id() != input.entity_id() {
            return None;
        }
        let (list, pending, failed) = completing.view();
        let kind = completing.trigger.kind;
        let active = completing.active(list.len());
        let query = completing.trigger.query.clone();
        let body: Vec<AnyElement> = if list.is_empty() {
            vec![
                div()
                    .px(px(4.))
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child(empty_copy(kind, pending, failed))
                    .into_any_element(),
            ]
        } else {
            list.into_iter()
                .enumerate()
                .map(|(index, completion)| {
                    let content: Vec<AnyElement> = match kind {
                        Kind::Mention => {
                            let actor: Actor = serde_json::from_value(completion.item.clone()).unwrap_or_default();
                            let initial = actor.login.chars().next().unwrap_or('?').to_uppercase().to_string();
                            let fallback = div()
                                .size(px(20.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .bg(t.muted)
                                .text_size(px(10.))
                                .child(initial);
                            let name = actor.name.clone().filter(|name| *name != actor.login);
                            let mut row = vec![
                                avatar_or(Some(&actor), 20., fallback).into_any_element(),
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(actor.login.clone())
                                    .into_any_element(),
                            ];
                            row.extend(name.map(|name| {
                                div().min_w_0().truncate().text_color(t.muted_foreground).child(name).into_any_element()
                            }));
                            row
                        }
                        Kind::Reference => {
                            let (icon, color) = reference_glyph(&completion.item, t);
                            vec![
                                Icon::new(icon).with_size(px(16.)).text_color(color).into_any_element(),
                                div()
                                    .flex_none()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(completion.token.clone())
                                    .into_any_element(),
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(t.muted_foreground)
                                    .child(completion.item["title"].as_str().unwrap_or_default().to_owned())
                                    .into_any_element(),
                            ]
                        }
                    };
                    let token = completion.token;
                    let hovered = query.clone();
                    h_flex()
                        .id(ElementId::Name(format!("pr-completion:{token}").into()))
                        .gap(px(8.))
                        .px(px(8.))
                        .py(px(6.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .text_sm()
                        .when(index == active, |el| el.bg(t.accent).text_color(t.foreground))
                        .children(content)
                        .on_mouse_move(cx.listener(move |this, _, _, cx| {
                            let Some(completing) = this.pr_write().and_then(|w| w.completion.as_mut()) else { return };
                            if completing.highlighted.1 != index || completing.highlighted.0 != hovered {
                                completing.highlighted = (hovered.clone(), index);
                                cx.notify();
                            }
                        }))
                        // Picks on press, so the textarea keeps focus and its caret.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.pick_pr_completion(&token, window, cx);
                            }),
                        )
                        .into_any_element()
                })
                .collect()
        };
        let owner = input.clone();
        let popup = v_flex()
            .id("pr-completions")
            // A click outside the list and its textarea closes it, even where nothing takes focus.
            .on_mouse_down_out(cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                if !owner.read(cx).input_bounds().contains(&event.position) {
                    this.close_pr_completion(owner.entity_id(), cx);
                }
            }))
            .w(px(if kind == Kind::Reference { 320. } else { 256. }))
            .mt(px(4.))
            .py(px(8.))
            .px(px(6.))
            .rounded(px(8.))
            .border_1()
            .border_color(t.border)
            .bg(t.popover)
            .shadow_lg()
            .children(body);
        // Above the comment popover it may open from.
        let priority = gpui_kit::base::POPUP_PRIORITY + 1;
        Some(
            deferred(anchored().snap_to_window_with_margin(px(8.)).child(popup))
                .with_priority(priority)
                .into_any_element(),
        )
    }
}
