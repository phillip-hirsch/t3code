//! The composer (`ChatComposer.tsx`, `ComposerSurface.tsx`), the pending
//! approval rows above it (`TimelinePendingApprovalRow.tsx`) and the question
//! banner attached to it (`ComposerPendingUserInputPanel.tsx`).

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, BoxShadow, Context, ElementId, Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px, svg,
};
use serde_json::{Value, json};

use super::{Target, Workspace, new_id};
use crate::model::{RuntimeRequest, TurnItem};
use crate::model_label;
use crate::requests::{self, Question, QuestionDraft};
use crate::theme::{FONT_MONO, Tokens, tokens};

/// `max-h-52` (208px) over the 22.75px line.
pub(super) const EDITOR_MAX_ROWS: usize = 9;
const LINE_HEIGHT: f32 = 22.75;
const DEFAULT_RUNTIME_MODE: &str = "full-access";
const DEFAULT_PLACEHOLDER: &str = "Ask anything. @ files, $ skills, / commands";
/// Single-select picks advance after this, so the pick is seen first.
const AUTO_ADVANCE: Duration = Duration::from_millis(200);

/// `runtimeModeConfig.ts`.
fn runtime_modes() -> [(&'static str, &'static str, Lucide); 4] {
    [
        ("approval-required", "Supervised", Lucide::Lock),
        ("auto-accept-edits", "Auto-accept edits", Lucide::PenLine),
        ("auto", "Auto", Lucide::Sparkles),
        ("full-access", "Full access", Lucide::ShieldOff),
    ]
}

/// The editor's scrollbar, drawn only once the text overflows the 208px cap.
pub(super) fn editor_scrollbar() -> gpui_kit::base::input::EditorScrollbarRenderer {
    use gpui_kit::base::ScrollbarHandle as _;
    Rc::new(|context, _, _| {
        if context.content_size().height <= context.viewport_bounds().size.height + px(0.5) {
            return div().into_any_element();
        }
        gpui_kit::base::Scrollbar::vertical(&context).into_any_element()
    })
}

impl Workspace {
    pub(super) fn on_composer_change(&mut self, cx: &mut Context<Self>) {
        let empty = self.composer.read(cx).text().len() == 0;
        if empty != self.composer_empty {
            self.composer_empty = empty;
            cx.notify();
        }
    }

    /// `set_value` emits no `Change`, so this keeps `composer_empty` in step.
    pub(super) fn set_composer_text(
        &mut self,
        text: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = text.into();
        self.composer.update(cx, |composer, cx| composer.set_value(text, window, cx));
        self.on_composer_change(cx);
    }

    /// Opens a fresh draft when the first pending question changes.
    pub(super) fn sync_question(&mut self) {
        let projection = self.thread.as_ref().and_then(|t| t.state.projection.as_ref());
        let first = projection.and_then(|p| p.pending_user_inputs().into_iter().next());
        let Some((request, item)) = first else {
            self.question = None;
            return;
        };
        if self.question.as_ref().is_some_and(|q| q.request_id == request.id) {
            return;
        }
        let questions = requests::questions(item);
        self.question = (!questions.is_empty()).then(|| QuestionDraft::new(request.id.clone(), questions));
    }

    fn pending_question_request(&self) -> Option<(&RuntimeRequest, &TurnItem)> {
        let id = &self.question.as_ref()?.request_id;
        let projection = self.thread.as_ref()?.state.projection.as_ref()?;
        projection.pending_user_inputs().into_iter().find(|(request, _)| &request.id == id)
    }

    /// The composer text is the active question's custom answer.
    fn save_custom_answer(&mut self, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        let Some(draft) = &mut self.question else { return };
        let question = &draft.questions[draft.index];
        if text.is_empty() && !draft.drafts.contains_key(&question.id) {
            return;
        }
        let next = requests::set_custom(question, draft.drafts.get(&question.id), text);
        draft.drafts.insert(question.id.clone(), next);
    }

