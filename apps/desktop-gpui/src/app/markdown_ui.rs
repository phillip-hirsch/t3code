//! Chat markdown extras (`ChatMarkdown.tsx`): the external link menu
//! (`externalLinkContextMenu.ts`), the table copy menu (`MarkdownTable.tsx`) and inline
//! skill chips (`SkillInlineText.tsx`), plus the popup menu they open at the pointer.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::text::{MarkdownNode, MarkdownParseContext, MarkdownPlugin};
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, ClipboardItem, Context, DismissEvent, Entity, Focusable as _, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Point, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity, Window, anchored, deferred, div, px, rgb,
};

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use serde_json::json;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::Workspace;
use super::side_chat::{ChatPane, PaneOwner};
use crate::artifact_templates::{Kind as TemplateKind, Template};
use crate::file_links;
use crate::theme::Tokens;

/// The skill a `t3-skill:` link names; [`skill_links`] writes them.
const SKILL_SCHEME: &str = "t3-skill:";

pub(super) struct ChatMenu {
    view: Entity<PopupMenu>,
    position: Point<Pixels>,
    /// The pane and thread whose timeline opened it: it closes once they go, as unmounting does.
    owner: Option<PaneOwner>,
    _dismiss: Subscription,
}

impl Workspace {
    /// Opens `build`'s menu at `position` (window coordinates) until it dismisses, or until the
    /// thread `pane` shows (if a timeline's chip opened it) leaves that pane.
    pub(super) fn open_chat_menu(
        &mut self,
        pane: Option<ChatPane>,
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
        let owner = pane.and_then(|pane| self.pane_owner(pane));
        self.chat_menu = Some(ChatMenu { view, position, owner, _dismiss: dismiss });
        cx.notify();
    }

    /// Closes the timeline's link hover and chat menu once the pane and thread they belong to go.
    pub(super) fn release_left_chat_overlays(&mut self) {
        let menu_owner = self.chat_menu.as_ref().and_then(|menu| menu.owner.as_ref());
        if menu_owner.is_some_and(|owner| !self.owner_shown(owner)) {
            self.chat_menu = None;
        }
        self.release_left_link_hover();
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
        pane: Option<ChatPane>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !is_web_link(url) {
            return false;
        }
        let in_browser = self.preview_thread();
        let thread_link = in_browser.as_ref().and_then(|open| {
            let key = crate::model::ScopedId::new(open.environment.clone(), open.thread_id.as_str());
            let thread = self.shell.thread(&key)?;
            let env = &thread.environment_id;
            let supported =
                self.can_operate(env) && self.env_info(env).is_some_and(|info| info.capabilities.is_object());
            Some((key, pr_link_state(url, &thread.pull_requests).filter(|_| supported)?))
        });
        let view = cx.weak_entity();
        let url = url.to_owned();
        self.open_chat_menu(pane, position, window, cx, move |mut menu, _, _| {
            if let Some((thread, linked)) = thread_link {
                let (view, url) = (view.clone(), url.clone());
                let label = if linked { "Unlink from thread" } else { "Link to thread" };
                menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                    let _ = view.update(cx, |w, cx| w.change_pr_link_by_url(&thread, &url, !linked, window, cx));
                }));
            }
            if let Some(thread) = in_browser {
                let (view, url) = (view.clone(), url.clone());
                menu = menu.item(PopupMenuItem::new("Open in integrated browser").on_click(move |_, window, cx| {
                    let (thread, url) = (thread.clone(), url.clone());
                    let _ = view.update(cx, |w, cx| w.request_preview_tab(thread, Some(url), true, window, cx));
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

    /// `pullRequestLinking.changeLink`: links or unlinks the change request `url` names.
    fn change_pr_link_by_url(
        &mut self,
        thread: &crate::model::ScopedId,
        url: &str,
        linked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_operate(&thread.env) {
            return;
        }
        let (Some(client), Some(mut command)) =
            (self.env_client(&thread.env), pr_link_command(&thread.id, url, linked))
        else {
            return;
        };
        command["commandId"] = json!(super::new_id());
        let title = if linked { "Unable to link pull request" } else { "Unable to unlink pull request" };
        cx.spawn_in(window, async move |_, cx| {
            if let Err(error) = client.request("orchestration.dispatchCommand", command).await {
                let _ = cx.update(|window, cx| {
                    window.push_notification(Notification::error(error.to_string()).title(title), cx);
                });
            }
        })
        .detach();
    }

    /// A skill chip's popover (`SkillChipPopover`): its name, description and instructions.
    pub(super) fn open_skill_menu(
        &mut self,
        name: &str,
        pane: Option<ChatPane>,
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
        // The instructions open in the thread's file panel, on the machine the skill lives on.
        let path = skill.path.clone().filter(|p| !p.is_empty() && self.preview_thread_id().is_some());
        let view = cx.weak_entity();
        self.open_chat_menu(pane, position, window, cx, move |menu, _, _| {
            let label = skill.label.clone();
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
            match path {
                Some(path) => {
                    menu.separator().item(PopupMenuItem::new("View instructions").on_click(move |_, window, cx| {
                        let _ = view.update(cx, |w, cx| w.open_file(&path, None, window, cx));
                    }))
                }
                None => menu,
            }
        });
    }

    /// The composer provider's user-invocable skills, as chips name them.
    fn markdown_skills(&self) -> Vec<InlineSkill> {
        let Some(provider) = self.composer_model().and_then(|s| self.server_info().provider(&s.instance_id).cloned())
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

    /// The markdown source a message parses: its `$skill` tokens and the `model:` references
    /// this environment knows become chip links.
    pub(super) fn with_skill_links(&self, text: &str) -> String {
        if !may_hold_chips(text) {
            return text.to_owned();
        }
        let skills = if text.contains(crate::composer_text::is_currency) { self.markdown_skills() } else { Vec::new() };
        skill_links(text, &skills, |instance_id, model| {
            self.model_mention(instance_id, model).map(|m| (m.label, m.driver))
        })
    }

    /// A model chip's popover (`ModelMentionChip`): the model, its provider and the reference.
    pub(super) fn open_model_menu(
        &mut self,
        instance_id: &str,
        model: &str,
        pane: Option<ChatPane>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mention) = self.model_mention(instance_id, model) else { return };
        let source = crate::composer_text::model_mention(instance_id, model);
        let t = *crate::theme::tokens(cx);
        self.open_chat_menu(pane, position, window, cx, move |menu, _, _| {
            menu.item(
                PopupMenuItem::element(move |_, _| {
                    super::composer::model_mention_details(&mention, &source, &t).p(px(4.))
                })
                .disabled(true),
            )
        });
    }
}

/// `resolveMarkdownFileLinkTarget`: a link to a file on the host, as an absolute path that
/// keeps any `:line:column`. Web links, other schemes and fragments are not files.
pub(super) fn file_link_target(href: &str, cwd: Option<&str>) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') || href.starts_with("//") {
        return None;
    }
    let windows = |p: &str| {
        let b = p.as_bytes();
        b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/')
    };
    let path = match href.strip_prefix("file://") {
        Some(rest) => {
            let rest = rest.strip_prefix("localhost").unwrap_or(rest);
            if windows(rest.trim_start_matches('/')) {
                rest.trim_start_matches('/').to_owned()
            } else {
                rest.to_owned()
            }
        }
        None if windows(href) || href.starts_with('/') => href.to_owned(),
        None => {
            let scheme = href.find(':').is_some_and(|colon| {
                colon > 1 && href[..colon].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
            });
            if scheme || href.starts_with('~') {
                return None;
            }
            let root = cwd?;
            let separator = if windows(root) { '\\' } else { '/' };
            format!(
                "{}{separator}{}",
                root.trim_end_matches(['/', '\\']),
                href.trim_start_matches("./").replace('/', &separator.to_string())
            )
        }
    };
    let path = path.split(['?', '#']).next().unwrap_or_default().replace("%20", " ");
    (!path.is_empty()).then_some(path)
}

/// The path without its `:line:column` (`splitFilePathPosition`).
pub(super) fn without_position(target: &str) -> &str {
    let mut path = target;
    for _ in 0..2 {
        match path.rsplit_once(':') {
            Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) && head.len() > 2 => {
                path = head;
            }
            _ => break,
        }
    }
    path
}

/// `workspaceRelativeFilePath`: `target` under `cwd`, with forward slashes; `None` outside it.
pub(super) fn workspace_relative(target: &str, cwd: Option<&str>) -> Option<String> {
    let root = cwd?.trim_end_matches(['/', '\\']).replace('\\', "/");
    let target = target.replace('\\', "/");
    let rest = target.get(root.len()..)?.strip_prefix('/')?;
    if !target[..root.len()].eq_ignore_ascii_case(&root) {
        return None;
    }
    // `/repo/../x` starts with the root but escapes it; the server rejects it as a workspace path.
    let mut depth = 0_usize;
    for segment in rest.split('/') {
        match segment {
            ".." => depth = depth.checked_sub(1)?,
            "" | "." => {}
            _ => depth += 1,
        }
    }
    Some(rest.to_owned())
}

impl Workspace {
    /// A file link's primary action: open it in the preferred editor (`handleOpenInEditor`).
    pub(super) fn open_file_link(&mut self, target: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.preferred_editor().map(str::to_owned) else {
            window.push_notification(
                Notification::error("No available editor can open this file.").title("Unable to open file"),
                cx,
            );
            return;
        };
        self.shell_open(json!({"cwd": target, "editor": editor}), "Unable to open file", window, cx);
    }

