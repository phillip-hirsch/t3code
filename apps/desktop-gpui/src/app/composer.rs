use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::{
    Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, prelude::FluentBuilder as _, px,
};

use gpui_kit::component::input::Textarea;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use serde_json::Value;

use super::{CONTENT_WIDTH, Connection, Workspace};
use crate::model::ModelSelection;

impl Workspace {
    pub(super) fn render_approvals(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let projection = self.thread.as_ref()?.state.projection.as_ref()?;
        let approvals = projection.pending_approvals();
        if approvals.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let mut column = v_flex().gap_2().w_full().max_w(px(CONTENT_WIDTH)).mx_auto().px_4().pb_2();
        for (request, item) in approvals {
            let prompt = item.and_then(|i| i.str("prompt").or(i.str("reason"))).unwrap_or_default().to_owned();
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
                .filter(|options: &Vec<_>| !options.is_empty())
                .unwrap_or_else(|| {
                    vec![
                        ("accept".into(), "Approve".into()),
                        ("acceptForSession".into(), "Approve for session".into()),
                        ("decline".into(), "Decline".into()),
                    ]
                });
            let live = request.is_live();
            let mut buttons = h_flex().gap_2().flex_wrap();
            for (decision, label) in options {
                let request_id = request.id.clone();
                let id = SharedString::from(format!("approval-{}-{decision}", request.id));
                let button = Button::new(id).small().label(label).disabled(!live);
                let button = if decision.starts_with("accept") { button.primary() } else { button.outline() };
                buttons = buttons.child(button.on_click(
                    cx.listener(move |this, _, _, cx| this.respond(request_id.clone(), decision.clone(), cx)),
                ));
            }
            column = column.child(
                v_flex()
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.warning.opacity(0.5))
                    .bg(theme.warning.opacity(0.08))
                    .child(div().font_medium().child(format!("Approval needed: {}", request.kind)))
                    .when(!prompt.is_empty(), |el| {
                        el.child(
                            div().font_family("monospace").text_xs().max_h(px(160.)).overflow_hidden().child(prompt),
                        )
                    })
                    .when(!live, |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("This request can no longer be answered here."),
                        )
                    })
                    .child(buttons),
            );
        }
        Some(column)
    }

    fn render_model_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label: SharedString = match self.composer_model() {
            Some(model) => self.server_info.model_name(&model).map(str::to_owned).unwrap_or(model.model).into(),
            None => "Pick a model".into(),
        };
        let choices: Rc<Vec<(SharedString, ModelSelection)>> = Rc::new(self.server_info.model_choices());
        let current = self.composer_model();
        let view = cx.entity();
        Button::new("model-picker")
            .ghost()
            .small()
            .label(label)
            .disabled(self.target.is_none() || choices.is_empty())
            .dropdown_menu(move |mut menu, window, _| {
                for (label, selection) in choices.iter() {
                    let picked = selection.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label.clone()).checked(current.as_ref() == Some(selection)).on_click(
                            window.listener_for(&view, move |this, _, _, cx| {
                                this.model_override = Some(picked.clone());
                                cx.notify();
                            }),
                        ),
                    );
                }
                menu.max_h(px(360.))
            })
    }

    pub(super) fn render_composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let running = self.thread.as_ref().and_then(|t| t.state.projection.as_ref()).is_some_and(|p| p.is_running());
        let can_send = self.target.is_some() && matches!(self.connection, Connection::Connected(_));
        let action = if running {
            Button::new("stop")
                .small()
                .danger()
                .icon(IconName::Square)
                .label("Stop")
                .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
        } else {
            Button::new("send")
                .small()
                .primary()
                .icon(IconName::ArrowUp)
                .label("Send")
                .loading(self.sending)
                .disabled(!can_send)
                .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
        };
        div().w_full().flex_none().px_4().pb_4().child(
            v_flex()
                .w_full()
                .max_w(px(CONTENT_WIDTH))
                .mx_auto()
                .gap_1()
                .p_2()
                .rounded_xl()
                .border_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .child(Textarea::new(&self.composer).appearance(false).disabled(self.target.is_none()))
                .child(h_flex().justify_between().child(self.render_model_picker(cx)).child(action)),
        )
    }
}