    fn load_custom_answer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = &self.question else { return };
        let question = &draft.questions[draft.index];
        let text = draft.drafts.get(&question.id).and_then(|d| d.custom.clone()).unwrap_or_default();
        self.set_composer_text(text, window, cx);
    }

    /// Next question, or submit on the last; a no-op while the active one is unanswered.
    pub(super) fn advance_question(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_custom_answer(cx);
        let Some(draft) = &mut self.question else { return };
        let question = &draft.questions[draft.index];
        if requests::answer(question, draft.drafts.get(&question.id)).is_none() {
            return;
        }
        if draft.index + 1 < draft.questions.len() {
            draft.index += 1;
            self.load_custom_answer(window, cx);
            cx.notify();
            return;
        }
        let Some(answers) = requests::answers(&draft.questions, &draft.drafts) else { return };
        let request_id = draft.request_id.clone();
        if self.responding.is_some() {
            return;
        }
        self.set_composer_text("", window, cx);
        self.respond(request_id, json!({"answers": answers}), cx);
    }

    fn previous_question(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_custom_answer(cx);
        let Some(draft) = &mut self.question else { return };
        draft.index = draft.index.saturating_sub(1);
        self.load_custom_answer(window, cx);
        cx.notify();
    }

    fn pick_option(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = &mut self.question else { return };
        let question = &draft.questions[draft.index];
        let next = requests::toggle(question, draft.drafts.get(&question.id), &value);
        let advance = !question.multi_select;
        draft.drafts.insert(question.id.clone(), next);
        let at = (draft.request_id.clone(), draft.index);
        self.set_composer_text("", window, cx);
        cx.notify();
        if advance {
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(AUTO_ADVANCE).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    if this.question.as_ref().is_some_and(|q| (q.request_id.clone(), q.index) == at) {
                        this.advance_question(window, cx);
                    }
                });
            })
            .detach();
        }
    }

    fn dismiss_question(&mut self, request_id: String, cx: &mut Context<Self>) {
        let Some(thread) = &self.thread else { return };
        let command = json!({"type": "thread.user-input.dismiss", "commandId": new_id(), "threadId": thread.id, "requestId": request_id});
        self.dispatch(command, cx);
    }

    /// Sending while an approval waits declines it: with the text as feedback
    /// when the provider takes feedback (returns true), else before the message goes out.
    pub(super) fn answer_approvals_with(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(projection) = self.thread.as_ref().and_then(|t| t.state.projection.as_ref()) else { return false };
        let live: Vec<(String, bool)> = projection
            .pending_approvals()
            .into_iter()
            .filter(|(request, _)| request.is_live())
            .map(|(request, _)| (request.id.clone(), request.supports_feedback()))
            .collect();
        match live.first() {
            None => false,
            Some((id, true)) => {
                let id = id.clone();
                self.set_composer_text("", window, cx);
                self.respond(id, json!({"decision": "decline", "feedback": text}), cx);
                true
            }
            Some(_) => {
                for (id, _) in live {
                    self.respond(id, json!({"decision": "decline"}), cx);
                }
                false
            }
        }
    }

    fn default_runtime_mode(&self) -> &str {
        self.server_info.default_runtime_mode.as_deref().unwrap_or(DEFAULT_RUNTIME_MODE)
    }

    pub(super) fn runtime_mode(&self) -> String {
        let mode = match &self.target {
            Some(Target::Thread(_)) => self
                .thread
                .as_ref()
                .and_then(|t| t.state.projection.as_ref())
                .and_then(|p| p.thread.runtime_mode.clone()),
            _ => self.runtime_mode_override.clone(),
        };
        mode.unwrap_or_else(|| self.default_runtime_mode().to_owned())
    }

    fn set_runtime_mode(&mut self, mode: &str, cx: &mut Context<Self>) {
        match &self.target {
            Some(Target::Thread(thread_id)) => {
                let command = json!({"type": "thread.runtime-mode.set", "commandId": new_id(), "threadId": thread_id, "runtimeMode": mode});
                self.dispatch(command, cx);
            }
            _ => self.runtime_mode_override = Some(mode.to_owned()),
        }
        cx.notify();
    }

    /// `ChatComposer.tsx` placeholder precedence.
    fn placeholder(&self, approval: Option<&RuntimeRequest>) -> &'static str {
        if let Some(approval) = approval {
            return if approval.supports_feedback() {
                "Send feedback to decline this request"
            } else {
                "Send a message to decline and redirect the agent"
            };
        }
        if let Some(draft) = &self.question {
            return if draft.questions[draft.index].allows_custom() {
                "Type your own answer, or leave this blank to use the selected option"
            } else {
                "Choose an option above"
            };
        }
        if self.target.is_none() {
            return "Choose a project above to start a thread";
        }
        if self.server_info.model_choices().is_empty() {
            return "Enable a provider in Settings to send a message";
        }
        DEFAULT_PLACEHOLDER
    }

    pub(super) fn render_composer(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (lane, _) = self.chat_layout(window);
        let t = *tokens(cx);
        let projection = self.thread.as_ref().and_then(|t| t.state.projection.as_ref());
        let approvals = projection.map(|p| p.pending_approvals()).unwrap_or_default();
        let first_approval = approvals.first().map(|(request, _)| *request);
        let placeholder = self.placeholder(first_approval);
        let outline = if t.dark { white(0.05) } else { black(0.08) };
        let surface = v_flex()
            .w_full()
            .rounded(px(16.))
            .border_1()
            .border_color(outline)
            // The glass fill over the canvas, opaque so the banner tucked under it stays hidden.
            .bg(t.background.blend(t.card.opacity(0.8)))
            .shadow(if t.dark {
                vec![BoxShadow::new(px(0.), px(1.), white(0.03)).inset()]
            } else {
                vec![BoxShadow::new(px(0.), px(1.), black(0.05)).blur_radius(px(2.))]
            })
            .child(
                div()
                    .relative()
                    .px(px(16.))
                    .pt(px(16.))
                    .pb(px(8.))
                    .child(
                        // xsmall pads the editor 4px a side, the web's `-m-1 p-1`.
                        Textarea::new(&self.composer)
                            .appearance(false)
                            .xsmall()
                            .text_size(px(14.))
                            .line_height(px(LINE_HEIGHT))
                            .mx(px(-4.))
                            .disabled(self.target.is_none()),
                    )
                    .when(self.composer_empty, |el| {
                        el.child(
                            div()
                                .absolute()
                                .top(px(16.))
                                .left(px(16.))
                                .right(px(16.))
                                .text_size(px(14.))
                                .line_height(px(LINE_HEIGHT))
                                .text_color(t.muted_foreground)
                                .truncate()
                                .child(placeholder),
                        )
                    }),
            )
            .child(self.render_footer(&t, cx));
        div().w_full().flex_none().pt(px(8.)).pl(px(lane.left)).pr(px(lane.right)).pb(px(20.)).child(
            v_flex()
                .w_full()
                .children(approvals.iter().map(|(request, item)| self.render_approval(request, *item, &t, cx)))
                .children(self.render_question(&t, cx))
                .child(surface),
        )
    }

    fn render_footer(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let attach = div()
            .id("attach")
            .size(px(28.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .opacity(0.64)
            .child(Icon::new(Lucide::Paperclip).with_size(px(16.)).text_color(t.muted_foreground).mt(px(1.)))
            .tooltip(|window, cx| Tooltip::new("Attach files").build(window, cx));
        let mode = self.runtime_mode();
        h_flex()
            .items_center()
            .px(px(8.))
            .pb(px(8.))
            .when(self.question.is_some(), |el| el.pt(px(8.)))
            .child(div().mr(px(8.)).child(attach))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .justify_end()
                            .gap(px(2.))
                            .when(mode != self.default_runtime_mode(), |el| {
                                el.child(self.render_mode_chip(&mode, t, cx))
                            })
                            .child(self.render_model_picker(t, cx)),
                    )
                    .child(self.render_primary_action(t, cx)),
            )
    }

    fn render_mode_chip(&self, mode: &str, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let (_, label, icon) = runtime_modes().into_iter().find(|(id, ..)| *id == mode).unwrap_or(runtime_modes()[3]);
        let view = cx.entity();
        let current = mode.to_owned();
        control_button("runtime-mode", t)
            .pl(px(6.))
            .child(Icon::new(icon).with_size(px(16.)).text_color(t.muted_foreground))
            .child(div().text_color(t.foreground.opacity(0.8)).child(label))
            .dropdown_menu_with_anchor(gpui_kit::Anchor::BottomRight, move |menu, window, _| {
                permission_items(menu, &current, &view, window)
            })
    }

    fn render_model_picker(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.composer_model();
        let provider = current.as_ref().and_then(|s| self.server_info.provider(&s.instance_id));
        let model = current.as_ref().and_then(|s| self.server_info.model(s));
        let driver = provider.map(|p| p.driver.as_str()).unwrap_or_default();
        let name: SharedString = match (&current, model) {
            (_, Some(model)) => model_label::trigger_model_name(model, driver).into(),
            (Some(selection), None) => selection.model.clone().into(),
            (None, None) => "Pick a model".into(),
        };
        let descriptors =
            model.and_then(|m| m.capabilities.as_ref()).map(|c| c.option_descriptors.as_slice()).unwrap_or(&[]);
        let effort = current.as_ref().and_then(|s| model_label::effort_label(descriptors, s));
        // Effort levels the picker offers for the current model.
        let levels: Option<(String, Vec<(String, String)>, Option<String>)> =
            model_label::reasoning(descriptors).filter(|d| d.kind == "select").map(|d| {
                let choices = d
                    .options
                    .iter()
                    .filter(|o| !d.prompt_injected_values.contains(&o.id))
                    .map(|o| (o.id.clone(), o.label.clone()))
                    .collect();
                let value = current
                    .as_ref()
                    .and_then(|s| model_label::current_value(d, s))
                    .and_then(|v| v.as_str().map(str::to_owned));
                (d.id.clone(), choices, value)
            });
        let icon = provider.and_then(|p| crate::assets::provider_icon(&p.driver));
        let choices = Rc::new(self.server_info.model_choices());
        let mode = self.runtime_mode();
        let view = cx.entity();
        let disabled = self.target.is_none() || choices.is_empty();
        control_button("model-picker", t)
            .min_w(px(52.))
            .disabled(disabled)
            .children(icon.map(|(path, tint)| {
                svg().path(path).size(px(16.)).flex_none().text_color(tint.unwrap_or(t.foreground))
            }))
            .child(div().max_w(px(224.)).truncate().text_color(t.foreground.opacity(0.8)).child(name))
            .children(
                effort.map(|effort| {
                    div().ml(px(-2.)).max_w(px(96.)).truncate().text_color(t.muted_foreground).child(effort)
                }),
            )
            .dropdown_menu_with_anchor(gpui_kit::Anchor::BottomRight, move |mut menu, window, _| {
                let mut provider_name: Option<&str> = None;
                for (label, selection) in choices.iter() {
                    let (group, model) = label.split_once(" · ").unwrap_or(("", label.as_ref()));
                    if provider_name != Some(group) {
                        if provider_name.is_some() {
                            menu = menu.separator();
                        }
                        menu = menu.label(group.to_owned());
                        provider_name = Some(group);
                    }
                    let picked = selection.clone();
                    let checked = current
                        .as_ref()
                        .is_some_and(|c| c.instance_id == picked.instance_id && c.model == picked.model);
                    menu = menu.item(PopupMenuItem::new(model.to_owned()).checked(checked).on_click(
                        window.listener_for(&view, move |this, _, _, cx| {
                            this.model_override = Some(picked.clone());
                            cx.notify();
                        }),
                    ));
                }
                if let (Some((id, levels, value)), Some(selection)) = (&levels, &current) {
                    menu = menu.separator().label("Effort");
                    for (level, label) in levels {
                        let picked = model_label::with_option(selection, id, Value::String(level.clone()));
                        menu = menu.item(
                            PopupMenuItem::new(label.clone()).checked(value.as_ref() == Some(level)).on_click(
                                window.listener_for(&view, move |this, _, _, cx| {
                                    this.model_override = Some(picked.clone());
                                    cx.notify();
                                }),
                            ),
                        );
                    }
                }
                permission_items(menu.separator(), &mode, &view, window).max_h(px(420.)).scrollable(true)
            })
    }

    fn render_primary_action(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let running = self.thread.as_ref().and_then(|t| t.state.projection.as_ref()).is_some_and(|p| p.is_running());
        let stop = || {
            round_button("stop", t.error.opacity(0.9), t.error)
                .hover(|s| s.bg(t.error))
                .child(div().size(px(8.)).rounded(px(1.5)).bg(t.primary_foreground))
                .tooltip(|window, cx| Tooltip::new("Interrupt").build(window, cx))
                .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
        };
        if let Some(draft) = &self.question {
            let responding = self.responding.as_ref() == Some(&draft.request_id);
            let can_respond =
                self.pending_question_request().is_some_and(|(r, _)| r.capability() != Some("not_resumable"));
            let last = draft.index + 1 == draft.questions.len();
            let question = &draft.questions[draft.index];
            let answered = !self.composer_empty && question.allows_custom()
                || requests::answer(question, draft.drafts.get(&question.id)).is_some();
            let ready = if last {
                draft.questions.iter().enumerate().all(|(i, q)| {
                    (i == draft.index && answered) || requests::answer(q, draft.drafts.get(&q.id)).is_some()
                })
            } else {
                answered
            };
            let label = match (responding, last) {
                (true, _) => "Submitting...",
                (false, false) => "Next question",
                (false, true) if draft.index > 0 => "Submit answers",
                (false, true) => "Submit answer",
            };
            let enabled = ready && can_respond && !responding;
            return h_flex()
                .gap(px(8.))
                .when(running, |el| el.child(stop()))
                .when(draft.index > 0, |el| {
                    el.child(
                        Button::new("previous-question")
                            .outline()
                            .small()
                            .label("Previous")
                            .disabled(responding)
                            .on_click(cx.listener(|this, _, window, cx| this.previous_question(window, cx))),
                    )
                })
                .child(
                    div()
                        .id("submit-answer")
                        .h(px(28.))
                        .px(px(16.))
                        .flex()
                        .items_center()
                        .rounded_full()
                        .bg(t.primary)
                        .text_color(t.primary_foreground)
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .shadow(action_shadow(t.primary))
                        .child(label)
                        .when(!enabled, |el| el.opacity(0.64))
                        .when(enabled, |el| {
                            el.cursor_pointer()
                                .hover(|s| s.bg(t.primary_hover))
                                .on_click(cx.listener(|this, _, window, cx| this.advance_question(window, cx)))
                        }),
                )
                .into_any_element();
        }
        if running && self.composer_empty {
            return stop().into_any_element();
        }
        let can_send = !self.composer_empty && self.target.is_some() && self.client().is_some();
        let (fill, icon) = if can_send || self.sending {
            (t.primary, t.primary_foreground)
        } else {
            (t.foreground.opacity(0.12), t.muted_foreground)
        };
        let content: AnyElement = if self.sending {
            Spinner::new().small().color(icon).into_any_element()
        } else if running {
            Icon::new(Lucide::CornerUpRight).with_size(px(16.)).text_color(icon).into_any_element()
        } else {
            svg().path("icons/app/send.svg").size(px(14.)).text_color(icon).into_any_element()
        };
        round_button("send", fill, t.primary)
            .child(content)
            .when(!can_send, |el| el.shadow(Vec::new()))
            .when(can_send, |el| {
                el.cursor_pointer()
                    .hover(|s| s.bg(t.primary_hover))
                    .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
            })
            .when(running && can_send, |el| el.tooltip(|window, cx| Tooltip::new("Click to steer").build(window, cx)))
            .into_any_element()
    }

    fn render_approval(
        &self,
        request: &RuntimeRequest,
        item: Option<&TurnItem>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (icon, label) = match request.kind.as_str() {
            "command" => (Lucide::Terminal, "Command approval"),
            "file-change" => (Lucide::SquarePen, "File change approval"),
            "file-read" => (Lucide::Eye, "File read approval"),
            "permission" => (Lucide::Wrench, "App permission approval"),
            _ => (Lucide::Hammer, "App access approval"),
        };
        let detail = item.and_then(|i| i.str("prompt")).filter(|p| !p.is_empty()).unwrap_or(label).to_owned();
        let app_name = item.and_then(|i| i.str("appName")).map(str::to_owned);
        let reason = item.and_then(|i| i.str("reason")).map(str::to_owned);
        let options: Vec<(String, String)> = item
            .and_then(|i| i.raw.get("options"))
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .filter_map(|o| {
                        Some((o.get("decision")?.as_str()?.to_owned(), o.get("label")?.as_str()?.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_else(|| {
                [
                    ("cancel", "Cancel"),
                    ("decline", "Decline"),
                    ("acceptForSession", "Always allow this session"),
                    ("accept", "Approve"),
                ]
                .map(|(d, l)| (d.to_owned(), l.to_owned()))
                .to_vec()
            });
        let enabled = request.is_live() && self.responding.as_ref() != Some(&request.id);
        let inline = |decision: &str, label: &str, tone: Hsla, cx: &mut Context<Self>| {
            let request_id = request.id.clone();
            let decision = decision.to_owned();
            div()
                .id(ElementId::Name(format!("approval-{}-{decision}", request.id).into()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(tone)
                .child(label.to_owned())
                .when(!enabled, |el| el.opacity(0.64))
                .when(enabled, |el| {
                    el.cursor_pointer().hover(|s| s.underline().text_color(t.foreground)).on_click(cx.listener(
                        move |this, _, _, cx| this.respond(request_id.clone(), json!({"decision": decision}), cx),
                    ))
                })
        };
        let primary: Vec<_> = options.iter().filter(|(d, _)| d == "decline" || d == "accept").collect();
        let more: Rc<Vec<(String, String)>> =
            Rc::new(options.iter().filter(|(d, _)| d != "decline" && d != "accept").cloned().collect());
        let mut actions = h_flex().flex_wrap().items_center().gap_x(px(8.)).gap_y(px(4.));
        if request.is_live() {
            actions = actions
                .child(
                    h_flex()
                        .gap(px(4.))
                        .text_color(t.warning)
                        .child(Icon::new(Lucide::Shield).with_size(px(12.)))
                        .child("Waiting for approval"),
                )
                .child(div().text_color(t.muted_foreground).child("·"));
            for (decision, label) in primary {
                let tone = if decision == "accept" { t.foreground } else { t.muted_foreground };
                actions = actions.child(inline(decision, label, tone, cx));
            }
            if !more.is_empty() {
                let view = cx.entity();
                let request_id = request.id.clone();
                actions = actions.child(
                    Button::new(ElementId::Name(format!("approval-more-{}", request.id).into()))
                        .ghost()
                        .xsmall()
                        .label("More")
                        .h(px(16.))
                        .px(px(0.))
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.muted_foreground)
                        .disabled(!enabled)
                        .dropdown_menu(move |mut menu, window, _| {
                            for (decision, label) in more.iter() {
                                let request_id = request_id.clone();
                                let decision = decision.clone();
                                menu = menu.item(PopupMenuItem::new(label.clone()).on_click(window.listener_for(
                                    &view,
                                    move |this, _, _, cx| {
                                        this.respond(request_id.clone(), json!({"decision": decision}), cx)
                                    },
                                )));
                            }
                            menu
                        }),
                );
            }
        } else {
            actions = actions.child(
                div().text_color(t.warning).child("Provider process is gone. Interrupt or restart the run to respond."),
            );
        }
        v_flex()
            .px(px(2.))
            .py(px(2.))
            .mb(px(8.))
            .child(
                h_flex()
                    .items_start()
                    .gap(px(6.))
                    .min_h(px(24.))
                    .child(
                        div()
                            .w(px(24.))
                            .h(px(20.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(icon).with_size(px(16.)).text_color(t.muted_foreground.opacity(0.7))),
                    )
                    .child(
                        div()
                            .id(ElementId::Name(format!("approval-detail-{}", request.id).into()))
                            .flex_1()
                            .min_w_0()
                            .max_h(px(160.))
                            .overflow_y_scroll()
                            .font_family(FONT_MONO)
                            .text_xs()
                            .line_height(px(20.))
                            .text_color(t.foreground)
                            .child(detail),
                    ),
            )
            .child(
                v_flex()
                    .ml(px(30.))
                    .gap(px(4.))
                    .pb(px(4.))
                    .text_xs()
                    .children(app_name.map(|a| div().text_color(t.muted_foreground).child(a)))
                    .children(reason.map(|r| div().text_color(t.muted_foreground).child(r)))
                    .child(actions),
            )
            .into_any_element()
    }

    fn render_question(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let draft = self.question.as_ref()?;
        let (request, item) = self.pending_question_request()?;
        let question = &draft.questions[draft.index];
        let disabled = request.capability() == Some("not_resumable") || self.responding.as_ref() == Some(&request.id);
        let dismissible = item.str("responseMode") == Some("message") || request.capability() == Some("message");
        let collapsed = draft.collapsed;
        let count = draft.questions.len();
        let header = h_flex()
            .id("question-toggle")
            .w_full()
            .min_h(px(24.))
            .px(px(16.))
            .pt(px(6.))
            .gap(px(4.))
            .rounded(px(12.))
            .cursor_pointer()
            .hover(|s| s.bg(t.foreground.opacity(0.05)))
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(draft) = &mut this.question {
                    draft.collapsed = !draft.collapsed;
                    cx.notify();
                }
            }))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .pl(px(6.))
                    .gap(px(4.))
                    .child(
                        div()
                            .min_w_0()
                            .when(collapsed, |el| el.max_w(gpui_kit::relative(0.5)))
                            .truncate()
                            .text_color(t.muted_foreground)
                            .child(question.header.clone()),
                    )
                    .when(collapsed, |el| {
                        el.child(div().flex_none().text_color(t.muted_foreground.opacity(0.4)).child("·")).child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(t.muted_foreground)
                                .child(question.question.clone()),
                        )
                    }),
            )
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .when(count > 1, |el| {
                        el.child(
                            div()
                                .min_w(px(24.))
                                .flex()
                                .justify_center()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(t.muted_foreground)
                                .child(format!("{}/{count}", draft.index + 1)),
                        )
                    })
                    .child(icon_box(t).child(
                        Icon::new(if collapsed { Lucide::ChevronUp } else { Lucide::ChevronDown }).with_size(px(14.)),
                    ))
                    .when(dismissible, |el| {
                        let request_id = request.id.clone();
                        el.child(
                            icon_box(t)
                                .id("question-dismiss")
                                .cursor_pointer()
                                .hover(|s| s.bg(t.accent))
                                .child(Icon::new(Lucide::X).with_size(px(14.)))
                                .tooltip(|window, cx| {
                                    Tooltip::new("Dismiss question without answering").build(window, cx)
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.dismiss_question(request_id.clone(), cx);
                                })),
                        )
                    }),
            );
        let body = (!collapsed).then(|| {
            let draft_answer = draft.drafts.get(&question.id);
            v_flex()
                .pl(px(22.))
                .pr(px(16.))
                .pb(px(4.))
                .child(
                    div()
                        .pt(px(8.))
                        .text_sm()
                        .line_height(px(LINE_HEIGHT))
                        .text_color(t.foreground.opacity(0.85))
                        .child(TextView::markdown(
                            ElementId::Name(format!("question-{}-{}", request.id, question.id).into()),
                            question.question.clone(),
                        )),
                )
                .child(v_flex().ml(px(-6.)).mt(px(6.)).children(question.options.iter().enumerate().map(
                    |(index, option)| {
                        let value = option.value().to_owned();
                        let selected = self.composer_empty && requests::is_selected(question, draft_answer, &value);
                        render_option(question, option, index, selected, disabled, t).when(!disabled, |el| {
                            el.on_click(
                                cx.listener(move |this, _, window, cx| this.pick_option(value.clone(), window, cx)),
                            )
                        })
                    },
                )))
        });
        let outline = if t.dark { white(0.05) } else { black(0.08) };
        Some(
            div()
                .mx(px(22.))
                .mb(px(-17.))
                .child(
                    v_flex()
                        .px(px(4.))
                        .pt(px(4.))
                        // 4px padding plus the 17px the composer covers.
                        .pb(px(21.))
                        .rounded_t(px(16.))
                        .border_1()
                        .border_color(outline)
                        .bg(t.card.opacity(0.8))
                        .text_xs()
                        .line_height(px(16.))
                        .shadow(if t.dark {
                            vec![
                                BoxShadow::new(px(0.), px(14.), black(0.75))
                                    .blur_radius(px(32.))
                                    .spread_radius(px(-18.)),
                            ]
                        } else {
                            vec![BoxShadow::new(px(0.), px(1.), black(0.05)).blur_radius(px(2.))]
                        })
                        .child(header)
                        .children(body),
                )
                .into_any_element(),
        )
    }
}

