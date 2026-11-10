//! The main column's 40px header: the "project / thread" breadcrumb, which
//! doubles as the window's drag region (`ChatView.tsx` header + `ChatHeader.tsx`).

use gpui_kit::component::input::Input;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Sizable as _, StyledExt as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FocusHandle, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, canvas, div, px,
};

use super::titlebar::{self, CAPTION_BUTTONS_WIDTH, RIGHT_RESIZE_INSET, TITLEBAR_HEIGHT, icon};
use super::{Target, Workspace};
use crate::theme;

impl Workspace {
    pub(super) fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *theme::tokens(cx);
        // Clears the caption buttons plus 12px, like `--workspace-native-controls-inset`.
        let controls_inset = CAPTION_BUTTONS_WIDTH + px(12.) - RIGHT_RESIZE_INSET;
        // An open inline right panel owns the titlebar controls, so the header keeps only its `px-5`;
        // a sheet leaves them to the header (`inlineRightPanelOwnsTitleBar`).
        let panel_open = self.right_panel_docked_width(window).is_some()
            || (self.page == Some(super::pages::Page::PullRequests) && self.pull_request_panel_shown());
        let header = h_flex()
            .id("chat-header")
            .h(TITLEBAR_HEIGHT)
            .flex_none()
            .pl(if self.sidebar_open.docked() { px(20.) } else { titlebar::collapsed_content_left(window) })
            .pr(if panel_open { px(20.) } else { controls_inset })
            .bg(tokens.background);
        let header = titlebar::drag_region(header);
        if let Some(page) = self.page.filter(|page| !page.is_modal()) {
            return header.child(div().text_sm().font_medium().text_color(tokens.foreground).child(page.title()));
        }
        let mut link_origin: Option<SharedString> = None;
        let (project, title, parent) = match &self.target {
            Some(Target::Thread { id, .. }) => {
                let shell = self.shell.threads.iter().find(|thread| &thread.id == id);
                let projection = self.thread.as_ref().and_then(|t| t.state.projection.as_ref());
                let title: SharedString = shell
                    .map(|thread| thread.title.clone())
                    .or_else(|| projection.map(|p| p.thread.title.clone()))
                    .unwrap_or_default()
                    .into();
                let project = shell.and_then(|thread| self.shell.projects.iter().find(|p| p.id == thread.project_id));
                let parent = shell
                    .and_then(|thread| thread.lineage.parent_thread_id.as_ref())
                    .and_then(|parent| self.shell.threads.iter().find(|thread| &thread.id == parent));
                link_origin = shell.and_then(|thread| thread.link_origin.as_ref()).map(|o| o.label.clone().into());
                (project, title, parent)
            }
            Some(Target::Draft { project_id, .. }) => {
                (self.shell.projects.iter().find(|p| &p.id == project_id), "New thread".into(), None)
            }
            // `NoActiveThreadState`: a bordered header with a quiet label.
            None => {
                return header
                    .border_b_1()
                    .border_color(tokens.border)
                    .child(div().text_xs().text_color(tokens.muted_foreground.opacity(0.5)).child("No active thread"));
            }
        };