    fn shell_open(
        &mut self,
        input: serde_json::Value,
        failure: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client() else { return };
        cx.spawn_in(window, async move |_, cx| {
            if let Err(error) = client.request("shell.openInEditor", input).await {
                let _ = cx.update(|window, cx| {
                    window.push_notification(Notification::error(error.to_string()).title(failure), cx);
                });
            }
        })
        .detach();
    }

    /// The label of the file manager reveal, when the server offers one (`markdownServerConfigFields`).
    fn reveal_label(&self) -> Option<&'static str> {
        let config = &self.server_info().config;
        let offered = config["shellRevealInFileManager"] == true
            && self.server_info().available_editors.iter().any(|e| e == "file-manager");
        if !offered {
            return None;
        }
        Some(match config["shellRevealInFileManagerKind"].as_str() {
            Some("finder") => "Reveal in Finder",
            Some("file-explorer") => "Reveal in File Explorer",
            Some(_) => "Reveal in Files",
            None => match self.server_info().platform_os.as_deref() {
                Some("darwin") => "Reveal in Finder",
                Some("windows") => "Reveal in File Explorer",
                _ => "Reveal in Files",
            },
        })
    }

    /// `onOpenInBrowser`: an HTML or PDF file of a thread, which has the integrated browser.
    fn can_open_file_in_browser(&self, target: &str) -> bool {
        self.preview_thread_id().is_some() && is_browser_preview_file(without_position(target))
    }

    /// A file link's click: a PDF opens in the integrated browser unless the platform modifier
    /// asks for the editor (`shouldUseMarkdownFileBrowserPrimaryAction`).
    pub(super) fn opens_file_link_in_browser(&self, target: &str, editor: bool) -> bool {
        let editor = editor && self.preferred_editor().is_some();
        !editor && self.can_open_file_in_browser(target) && opens_in_browser_by_default(without_position(target))
    }

    /// `showFileContextMenu`: open, reveal, and copy the relative or full path.
    pub(super) fn open_file_menu(
        &mut self,
        target: String,
        pane: Option<ChatPane>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = self.git_cwd();
        let relative = workspace_relative(without_position(&target), cwd.as_deref())
            .map(|rel| format!("{rel}{}", &target[without_position(&target).len()..]));
        let open_label: SharedString = match self.preferred_editor() {
            Some(editor) if editor != "file-manager" => {
                format!("Open in {}", super::details::editor_label(editor)).into()
            }
            _ => "Open in editor".into(),
        };
        let can_open = self.preferred_editor().is_some();
        let in_browser = self.can_open_file_in_browser(&target);
        let reveal = self.reveal_label();
        let view = cx.weak_entity();
        self.open_chat_menu(pane, position, window, cx, move |mut menu, _, _| {
            if can_open {
                let (view, target) = (view.clone(), target.clone());
                menu = menu.item(PopupMenuItem::new(open_label.clone()).on_click(move |_, window, cx| {
                    let _ = view.update(cx, |w, cx| w.open_file_link(&target, window, cx));
                }));
            }
            if in_browser {
                let (view, path) = (view.clone(), without_position(&target).to_owned());
                menu = menu.item(PopupMenuItem::new("Open in integrated browser").on_click(move |_, window, cx| {
                    let _ = view.update(cx, |w, cx| w.open_host_file_in_browser(&path, window, cx));
                }));
            }
            if let Some(label) = reveal {
                let (view, path) = (view.clone(), without_position(&target).to_owned());
                menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                    let input = json!({"cwd": path, "editor": "file-manager", "reveal": true});
                    let _ = view.update(cx, |w, cx| w.shell_open(input, "Unable to reveal file", window, cx));
                }));
            }
            let copy = |value: String, title: &'static str| {
                move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                    cx.write_to_clipboard(ClipboardItem::new_string(value.clone()));
                    window.push_notification(Notification::success(value.clone()).title(format!("{title} copied")), cx);
                }
            };
            if let Some(relative) = relative.clone() {
                menu = menu.item(PopupMenuItem::new("Copy relative path").on_click(copy(relative, "Relative path")));
            }
            menu.item(PopupMenuItem::new("Copy full path").on_click(copy(target, "Full path")))
        });
    }
}

#[derive(Clone, Debug)]
pub(super) struct InlineSkill {
    pub name: String,
    pub label: String,
    pub description: Option<String>,
    pub path: Option<String>,
}

/// Whether the thread links the change request `url` names, among its visible links
/// (`pullRequestLinking.isLinked`); `None` when `url` names none.
fn pr_link_state(url: &str, links: &[crate::thread_pull_requests::PullRequestLink]) -> Option<bool> {
    let (host, repository, number) = crate::pull_request_detail::parse_change_request_url(url)?;
    Some(crate::thread_pull_requests::visible(links).any(|link| {
        link.number == number
            && link.host.eq_ignore_ascii_case(&host)
            && link.repository.eq_ignore_ascii_case(&repository)
    }))
}

/// `planThreadPullRequestMutation` (multiple links), without its command id.
fn pr_link_command(thread_id: &str, url: &str, linked: bool) -> Option<serde_json::Value> {
    let (host, repository, number) = crate::pull_request_detail::parse_change_request_url(url)?;
    let repository = repository.to_lowercase();
    Some(if linked {
        json!({"type": "thread.pull-request.link", "threadId": thread_id, "host": host, "repository": repository,
            "number": number, "url": url, "source": "manual"})
    } else {
        json!({"type": "thread.pull-request.unlink", "threadId": thread_id, "host": host, "repository": repository,
            "number": number})
    })
}

/// The path without its query or fragment, lowercased.
fn bare_path(path: &str) -> String {
    path.split(['?', '#']).next().unwrap_or_default().to_lowercase()
}

/// `isBrowserPreviewFile`.
fn is_browser_preview_file(path: &str) -> bool {
    let path = bare_path(path);
    [".html", ".htm", ".pdf"].iter().any(|ext| path.ends_with(ext))
}

/// `shouldOpenMarkdownFileLinkInBrowserByDefault`.
fn opens_in_browser_by_default(path: &str) -> bool {
    bare_path(path).ends_with(".pdf")
}

/// `resolveExternalWebLink`: only http(s) links get the menu.
fn is_web_link(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://") || url.starts_with("//")
}

/// Whether `text` can name a skill or model chip at all.
pub(super) fn may_hold_chips(text: &str) -> bool {
    text.contains(crate::composer_text::is_currency) || text.contains("model:")
}

/// `matchInlineSkills` over prose: each `$name` of a known skill, outside code, becomes
/// `[Label](t3-skill:name)` for [`SkillChips`] to draw, and each `model:` reference `model`
/// names (as label and driver) becomes `[Label](t3-model:driver/instance/model)` for
/// [`ModelChips`] (`SkillInlineText`).
pub(super) fn skill_links(
    text: &str,
    skills: &[InlineSkill],
    model: impl Fn(&str, &str) -> Option<(String, String)>,
) -> String {
    let mut links: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    for prose in prose_ranges(text) {
        let part = &text[prose.clone()];
        // A token may end the text, as the web's `(?=\s|$)` allows.
        let probe = format!("{part} ");
        let at = |range: std::ops::Range<usize>| prose.start + range.start..prose.start + range.end;
        for (range, chip) in crate::composer_text::chips(&probe, |name| skills.iter().any(|s| s.name == name)) {
            let crate::composer_text::Chip::Skill { name } = chip else { continue };
            let Some(skill) = skills.iter().find(|s| s.name == name) else { continue };
            links.push((at(range), format!("[{}]({SKILL_SCHEME}{})", skill.label.replace(']', "\\]"), skill.name)));
        }
        for mention in crate::composer_text::model_mentions(part) {
            let Some((label, driver)) = model(&mention.instance_id, &mention.model) else { continue };
            let url = model_link(&driver, &mention.instance_id, &mention.model);
            links.push((at(mention.range), format!("[{}]({url})", escape_link_text(&label))));
        }
    }
    links.sort_by_key(|(range, _)| range.start);
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (range, link) in links {
        if range.start < last {
            continue;
        }
        out.push_str(&text[last..range.start]);
        out.push_str(&link);
        last = range.end;
    }
    out.push_str(&text[last..]);
    out
}

/// Source ranges of the prose text nodes the renderer draws, outside links, link references,
/// images, definitions, HTML and code, as the web leaves `a` and `code` children alone.
fn prose_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    use markdown::mdast::Node;
    fn walk(node: &Node, out: &mut Vec<std::ops::Range<usize>>) {
        match node {
            Node::Text(_) => out.extend(node.position().map(|p| p.start.offset..p.end.offset)),
            Node::Link(_)
            | Node::LinkReference(_)
            | Node::Image(_)
            | Node::ImageReference(_)
            | Node::Definition(_)
            | Node::Html(_)
            | Node::InlineCode(_)
            | Node::Code(_) => {}
            _ => node.children().into_iter().flatten().for_each(|child| walk(child, out)),
        }
    }
    // Unclaimed math renders as prose, so it parses without math, as the renderer re-parses it.
    let Ok(root) = markdown::to_mdast(text, &markdown::ParseOptions::gfm()) else { return Vec::new() };
    let mut out = Vec::new();
    walk(&root, &mut out);
    out
}

/// Draws `t3-skill:` links as the composer's skill chip; a click opens its popover.
pub(super) struct SkillChips {
    pub workspace: WeakEntity<Workspace>,
    /// The pane whose timeline draws them, which owns the popover they open.
    pub pane: ChatPane,
}