fn render_option(
    question: &Question,
    option: &requests::QuestionOption,
    index: usize,
    selected: bool,
    disabled: bool,
    t: &Tokens,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let indicator = if question.multi_select {
        div()
            .size(px(16.))
            .rounded(px(4.))
            .border_1()
            .flex()
            .items_center()
            .justify_center()
            .when(selected, |el| {
                el.bg(t.primary)
                    .border_color(t.primary)
                    .child(Icon::new(Lucide::Check).with_size(px(12.)).text_color(t.primary_foreground))
            })
            .when(!selected, |el| {
                el.border_color(t.input).bg(if t.dark { t.input.opacity(0.32) } else { t.background })
            })
    } else {
        div()
            .size(px(16.))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .when(selected, |el| el.bg(t.primary).child(div().size(px(6.)).rounded_full().bg(t.primary_foreground)))
            .when(!selected, |el| {
                el.border_1().border_color(t.input).bg(if t.dark { t.input.opacity(0.32) } else { t.background })
            })
    };
    h_flex()
        .id(ElementId::Name(format!("option-{}-{}", question.id, option.value()).into()))
        .w_full()
        .items_start()
        .gap(px(10.))
        .rounded(px(6.))
        .py(px(6.))
        .pl(px(6.))
        .text_color(if selected { t.foreground } else { t.foreground.opacity(0.7) })
        .when(!selected && !disabled, |el| el.hover(|s| s.bg(t.muted.opacity(0.3))))
        .when(disabled, |el| el.opacity(0.5))
        .when(!disabled, |el| el.cursor_pointer())
        .child(div().h(px(20.)).flex().items_center().flex_none().child(indicator))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(div().text_sm().line_height(px(20.)).font_weight(FontWeight::MEDIUM).child(option.label.clone()))
                .children(
                    option
                        .description
                        .clone()
                        .filter(|d| *d != option.label)
                        .map(|d| div().text_size(px(11.)).line_height(px(16.)).text_color(t.muted_foreground).child(d)),
                ),
        )
        .when(index < 9, |el| {
            el.child(
                div().w(px(24.)).h(px(20.)).flex_none().flex().items_center().justify_center().child(
                    div()
                        .h(px(20.))
                        .min_w(px(20.))
                        .px(px(4.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .bg(t.foreground.opacity(0.07))
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.muted_foreground)
                        .child(format!("{}", index + 1)),
                ),
            )
        })
}

