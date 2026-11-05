use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::{
    App, Context, ElementId, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _, px,
};

use super::{Connection, Target, Workspace, project_name};
use crate::model::{ProjectShell, ThreadPill, ThreadShell};

impl Workspace {
    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (sidebar, sidebar_foreground, sidebar_border) = {
            let theme = cx.theme();
            (theme.sidebar, theme.sidebar_foreground, theme.sidebar_border)
        };
        let selected = match &self.target {
            Some(Target::Thread(id)) => Some(id.as_str()),
            _ => None,
        };
        let draft_project = match &self.target {
            Some(Target::Draft { project_id }) => Some(project_id.as_str()),
            _ => None,
        };
        let mut projects: Vec<&ProjectShell> = self.shell.projects.iter().filter(|p| p.archived_at.is_none()).collect();
        projects.sort_by(|a, b| b.pinned_at.is_some().cmp(&a.pinned_at.is_some()).then_with(|| a.title.cmp(&b.title)));

        let mut list = v_flex().gap_3().p_2();
        for project in projects {
            let project_id = project.id.clone();
            let mut group =
                v_flex().gap_0p5().child(
                    h_flex()
                        .justify_between()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(sidebar_foreground.opacity(0.7))
                        .child(div().truncate().child(project_name(project)))
                        .child(
                            Button::new(SharedString::from(format!("new-thread-{project_id}")))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Plus)
                                .tooltip("New thread")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.start_draft(project_id.clone(), window, cx)
                                })),
                        ),
                );
            if draft_project == Some(project.id.as_str()) {
                group = group.child(thread_row_frame("draft".into(), true, cx).child("New thread"));
            }
            for thread in self.shell.project_threads(&project.id) {
                group = group.child(self.render_thread_row(thread, selected == Some(thread.id.as_str()), cx));
            }
            list = list.child(group);
        }

        v_flex()
            .w(px(280.))
            .h_full()
            .flex_none()
            .bg(sidebar)
            .text_color(sidebar_foreground)
            .border_r_1()
            .border_color(sidebar_border)
            .child(h_flex().h(px(44.)).px_4().font_semibold().child("T3 Code"))
            .child(div().id("sidebar-scroll").flex_1().min_h_0().overflow_y_scroll().child(list))
            .child(self.render_connection(cx))
    }

    fn render_thread_row(&self, thread: &ThreadShell, selected: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let thread_id = thread.id.clone();
        let theme = cx.theme();
        let pill = match thread.pill() {
            ThreadPill::Approval => Some(("Approval", theme.warning)),
            ThreadPill::Input => Some(("Input", theme.info)),
            ThreadPill::Waiting => Some(("Waiting", theme.muted_foreground)),
            ThreadPill::Working => Some(("Working", theme.primary)),
            ThreadPill::Failed => Some(("Failed", theme.danger)),
            ThreadPill::Limited => Some(("Limited", theme.warning)),
            ThreadPill::PlanReady => Some(("Plan ready", theme.info)),
            ThreadPill::Idle => None,
        };
        thread_row_frame(SharedString::from(thread.id.clone()), selected, cx)
            .on_click(cx.listener(move |this, _, window, cx| this.select_thread(thread_id.clone(), window, cx)))
            .child(div().flex_1().min_w_0().truncate().child(thread.title.clone()))
            .children(pill.map(|(label, color)| status_badge(label, color)))
    }

    fn render_connection(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (label, color): (SharedString, Hsla) = match &self.connection {
            Connection::Starting => ("Starting server…".into(), theme.muted_foreground),
            Connection::Connecting => ("Connecting…".into(), theme.muted_foreground),
            Connection::Connected(_) if !self.shell.live => ("Syncing…".into(), theme.muted_foreground),
            Connection::Connected(_) => ("Connected".into(), theme.success),
            Connection::Disconnected(reason) => (format!("Reconnecting: {reason}").into(), theme.warning),
            Connection::Failed(reason) => (reason.clone(), theme.danger),
        };
        h_flex()
            .gap_2()
            .px_4()
            .py_2()
            .border_t_1()
            .border_color(theme.sidebar_border)
            .text_xs()
            .child(div().size(px(6.)).flex_none().rounded_full().bg(color))
            .child(div().min_w_0().truncate().text_color(theme.muted_foreground).child(label))
    }
}

fn thread_row_frame(id: SharedString, selected: bool, cx: &App) -> gpui_kit::Stateful<gpui_kit::Div> {
    let theme = cx.theme();
    h_flex()
        .id(ElementId::Name(id))
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .when(selected, |el| el.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground))
        .when(!selected, |el| el.hover(|style| style.bg(theme.sidebar_accent.opacity(0.5))))
}

fn status_badge(label: &'static str, color: Hsla) -> impl IntoElement {
    h_flex()
        .flex_none()
        .gap_1()
        .text_xs()
        .text_color(color)
        .child(div().size(px(6.)).rounded_full().bg(color))
        .child(label)
}
