//! Chat markdown extras (`ChatMarkdown.tsx`): the external link menu
//! (`externalLinkContextMenu.ts`), the table copy menu (`MarkdownTable.tsx`) and inline
//! skill chips (`SkillInlineText.tsx`), plus the popup menu they open at the pointer.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::text::{MarkdownNode, MarkdownParseContext, MarkdownPlugin};
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, ClipboardItem, Context, DismissEvent, Entity, Focusable as _, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, Point, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, WeakEntity, Window, anchored, deferred, div, px, rgb,
};

use super::Workspace;
use crate::theme::Tokens;

/// The skill a `t3-skill:` link names; [`skill_links`] writes them.
const SKILL_SCHEME: &str = "t3-skill:";

pub(super) struct ChatMenu {
    view: Entity<PopupMenu>,
    position: Point<Pixels>,
    _dismiss: Subscription,
}

impl Workspace {
    /// Opens `build`'s menu at `position` (window coordinates) until it dismisses.
    fn open_chat_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
        build: impl FnOnce(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu,
    ) {
        let view = PopupMenu::build(window, cx, build);
        let previous = window.focused(cx);
        view.focus_handle(cx).focus(window, cx);
        let dismiss = cx.subscribe_in(&view, window, move |this, _, _: &DismissEvent, window, cx| {
            this.chat_menu = None;
            if let Some(previous) = &previous {
                previous.focus(window, cx);
            }
            cx.notify();
        });
        self.chat_menu = Some(ChatMenu { view, position, _dismiss: dismiss });
        cx.notify();
    }

    pub(super) fn render_chat_menu(&self) -> Option<AnyElement> {
        let menu = self.chat_menu.as_ref()?;
        Some(
            deferred(anchored().position(menu.position).snap_to_window_with_margin(px(8.)).child(menu.view.clone()))
                .with_priority(3)
                .into_any_element(),
        )
    }

    /// `showExternalLinkContextMenu` for an http(s) link: integrated browser, system browser, copy.
    pub(super) fn open_link_menu(
        &mut self,
        url: &str,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !is_web_link(url) {
            return false;
        }
        let in_preview = self.preview_thread_id().map(str::to_owned);
        let view = cx.weak_entity();
        let url = url.to_owned();
        self.open_chat_menu(position, window, cx, move |mut menu, _, _| {
            if let Some(thread_id) = in_preview {
                let (view, url) = (view.clone(), url.clone());
                menu = menu.item(PopupMenuItem::new("Open in integrated browser").on_click(move |_, window, cx| {
                    let (thread_id, url) = (thread_id.clone(), url.clone());
                    let _ = view.update(cx, |w, cx| w.request_preview_tab(thread_id, Some(url), true, window, cx));
                }));
            }
            let open = url.clone();
            menu.item(PopupMenuItem::new("Open in system browser").on_click(move |_, _, cx| cx.open_url(&open))).item(
                PopupMenuItem::new("Copy Link")
                    .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(url.clone()))),
            )
        });
        true
    }

    /// A skill chip's popover (`SkillChipPopover`): its name, description and instructions.
    pub(super) fn open_skill_menu(
        &mut self,
        name: &str,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(skill) = self.markdown_skills().into_iter().find(|s| s.name == name) else { return };
        let description = skill
            .description
            .clone()
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| "No description is available for this skill.".into());
        self.open_chat_menu(position, window, cx, move |menu, _, _| {
            let (label, path) = (skill.label.clone(), skill.path.clone());
            let menu = menu.item(
                PopupMenuItem::element(move |_, _| {
                    v_flex()
                        .gap(px(12.))
                        .p(px(4.))
                        .max_w(px(320.))
                        .text_sm()
                        .child(div().font_weight(FontWeight::MEDIUM).child(label.clone()))
                        .child(div().child(description.clone()))
                })
                .disabled(true),
            );
            match path.filter(|p| !p.is_empty()) {
                // There is no file preview panel yet, so the instructions open where the system opens Markdown.
                Some(path) => menu.separator().item(
                    PopupMenuItem::new("View instructions")
                        .on_click(move |_, _, cx| cx.open_url(&format!("file:///{}", path.replace('\\', "/")))),
                ),
                None => menu,
            }
        });
    }

    /// The composer provider's user-invocable skills, as chips name them.
    fn markdown_skills(&self) -> Vec<InlineSkill> {
        let Some(provider) = self.composer_model().and_then(|s| self.server_info.provider(&s.instance_id).cloned())
        else {
            return Vec::new();
        };
        let cwd = self.git_cwd();
        let (skills, _) = crate::composer_search::provider_lists(&provider, cwd.as_deref());
        crate::composer_search::invocable(skills)
            .into_iter()
            .map(|skill| InlineSkill {
                name: skill.name.clone(),
                label: skill
                    .display_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .unwrap_or(&skill.name)
                    .to_owned(),
                description: skill.description.clone(),
                path: Some(skill.path.clone()),
            })
            .collect()
    }

    /// The markdown source a message parses: its `$skill` tokens become chip links.
    pub(super) fn with_skill_links(&self, text: &str) -> String {
        if !text.contains(|c: char| crate::composer_text::is_currency(c)) {
            return text.to_owned();
        }
        skill_links(text, &self.markdown_skills())
    }
}