/// `ComposerControl size="sm"`: 28px tall, 8px padding, 6px gap, text-sm medium.
fn control_button(id: &'static str, t: &Tokens) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .h(px(28.))
        .px(px(8.))
        .gap(px(6.))
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.foreground.opacity(0.8))
}

/// The 32px send/stop circle with its tinted shadow and inset highlight.
fn round_button(id: &'static str, fill: Hsla, shadow: Hsla) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .size(px(32.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(fill)
        .text_color(gpui_kit::white())
        .shadow(action_shadow(shadow))
}

fn action_shadow(tint: Hsla) -> Vec<BoxShadow> {
    vec![
        BoxShadow::new(px(0.), px(1.), tint.opacity(0.24)).blur_radius(px(2.)),
        BoxShadow::new(px(0.), px(1.), white(0.16)).inset(),
    ]
}

fn icon_box(t: &Tokens) -> gpui_kit::Div {
    div().size(px(24.)).flex().items_center().justify_center().rounded(px(8.)).text_color(t.foreground)
}

fn permission_items(menu: PopupMenu, current: &str, view: &Entity<Workspace>, window: &mut Window) -> PopupMenu {
    let mut menu = menu.label("Permissions");
    for (mode, label, _) in runtime_modes() {
        menu = menu.item(
            PopupMenuItem::new(label)
                .checked(mode == current)
                .on_click(window.listener_for(view, move |this, _, _, cx| this.set_runtime_mode(mode, cx))),
        );
    }
    menu
}

fn white(alpha: f32) -> Hsla {
    gpui_kit::hsla(0., 0., 1., alpha)
}

fn black(alpha: f32) -> Hsla {
    gpui_kit::hsla(0., 0., 0., alpha)
}
