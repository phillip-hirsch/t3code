//! The main column's 40px header: the "project / thread" breadcrumb, which
//! doubles as the window's drag region (`ChatView.tsx` header + `ChatHeader.tsx`).

use gpui_kit::component::input::Input;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Sizable as _, StyledExt as _, h_flex};
use gpui_kit::{
    AnyElement, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, WindowControlArea, canvas, div, px, rgb,
};

use super::titlebar::{CAPTION_BUTTONS_WIDTH, COLLAPSED_CONTENT_LEFT, RIGHT_RESIZE_INSET, TITLEBAR_HEIGHT, icon};
use super::{Target, Workspace};
use crate::theme;

impl Workspace {
    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *theme::tokens(cx);
        // Clears the caption buttons plus 12px, like `--workspace-native-controls-inset`.
        let controls_inset = CAPTION_BUTTONS_WIDTH + px(12.) - RIGHT_RESIZE_INSET;
        // An open right panel owns the titlebar controls, so the header only keeps `pr-10`.
        let panel_open = self.right_panel_open();
        let header = h_flex()
            .id("chat-header")
            .h(TITLEBAR_HEIGHT)
            .flex_none()
            .pl(if self.sidebar_open { px(20.) } else { COLLAPSED_CONTENT_LEFT })
            .pr(if panel_open { px(0.) } else { controls_inset })
            .bg(tokens.background)
            .window_control_area(WindowControlArea::Drag);
        if let Some(page) = self.page.filter(|page| !page.is_modal()) {
            return header.child(div().text_sm().font_medium().text_color(tokens.foreground).child(page.title()));
        }
        let (project, title, parent) = match &self.target {
            Some(Target::Thread(id)) => {
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
                (project, title, parent)
            }
            Some(Target::Draft { project_id }) => {
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

        let crumb = |id: &'static str| {
            h_flex()
                .id(id)
                .flex_none()
                .min_w_0()
                .gap(px(6.))
                .rounded(px(4.))
                .text_color(tokens.muted_foreground)
                .cursor_pointer()
                .occlude()
                .hover(|style| style.text_color(tokens.foreground))
        };
        let separator = || div().flex_none().text_color(tokens.muted_foreground).child("/");
        let mut breadcrumb =
            h_flex().flex_1().min_w_0().gap_3().pr(px(if panel_open { 40. } else { 96. })).text_sm().font_medium();
        if let Some(project) = project {
            // `ChatHeader.tsx` names the project by its title, not its repository.
            let name: SharedString = project.title.clone().into();
            let project_id = project.id.clone();
            let tooltip: SharedString = format!("New thread in {name}").into();
            breadcrumb = breadcrumb
                .child(
                    crumb("crumb-project")
                        .child(project_monogram(&project.title, tokens.dark))
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
                    crumb("crumb-parent")
                        .flex_shrink(1.)
                        .child(div().min_w_0().truncate().child(parent.title.clone()))
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.select_thread(parent_id.clone(), window, cx)),
                        ),
                )
                .child(separator());
        }
        let title = match (&self.target, &self.title.rename) {
            (Some(Target::Thread(_)), Some(rename)) => div()
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
            (Some(Target::Thread(_)), None) => self.render_title_button(title, &tokens, cx),
            _ => div().min_w(px(40.)).flex_1().truncate().text_color(tokens.foreground).child(title).into_any_element(),
        };
        header.child(breadcrumb.child(title)).children(self.render_title_menu())
    }

    /// The server thread's title: a menu on click (after the double-click interval), rename on double-click.
    fn render_title_button(&self, title: SharedString, tokens: &theme::Tokens, cx: &mut Context<Self>) -> AnyElement {
        let bounds = self.title.bounds.clone();
        h_flex()
            .id("thread-title")
            .group("thread-title")
            .relative()
            .min_w(px(40.))
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
                    .group_hover("thread-title", |style| style.visible())
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
                canvas(move |element_bounds, _, _| bounds.set(element_bounds), |_, _, _, _| {}).absolute().size_full(),
            )
            // Mouse-down, like the panel toggles: the drag region swallows the release.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui_kit::MouseDownEvent, window, cx| {
                    this.click_title(event.click_count, false, window, cx)
                }),
            )
            .into_any_element()
    }
}

