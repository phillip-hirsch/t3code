//! Header extras: updating a branch behind its base (`PullRequestBaseBranch`), the checks
//! popover (`PullRequestChecksPopover`), native stacks (`PullRequestStackMenu`), threads linked
//! to the pull request (`PullRequestThreadLinks`) and maintainer edits (`PullRequestActionsMenu`).

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::popover::Popover;
use gpui_kit::{KeyDownEvent, Render, WeakEntity};

use super::*;
use crate::app::palette::LinkedThread;
use crate::pull_request_detail::Stack;

/// `ChecksBody`: checks needing attention and running ones first; completed ones fold behind
/// "Show all" while any of those show.
pub(in crate::app::pages) fn checks_body(
    checks: &[Check],
    show_all: bool,
    on_toggle: impl Fn(&mut App) + 'static,
    t: &Tokens,
) -> AnyElement {
    if checks.is_empty() {
        return div().text_xs().text_color(t.muted_foreground).child("No checks reported").into_any_element();
    }
    let (attention, running, completed) = logic::group_checks(checks);
    let collapsible = !(completed.is_empty() || (attention.is_empty() && running.is_empty()));
    let rows = attention
        .iter()
        .chain(running.iter())
        .copied()
        .chain(completed.iter().copied().filter(|_| show_all || !collapsible))
        .enumerate()
        .map(|(index, check)| {
            let (icon, color) = check_status_icon(&check.status, t);
            let url = check.url.clone();
            h_flex()
                .gap(px(8.))
                .text_xs()
                .child(Icon::new(icon).with_size(px(14.)).text_color(color))
                .child(div().flex_1().min_w_0().truncate().child(check.name.clone()))
                .child(div().flex_none().text_color(t.muted_foreground).child(logic::check_status_label(check)))
                .children(url.map(|url| {
                    div()
                        .id(ElementId::Name(format!("pr-check-details-{index}-{}", check.name).into()))
                        .flex_none()
                        .cursor_pointer()
                        .text_color(t.primary)
                        .hover(gpui_kit::Styled::underline)
                        .child("Details")
                        .on_click(move |_, _, cx| cx.open_url(&url))
                }))
        });
    v_flex()
        .child(v_flex().id("pr-checks-list").max_h(px(256.)).overflow_y_scroll().gap(px(4.)).children(rows))
        .when(collapsible, |el| {
            el.child(
                div().mt(px(4.)).child(
                    ghost_button("pr-checks-show-all", t)
                        .child(if show_all { "Show less" } else { "Show all" })
                        .on_click(move |_, _, cx| on_toggle(cx)),
                ),
            )
        })
        .into_any_element()
}

/// The pull request's key as threads record it: the link's host and lowercase repository.
fn thread_key(detail: &Detail) -> Option<(String, String, u64)> {
    let (host, repository, number) = logic::parse_change_request_url(&detail.url)?;
    Some((host, repository.to_lowercase(), number))
}