impl MarkdownPlugin for SkillChips {
    fn name(&self) -> &'static str {
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

    fn render(&self, node: &MarkdownNode, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let name = node.data::<String>().cloned().unwrap_or_default();
        let (workspace, pane) = (self.workspace.clone(), Some(self.pane));
        let tokens = crate::theme::tokens(cx);
        super::chat::focusable(h_flex().id(SharedString::from(format!("skill-chip:{name}"))), tokens)
            .gap(px(3.))
            .rounded(px(4.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(0xb261be))
            .cursor_pointer()
            .child(Icon::new(IconName::Box).size(px(14.)).flex_none())
            .child(node.as_text().to_owned())
            .on_click(move |event, window, cx| {
                let position = event.position();
                let _ = workspace.update(cx, |w, cx| w.open_skill_menu(&name, pane, position, window, cx));
            })
    }
}

/// `AssistantCitationChip`: a citation link draws as its quote chip; a click opens the quote
/// with Go to source. Copying keeps the citation's Markdown.
pub(super) struct CitationChips {
    pub workspace: WeakEntity<Workspace>,
    pub pane: ChatPane,
}

impl MarkdownPlugin for CitationChips {
    fn name(&self) -> &'static str {
        "t3-citation"
    }

    fn parse(
        &self,
        node: &gpui_kit::component::text::markdown_ast::Node,
        cx: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        let gpui_kit::component::text::markdown_ast::Node::Link(link) = node else { return None };
        let citation = crate::assistant_citations::parse_href(&link.url)?;
        let label = citation.label();
        let markdown = cx.node_source(node).map_or_else(|| citation.markdown(), str::to_owned);
        Some(MarkdownNode::new("t3-citation", citation).text(label).markdown(markdown))
    }

    fn render(&self, node: &MarkdownNode, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(citation) = node.data::<crate::assistant_citations::AssistantCitation>().cloned() else {
            return div().into_any_element();
        };
        let (workspace, pane) = (self.workspace.clone(), Some(self.pane));
        let id = format!("citation-chip:{}:{}:{}", citation.message_id, citation.start, citation.end);
        super::chat::focusable(h_flex().id(SharedString::from(id)), crate::theme::tokens(cx))
            .max_w_full()
            .gap(px(3.))
            .rounded(px(4.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(super::citations::CITATION_ACCENT))
            .cursor_pointer()
            .child(Icon::new(IconName::Quote).size(px(14.)).flex_none())
            .child(div().min_w_0().truncate().hover(gpui_kit::Styled::underline).child(node.as_text().to_owned()))
            .on_click(move |event, window, cx| {
                let (citation, position) = (citation.clone(), event.position());
                let _ = workspace.update(cx, |w, cx| w.open_citation_menu(citation, pane, position, window, cx));
            })
            .into_any_element()
    }
}

/// The model a `t3-model:` link names; [`skill_links`] writes them.
const MODEL_SCHEME: &str = "t3-model:";

/// `t3-model:{driver}/{instance}/{model}`, the model percent-encoded so any slug survives
/// Markdown's link destination rules; the driver gives the chip its look without a lookup.
fn model_link(driver: &str, instance_id: &str, model: &str) -> String {
    use std::fmt::Write as _;
    let mut url = format!("{MODEL_SCHEME}{driver}/{instance_id}/");
    for byte in model.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~:/".contains(&byte) {
            url.push(char::from(byte));
        } else {
            let _ = write!(url, "%{byte:02X}");
        }
    }
    url
}

/// The driver, instance and model of a [`model_link`].
fn parse_model_link(url: &str) -> Option<ModelChip> {
    let (driver, rest) = url.strip_prefix(MODEL_SCHEME)?.split_once('/')?;
    let (instance_id, encoded) = rest.split_once('/')?;
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut rest = encoded.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%' {
            let hex = std::str::from_utf8(tail.get(..2)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = &tail[2..];
        } else {
            bytes.push(byte);
            rest = tail;
        }
    }
    Some(ModelChip {
        driver: driver.to_owned(),
        instance_id: instance_id.to_owned(),
        model: String::from_utf8(bytes).ok()?,
    })
}

/// Link text that reads back as `text`: every ASCII punctuation mark escaped.
fn escape_link_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
struct ModelChip {
    driver: String,
    instance_id: String,
    model: String,
}

/// Draws `t3-model:` links as the composer's model chip (`ModelMentionChip`): the provider's
/// logo and the model's name; a click, Enter or Space opens its details.
pub(super) struct ModelChips {
    pub workspace: WeakEntity<Workspace>,
    pub pane: ChatPane,
}