/// `ProjectMonogram` at the breadcrumb's 14px: two letters on a 14% tint of the project's color.
fn project_monogram(project_title: &str, dark: bool) -> AnyElement {
    let (text, color) = project_identity(project_title);
    let color: Hsla = rgb(if dark { color.dark } else { color.light }).into();
    div()
        .size(px(14.))
        .flex_none()
        .rounded(px(3.5))
        .bg(color.opacity(0.14))
        .flex()
        .items_center()
        .justify_center()
        .font_family(theme::FONT_MONO)
        .font_weight(FontWeight::BOLD)
        .text_size(px(8.))
        .line_height(px(14.))
        .text_color(color)
        .child(text)
        .into_any_element()
}

struct ProjectColor {
    light: u32,
    dark: u32,
}

/// `PROJECT_ICON_COLORS` order (`apps/web/src/projectIconColors.ts`): Tailwind 600 / 400.
const PROJECT_COLORS: [ProjectColor; 18] = [
    ProjectColor { light: 0x4a5565, dark: 0x99a1af },
    ProjectColor { light: 0xe7000b, dark: 0xff6467 },
    ProjectColor { light: 0xf54900, dark: 0xff8904 },
    ProjectColor { light: 0xe17100, dark: 0xffb900 },
    ProjectColor { light: 0xd08700, dark: 0xfdc700 },
    ProjectColor { light: 0x5ea500, dark: 0x9ae600 },
    ProjectColor { light: 0x00a63e, dark: 0x05df72 },
    ProjectColor { light: 0x009966, dark: 0x00d492 },
    ProjectColor { light: 0x009689, dark: 0x00d5be },
    ProjectColor { light: 0x0092b8, dark: 0x00d3f2 },
    ProjectColor { light: 0x0084d1, dark: 0x00bcff },
    ProjectColor { light: 0x155dfc, dark: 0x51a2ff },
    ProjectColor { light: 0x4f39f6, dark: 0x7c86ff },
    ProjectColor { light: 0x7f22fe, dark: 0xa684ff },
    ProjectColor { light: 0x9810fa, dark: 0xc27aff },
    ProjectColor { light: 0xc800de, dark: 0xed6aff },
    ProjectColor { light: 0xe60076, dark: 0xfb64b6 },
    ProjectColor { light: 0xec003f, dark: 0xff637e },
];

/// `deriveProjectIdentity` in `apps/web/src/projectIdentity.ts`.
fn project_identity(project_title: &str) -> (SharedString, &'static ProjectColor) {
    let name = project_title.trim();
    let words: Vec<&str> = name.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()).collect();
    let monogram = match words.first() {
        None => "PR".to_owned(),
        Some(first) => {
            let mut glyphs = first.chars();
            let head = glyphs.next().unwrap_or('P');
            let second = glyphs
                .clone()
                .find(|glyph| glyph.is_numeric())
                .or_else(|| if words.len() > 1 { words.last()?.chars().next() } else { glyphs.last() })
                .unwrap_or(head);
            format!("{head}{second}").to_uppercase().chars().take(2).collect()
        }
    };
    let seed = name.to_lowercase();
    let seed = if seed.is_empty() { "project".to_owned() } else { seed };
    let index = seed.chars().fold(0usize, |index, glyph| (index * 31 + glyph as usize) % PROJECT_COLORS.len());
    (monogram.into(), &PROJECT_COLORS[index])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monograms_follow_the_web() {
        let monogram = |name| project_identity(name).0;
        assert_eq!(monogram("t3code"), "T3");
        assert_eq!(monogram("japan-trip-december-2026"), "J2");
        assert_eq!(monogram("examples"), "ES");
        assert_eq!(monogram(".claude"), "CE");
        assert_eq!(monogram("x"), "XX");
        assert_eq!(monogram("--"), "PR");
    }

    #[test]
    fn color_hash_matches_the_web() {
        // index = (index * 31 + codePoint) % 18 over "ab": (97 % 18) = 7, (7 * 31 + 98) % 18 = 9.
        assert_eq!(project_identity("AB").1.dark, PROJECT_COLORS[9].dark);
    }
}