impl Workspace {
    /// `key`'s stack and linked threads, read once the detail says what the host supports.
    pub(super) fn read_pr_extras_of(&mut self, key: &str, cx: &mut Context<Self>) {
        self.follow_pr_checks(key, cx);
        self.read_pr_stack_of(key, cx);
        let Some(client) = self.pull_request_client_of(key) else { return };
        let Some(panel) = self.pr_panel_ref(key) else { return };
        let Some(detail) = panel.detail.clone() else { return };
        let key = key.to_owned();
        let mut reference = panel.reference.to_json();
        let Some((host, repository, number)) = thread_key(&detail) else { return };
        reference["host"] = json!(host);
        reference["repository"] = json!(repository);
        reference["number"] = json!(number);
        cx.spawn(async move |this, cx| {
            let result = client.request("pullRequests.linkedThreads", reference).await;
            let _ = this.update(cx, |this, cx| {
                let Some(panel) = this.pr_panel(&key) else { return };
                if let Ok(value) = result {
                    panel.linked = Some(
                        value["threads"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|t| {
                                Some(LinkedThread {
                                    id: t["id"].as_str()?.to_owned(),
                                    title: t["title"].as_str().unwrap_or_default().to_owned(),
                                    archived: !t["archivedAt"].is_null(),
                                })
                            })
                            .collect(),
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `key`'s stack membership (`usePullRequestStack`), on a host with stacks; Refresh and a
    /// failed lookup's retry read it again, keeping the stack shown meanwhile as stale.
    pub(super) fn read_pr_stack_of(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(client) = self.pull_request_client_of(key) else { return };
        let Some(panel) = self.pr_panel(key) else { return };
        let Some(detail) = panel.detail.clone().filter(|d| d.capabilities.stacks == Some(true)) else { return };
        panel.stack_fresh = false;
        let mut input = panel.reference.to_json();
        if input.get("host").is_none()
            && let Some((host, _, _)) = logic::parse_change_request_url(&detail.url)
        {
            input["host"] = json!(host);
        }
        let key = key.to_owned();
        cx.spawn(async move |this, cx| {
            let result = client.request("pullRequests.stack", input).await;
            let _ = this.update(cx, |this, cx| {
                let Some(panel) = this.pr_panel(&key) else { return };
                match result.map(serde_json::from_value::<Option<Stack>>) {
                    Ok(Ok(stack)) => {
                        panel.stack = stack.map(Rc::new);
                        panel.stack_fresh = true;
                        panel.stack_failed = false;
                    }
                    Ok(Err(error)) => {
                        log::warn!("unreadable pull request stack: {error}");
                        panel.stack_failed = true;
                    }
                    Err(_) => panel.stack_failed = true,
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The shown pull request's stack, read again.
    fn retry_pr_stack(&mut self, cx: &mut Context<Self>) {
        if let Some(key) = self.shown_pr_key() {
            self.read_pr_stack_of(&key, cx);
            cx.notify();
        }
    }

    /// The base branch, warning with the update buttons when the branch is behind it.
    pub(super) fn render_pr_base_branch(&self, detail: &Detail, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let base = detail.base_branch.clone();
        let Some((behind, methods)) = logic::base_freshness(detail) else {
            return div().flex_none().child(base).into_any_element();
        };
        let behind = behind.map_or(String::new(), |n| format!(" by {n} {}", if n == 1 { "commit" } else { "commits" }));
        let summary = format!("This branch is out-of-date with {base}{behind}.");
        let pending = self.pull_requests.panel.as_ref().is_some_and(|p| p.pending.is_some());
        let view = cx.entity();
        let warning = t.warning_foreground;
        let trigger = Button::new("pr-base-behind")
            .ghost()
            .xsmall()
            .h(px(18.))
            .px(px(2.))
            .gap(px(4.))
            .font_family(FONT_MONO)
            .text_size(px(12.))
            .text_color(warning)
            .child(base)
            .child(Icon::new(Lucide::TriangleAlert).with_size(px(14.)).text_color(warning));
        Popover::new("pr-base-behind-popover")
            .anchor(gpui_kit::Anchor::TopLeft)
            .trigger(trigger)
            .max_w(px(320.))
            .p(px(10.))
            .content(move |_, _, cx| {
                let t = *theme::tokens(cx);
                let view = view.clone();
                v_flex()
                    .font_family(theme::FONT_SANS)
                    .text_xs()
                    .child(div().text_color(t.foreground).child(summary.clone()))
                    .child(div().mt(px(2.)).text_color(t.muted_foreground).child("Changes can be cleanly merged."))
                    .when(!methods.is_empty(), |el| {
                        el.child(h_flex().mt(px(8.)).flex_wrap().gap(px(6.)).children(methods.iter().map(|method| {
                            let rebase = method == "rebase";
                            let view = view.clone();
                            xs_outline(if rebase { "pr-update-rebase" } else { "pr-update-merge" }, &t)
                                .disabled(pending)
                                .child(Icon::new(Lucide::GitMerge).with_size(px(12.)))
                                .child(if rebase { "Update with rebase" } else { "Update branch" })
                                .on_click(move |_, window, cx| {
                                    let method = if rebase { "rebase" } else { "merge" };
                                    view.update(cx, |this, cx| {
                                        this.perform_pr_action_with("update-branch", None, Some(method), window, cx);
                                    });
                                })
                        })))
                    })
            })
            .into_any_element()
    }

    /// The checks state icon, opening the list of checks.
    pub(super) fn render_pr_checks_popover(
        &self,
        detail: &Detail,
        state: &'static str,
        stale: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some((icon, color, label)) = checks_glyph(state, t) else { return div().into_any_element() };
        let checks = detail.checks.clone();
        let summary = (!stale).then(|| logic::summarize_checks(&checks));
        let show_all = self.pull_requests.panel.as_ref().is_some_and(|p| p.checks_all);
        let view = cx.entity();
        let trigger = Button::new("pr-checks-popover")
            .ghost()
            .xsmall()
            .size(px(18.))
            .p_0()
            .tooltip(label)
            .child(Icon::new(icon).with_size(px(14.)).text_color(color));
        Popover::new("pr-checks-popover-content")
            .anchor(gpui_kit::Anchor::TopLeft)
            .trigger(trigger)
            .w(px(360.))
            .p(px(12.))
            .content(move |_, _, cx| {
                let t = *theme::tokens(cx);
                let view = view.clone();
                v_flex()
                    .child(div().mb(px(8.)).text_sm().font_weight(FontWeight::MEDIUM).child(label))
                    .children(
                        summary.clone().map(|s| div().mb(px(8.)).text_xs().text_color(t.muted_foreground).child(s)),
                    )
                    .map(|el| {
                        if stale {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(t.muted_foreground)
                                    .child("Check details are out of date. Refresh the pull request to update them."),
                            )
                        } else {
                            el.child(checks_body(
                                &checks,
                                show_all,
                                move |cx| {
                                    view.update(cx, |this, cx| {
                                        if let Some(panel) = this.pull_requests.panel.as_mut() {
                                            panel.checks_all = !panel.checks_all;
                                        }
                                        cx.notify();
                                    });
                                },
                                &t,
                            ))
                        }
                    })
            })
            .into_any_element()
    }

    /// Linked threads: a count that opens the palette on the pull request's link, listing them,
    /// archived ones too (`PullRequestThreadLinks` `display="count"`).
    pub(super) fn render_pr_thread_count(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let panel = self.pull_requests.panel.as_ref()?;
        let linked = panel.linked.clone()?;
        if linked.is_empty() {
            return None;
        }
        let url = panel.detail.as_ref()?.url.clone();
        let environment = SharedString::from(panel.reference.environment_id.clone());
        let tip = format!(
            "Linked from {} {}. Search in the command palette.",
            linked.len(),
            if linked.len() == 1 { "thread" } else { "threads" }
        );
        Some(
            ghost_button("pr-linked-threads", t)
                .tooltip(tip)
                .child(Icon::new(Lucide::MessageSquare).with_size(px(14.)))
                .child(linked.len().to_string())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_palette_search(url.clone(), environment.clone(), linked.clone(), window, cx);
                }))
                .into_any_element(),
        )
    }

    /// "Link to thread": the pull request's environment's active threads, searchable (`ThreadPicker`).
    pub(super) fn open_pr_thread_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.pull_requests.panel.as_ref() else { return };
        let Some(detail) = panel.detail.clone() else { return };
        let environment = SharedString::from(panel.reference.environment_id.clone());
        let term = logic::terminology(&detail.provider).0;
        let workspace = cx.weak_entity();
        let picker = cx.new(|cx| ThreadPicker::new(workspace, environment, detail.url.clone(), window, cx));
        // Closing returns focus to the actions trigger the keyboard opened it from.
        if window.last_input_was_keyboard() {
            self.pull_requests.actions_focus.focus(window, cx);
        }
        let search = picker.read(cx).search.clone();
        let focus = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if focus.replace(false) {
                search.update(cx, |input, cx| input.focus(window, cx));
            }
            dialog.entrance(DialogEntrance::None).title(format!("Link {term} to a thread")).child(picker.clone())
        });
    }

    /// The thread showing the pull request, when it lives on the pull request's environment, and
    /// whether it links it (`usePullRequestLinking().isLinked`).
    pub(super) fn pr_current_thread(&self, environment_id: &str, thread_id: &str, url: &str) -> Option<(String, bool)> {
        let thread = self.shell.threads.iter().find(|t| t.id == thread_id && t.environment_id == environment_id)?;
        Some((thread.id.clone(), crate::thread_pull_requests::links_url(&thread.pull_requests, url)))
    }

    /// `thread.pull-request.link` or `.unlink`, then the linked threads are read again.
    pub(super) fn change_pr_thread_link(
        &mut self,
        thread_id: String,
        link: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Both entry points list only threads on the pull request's environment.
        let Some(client) = self.pull_request_client() else { return };
        let Some(panel) = self.pull_requests.panel.as_ref() else { return };
        let Some(detail) = panel.detail.clone() else { return };
        let key = panel.reference.key();
        let Some((host, repository, number)) = thread_key(&detail) else { return };
        let term = logic::terminology(&detail.provider).0;
        let id = super::super::super::new_id();
        let command = if link {
            json!({
                "type": "thread.pull-request.link", "commandId": id, "threadId": thread_id,
                "host": host, "repository": repository, "number": number, "url": detail.url, "source": "manual",
            })
        } else {
            json!({
                "type": "thread.pull-request.unlink", "commandId": id, "threadId": thread_id,
                "host": host, "repository": repository, "number": number,
            })
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(_) => this.read_pr_extras_of(&key, cx),
                Err(error) => {
                    let title = if link {
                        format!("Could not link the {term}")
                    } else {
                        format!("Could not unlink the {term}")
                    };
                    toast_error(window, cx, title, error.to_string());
                }
            });
        })
        .detach();
    }

    /// "Allow edits from maintainers" (`pullRequests.update` with `maintainerCanModify`).
    pub(super) fn save_pr_maintainer_edits(&mut self, value: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.pull_request_client() else { return };
        let Some(panel) = self.pull_requests.panel.as_mut() else { return };
        if panel.maintainer_saving {
            return;
        }
        panel.maintainer_saving = true;
        panel.maintainer_choice = Some(value);
        let key = panel.reference.key();
        let mut input = panel.reference.to_json();
        input["maintainerCanModify"] = json!(value);
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("pullRequests.update", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(panel) = this.pr_panel(&key) else { return };
                panel.maintainer_saving = false;
                match result {
                    Ok(_) => this.read_pr_detail_of(&key, cx),
                    Err(error) => {
                        panel.maintainer_choice = None;
                        toast_error(
                            window,
                            cx,
                            "Could not save maintainer access",
                            logic::readable_failure(&list_logic::error_message(&error), "Try again."),
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The stack's place and its menu, and "Merge stack" when the shown layer can merge.
    pub(super) fn render_pr_stack(
        &self,
        detail: &Rc<Detail>,
        method: &'static str,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(panel) = self.pull_requests.panel.as_ref() else { return Vec::new() };
        let Some(stack) = panel.stack.clone() else {
            if panel.stack_failed {
                return vec![
                    ghost_button("pr-stack-retry", t)
                        .child("Retry stack lookup")
                        .on_click(cx.listener(|this, _, _, cx| this.retry_pr_stack(cx)))
                        .into_any_element(),
                ];
            }
            return Vec::new();
        };
        // A stack kept from before a failed or running lookup may have moved (`stale`).
        let plan = logic::stack_plan(&stack, detail.number, !panel.stack_fresh || panel.stack_failed);
        let actions = detail.capabilities.stack_actions == Some(true);
        let can_merge = actions && logic::can(detail, "merge") && !logic::allowed_merge_methods(detail).is_empty();
        let can_rebase = actions && detail.viewer_permissions.stack_rebase == Some(true);
        let busy = panel.pending.is_some();
        let view = cx.entity();
        let reference = panel.reference.clone();
        let (term, plural) =
            (logic::terminology(&detail.provider).0, format!("{}s", logic::terminology(&detail.provider).0));
        let menu_stack = stack.clone();
        let (merge_count, merge_blocked) = (plan.merge.len(), plan.merge_blocked);
        let (merge_disabled, rebase_disabled) = (plan.merge_disabled || busy, plan.rebase_disabled || busy);
        let position = plan.position;
        let current = detail.number;
        let failed = panel.stack_failed;
        let mut out = vec![
            ghost_button("pr-stack", t)
                .tooltip(format!("View stack #{}, layer {position} of {}", stack.number, stack.layers.len()))
                .child(Icon::new(Lucide::Layers).with_size(px(14.)))
                .child(format!("{position}/{}", stack.layers.len()))
                .when(failed, |el| el.child(Icon::new(Lucide::TriangleAlert).with_size(px(12.)).text_color(t.warning)))
                .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu: PopupMenu, window, _| {
                    menu = menu.label(format!("Stack #{}", menu_stack.number));
                    if failed {
                        menu = menu.item(
                            PopupMenuItem::new("Retry stack refresh")
                                .on_click(window.listener_for(&view, |this, _, _, cx| this.retry_pr_stack(cx))),
                        );
                    }
                    for layer in menu_stack.layers.iter().rev() {
                        let number = layer.number;
                        let label =
                            format!("#{number} {}", layer.title.clone().unwrap_or_else(|| layer.head_branch.clone()));
                        let reference = Reference { number, ..reference.clone() };
                        let icon = match (layer.state, layer.is_draft) {
                            (list_logic::State::Merged, _) => Lucide::GitMerge,
                            (list_logic::State::Closed, _) => Lucide::GitPullRequestClosed,
                            (_, true) => Lucide::GitPullRequestDraft,
                            _ => Lucide::GitPullRequestArrow,
                        };
                        menu = menu.item(PopupMenuItem::new(label).icon(icon).checked(number == current).on_click(
                            window.listener_for(&view, move |this, _, _, cx| {
                                let seed = this.pull_requests.entry_for(&reference);
                                this.open_pull_request_panel(reference.clone(), seed, cx);
                            }),
                        ));
                    }
                    if can_merge || can_rebase {
                        menu = menu.separator();
                    }
                    if can_merge {
                        let stack = menu_stack.clone();
                        menu = menu.item(
                            PopupMenuItem::new(format!("Merge stack ({merge_count})"))
                                .icon(Lucide::GitMerge)
                                .disabled(merge_disabled)
                                .on_click(window.listener_for(&view, move |this, _, window, cx| {
                                    this.confirm_pr_stack(stack.clone(), "merge", method, window, cx);
                                })),
                        );
                    }
                    if can_rebase {
                        let stack = menu_stack.clone();
                        menu = menu.item(
                            PopupMenuItem::new("Rebase stack")
                                .icon(Lucide::RefreshCw)
                                .disabled(rebase_disabled)
                                .on_click(window.listener_for(&view, move |this, _, window, cx| {
                                    this.confirm_pr_stack(stack.clone(), "update-branch", method, window, cx);
                                })),
                        );
                    }
                    if can_merge && merge_blocked {
                        menu = menu.label("Every layer being merged must be open and ready for review.");
                    }
                    menu
                })
                .into_any_element(),
        ];
        if can_merge && plan.selected_open {
            let tip = format!(
                "Merge stack through #{} into {} ({} {})",
                detail.number,
                stack.base,
                plan.merge.len(),
                if plan.merge.len() == 1 { term.to_owned() } else { plural }
            );
            out.push(
                Button::new("pr-merge-stack")
                    .primary()
                    .xsmall()
                    .h(px(24.))
                    .px(px(8.))
                    .gap(px(4.))
                    .text_xs()
                    .disabled(merge_disabled)
                    .tooltip(tip)
                    .child(Icon::new(Lucide::GitMerge).with_size(px(14.)))
                    .child("Merge stack")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.confirm_pr_stack(stack.clone(), "merge", method, window, cx);
                    }))
                    .into_any_element(),
            );
        }
        out
    }

    /// The stack action's dialog, listing the layers it covers.
    fn confirm_pr_stack(
        &mut self,
        stack: Rc<Stack>,
        action: &'static str,
        method: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(detail) = self.pull_requests.panel.as_ref().and_then(|p| p.detail.clone()) else { return };
        let plan = logic::stack_plan(&stack, detail.number, false);
        let merge = action == "merge";
        let layers = if merge { plan.merge } else { plan.rebase };
        let term = logic::terminology(&detail.provider).0;
        let noun = if layers.len() == 1 { term.to_owned() } else { format!("{term}s") };
        let host = list_logic::open_on_host_label(&detail.provider).trim_start_matches("Open on ").to_owned();
        let (title, body) = if merge {
            (
                format!("Merge {} {noun}?", layers.len()),
                format!(
                    "Merge #{} and its unmerged layers below into {} using {method}. {host} checks their rules before merging or queueing them and rebases the remaining stack after merging.",
                    detail.number, stack.base
                ),
            )
        } else {
            (
                format!("Rebase {} {noun}?", layers.len()),
                format!(
                    "Rebase the remote branches from bottom to top onto {}. This rewrites branch history and may restart checks. If a layer fails, earlier updates remain.",
                    stack.base
                ),
            )
        };
        let rows: Vec<String> = stack
            .layers
            .iter()
            .filter(|l| layers.contains(&l.number))
            .map(|l| format!("#{} {}", l.number, l.title.clone().unwrap_or_else(|| l.head_branch.clone())))
            .collect();
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let t = *theme::tokens(cx);
            let (view, stack, layers) = (view.clone(), stack.clone(), layers.clone());
            let confirm = Button::new("pr-stack-confirm")
                .primary()
                .label(if merge { "Merge stack" } else { "Rebase stack" })
                .on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    let (stack, layers) = (stack.clone(), layers.clone());
                    let _ =
                        view.update(cx, |this, cx| this.run_pr_stack_action(stack, action, method, layers, window, cx));
                });
            dialog
                .entrance(DialogEntrance::None)
                .title(title.clone())
                .child(
                    v_flex()
                        .gap(px(12.))
                        .child(div().text_sm().text_color(t.muted_foreground).child(body.clone()))
                        .child(v_flex().gap(px(4.)).children(rows.iter().map(|row| {
                            div()
                                .rounded(px(6.))
                                .bg(Hsla { a: t.muted.a * 0.5, ..t.muted })
                                .px(px(12.))
                                .py(px(8.))
                                .text_sm()
                                .child(row.clone())
                        }))),
                )
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().child(Button::new("pr-stack-cancel").label("Cancel").outline()))
                        .child(confirm),
                )
        });
    }

    /// `runAction` scoped to the stack, with the heads the menu showed so a moved layer refuses.
    fn run_pr_stack_action(
        &mut self,
        stack: Rc<Stack>,
        action: &'static str,
        method: &'static str,
        layers: Vec<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.pull_request_client() else { return };
        let Some(panel) = self.pull_requests.panel.as_mut() else { return };
        if panel.pending.is_some() || !panel.stack_fresh || panel.stack_failed {
            return;
        }
        let merge = action == "merge";
        let target = if merge { Some(panel.reference.number) } else { stack.layers.last().map(|l| l.number) };
        let Some(target) = target else { return };
        panel.pending = Some(action);
        let key = panel.reference.key();
        let mut input = panel.reference.to_json();
        input["number"] = json!(target);
        input["stackNumber"] = json!(stack.number);
        input["expectedStackHeads"] = logic::stack_heads(&stack, &layers);
        input["action"] = json!(action);
        if merge {
            input["mergeMethod"] = json!(method);
        } else {
            input["updateMethod"] = json!("rebase");
        }
        let host = self
            .pull_requests
            .panel
            .as_ref()
            .and_then(|p| p.detail.as_ref())
            .map(|d| list_logic::open_on_host_label(&d.provider).trim_start_matches("Open on ").to_owned())
            .unwrap_or_default();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("pullRequests.runAction", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(panel) = this.pr_panel(&key) {
                    panel.pending = None;
                }
                match result {
                    Err(error) => toast_error(
                        window,
                        cx,
                        if merge { "Could not merge the stack" } else { "Could not rebase the stack" },
                        logic::readable_failure(
                            &list_logic::error_message(&error),
                            if merge {
                                "The host refused the merge. Check that you have write access, that the checks it requires have passed, and that the branch is not conflicting."
                            } else {
                                "The host refused the rebase. A rebase stops at the first commit that does not apply cleanly."
                            },
                        ),
                    ),
                    Ok(_) if merge => window.push_notification(
                        Notification::success(format!("{host} merged the stack or added it to its merge queue."))
                            .title("Stack merge request completed"),
                        cx,
                    ),
                    Ok(_) => toast_success(window, cx, "Stack rebased"),
                }
                this.read_pr_detail_of(&key, cx);
                this.read_pr_extras_of(&key, cx);
                this.read_pull_requests(false, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

/// One row of the thread picker.
#[derive(Debug, PartialEq)]
struct Candidate {
    id: String,
    title: String,
    project: String,
    /// It links the pull request already, so it shows "Linked" and cannot be picked.
    linked: bool,
}

/// `ThreadPicker`'s rows: `environment`'s active threads whose title or project name contains
/// `query`, newest first.
fn picker_candidates(
    threads: &[crate::model::ThreadShell],
    projects: &[crate::model::ProjectShell],
    environment: &str,
    query: &str,
    url: &str,
) -> Vec<Candidate> {
    let search = query.trim().to_lowercase();
    let project_name = |id: &str| {
        projects.iter().find(|p| p.environment_id == environment && p.id == id).map_or("", |p| p.title.as_str())
    };
    let mut rows: Vec<(&str, Candidate)> = threads
        .iter()
        .filter(|t| t.environment_id == environment && t.archived_at.is_none())
        .filter(|t| format!("{} {}", t.title, project_name(&t.project_id)).to_lowercase().contains(&search))
        .map(|t| {
            let candidate = Candidate {
                id: t.id.clone(),
                title: t.title.clone(),
                project: project_name(&t.project_id).to_owned(),
                linked: crate::thread_pull_requests::links_url(&t.pull_requests, url),
            };
            (t.updated_at.as_str(), candidate)
        })
        .collect();
    rows.sort_by(|a, b| b.0.cmp(a.0));
    rows.into_iter().map(|(_, candidate)| candidate).collect()
}

/// The row the arrows reach from `current` (`autoHighlight="always"`: the first pickable row
/// until they move), skipping rows that cannot be picked and wrapping at the ends.
fn picker_step(rows: &[Candidate], current: Option<usize>, down: bool) -> Option<usize> {
    let pickable: Vec<usize> = rows.iter().enumerate().filter(|(_, row)| !row.linked).map(|(i, _)| i).collect();
    let at = current.and_then(|current| pickable.iter().position(|&i| i == current));
    let next = match (at, down) {
        (None, _) => 0,
        (Some(at), true) => (at + 1) % pickable.len().max(1),
        (Some(at), false) => (at + pickable.len().max(1) - 1) % pickable.len().max(1),
    };
    pickable.get(next).copied()
}

/// `useThreadSearch([environment], query, { titlesOnly: true })`: an environment whose shell
/// leaves older settled threads out finds their titles from two characters.
fn picker_search_input(query: &str, before: Option<&str>) -> Option<Value> {
    let query: String = query.trim().chars().take(200).collect::<String>().trim().to_owned();
    let before = before.filter(|_| query.chars().count() >= 2)?;
    Some(json!({"query": query, "olderSettledBefore": before, "titlesOnly": true}))
}

/// `ThreadPicker`: searches the pull request's environment's threads, loading older settled
/// ones the shell left out, and picks one with a click or the arrows and Enter.
struct ThreadPicker {
    workspace: WeakEntity<Workspace>,
    environment: SharedString,
    url: String,
    search: Entity<InputState>,
    /// The thread the arrows or the pointer last highlighted.
    highlight: Option<String>,
    scroll: ScrollHandle,
    _search: Option<Task<()>>,
    /// The shell revision last drawn.
    revision: Option<u64>,
    _subscriptions: Vec<Subscription>,
}

impl ThreadPicker {
    fn new(
        workspace: WeakEntity<Workspace>,
        environment: SharedString,
        url: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search threads or projects..."));
        let typed = cx.subscribe(&search, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.highlight = None;
                this.scroll.scroll_to_item(0);
                this.search_older(cx);
                cx.notify();
            }
        });
        // Threads the search loads, and any other shell change, show as they land.
        let shell = workspace.upgrade().map(|workspace| {
            cx.observe(&workspace, |this: &mut Self, workspace, cx| {
                let revision = workspace.read(cx).shell.revision;
                if this.revision.replace(revision) != Some(revision) {
                    cx.notify();
                }
            })
        });
        Self {
            workspace,
            environment,
            url,
            search,
            highlight: None,
            scroll: ScrollHandle::new(),
            _search: None,
            revision: None,
            _subscriptions: [Some(typed), shell].into_iter().flatten().collect(),
        }
    }

    fn rows(&self, cx: &App) -> Vec<Candidate> {
        let Some(workspace) = self.workspace.upgrade() else { return Vec::new() };
        let shell = &workspace.read(cx).shell;
        let query = self.search.read(cx).value();
        picker_candidates(&shell.threads, &shell.projects, &self.environment, &query, &self.url)
    }

    /// The highlighted row: the last one highlighted while it can be picked, else the first.
    fn highlighted(&self, rows: &[Candidate]) -> Option<usize> {
        let kept = self.highlight.as_ref().and_then(|id| rows.iter().position(|r| &r.id == id && !r.linked));
        kept.or_else(|| picker_step(rows, None, true))
    }

    /// The query's older settled threads, after the search's debounce, join the shell.
    fn search_older(&mut self, cx: &mut Context<Self>) {
        self._search = None;
        let Some(workspace) = self.workspace.upgrade() else { return };
        let workspace = workspace.read(cx);
        let before = workspace.shell.older_settled(&self.environment).map(|cut| cut.before.as_str());
        let query = self.search.read(cx).value().to_string();
        let Some(input) = picker_search_input(&query, before) else { return };
        let (Some(client), generation) =
            (workspace.env_client(&self.environment), workspace.shell.generation(&self.environment))
        else {
            return;
        };
        let environment = self.environment.clone();
        self._search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(120)).await;
            let answer = client.request("orchestration.searchThreads", input).await;
            let rows = answer.map_err(|error| error.to_string()).and_then(|value| {
                serde_json::from_value::<crate::model::ThreadRows>(value).map_err(|error| error.to_string())
            });
            let rows = match rows {
                Ok(rows) => rows,
                Err(error) => {
                    log::warn!("orchestration.searchThreads: {error}");
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                let Some(workspace) = this.workspace.upgrade() else { return };
                workspace.update(cx, |workspace, cx| {
                    if workspace.shell.mount_rows(&environment, generation, rows) {
                        workspace.notify_sidebar(cx);
                        cx.notify();
                    }
                });
            });
        }));
    }

    fn pick(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |this, cx| this.change_pr_thread_link(id, true, window, cx));
        }
    }

    /// The combobox keys: Up and Down move the highlight, Enter picks it.
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.modifiers.modified() {
            return;
        }
        let rows = self.rows(cx);
        match event.keystroke.key.as_str() {
            key @ ("down" | "up") => {
                if let Some(next) = picker_step(&rows, self.highlighted(&rows), key == "down") {
                    self.highlight = Some(rows[next].id.clone());
                    self.scroll.scroll_to_item(next);
                    cx.notify();
                }
            }
            "enter" => match self.highlighted(&rows) {
                Some(index) => self.pick(rows[index].id.clone(), window, cx),
                None => return,
            },
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for ThreadPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let rows = self.rows(cx);
        let highlighted = self.highlighted(&rows);
        let list: Vec<AnyElement> = if rows.is_empty() {
            vec![
                div()
                    .px(px(12.))
                    .py(px(24.))
                    .text_center()
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .child("No active threads found.")
                    .into_any_element(),
            ]
        } else {
            rows.into_iter()
                .enumerate()
                .map(|(index, row)| {
                    let id = row.id.clone();
                    let hover_id = row.id.clone();
                    let title = if row.title.is_empty() { "Untitled thread".to_owned() } else { row.title };
                    h_flex()
                        .id(ElementId::Name(format!("pr-link-thread-{}", row.id).into()))
                        .gap(px(8.))
                        .px(px(8.))
                        .py(px(6.))
                        .rounded(px(6.))
                        .text_sm()
                        .when(highlighted == Some(index), |el| el.bg(t.accent))
                        .child(Icon::new(Lucide::MessageSquare).with_size(px(16.)).flex_none())
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().truncate().child(title))
                                .child(div().truncate().text_xs().text_color(t.muted_foreground).child(row.project)),
                        )
                        .map(|el| {
                            if row.linked {
                                el.opacity(0.64)
                                    .child(Icon::new(Lucide::Check).with_size(px(14.)).flex_none())
                                    .child(div().flex_none().text_xs().text_color(t.muted_foreground).child("Linked"))
                            } else {
                                el.cursor_pointer()
                                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                                        if this.highlight.as_ref() != Some(&hover_id) {
                                            this.highlight = Some(hover_id.clone());
                                            cx.notify();
                                        }
                                    }))
                                    .on_click(cx.listener(move |this, _, window, cx| this.pick(id.clone(), window, cx)))
                            }
                        })
                        .into_any_element()
                })
                .collect()
        };
        v_flex().gap(px(8.)).capture_key_down(cx.listener(Self::key_down)).child(Input::new(&self.search)).child(
            v_flex()
                .id("pr-link-threads")
                .max_h(px(320.))
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .children(list),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Candidate, picker_candidates, picker_search_input, picker_step};
    use crate::model::{ProjectShell, ThreadShell};
    use serde_json::json;

    const URL: &str = "https://github.com/o/r/pull/7";

    fn thread(env: &str, id: &str, title: &str, updated: &str, extra: serde_json::Value) -> ThreadShell {
        let mut value = json!({
            "id": id, "projectId": "p", "title": title, "status": "idle",
            "createdAt": updated, "updatedAt": updated, "lineage": {"rootThreadId": id},
        });
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        ThreadShell { environment_id: env.to_owned().into(), ..serde_json::from_value(value).unwrap() }
    }

    fn row(id: &str, linked: bool) -> Candidate {
        Candidate { id: id.into(), title: String::new(), project: String::new(), linked }
    }

    #[test]
    fn the_picker_lists_its_environments_active_threads_newest_first() {
        let link = json!({"pullRequests": [{"host": "github.com", "repository": "o/r", "number": 7, "url": URL,
            "source": "manual", "linkedAt": "2026-11-03T00:00:00.000Z"}]});
        let threads = [
            thread("a", "old", "Fix login", "2026-11-01T00:00:00.000Z", json!({})),
            thread("a", "new", "Docs", "2026-11-03T00:00:00.000Z", link),
            thread(
                "a",
                "gone",
                "Fix more",
                "2026-11-04T00:00:00.000Z",
                json!({"archivedAt": "2026-11-04T00:00:00.000Z"}),
            ),
            thread("b", "other", "Fix elsewhere", "2026-11-05T00:00:00.000Z", json!({})),
        ];
        let project: ProjectShell =
            serde_json::from_value(json!({"id": "p", "title": "Webapp", "workspaceRoot": "/r"})).unwrap();
        let projects = [ProjectShell { environment_id: "a".into(), ..project }];
        let ids = |query: &str| -> Vec<(String, bool)> {
            picker_candidates(&threads, &projects, "a", query, URL).into_iter().map(|c| (c.id, c.linked)).collect()
        };
        assert_eq!(ids(""), [("new".into(), true), ("old".into(), false)]);
        assert_eq!(ids(" fix "), [("old".into(), false)]);
        assert_eq!(ids("webapp").len(), 2, "the project name matches too");
    }

    #[test]
    fn the_arrows_skip_linked_threads_and_wrap() {
        let rows = [row("a", true), row("b", false), row("c", true), row("d", false)];
        assert_eq!(picker_step(&rows, None, true), Some(1), "the first pickable row starts highlighted");
        assert_eq!(picker_step(&rows, Some(1), true), Some(3));
        assert_eq!(picker_step(&rows, Some(3), true), Some(1));
        assert_eq!(picker_step(&rows, Some(1), false), Some(3));
        assert_eq!(picker_step(&[row("a", true)], None, true), None);
        assert_eq!(picker_step(&[], None, false), None);
    }

    #[test]
    fn the_picker_searches_older_settled_titles_from_two_characters() {
        let before = Some("2026-10-01T00:00:00.000Z");
        assert_eq!(picker_search_input("f", before), None);
        assert_eq!(picker_search_input("fix", None), None, "a shell with every thread needs no search");
        assert_eq!(
            picker_search_input(" fx ", before),
            Some(json!({"query": "fx", "olderSettledBefore": before, "titlesOnly": true}))
        );
    }
}