impl MarkdownPlugin for ModelChips {
    fn name(&self) -> &'static str {
        "t3-model"
    }

    fn parse(
        &self,
        node: &gpui_kit::component::text::markdown_ast::Node,
        context: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        let gpui_kit::component::text::markdown_ast::Node::Link(link) = node else { return None };
        let chip = parse_model_link(&link.url)?;
        // Where the chip sits keeps two chips of one model apart, each with its own focus.
        let at = context.offset() + node.position().map_or(0, |position| position.start.offset);
        let label = link_text(&link.children);
        let source = crate::composer_text::model_mention(&chip.instance_id, &chip.model);
        Some(
            MarkdownNode::new("t3-model", (chip, at))
                .accessibility_label(format!("Model {label}"))
                .text(label)
                .markdown(source),
        )
    }

    fn render(&self, node: &MarkdownNode, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some((chip, at)) = node.data::<(ModelChip, usize)>().cloned() else { return div().into_any_element() };
        let accent = super::composer::model_chip_accent(&chip.driver);
        let (workspace, pane) = (self.workspace.clone(), Some(self.pane));
        let id = format!("model-chip:{at}:{}/{}", chip.instance_id, chip.model);
        let focus = window
            .use_keyed_state(SharedString::from(format!("{id}:focus")), cx, |_, cx| cx.focus_handle().tab_stop(true))
            .read(cx)
            .clone();
        // `focus-visible:outline-2 -outline-offset-2 outline-current`, drawn over the chip so the prose doesn't move.
        let ring = (focus.is_focused(window) && window.last_input_was_keyboard())
            .then(|| div().absolute().inset_0().rounded(px(4.)).border_2().border_color(accent));
        h_flex()
            .id(SharedString::from(id))
            .track_focus(&focus)
            .relative()
            .gap(px(3.))
            .rounded(px(4.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(accent)
            .cursor_pointer()
            .child(super::composer::model_chip_icon(&chip.driver, accent).size(px(14.)).flex_none())
            .child(node.as_text().to_owned())
            .children(ring)
            .on_click(move |event, window, cx| {
                let position = event.position();
                let _ = workspace
                    .update(cx, |w, cx| w.open_model_menu(&chip.instance_id, &chip.model, pane, position, window, cx));
            })
            .into_any_element()
    }
}

/// What a file chip opens: the path as written (with any position) and the path its icon reads.
struct FileChip {
    path: String,
    line: Option<u32>,
    icon_path: String,
    label: String,
    /// A descriptive link, whose own text flows as prose before the chip.
    after_text: bool,
}

/// The text of `nodes` and everything inside them (`nodeToPlainText`).
pub(super) fn link_text(nodes: &[gpui_kit::component::text::markdown_ast::Node]) -> String {
    use gpui_kit::component::text::markdown_ast::Node;
    nodes
        .iter()
        .map(|node| match node {
            Node::Strong(strong) => link_text(&strong.children),
            Node::Emphasis(emphasis) => link_text(&emphasis.children),
            Node::Delete(delete) => link_text(&delete.children),
            Node::InlineCode(code) => code.value.clone(),
            Node::Break(_) => " ".to_owned(),
            other => other.to_string(),
        })
        .collect()
}

/// A Markdown link to a file: its destination, and its text unless that text is the path.
fn file_link(text: &str, url: &str) -> Option<(file_links::FilePosition, Option<String>)> {
    let file = file_links::parse_file_link(url)?;
    let description = (!file_links::is_file_link_label(text, url)).then(|| text.trim().to_owned());
    Some((file, description.filter(|text| !text.is_empty())))
}

/// `MarkdownFileLink`: inline code that names a file, and a link to a file, draw as a chip with
/// the file's icon; a link with descriptive text keeps it before the chip, wrapping as prose. A
/// click previews the file in the panel (an editor with the platform modifier, or outside the
/// workspace); a right click opens the file menu.
pub(super) struct FileChips {
    pub workspace: WeakEntity<Workspace>,
    pub pane: ChatPane,
    pub dark: bool,
    /// `text-icon-muted`, the folder icon's color.
    pub folder_color: Hsla,
    pub cwd: Option<String>,
    pub kinds: Arc<EntryKinds>,
    pub suffixes: ParentSuffixes,
}

/// What `filesystem.getMetadata` said about chip targets (`useFileMetadata`), by absolute path.
#[derive(Default)]
pub(super) struct EntryKinds(Mutex<HashMap<String, EntryKind>>);

#[derive(Clone, Copy, PartialEq, Debug)]
enum EntryKind {
    Wanted,
    Asking,
    Known { directory: bool },
}

impl EntryKinds {
    /// Whether the server called `target` a directory; an unknown target is queued, and the
    /// return value says whether it is newly queued.
    fn directory(&self, target: &str) -> (bool, bool) {
        let mut kinds = self.0.lock().expect("entry kinds lock poisoned");
        match kinds.get(target) {
            Some(EntryKind::Known { directory }) => (*directory, false),
            Some(_) => (false, false),
            None => {
                kinds.insert(target.to_owned(), EntryKind::Wanted);
                (false, true)
            }
        }
    }

    /// The queued targets, now marked as being asked about.
    fn take_wanted(&self) -> Vec<String> {
        let mut kinds = self.0.lock().expect("entry kinds lock poisoned");
        let wanted: Vec<String> =
            kinds.iter().filter(|(_, kind)| **kind == EntryKind::Wanted).map(|(path, _)| path.clone()).collect();
        for path in &wanted {
            kinds.insert(path.clone(), EntryKind::Asking);
        }
        wanted
    }

    /// Records one batch's answer; entries follow request order and a failed request
    /// leaves every path a file, as the web's icon does without metadata.
    fn record(&self, paths: &[String], answer: Option<&serde_json::Value>) {
        let mut kinds = self.0.lock().expect("entry kinds lock poisoned");
        for (ix, path) in paths.iter().enumerate() {
            let directory = answer.is_some_and(|value| value["entries"][ix]["kind"] == "directory");
            kinds.insert(path.clone(), EntryKind::Known { directory });
        }
    }
}

/// The absolute host path `getFileMetadataQueryAtom` asks about for a chip, if any: never a
/// network path, which would leak the user's NTLM credentials to its host.
fn metadata_target(path: &str, cwd: Option<&str>) -> Option<String> {
    let target = file_links::resolve_target(without_position(path), cwd)?;
    let network = target.starts_with("\\\\") || target.starts_with("//");
    (!network && target.len() <= 512).then_some(target)
}

impl Workspace {
    /// Asks the server about queued chip targets, 64 at a time (`FILESYSTEM_METADATA_BATCH_LIMIT`).
    fn ask_entry_kinds(&mut self, cx: &mut Context<Self>) {
        let kinds = self.entry_kinds.clone();
        let wanted = kinds.take_wanted();
        if wanted.is_empty() {
            return;
        }
        let Some(client) = self.client() else {
            kinds.record(&wanted, None);
            return;
        };
        cx.spawn(async move |this, cx| {
            for paths in wanted.chunks(64) {
                let answer = client.request("filesystem.getMetadata", json!({ "paths": paths })).await.ok();
                kinds.record(paths, answer.as_ref());
            }
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }
}

/// The parent suffixes of the source last parsed (`buildFileLinkParentSuffixByPath`), so its
/// chips share one scan of the message.
#[derive(Default)]
pub(super) struct ParentSuffixes(Mutex<Option<ParsedSuffixes>>);

struct ParsedSuffixes {
    source: String,
    suffixes: Arc<HashMap<String, String>>,
}

impl ParentSuffixes {
    fn of(&self, source: &str) -> Arc<HashMap<String, String>> {
        let mut cached = self.0.lock().expect("parent suffixes lock poisoned");
        if let Some(parsed) = cached.as_ref().filter(|parsed| parsed.source == source) {
            return parsed.suffixes.clone();
        }
        let paths = file_links::document_paths(source);
        let suffixes = Arc::new(file_links::parent_suffixes(paths.iter().map(String::as_str)));
        *cached = Some(ParsedSuffixes { source: source.to_owned(), suffixes: suffixes.clone() });
        suffixes
    }
}

impl MarkdownPlugin for FileChips {
    fn name(&self) -> &'static str {
        "t3-file"
    }

    fn parse(
        &self,
        node: &gpui_kit::component::text::markdown_ast::Node,
        cx: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        use gpui_kit::component::text::markdown_ast::Node;
        let (path, file, after_text) = match node {
            Node::InlineCode(code) => {
                let candidate = file_links::inline_code_candidate(&code.value)?;
                let file = file_links::split_position(&candidate, "");
                (candidate, file, false)
            }
            Node::Link(link)
                if !link.url.starts_with(SKILL_SCHEME)
                    && !link.url.starts_with(MODEL_SCHEME)
                    && !link.url.starts_with("t3-thread:") =>
            {
                let (file, description) = file_link(&link_text(&link.children), &link.url)?;
                (file_links::with_position(&file), file, description.is_some())
            }
            _ => return None,
        };
        let label = file_links::chip_label(&file, &self.suffixes.of(cx.source()));
        let markdown = cx.node_source(node).unwrap_or_default().to_owned();
        // The space the web puts between a link's text and its chip.
        let text = if after_text { format!(" {label}") } else { label.clone() };
        let node =
            MarkdownNode::new("t3-file", FileChip { path, line: file.line, icon_path: file.path, label, after_text })
                .text(text)
                .markdown(markdown);
        Some(if after_text { node.after_children() } else { node })
    }

    fn render(&self, node: &MarkdownNode, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(chip) = node.data::<FileChip>() else { return div().into_any_element() };
        let target = metadata_target(&chip.path, self.cwd.as_deref());
        let (directory, queued) = target.as_deref().map_or((false, false), |target| self.kinds.directory(target));
        if queued {
            let workspace = self.workspace.clone();
            cx.defer(move |cx| {
                let _ = workspace.update(cx, Workspace::ask_entry_kinds);
            });
        }
        let (asset, icon_color) = crate::file_icons::file_icon(&chip.icon_path, self.dark);
        // `PierreEntryIcon`: a directory has no Pierre icon, so it takes Lucide's folder.
        let icon = if directory {
            Icon::new(IconName::Folder).text_color(self.folder_color)
        } else {
            Icon::empty().path(asset).text_color(icon_color)
        };
        let ink = document_ink(asset).unwrap_or_else(|| rgb(0x0096af));
        let (path, line) = (chip.path.clone(), chip.line);
        let (open, menu) = (self.workspace.clone(), self.workspace.clone());
        let (menu_path, pane) = (path.clone(), self.pane);
        let open_menu = std::rc::Rc::new(move |position: Point<Pixels>, window: &mut Window, cx: &mut App| {
            let _ = menu.update(cx, |w, cx| {
                let target = file_links::resolve_target(&menu_path, w.git_cwd().as_deref());
                if let Some(target) = target {
                    w.with_workspace_match(pane, &menu_path, target, window, cx, move |w, target, window, cx| {
                        w.open_file_menu(target, Some(pane), position, window, cx);
                    });
                }
            });
        });
        let key_menu = open_menu.clone();
        let tooltip: SharedString = path.clone().into();
        let element = super::chat::focusable(
            h_flex().id(SharedString::from(format!("file-chip:{path}"))),
            crate::theme::tokens(cx),
        )
        .max_w_full()
        .gap(px(3.))
        .rounded(px(4.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ink)
        .cursor_pointer()
        .child(icon.size(px(14.)).flex_none())
        .child(div().min_w_0().truncate().hover(gpui_kit::Styled::underline).child(chip.label.clone()))
        .tooltip(move |window, cx| code_tooltip(tooltip.clone(), window, cx))
        .on_click(move |event, window, cx| {
            let editor = event.modifiers().secondary(cx);
            let _ = open.update(cx, |w, cx| w.open_file_chip(pane, &path, line, editor, window, cx));
        })
        .on_mouse_down(gpui_kit::MouseButton::Right, move |event, window, cx| {
            cx.stop_propagation();
            open_menu(event.position, window, cx);
        })
        .on_context_menu_key(move |position, window, cx| key_menu(position, window, cx));
        // The link's text flows before the chip as plain prose; only the chip opens the file.
        if chip.after_text { element.ml(px(4.)).into_any_element() } else { element.into_any_element() }
    }
}

/// `documentChipKindForFileName`: a document's kind ink for its icon asset, `None` for others.
pub(super) fn document_ink(asset: &str) -> Option<gpui_kit::Rgba> {
    Some(match asset.rsplit('/').next().unwrap_or_default() {
        "t3-file-icon-pdf.svg" => rgb(0xd55755),
        "t3-file-icon-word.svg" => rgb(0x368bd3),
        "t3-file-icon-spreadsheet.svg" => rgb(0x449c56),
        "t3-file-icon-slides.svg" => rgb(0xc26e27),
        "t3-file-icon-epub.svg" => rgb(0x8871de),
        _ => return None,
    })
}

/// `TooltipPopup variant="code"`: monospace 11px, relaxed leading, wrapping anywhere within
/// `max-w-120` (480px, less the popup's `px-2` and border), so a long path keeps its file name.
fn code_tooltip(text: SharedString, window: &mut Window, cx: &mut App) -> gpui_kit::AnyView {
    let mono = crate::theme::tokens(cx).fonts.mono;
    gpui_kit::component::tooltip::Tooltip::element(move |_, _| {
        v_flex()
            .max_w(px(480. - 18.))
            .font_family(mono)
            .text_size(px(11.))
            .line_height(gpui_kit::relative(1.625))
            .child(text.clone())
    })
    .build(window, cx)
}

impl Workspace {
    /// `handleOpenInFilePreview`: a file in the workspace, or a host file or folder outside it,
    /// opens in the thread's panel; the platform modifier, or host media, opens the preferred editor.
    pub(super) fn open_file_chip(
        &mut self,
        pane: ChatPane,
        path: &str,
        line: Option<u32>,
        editor: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = file_links::resolve_target(path, self.git_cwd().as_deref()) else { return };
        self.with_workspace_match(pane, path, target, window, cx, move |this, target, window, cx| {
            if this.opens_file_link_in_browser(&target, editor) {
                this.open_host_file_in_browser(without_position(&target), window, cx);
                return;
            }
            let path = without_position(&target);
            let panel_path = workspace_relative(path, this.git_cwd().as_deref())
                .or_else(|| super::files::opens_host_path_in_panel(path).then(|| path.to_owned()));
            match panel_path {
                Some(panel_path) if !editor && matches!(this.target, Some(super::Target::Thread { .. })) => {
                    this.open_file(&panel_path, line, window, cx);
                }
                _ => this.open_file_link(&target, window, cx),
            }
        });
    }
}

/// A thread link's ids and its written text, when that is a label and not the link itself.
struct ThreadChip {
    link: crate::thread_links::ThreadLink,
    label: Option<String>,
}

/// `MarkdownThreadLink`: a thread link draws as the thread's chip, named by its current title
/// and opening it; a link no thread answers to, once the server says so, draws as an unresolved
/// chip.
pub(super) struct ThreadChips {
    pub workspace: WeakEntity<Workspace>,
}

impl MarkdownPlugin for ThreadChips {
    fn name(&self) -> &'static str {
        "t3-thread"
    }

    fn parse(
        &self,
        node: &gpui_kit::component::text::markdown_ast::Node,
        cx: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        let gpui_kit::component::text::markdown_ast::Node::Link(link) = node else { return None };
        let parsed = crate::thread_links::parse(&link.url)?;
        let written = link_text(&link.children);
        // An autolink's text is its own URL, which is no label.
        let label = (!written.is_empty() && crate::thread_links::parse(&written).is_none()).then_some(written);
        let text = label.clone().unwrap_or_else(|| "Thread".to_owned());
        let markdown = cx.node_source(node).unwrap_or_default().to_owned();
        Some(MarkdownNode::new("t3-thread", ThreadChip { link: parsed, label }).text(text).markdown(markdown))
    }

    fn render(&self, node: &MarkdownNode, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(chip) = node.data::<ThreadChip>() else { return div().into_any_element() };
        thread_chip(&chip.link, chip.label.clone(), &self.workspace, cx)
    }
}

/// `MarkdownThreadLink` and `ThreadContextChip`: the thread's project icon and title, which
/// opens it; a thread not here draws dashed with why. A thread the shell leaves out is asked of
/// its server (`threadLinkMetadata`), outside render.
pub(super) fn thread_chip(
    link: &crate::thread_links::ThreadLink,
    label: Option<String>,
    workspace: &WeakEntity<Workspace>,
    cx: &mut App,
) -> AnyElement {
    let chip = ThreadChip { link: link.clone(), label };
    let Some(workspace) = workspace.upgrade() else { return div().into_any_element() };
    let tokens = *crate::theme::tokens(cx);
    let message_environment = workspace.read(cx).active_environment().cloned();
    let stale = workspace.read(cx).stale_thread_link_lookup(&chip.link, message_environment.as_deref());
    if let Some((environment, ids)) = stale {
        let workspace = workspace.downgrade();
        cx.defer(move |cx| {
            let _ = workspace.update(cx, |w, cx| w.ask_thread_link_metadata(environment, ids, cx));
        });
    }
    let w = workspace.read(cx);
    let target = w.resolve_thread_link(&chip.link, message_environment.as_deref());
    let id = SharedString::from(format!(
        "thread-chip:{}:{}",
        chip.link.environment_id.as_deref().unwrap_or_default(),
        chip.link.thread_id
    ));
    let row = h_flex().id(id).max_w_full().gap(px(3.)).rounded(px(4.)).font_weight(FontWeight::MEDIUM);
    let LinkTarget::Thread { environment, thread_id, title, project_id } = target else {
        let unknown = crate::thread_links::unknown_label(&chip.link, message_environment.as_deref());
        let label: SharedString = chip.label.clone().map_or_else(|| unknown.into(), Into::into);
        return row
            .child(Icon::new(IconName::CircleDashed).size(px(14.)).flex_none())
            .child(div().min_w_0().truncate().child(label))
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(unknown).build(window, cx))
            .into_any_element();
    };
    let project = project_id
        .and_then(|project_id| w.shell.projects.iter().find(|p| p.id == project_id && p.environment_id == environment));
    let icon = match project {
        Some(project) => super::sidebar::project_icon(project, &tokens, false, 14., w.favicon(project)),
        None => Icon::new(IconName::MessagesSquare).size(px(14.)).flex_none().into_any_element(),
    };
    let title: SharedString = match title.as_deref().map(str::trim) {
        Some(title) if !title.is_empty() => title.to_owned().into(),
        _ => chip.label.unwrap_or_else(|| "Thread".to_owned()).into(),
    };
    let open = workspace.downgrade();
    super::chat::focusable(row, &tokens)
        .text_color(rgb(0x009c96))
        .cursor_pointer()
        .child(div().flex_none().child(icon))
        .child(div().min_w_0().truncate().hover(gpui_kit::Styled::underline).child(title))
        .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("Open thread").build(window, cx))
        .on_click(move |_, window, cx| {
            let _ = open.update(cx, |w, cx| w.select_thread_in(environment.clone(), thread_id.clone(), window, cx));
        })
        .into_any_element()
}