#[derive(Clone, Debug)]
pub(super) struct InlineSkill {
    pub name: String,
    pub label: String,
    pub description: Option<String>,
    pub path: Option<String>,
}

/// `resolveExternalWebLink`: only http(s) links get the menu.
fn is_web_link(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://") || url.starts_with("//")
}

/// `matchInlineSkills` over prose: each `$name` of a known skill, outside code, becomes
/// `[Label](t3-skill:name)` for [`SkillChips`] to draw.
pub(super) fn skill_links(text: &str, skills: &[InlineSkill]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fenced = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            out.push_str(line);
            continue;
        }
        if fenced || line.starts_with("    ") || line.starts_with('\t') {
            out.push_str(line);
            continue;
        }
        // Backtick spans stay as written.
        for (index, part) in line.split('`').enumerate() {
            if index > 0 {
                out.push('`');
            }
            if index % 2 == 1 {
                out.push_str(part);
                continue;
            }
            let mut last = 0;
            // A token may end the part, as the web's `(?=\s|$)` allows.
            let probe = format!("{part} ");
            for (range, chip) in crate::composer_text::chips(&probe, |name| skills.iter().any(|s| s.name == name)) {
                let crate::composer_text::Chip::Skill { name } = chip else { continue };
                let Some(skill) = skills.iter().find(|s| s.name == name) else { continue };
                out.push_str(&part[last..range.start]);
                out.push_str(&format!("[{}]({SKILL_SCHEME}{})", skill.label.replace(']', "\\]"), skill.name));
                last = range.end;
            }
            out.push_str(&part[last..]);
        }
    }
    out
}

/// Draws `t3-skill:` links as the composer's skill chip; a click opens its popover.
pub(super) struct SkillChips {
    pub workspace: WeakEntity<Workspace>,
}

impl MarkdownPlugin for SkillChips {
    fn name(&self) -> &str {
        "t3-skill"
    }

    fn parse(
        &self,
        node: &gpui_kit::component::text::markdown_ast::Node,
        _: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        let gpui_kit::component::text::markdown_ast::Node::Link(link) = node else { return None };
        let name = link.url.strip_prefix(SKILL_SCHEME)?.to_owned();
        let label: String = link
            .children
            .iter()
            .map(|child| match child {
                gpui_kit::component::text::markdown_ast::Node::Text(text) => text.value.as_str(),
                _ => "",
            })
            .collect();
        Some(MarkdownNode::new("t3-skill", name.clone()).text(label).markdown(format!("${name}")))
    }

