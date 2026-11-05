use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, StyledExt as _, h_flex};
use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};

use super::{Target, Workspace, project_name};

impl Workspace {
    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (title, subtitle): (SharedString, Option<SharedString>) = match (&self.target, &self.thread) {
            (Some(Target::Thread(_)), Some(thread)) => match &thread.state.projection {
                Some(projection) => {
                    let branch = projection.thread.branch.clone();
                    (projection.thread.title.clone().into(), branch.map(Into::into))
                }
                None => ("Loading…".into(), None),
            },
            (Some(Target::Draft { project_id }), _) => {
                let project = self.shell.projects.iter().find(|p| &p.id == project_id);
                (format!("New thread in {}", project.map(project_name).unwrap_or_default()).into(), None)
            }
            _ => ("".into(), None),
        };
        h_flex()
            .h(px(44.))
            .flex_none()
            .px_4()
            .gap_3()
            .border_b_1()
            .border_color(theme.border)
            .child(div().min_w_0().truncate().font_medium().child(title))
            .children(subtitle.map(|s| div().flex_none().text_xs().text_color(theme.muted_foreground).child(s)))
    }

    pub(super) fn render_error(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let error = self.error.clone()?;
        let theme = cx.theme();
        Some(
            h_flex()
                .mx_4()
                .mt_2()
                .px_3()
                .py_2()
                .gap_2()
                .rounded_md()
                .bg(theme.danger.opacity(0.12))
                .text_color(theme.danger)
                .child(div().flex_1().min_w_0().child(error))
                .child(Button::new("dismiss-error").ghost().xsmall().icon(IconName::Close).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.error = None;
                        cx.notify();
                    },
                ))),
        )
    }
}