/// A thread's title and project as `orchestration.getThreadLinkMetadata` reports them; a server
/// without it answers `getThreadTitles`, which names no project.
#[derive(Clone, Debug, PartialEq)]
struct LinkMetadata {
    title: String,
    project_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
enum LinkAnswer {
    Asking,
    Found(LinkMetadata),
    /// The server answered without it: no thread there has this id.
    Absent,
    /// Only an answer proves a thread gone, so the link stays open.
    Failed,
}

struct LinkLookup {
    socket: crate::rpc::SocketId,
    /// The environment's shell removals when asked; a removal asks again (`refreshThreadLookup`).
    removals: u64,
    answer: LinkAnswer,
}

/// What servers said of linked threads their shells leave out, by environment and thread
/// (`orchestrationEnvironment.threadLinkMetadata`). A new socket or a removed thread asks again.
#[derive(Default)]
pub(super) struct ThreadLinkLookups(HashMap<(SharedString, String), LinkLookup>);

impl ThreadLinkLookups {
    fn answer(&self, environment: &SharedString, thread_id: &str) -> Option<&LinkAnswer> {
        self.0.get(&(environment.clone(), thread_id.to_owned())).map(|lookup| &lookup.answer)
    }

    fn is_current(
        &self,
        environment: &SharedString,
        thread_id: &str,
        socket: &crate::rpc::SocketId,
        removals: u64,
    ) -> bool {
        self.0
            .get(&(environment.clone(), thread_id.to_owned()))
            .is_some_and(|lookup| lookup.socket == *socket && lookup.removals == removals)
    }

    /// Marks `ids` asked on `socket`, replacing what an earlier ask said.
    fn ask(&mut self, environment: &SharedString, ids: &[String], socket: &crate::rpc::SocketId, removals: u64) {
        for id in ids {
            let lookup = LinkLookup { socket: socket.clone(), removals, answer: LinkAnswer::Asking };
            self.0.insert((environment.clone(), id.clone()), lookup);
        }
    }

    /// Records the answer to one ask (`None` when it failed), unless a later ask replaced it.
    /// `full` answers are `getThreadLinkMetadata`'s, the others `getThreadTitles`'.
    fn record(
        &mut self,
        environment: &SharedString,
        ids: &[String],
        (socket, removals): (&crate::rpc::SocketId, u64),
        answer: Option<&serde_json::Value>,
        full: bool,
    ) {
        for id in ids {
            let Some(lookup) = self.0.get_mut(&(environment.clone(), id.clone())) else { continue };
            if lookup.socket != *socket || lookup.removals != removals {
                continue;
            }
            lookup.answer = match answer.map(|answer| &answer[id.as_str()]) {
                None => LinkAnswer::Failed,
                Some(entry) => {
                    let (title, project_id) = if full {
                        (entry["title"].as_str(), entry["projectId"].as_str())
                    } else {
                        (entry.as_str(), None)
                    };
                    title.map_or(LinkAnswer::Absent, |title| {
                        LinkAnswer::Found(LinkMetadata {
                            title: title.to_owned(),
                            project_id: project_id.map(str::to_owned),
                        })
                    })
                }
            };
        }
    }
}

/// What a thread link shows and opens (`MarkdownThreadLink`).
#[derive(Debug, PartialEq)]
enum LinkTarget {
    /// The thread the shell or the server names, or, until the server answers (or when it
    /// can't), the thread as written, named by the link's own text.
    Thread { environment: SharedString, thread_id: String, title: Option<String>, project_id: Option<String> },
    /// The server has no such thread, and no other environment holds a unique match.
    Missing,
}

/// `threadLinkCandidates`: the environment a link names (or the message's), and its thread as
/// written, then percent-decoded.
fn link_candidates(
    link: &crate::thread_links::ThreadLink,
    message_environment: Option<&str>,
) -> Option<(SharedString, Vec<String>)> {
    let environment = link.environment_id.as_deref().or(message_environment)?;
    let decoded = crate::thread_links::percent_decoded(&link.thread_id);
    Some((environment.to_owned().into(), std::iter::once(link.thread_id.clone()).chain(decoded).collect()))
}

/// The thread a link names: the shell's, as written, else the first candidate the server knows;
/// a v1 link the server denies takes a unique match in another environment.
fn resolve_thread_link(
    link: &crate::thread_links::ThreadLink,
    message_environment: Option<&str>,
    threads: &[crate::model::ThreadShell],
    lookups: &ThreadLinkLookups,
) -> LinkTarget {
    let Some((environment, ids)) = link_candidates(link, message_environment) else { return LinkTarget::Missing };
    let shell_target = |thread: &crate::model::ThreadShell| LinkTarget::Thread {
        environment: thread.environment_id.clone(),
        thread_id: thread.id.clone(),
        title: Some(thread.title.clone()),
        project_id: Some(thread.project_id.clone()),
    };
    if let Some(thread) = threads.iter().find(|t| t.id == ids[0] && t.environment_id == environment) {
        return shell_target(thread);
    }
    let answers: Vec<Option<&LinkAnswer>> = ids.iter().map(|id| lookups.answer(&environment, id)).collect();
    let found = ids.iter().zip(&answers).find_map(|(id, answer)| match answer {
        Some(LinkAnswer::Found(metadata)) => Some((id, metadata)),
        _ => None,
    });
    if let Some((id, metadata)) = found {
        return LinkTarget::Thread {
            environment,
            thread_id: id.clone(),
            title: Some(metadata.title.clone()),
            project_id: metadata.project_id.clone(),
        };
    }
    if !answers.iter().all(|answer| *answer == Some(&LinkAnswer::Absent)) {
        return LinkTarget::Thread { environment, thread_id: ids[0].clone(), title: None, project_id: None };
    }
    if link.environment_id.is_some() {
        return LinkTarget::Missing;
    }
    let elsewhere = ids.iter().find_map(|id| {
        let mut matches = threads.iter().filter(|t| t.id == *id && t.environment_id != environment);
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    });
    elsewhere.map_or(LinkTarget::Missing, shell_target)
}

impl Workspace {
    fn resolve_thread_link(
        &self,
        link: &crate::thread_links::ThreadLink,
        message_environment: Option<&str>,
    ) -> LinkTarget {
        resolve_thread_link(link, message_environment, &self.shell.threads, &self.thread_link_lookups)
    }