        // A breadcrumb button: a tab stop that Enter and Space activate, with the web's inset ring.
        let crumb = |id: &'static str, focus: &FocusHandle, cx: &mut Context<Self>| {
            h_flex()
                .id(id)
                .track_focus(focus)
                .relative()
                .flex_none()
                .min_w_0()
                .gap(px(6.))
                .rounded(px(4.))
                .text_color(tokens.muted_foreground)
                .cursor_pointer()
                .occlude()
                .hover(|style| style.text_color(tokens.foreground))
                .children(focus_ring(focus, window, tokens.ring))
                .on_mouse_down(MouseButton::Right, Self::header_context_menu(cx))
        };
        let [project_focus, parent_focus, _] = &self.title.crumbs;
        let separator = || div().flex_none().text_color(tokens.muted_foreground).child("/");
        // `WorkspaceBreadcrumb`'s nav clips its row inside the container's `pr-10` / `pr-24`.
        let mut breadcrumb = h_flex().flex_1().min_w_0().gap_3().overflow_hidden().text_sm().font_medium();
        if let Some(project) = project {
            // `ChatHeader.tsx` names the project by its title, not its repository.
            let name: SharedString = project.title.clone().into();
            let project_id = project.id.clone();
            let tooltip: SharedString = format!("New thread in {name}").into();
            breadcrumb = breadcrumb
                .child(
                    // `WorkspaceBreadcrumbItem className="shrink"`: the project gives way to the title's `min-w-10`.
                    crumb("crumb-project", project_focus, cx)
                        .flex_shrink(1.)
                        .child(super::sidebar::project_icon(project, &tokens, false, 14., self.favicon(project)))
                        .child(div().max_w(px(160.)).truncate().child(name))
                        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.start_draft(project_id.clone(), window, cx)),
                        ),
                )
                .child(separator());
        }
        if let Some(parent) = parent {
            let parent_id = parent.id.clone();
            breadcrumb = breadcrumb
                .child(
                    crumb("crumb-parent", parent_focus, cx)
                        .flex_shrink(1.)
                        .child(div().min_w_0().truncate().child(parent.title.clone()))
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.select_thread(parent_id.clone(), window, cx)),
                        ),
                )
                .child(separator());
        }
        let title = match (&self.target, &self.title.rename) {
            (Some(Target::Thread { .. }), Some(rename)) => div()
                .min_w(px(40.))
                .flex_1()
                .occlude()
                .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.cancel_rename(super::sidebar::MenuOrigin::Title, cx);
                    }
                }))
                .child(Input::new(&rename.input).small().text_color(tokens.foreground))
                .into_any_element(),
            (Some(Target::Thread { .. }), None) => self.render_title_button(title, &tokens, window, cx),
            _ => div().min_w(px(40.)).flex_1().truncate().text_color(tokens.foreground).child(title).into_any_element(),
        };
        // `Badge variant="info" size="sm"`: "From {origin}", hidden while renaming.
        let origin = link_origin.filter(|_| self.title.rename.is_none()).map(|label| {
            let tooltip: SharedString = format!("Started by an agent on {label} through a linked environment.").into();
            div()
                .id("thread-link-origin")
                .flex_none()
                .h(px(16.))
                .px(px(3.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .bg(tokens.info.opacity(if tokens.dark { 0.16 } else { 0.08 }))
                .text_color(tokens.info_foreground)
                .text_size(gpui_kit::rems(0.625))
                .line_height(px(10.))
                .font_weight(FontWeight::MEDIUM)
                .whitespace_nowrap()
                .child(format!("From {label}"))
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        });
        // `WorkspaceBreadcrumbItem current` (`min-w-10 flex-1`) in the clipped breadcrumb: the title
        // gives way first, then the chip is cut at the edge.
        let current = h_flex().min_w(px(40.)).flex_1().overflow_hidden().child(title).children(origin);
        let container = h_flex()
            .flex_1()
            .min_w_0()
            .gap_3()
            .pr(px(if panel_open { 40. } else { 96. }))
            .on_mouse_down(MouseButton::Right, Self::header_context_menu(cx));
        let share = self.render_guest_share_indicator(cx);
        header.child(container.child(breadcrumb.child(current)).children(share)).children(self.render_title_menu())
    }

    /// `onContextMenu` on the breadcrumb container. On Windows only its buttons get the click:
    /// the rest is the caption, where Windows shows the system menu as it does for Electron.
    fn header_context_menu(
        cx: &mut Context<Self>,
    ) -> impl Fn(&gpui_kit::MouseDownEvent, &mut Window, &mut gpui_kit::App) + 'static {
        cx.listener(|this, event: &gpui_kit::MouseDownEvent, window, cx| {
            cx.stop_propagation();
            this.context_click_header(event.position, window, cx);
        })
    }

    /// The server thread's title: a menu on click (after the double-click interval), rename on double-click.
    fn render_title_button(
        &self,
        title: SharedString,
        tokens: &theme::Tokens,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let bounds = self.title.bounds.clone();
        let focus = &self.title.crumbs[2];
        let quiet = &self.title.quiet_ring;
        if !focus.is_focused(window) {
            quiet.set(false);
        }
        let ring = focus_ring(focus, window, tokens.ring).filter(|_| !quiet.get());
        let chevron_shown = ring.is_some();
        h_flex()
            .id("thread-title")
            .group("thread-title")
            .track_focus(focus)
            .relative()
            .min_w_0()
            .flex_shrink(1.)
            .gap_1()
            .occlude()
            .cursor_pointer()
            .text_color(tokens.foreground)
            .child(div().min_w_0().truncate().child(title))
            .child(
                div()
                    .id("thread-title-chevron")
                    .flex_none()
                    .invisible()
                    .group_hover("thread-title", gpui_kit::Styled::visible)
                    .when(chevron_shown, gpui_kit::Styled::visible)
                    .child(icon("icons/chevron-down.svg").with_size(px(14.)).text_color(tokens.muted_foreground))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.click_title(1, true, window, cx);
                        }),
                    ),
            )
            .child(
                canvas(move |element_bounds, _, _| bounds.set(element_bounds), |_, (), _, _| {}).absolute().size_full(),
            )
            .children(ring)
            .on_key_down(cx.listener(|this, _, _, cx| {
                if this.title.quiet_ring.replace(false) {
                    cx.notify();
                }
            }))
            // Enter and Space open the menu at once, as a keyboard click does on the web.
            .on_click(cx.listener(|this, event: &gpui_kit::ClickEvent, window, cx| {
                if event.is_keyboard() {
                    this.click_title(1, true, window, cx);
                }
            }))
            .on_mouse_down(MouseButton::Right, Self::header_context_menu(cx))
            // Mouse-down, like the panel toggles: the drag region swallows the release.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui_kit::MouseDownEvent, window, cx| {
                    this.click_title(event.click_count, false, window, cx);
                }),
            )
            .into_any_element()
    }
}

/// `focus-visible:ring-2 ring-inset ring-ring`, drawn over the control so the text doesn't move.
fn focus_ring(focus: &FocusHandle, window: &Window, ring: gpui_kit::Hsla) -> Option<gpui_kit::Div> {
    (focus.is_focused(window) && window.last_input_was_keyboard())
        .then(|| div().absolute().inset_0().rounded(px(4.)).border_2().border_color(ring))
}