    fn render(&self, node: &MarkdownNode, _: &mut Window, _: &mut App) -> impl IntoElement {
        let name = node.data::<String>().cloned().unwrap_or_default();
        let workspace = self.workspace.clone();
        h_flex()
            .id(SharedString::from(format!("skill-chip:{name}")))
            .gap(px(3.))
            .rounded(px(4.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(0xb261be))
            .cursor_pointer()
            .child(Icon::new(IconName::Box).size(px(14.)).flex_none())
            .child(node.as_text().to_owned())
            .on_click(move |event, window, cx| {
                let position = event.position();
                let _ = workspace.update(cx, |w, cx| w.open_skill_menu(&name, position, window, cx));
            })
    }
}

/// `serializeTableElementToCsv`: cells with commas, quotes or newlines are quoted.
pub(super) fn table_csv(headers: &[String], rows: &[Vec<String>]) -> String {
    let cell = |value: &str| {
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.contains([',', '"', '\n']) {
            format!("\"{}\"", normalized.replace('"', "\"\""))
        } else {
            normalized
        }
    };
    std::iter::once(headers)
        .chain(rows.iter().map(Vec::as_slice))
        .filter(|row| !row.is_empty())
        .map(|row| row.iter().map(|c| cell(c)).collect::<Vec<_>>().join(","))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `MarkdownTable`'s copy button under each table: Copy as Markdown, Copy as CSV.
pub(super) fn table_actions(
    table: &gpui_kit::component::text::TableData,
    this: &WeakEntity<Workspace>,
    key: &SharedString,
    t: &Tokens,
    cx: &App,
) -> AnyElement {
    let id: SharedString = format!("table:{key}:{}", table.span.as_ref().map_or(0, |s| s.start)).into();
    let copied = this.upgrade().is_some_and(|w| w.read(cx).copy_feedback.id.as_ref() == Some(&id));
    let (markdown, csv) = (table.markdown.trim().to_owned(), table_csv(&table.headers, &table.rows));
    let this = this.clone();
    let muted = t.muted_foreground;
    h_flex()
        .mt(px(2.))
        .mx(px(-4.))
        .justify_end()
        .child(
            gpui_kit::component::button::Button::new("copy-table")
                .ghost()
                .xsmall()
                .icon(Icon::new(if copied { IconName::Check } else { IconName::Copy }).size(px(12.)))
                .tooltip(if copied { "Copied" } else { "Copy table" })
                .text_color(muted)
                .dropdown_menu(move |menu, _, _| {
                    let copy = |text: String, this: WeakEntity<Workspace>, id: SharedString| {
                        move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                            let _ = this.update(cx, |w, cx| w.copied(id.clone(), TABLE_COPIED_FOR, cx));
                        }
                    };
                    menu.item(PopupMenuItem::new("Copy as Markdown").on_click(copy(
                        markdown.clone(),
                        this.clone(),
                        id.clone(),
                    )))
                    .item(PopupMenuItem::new("Copy as CSV").on_click(copy(
                        csv.clone(),
                        this.clone(),
                        id.clone(),
                    )))
                }),
        )
        .into_any_element()
}

/// `useCopyToClipboard({ timeout: 1200 })`.
const TABLE_COPIED_FOR: std::time::Duration = std::time::Duration::from_millis(1200);

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, label: &str) -> InlineSkill {
        InlineSkill { name: name.into(), label: label.into(), description: None, path: None }
    }

    #[test]
    fn known_skills_outside_code_become_chip_links() {
        let skills = [skill("design", "Design"), skill("pdf:pdf", "PDF")];
        assert_eq!(
            skill_links("Use $design then $missing and $pdf:pdf.", &skills),
            "Use [Design](t3-skill:design) then $missing and $pdf:pdf."
        );
        assert_eq!(skill_links("Try `$design` here", &skills), "Try `$design` here");
        assert_eq!(
            skill_links("```\n$design \n```\n$design now", &skills),
            "```\n$design \n```\n[Design](t3-skill:design) now"
        );
        assert_eq!(skill_links("costs $5 and $10", &skills), "costs $5 and $10");
    }

    #[test]
    fn tables_copy_as_csv_like_the_web() {
        let headers = vec!["Step".to_owned(), "Result".to_owned()];
        let rows = vec![vec!["Read".to_owned(), "1, 2".to_owned()], vec!["Say \"hi\"".to_owned(), "a\n  b".to_owned()]];
        assert_eq!(table_csv(&headers, &rows), "Step,Result\nRead,\"1, 2\"\n\"Say \"\"hi\"\"\",a b");
    }
}
