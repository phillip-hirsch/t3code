//! The right panel's pull request surfaces (`RightPanelSurfaceContent` "pull-request" and
//! "pull-requests"): the thread's pull request in the detail panel the Pull requests page uses,
//! and the thread's linked pull requests (`ThreadPullRequestsPanel`).

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, ElementId, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use super::super::Workspace;
use super::super::right_panel::Surface;
use super::pull_requests::tone;
use crate::pull_request_detail::Reference;
use crate::theme::{self, Tokens};
use crate::thread_pull_requests::{self as links, PullRequestLink};

impl Workspace {
    fn open_thread_shell(&self) -> Option<&crate::model::ThreadShell> {
        let Some(super::super::Target::Thread(id)) = &self.target else { return None };
        self.shell.threads.iter().find(|t| &t.id == id)
    }

    /// `pullRequestPanelTarget`: the open thread's pull request, if it has one.
    pub(in crate::app) fn thread_pull_request_target(&self) -> Option<Reference> {
        let shell = self.open_thread_shell()?;
        links::panel_target(
            &shell.project_id,
            &shell.pull_requests,
            shell.linked_pull_request.as_ref(),
            shell.branch_pull_request.as_ref(),
        )
    }

    /// `pullRequestsSurfaceAvailable`: the thread links at least one pull request.
    pub(in crate::app) fn thread_links_pull_requests(&self) -> bool {
        self.open_thread_shell().is_some_and(|shell| links::visible(&shell.pull_requests).next().is_some())
    }

    /// The visible linked pull requests as sorted `[host, repository, number]` keys
    /// (`proactivePullRequestsKey`).
    pub(in crate::app) fn thread_pull_request_keys(&self) -> Vec<String> {
        let Some(shell) = self.open_thread_shell() else { return Vec::new() };
        let mut keys: Vec<String> = links::visible(&shell.pull_requests)
            .map(|link| serde_json::json!([link.host, link.repository, link.number]).to_string())
            .collect();
        keys.sort();
        keys
    }

    /// The Pull request surface: the detail panel, retargeted to this tab's pull request.
    pub(in crate::app) fn render_pull_request_surface(
        &mut self,
        reference: &Reference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.pull_requests.panel.as_ref().is_none_or(|panel| panel.reference() != reference) {
            let seed = self.pull_requests.entry_for(reference);
            self.open_pull_request_panel(reference.clone(), seed, cx);
        }
        self.render_pull_request_panel(window, cx)
    }

    /// The Linked pull requests surface.
    pub(in crate::app) fn render_linked_pull_requests(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let Some(shell) = self.open_thread_shell() else { return div().into_any_element() };
        let project_id = shell.project_id.clone();
        let visible: Vec<&PullRequestLink> = links::visible(&shell.pull_requests).collect();
        if visible.is_empty() {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .p_6()
                .text_center()
                .child(Icon::new(Lucide::Link).size(px(24.)).text_color(t.muted_foreground.opacity(0.6)))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("No linked pull requests"))
                .child(div().max_w(px(240.)).text_xs().text_color(t.muted_foreground).child(
                    "Pull requests the agent opens from this thread land here. Link one yourself from a URL or a number.",
                ))
                .into_any_element();
        }
        let open = visible.iter().filter(|l| l.snapshot.as_ref().is_none_or(|s| s.state == "open")).count();
        let rows: Vec<AnyElement> = visible.iter().map(|link| linked_row(link, &project_id, &t, cx)).collect();
        v_flex()
            .flex_1()
            .min_h_0()
            .child(v_flex().id("linked-pull-requests").flex_1().min_h_0().overflow_y_scroll().p(px(6.)).children(rows))
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .py(px(6.))
                    .border_t_1()
                    .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
                    .text_size(gpui_kit::rems(0.625))
                    .text_color(t.muted_foreground)
                    .child(format!("{open} open · {} linked", visible.len())),
            )
            .into_any_element()
    }
}

/// `SOURCE_LABELS`.
fn source_label(source: &str) -> &'static str {
    match source {
        "created" => "Created from this thread",
        "agent" => "Linked by the agent",
        "stack" => "Found in the stack",
        "stack-dismissed" => "Dismissed",
        _ => "Linked by you",
    }
}

fn state_glyph(link: &PullRequestLink, t: &Tokens) -> (Lucide, Hsla) {
    match link.snapshot.as_ref().map(|s| (s.state.as_str(), s.is_draft)) {
        None => (Lucide::GitPullRequest, t.muted_foreground),
        Some(("merged", _)) => (Lucide::GitMerge, tone(t, 0x7f22fe, 0xc4b4ff, 0.9)),
        Some(("closed", _)) => (Lucide::GitPullRequestClosed, tone(t, 0xe7000b, 0xffa2a2, 0.9)),
        Some((_, true)) => (Lucide::GitPullRequestDraft, tone(t, 0x71717b, 0x9f9fa9, 0.8)),
        Some(_) => (Lucide::GitPullRequestArrow, tone(t, 0x009966, 0x5ee9b5, 0.9)),
    }
}

/// A `LinkRow`: state, number, title and repository; it opens in the Pull request surface.
fn linked_row(link: &PullRequestLink, project_id: &str, t: &Tokens, cx: &mut Context<Workspace>) -> AnyElement {
    let (glyph, color) = state_glyph(link, t);
    let title: SharedString =
        link.snapshot.as_ref().map_or_else(|| link.repository.clone(), |s| s.title.clone()).into();
    let reference = Reference {
        project_id: project_id.to_owned(),
        host: Some(link.host.clone()),
        repository: link.repository.clone(),
        number: link.number,
    };
    let source: SharedString = source_label(&link.source).into();
    let hover = t.accent.opacity(0.6);
    h_flex()
        .id(ElementId::Name(format!("linked-pr:{}/{}#{}", link.host, link.repository, link.number).into()))
        .w_full()
        .gap_2()
        .py_1()
        .pl_2()
        .pr_1()
        .rounded(px(6.))
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .child(Icon::new(glyph).size(px(16.)).text_color(color))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            div()
                                .id(ElementId::Name(format!("linked-pr-number:{}", link.number).into()))
                                .flex_none()
                                .font_family(t.fonts.mono)
                                .text_xs()
                                .text_color(t.muted_foreground)
                                .child(format!("#{}", link.number))
                                .tooltip(move |window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(source.clone()).build(window, cx)
                                }),
                        )
                        .child(div().min_w_0().truncate().text_sm().child(title)),
                )
                .child(
                    div()
                        .truncate()
                        .font_family(t.fonts.mono)
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .when(link.snapshot.is_none(), |el| el.child(format!("{}/{}", link.host, link.repository)))
                        .when(link.snapshot.is_some(), |el| el.child(link.repository.clone())),
                ),
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            this.open_surface(Surface::PullRequest(reference.clone()), window, cx)
        }))
        .into_any_element()
}