    /// A link's candidates when the shell lacks it and this connection has not asked for them
    /// since the environment last removed a thread.
    fn stale_thread_link_lookup(
        &self,
        link: &crate::thread_links::ThreadLink,
        message_environment: Option<&str>,
    ) -> Option<(SharedString, Vec<String>)> {
        let (environment, ids) = link_candidates(link, message_environment)?;
        if self.shell.threads.iter().any(|t| t.id == ids[0] && t.environment_id == environment) {
            return None;
        }
        let socket = self.session(&environment)?.client.as_ref()?.socket_id();
        let removals = self.shell.removals(&environment);
        let stale = ids.iter().any(|id| !self.thread_link_lookups.is_current(&environment, id, &socket, removals));
        stale.then_some((environment, ids))
    }

    /// Asks `environment` for the titles and projects of linked threads its shell leaves out.
    fn ask_thread_link_metadata(&mut self, environment: SharedString, ids: Vec<String>, cx: &mut Context<Self>) {
        let Some(session) = self.session(&environment) else { return };
        let Some(client) = session.client.clone() else { return };
        let full =
            session.server_info.config.get("threadLinkMetadata").and_then(serde_json::Value::as_bool) == Some(true);
        let (socket, removals) = (client.socket_id(), self.shell.removals(&environment));
        if ids.iter().all(|id| self.thread_link_lookups.is_current(&environment, id, &socket, removals)) {
            return;
        }
        self.thread_link_lookups.ask(&environment, &ids, &socket, removals);
        let method = if full { "orchestration.getThreadLinkMetadata" } else { "orchestration.getThreadTitles" };
        cx.spawn(async move |this, cx| {
            let answer = client.request(method, json!({ "threadIds": ids })).await;
            if let Err(error) = &answer {
                log::warn!("{method}: {error}");
            }
            let _ = this.update(cx, |this, cx| {
                let asked = (&socket, removals);
                this.thread_link_lookups.record(&environment, &ids, asked, answer.as_ref().ok(), full);
                cx.notify();
            });
        })
        .detach();
    }
}

/// Where a chat link to a thread opens, as [`ThreadChips`] resolves it, so a reference-style
/// link keeps the environment it names (`MarkdownThreadLink`); a missing thread opens nothing.
pub(super) fn thread_link_destination(
    url: &str,
    message_environment: Option<&str>,
    workspace: &Workspace,
) -> Option<(SharedString, String)> {
    let link = crate::thread_links::parse(url)?;
    match workspace.resolve_thread_link(&link, message_environment) {
        LinkTarget::Thread { environment, thread_id, .. } => Some((environment, thread_id)),
        LinkTarget::Missing => None,
    }
}

/// `$…$` math when Render math is on (`MarkdownMath`), typeset; its TeX source until then and
/// when it does not parse. Copying keeps the TeX.
pub(super) struct InlineMath {
    pub workspace: WeakEntity<Workspace>,
    /// The markdown view, which re-lays out once a formula lands.
    pub key: SharedString,
}

impl MarkdownPlugin for InlineMath {
    fn name(&self) -> &'static str {
        "t3-inline-math"
    }

    fn parse(
        &self,
        node: &gpui_kit::component::text::markdown_ast::Node,
        _: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        let gpui_kit::component::text::markdown_ast::Node::InlineMath(math) = node else { return None };
        let tex = math.value.trim().to_owned();
        let source = format!("\\({tex}\\)");
        Some(MarkdownNode::new("t3-inline-math", tex).text(source.clone()).markdown(source))
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &gpui_kit::component::text::InlineRenderContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<gpui_kit::component::text::InlineElement> {
        let tex = node.data::<String>()?;
        super::math::inline_formula(&self.workspace, &self.key, tex, context, window, cx)
    }
}

/// `$$…$$` display math when Render math is on: its own centered line.
pub(super) struct DisplayMath {
    pub workspace: WeakEntity<Workspace>,
    pub key: SharedString,
}

impl MarkdownPlugin for DisplayMath {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        "t3-display-math"
    }

    fn parse(
        &self,
        node: &gpui_kit::component::text::markdown_ast::Node,
        _: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        let gpui_kit::component::text::markdown_ast::Node::Math(math) = node else { return None };
        let tex = math.value.trim().to_owned();
        let source = format!("\\[\n{tex}\n\\]");
        Some(MarkdownNode::new("t3-display-math", tex).text(source.clone()).markdown(source))
    }

    fn render(&self, node: &MarkdownNode, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tex = node.data::<String>().map_or("", String::as_str);
        let at = node.source_range().map_or(0, |range| range.start);
        div().w_full().my(gpui_kit::rems(0.65)).child(super::math::display_formula(
            &self.workspace,
            &self.key,
            tex,
            at,
            window,
            cx,
        ))
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
    // Serialized when the menu opens, not on every frame.
    let (headers, rows, markdown) = (table.headers.clone(), table.rows.clone(), table.markdown.trim().to_owned());
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
                        table_csv(&headers, &rows),
                        this.clone(),
                        id.clone(),
                    )))
                }),
        )
        .into_any_element()
}

/// `useCopyToClipboard({ timeout: 1200 })`.
const TABLE_COPIED_FOR: std::time::Duration = std::time::Duration::from_millis(1200);

/// `CodexArtifactTemplateCard`: a reply's `::artifact-template` line draws as a card with the
/// template kind's artwork, its name and kind, and Use template, which drafts the template's
/// prompt into the composer.
pub(super) struct ArtifactTemplates {
    pub workspace: WeakEntity<Workspace>,
    /// The message the cards are in, which picks the composer Use template writes to.
    pub key: SharedString,
    /// Whether Use template shows (`onUseArtifactTemplate`): in replies, not in prompts.
    pub usable: bool,
}

impl MarkdownPlugin for ArtifactTemplates {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        "t3-artifact-template"
    }

    fn parse(
        &self,
        node: &gpui_kit::component::text::markdown_ast::Node,
        cx: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        let gpui_kit::component::text::markdown_ast::Node::Paragraph(_) = node else { return None };
        let templates = crate::artifact_templates::parse_paragraph(cx.node_source(node)?)?;
        let text = templates.iter().map(Template::copy_text).collect::<Vec<_>>().join("\n\n");
        let label = templates.iter().map(|t| format!("{} template", t.display_name)).collect::<Vec<_>>().join(", ");
        Some(
            MarkdownNode::new("t3-artifact-template", templates)
                .text(text.clone())
                .markdown(text)
                .accessibility_label(label),
        )
    }

    fn render(&self, node: &MarkdownNode, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = crate::theme::tokens(cx);
        let at = node.source_range().map_or(0, |range| range.start);
        let templates = node.data::<Vec<Template>>().cloned().unwrap_or_default();
        v_flex().w_full().min_w_0().gap(gpui_kit::rems(0.65)).children(templates.into_iter().enumerate().map(
            |(ix, template)| {
                let id = SharedString::from(format!("artifact-template:{}:{at}:{ix}", self.key));
                artifact_template_card(id, template, self.usable.then(|| self.workspace.clone()), &self.key, t)
            },
        ))
    }
}

/// `ARTIFACT_TEMPLATE_ICON_BY_KIND`.
fn artifact_template_icon(kind: TemplateKind) -> IconName {
    match kind {
        TemplateKind::Document | TemplateKind::GoogleDocs => IconName::FileText,
        TemplateKind::Presentation | TemplateKind::GoogleSlides => IconName::Presentation,
        TemplateKind::Spreadsheet | TemplateKind::GoogleSheets => IconName::FileSpreadsheet,
        TemplateKind::Site => IconName::Globe,
        TemplateKind::Image => IconName::Image,
        TemplateKind::Email => IconName::Mail,
        TemplateKind::Slack => IconName::MessageSquare,
    }
}

fn artifact_template_card(
    id: SharedString,
    template: Template,
    workspace: Option<WeakEntity<Workspace>>,
    key: &SharedString,
    t: &Tokens,
) -> AnyElement {
    let shadow = vec![gpui_kit::BoxShadow::new(px(0.), px(1.), gpui_kit::hsla(0., 0., 0., 0.05)).blur_radius(px(2.))];
    let artwork = div()
        .relative()
        .size(px(36.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .border_1()
        .border_color(Hsla { a: t.border.a * 0.7, ..t.border })
        .bg(t.background)
        .text_color(t.muted_foreground)
        .shadow(shadow.clone())
        .child(Icon::new(artifact_template_icon(template.kind)).size(px(20.)))
        .child(
            div()
                .absolute()
                .right(px(-4.))
                .bottom(px(-4.))
                .size(px(16.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .border_color(t.background)
                .bg(t.primary)
                .text_color(t.primary_foreground)
                .shadow(shadow.clone())
                .child(Icon::new(IconName::Sparkles).size(px(10.))),
        );
    let identity = v_flex()
        .min_w_0()
        .child(
            div()
                .truncate()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.foreground)
                .child(template.display_name.clone()),
        )
        .child(div().text_xs().text_color(t.muted_foreground).child(template.kind.label()));
    let use_button = workspace.map(|workspace| {
        let key = key.clone();
        gpui_kit::component::button::Button::new(SharedString::from(format!("{id}:use")))
            .outline()
            .small()
            .flex_none()
            .h(px(t.breakpoint.pick(32., 28.)))
            .px(px(9.))
            .rounded(px(8.))
            .border_color(t.input)
            .bg(if t.dark { Hsla { a: t.input.a * 0.32, ..t.input } } else { t.popover })
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.foreground)
            .label("Use template")
            .on_click(move |_, window, cx| {
                let _ = workspace.update(cx, |w, cx| w.use_artifact_template(&key, &template, window, cx));
            })
    });
    h_flex()
        .id(id)
        .w_full()
        .min_w_0()
        .gap(px(12.))
        .px(px(12.))
        .py(px(10.))
        .rounded(px(12.))
        .border_1()
        .border_color(Hsla { a: t.border.a * 0.7, ..t.border })
        .bg(Hsla { a: t.card.a * 0.6, ..t.card })
        .text_color(t.foreground)
        .shadow(shadow)
        .child(h_flex().flex_1().min_w_0().gap(px(12.)).child(artwork).child(identity))
        .children(use_button)
        .into_any_element()
}

impl Workspace {
    /// `insertArtifactTemplate`: the template's prompt goes at the end of the composer of the
    /// chat its reply is in, after a space, unless the draft already ends with it; the composer
    /// takes focus.
    fn use_artifact_template(&mut self, key: &str, template: &Template, window: &mut Window, cx: &mut Context<Self>) {
        let composer = match super::chat::view_pane(key).0 {
            super::side_chat::ChatPane::Side => self.side_chat.ready_composer().cloned(),
            super::side_chat::ChatPane::Main => {
                self.composer_takes_citations(super::side_chat::ChatPane::Main).then(|| self.composer.clone())
            }
        };
        let Some(composer) = composer else {
            let notification =
                Notification::error("The composer is busy; try again once it is ready.").title("Unable to add to chat");
            window.push_notification(notification, cx);
            return;
        };
        let draft = composer.read(cx).value().to_string();
        if let Some(prompt) = template.prompt_to_append(&draft) {
            composer.update(cx, |state, cx| {
                let end = draft.len();
                let leading = !draft.is_empty() && !draft.ends_with(char::is_whitespace);
                state.set_selected_range(end..end, cx);
                state.replace(format!("{}{prompt}", if leading { " " } else { "" }), window, cx);
            });
            self.sync_composer_empty(cx);
        }
        composer.update(cx, |state, cx| state.focus(window, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, label: &str) -> InlineSkill {
        InlineSkill { name: name.into(), label: label.into(), description: None, path: None }
    }

    #[test]
    fn known_skills_outside_code_become_chip_links() {
        let skills = [skill("design", "Design"), skill("pdf:pdf", "PDF")];
        let links = |text: &str| skill_links(text, &skills, |_, _| None);
        assert_eq!(
            links("Use $design then $missing and $pdf:pdf."),
            "Use [Design](t3-skill:design) then $missing and $pdf:pdf."
        );
        assert_eq!(links("Try `$design` here"), "Try `$design` here");
        assert_eq!(links("```\n$design \n```\n$design now"), "```\n$design \n```\n[Design](t3-skill:design) now");
        assert_eq!(links("costs $5 and $10"), "costs $5 and $10");
        assert_eq!(
            links("see [$design](https://x.dev) or $design"),
            "see [$design](https://x.dev) or [Design](t3-skill:design)"
        );
    }

    #[test]
    fn known_model_references_outside_code_become_chip_links_that_parse_back() {
        let known = |instance_id: &str, model: &str| {
            (instance_id == "codex" && model.starts_with("gpt-6.1")).then(|| ("GPT-6.1".to_owned(), "codex".to_owned()))
        };
        let links = |text: &str| skill_links(text, &[skill("design", "Design")], known);
        assert_eq!(
            links("Ask (model:codex/gpt-6.1), not model:openai/gpt-4o, then $design"),
            "Ask ([GPT\\-6\\.1](t3-model:codex/codex/gpt-6.1)), not model:openai/gpt-4o, then [Design](t3-skill:design)"
        );
        assert_eq!(links("Try `model:codex/gpt-6.1` here"), "Try `model:codex/gpt-6.1` here");
        assert_eq!(links("```\nmodel:codex/gpt-6.1\n```\n"), "```\nmodel:codex/gpt-6.1\n```\n");
        // Links keep their text as written, as the web leaves `a` children alone.
        let linked = "[use model:codex/gpt-6.1 now](https://example.com) ![model:codex/gpt-6.1](a.png)";
        assert_eq!(links(linked), linked);
        let coded = "[use `x` model:codex/gpt-6.1 now](https://example.com)";
        assert_eq!(links(coded), coded, "a code span inside a link's text");
        let reference =
            "[use model:codex/gpt-6.1 and $design][ref]\n\n[ref]: https://x.dev \"model:codex/gpt-6.1 $design\"\n";
        assert_eq!(links(reference), reference, "reference links and their definitions");
        assert_eq!(
            links("**$design** and [model:codex/gpt-6.1]"),
            "**[Design](t3-skill:design)** and [[GPT\\-6\\.1](t3-model:codex/codex/gpt-6.1)]",
            "an undefined reference is prose"
        );
        assert_eq!(
            links("[a [b] model:codex/gpt-6.1](u (v)) model:codex/gpt-6.1"),
            "[a [b] model:codex/gpt-6.1](u (v)) [GPT\\-6\\.1](t3-model:codex/codex/gpt-6.1)"
        );
        let odd = "gpt-6.1(x) [1m]%";
        let url = model_link("codex", "codex", odd);
        assert!(!url.contains(['(', ')', ' ', '[', ']']), "{url}");
        let chip = parse_model_link(&url).unwrap();
        assert_eq!((chip.driver.as_str(), chip.instance_id.as_str(), chip.model.as_str()), ("codex", "codex", odd));
        assert_eq!(parse_model_link("t3-model:codex/codex/a%2"), None, "a cut escape");
    }

    #[test]
    fn file_links_resolve_against_the_workspace() {
        assert_eq!(file_link_target("src/a.rs:12", Some("/repo")).as_deref(), Some("/repo/src/a.rs:12"));
        assert_eq!(file_link_target("./a.rs", Some("C:\\repo")).as_deref(), Some("C:\\repo\\a.rs"));
        assert_eq!(file_link_target("file:///C:/repo/a.rs", None).as_deref(), Some("C:/repo/a.rs"));
        assert_eq!(file_link_target("https://x.dev", Some("/repo")), None);
        assert_eq!(file_link_target("mailto:a@b.c", Some("/repo")), None);
        assert_eq!(file_link_target("a.rs", None), None);
        assert_eq!(without_position("/repo/a.rs:12:3"), "/repo/a.rs");
        assert_eq!(workspace_relative("/repo/src/a.rs", Some("/repo")).as_deref(), Some("src/a.rs"));
        assert_eq!(workspace_relative("C:\\Repo\\a.rs", Some("c:\\repo")).as_deref(), Some("a.rs"));
        assert_eq!(workspace_relative("/other/a.rs", Some("/repo")), None);
        // A `..` that climbs above the root leaves the workspace, so the editor opens it.
        assert_eq!(workspace_relative("/repo/../outside.rs", Some("/repo")), None);
        assert_eq!(workspace_relative("/repo/src/../../x.rs", Some("/repo")), None);
        assert_eq!(workspace_relative("/repo/src/../x.rs", Some("/repo")).as_deref(), Some("src/../x.rs"));
        // Lowercasing the Kelvin sign changes its byte length; this sliced inside it.
        assert_eq!(workspace_relative("/repo/\u{212A}/a.rs", Some("/repo/k")), None);
    }

    #[test]
    fn descriptive_file_links_keep_their_text_beside_the_chip() {
        let (file, description) = file_link("Open implementation", "src/main.rs:12").expect("a file link");
        assert_eq!(
            (file.path.as_str(), file.line, description.as_deref()),
            ("src/main.rs", Some(12), Some("Open implementation"))
        );
        assert_eq!(file_link("src/main.rs", "src/main.rs").map(|(_, text)| text), Some(None), "its own path");
        assert!(file_link("Docs", "https://example.com/docs").is_none());
    }

    #[test]
    fn file_link_text_reads_through_nested_markdown() {
        use gpui_kit::component::text::markdown_ast::Node;
        let source = "[**Read implementation**](src/main.rs) and [ mixed *emphasis* and `code` ](a.rs)";
        let root = markdown::to_mdast(source, &markdown::ParseOptions::gfm()).expect("markdown parses");
        let paragraph = &root.children().expect("a root")[0];
        let texts: Vec<String> = paragraph
            .children()
            .expect("a paragraph")
            .iter()
            .filter_map(|node| match node {
                Node::Link(link) => Some(link_text(&link.children)),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["Read implementation", " mixed emphasis and code "]);
    }

    #[test]
    fn chips_ask_once_about_their_absolute_target() {
        assert_eq!(metadata_target("bench:12", Some("/home/u")).as_deref(), Some("/home/u/bench"));
        assert_eq!(metadata_target("bench", None), None);
        // A network path would send the user's credentials to its host.
        assert_eq!(metadata_target(r"\\host\share\a", Some(r"\\host\share")), None);
        let kinds = EntryKinds::default();
        assert_eq!(kinds.directory("/a"), (false, true));
        assert_eq!(kinds.directory("/b"), (false, true));
        assert_eq!(kinds.directory("/a"), (false, false), "queued once");
        let mut asked = kinds.take_wanted();
        asked.sort();
        assert_eq!(asked, ["/a", "/b"]);
        assert!(kinds.take_wanted().is_empty(), "in flight");
        kinds.record(&asked, Some(&json!({"entries": [{"kind": "directory"}, null]})));
        assert_eq!((kinds.directory("/a"), kinds.directory("/b")), ((true, false), (false, false)));
    }

    #[test]
    fn tables_copy_as_csv_like_the_web() {
        let headers = vec!["Step".to_owned(), "Result".to_owned()];
        let rows = vec![vec!["Read".to_owned(), "1, 2".to_owned()], vec!["Say \"hi\"".to_owned(), "a\n  b".to_owned()]];
        assert_eq!(table_csv(&headers, &rows), "Step,Result\nRead,\"1, 2\"\n\"Say \"\"hi\"\"\",a b");
    }

    fn thread(environment: &str, id: &str) -> crate::model::ThreadShell {
        let mut thread: crate::model::ThreadShell = serde_json::from_value(json!({
            "id": id, "projectId": "p", "title": "T", "status": "idle", "updatedAt": "2026-01-01T00:00:00Z",
        }))
        .unwrap();
        thread.environment_id = environment.to_owned().into();
        thread
    }

    #[test]
    fn thread_links_open_in_the_environment_they_name() {
        let threads = [thread("here", "abc"), thread("there", "abc"), thread("there", "only")];
        let mut lookups = ThreadLinkLookups::default();
        let open = |url: &str, lookups: &ThreadLinkLookups| match resolve_thread_link(
            &crate::thread_links::parse(url)?,
            Some("here"),
            &threads,
            lookups,
        ) {
            LinkTarget::Thread { environment, thread_id, .. } => Some((environment.to_string(), thread_id)),
            LinkTarget::Missing => None,
        };
        assert_eq!(open("t3-thread://v2/there/abc", &lookups), Some(("there".into(), "abc".into())));
        assert_eq!(open("https://app.t3.codes/threads/there/abc", &lookups), Some(("there".into(), "abc".into())));
        assert_eq!(open("t3-thread://v1/abc", &lookups), Some(("here".into(), "abc".into())), "v1 takes the message's");
        // Until the message's server answers, a link opens the thread as written there.
        assert_eq!(open("t3-thread://v1/only", &lookups), Some(("here".into(), "only".into())));
        assert_eq!(open("t3-thread://v2/here/only", &lookups), Some(("here".into(), "only".into())));
        let socket = crate::rpc::RpcClient::unopened().socket_id();
        let here = SharedString::from("here");
        let ids = ["only".to_owned()];
        lookups.ask(&here, &ids, &socket, 0);
        lookups.record(&here, &ids, (&socket, 0), Some(&json!({})), true);
        assert_eq!(open("t3-thread://v1/only", &lookups), Some(("there".into(), "only".into())), "then a unique match");
        assert_eq!(open("t3-thread://v2/here/only", &lookups), None, "a scoped link never jumps machines");
        assert_eq!(open("https://example.com", &lookups), None);
    }

    #[test]
    fn unloaded_thread_links_take_the_title_and_project_the_server_reports() {
        let link = |url: &str| crate::thread_links::parse(url).unwrap();
        let socket = crate::rpc::RpcClient::unopened().socket_id();
        let here = SharedString::from("here");
        let target = |thread_id: &str, title: Option<&str>, project_id: Option<&str>| LinkTarget::Thread {
            environment: here.clone(),
            thread_id: thread_id.to_owned(),
            title: title.map(str::to_owned),
            project_id: project_id.map(str::to_owned),
        };
        let resolve =
            |url: &str, lookups: &ThreadLinkLookups| resolve_thread_link(&link(url), Some("here"), &[], lookups);
        let mut lookups = ThreadLinkLookups::default();
        // Asked as written and decoded together; the written id wins when both answer.
        let ids = link_candidates(&link("t3-thread://v1/older%2Dthread"), Some("here")).unwrap().1;
        assert_eq!(ids, ["older%2Dthread", "older-thread"]);
        lookups.ask(&here, &ids, &socket, 0);
        assert_eq!(resolve("t3-thread://v1/older%2Dthread", &lookups), target("older%2Dthread", None, None));
        lookups.record(&here, &ids, (&socket, 1), Some(&json!({"older-thread": {"title": "Stale"}})), true);
        assert_eq!(
            resolve("t3-thread://v1/older%2Dthread", &lookups),
            target("older%2Dthread", None, None),
            "replaced"
        );
        lookups.record(&here, &ids, (&socket, 0), None, true);
        assert_eq!(resolve("t3-thread://v1/older%2Dthread", &lookups), target("older%2Dthread", None, None), "failed");
        lookups.ask(&here, &ids, &socket, 0);
        let answer = json!({"older-thread": {"title": "Decoded essay", "projectId": "essays"}});
        lookups.record(&here, &ids, (&socket, 0), Some(&answer), true);
        let decoded = target("older-thread", Some("Decoded essay"), Some("essays"));
        assert_eq!(resolve("t3-thread://v1/older%2Dthread", &lookups), decoded);
        // A server without `threadLinkMetadata` answers titles alone.
        let ids = ["titled".to_owned()];
        lookups.ask(&here, &ids, &socket, 0);
        lookups.record(&here, &ids, (&socket, 0), Some(&json!({"titled": "Older essay"})), false);
        assert_eq!(resolve("t3-thread://v2/here/titled", &lookups), target("titled", Some("Older essay"), None));
        assert!(lookups.is_current(&here, "titled", &socket, 0));
        assert!(!lookups.is_current(&here, "titled", &socket, 1), "a removal asks again");
        let reconnected = crate::rpc::RpcClient::unopened().socket_id();
        assert!(!lookups.is_current(&here, "titled", &reconnected, 0), "so does a new socket");
    }

    #[test]
    fn change_request_links_link_and_unlink_the_thread() {
        let link: crate::thread_pull_requests::PullRequestLink = serde_json::from_value(json!({
            "host": "github.com", "repository": "Acme/App", "number": 7, "url": "https://github.com/Acme/App/pull/7",
            "source": "manual", "linkedAt": "2026-01-01T00:00:00Z",
        }))
        .unwrap();
        let links = [link];
        assert_eq!(pr_link_state("https://GitHub.com/acme/app/pull/7/files", &links), Some(true));
        assert_eq!(pr_link_state("https://github.com/acme/app/pull/8", &links), Some(false));
        assert_eq!(pr_link_state("https://github.com/acme/app/issues/7", &links), None);
        let mut dismissed = links;
        dismissed[0].source = "stack-dismissed".into();
        assert_eq!(pr_link_state("https://github.com/acme/app/pull/7", &dismissed), Some(false));
        assert_eq!(
            pr_link_command("t", "https://github.com/Acme/App/pull/7", true).unwrap(),
            json!({"type": "thread.pull-request.link", "threadId": "t", "host": "github.com", "repository": "acme/app",
                "number": 7, "url": "https://github.com/Acme/App/pull/7", "source": "manual"})
        );
        assert_eq!(
            pr_link_command("t", "https://gitlab.com/g/p/-/merge_requests/3", false).unwrap(),
            json!({"type": "thread.pull-request.unlink", "threadId": "t", "host": "gitlab.com", "repository": "g/p",
                "number": 3})
        );
    }

    #[test]
    fn pdfs_and_pages_open_in_the_integrated_browser() {
        assert!(is_browser_preview_file("/r/docs/Guide.PDF") && is_browser_preview_file("/r/site/index.htm#top"));
        assert!(!is_browser_preview_file("/r/a.md") && !is_browser_preview_file("/r/pdf"));
        assert!(opens_in_browser_by_default("/r/a.pdf?page=2"));
        assert!(!opens_in_browser_by_default("/r/index.html"), "a page opens in the panel first");
    }
}
