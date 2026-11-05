//! Header extras: updating a branch behind its base (`PullRequestBaseBranch`), the checks
//! popover (`PullRequestChecksPopover`), native stacks (`PullRequestStackMenu`), threads linked
//! to the pull request (`PullRequestThreadLinks`) and maintainer edits (`PullRequestActionsMenu`).

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::popover::Popover;

use super::*;
use crate::pull_request_detail::Stack;

/// A thread that links this pull request (`pullRequests.linkedThreads`).
#[derive(Clone, Debug)]
pub(super) struct LinkedThread {
    id: String,
    title: String,
}

/// The pull request's key as threads record it: the link's host and lowercase repository.
fn thread_key(detail: &Detail) -> Option<(String, String, u64)> {
    let (host, repository, number) = logic::parse_change_request_url(&detail.url)?;
    Some((host, repository.to_lowercase(), number))
}

impl Workspace {
    /// The shown pull request's stack and linked threads.
    pub(super) fn read_pr_extras(&mut self, cx: &mut Context<Self>) {
        if let Some(key) = self.pull_requests.panel.as_ref().map(|p| p.reference.key()) {
            self.read_pr_extras_of(&key, cx);
        }
    }

    /// `key`'s stack and linked threads, read once the detail says what the host supports.
    pub(super) fn read_pr_extras_of(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        let Some(panel) = self.pr_panel_ref(key) else { return };
        let Some(detail) = panel.detail.clone() else { return };
        let key = key.to_owned();
        let mut reference = panel.reference.to_json();
        if detail.capabilities.stacks == Some(true) {
            let (client, key) = (client.clone(), key.clone());
            let mut input = reference.clone();
            if input.get("host").is_none()
                && let Some((host, _, _)) = logic::parse_change_request_url(&detail.url)
            {
                input["host"] = json!(host);
            }
            cx.spawn(async move |this, cx| {
                let result = client.request("pullRequests.stack", input).await;
                let _ = this.update(cx, |this, cx| {
                    let Some(panel) = this.pr_panel(&key) else { return };
                    match result.map(serde_json::from_value::<Option<Stack>>) {
                        Ok(Ok(stack)) => {
                            panel.stack = stack.map(Rc::new);
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
                            .filter(|t| t["archivedAt"].is_null())
                            .filter_map(|t| {
                                Some(LinkedThread {
                                    id: t["id"].as_str()?.to_owned(),
                                    title: t["title"].as_str().unwrap_or_default().to_owned(),
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
                                        this.perform_pr_action_with("update-branch", None, Some(method), window, cx)
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
                let (attention, running, completed) = logic::group_checks(&checks);
                let collapsible = !(attention.is_empty() && running.is_empty()) && !completed.is_empty();
                let visible: Vec<&Check> = attention
                    .iter()
                    .chain(running.iter())
                    .copied()
                    .chain(completed.iter().copied().filter(|_| show_all || !collapsible))
                    .collect();
                let rows: Vec<AnyElement> = visible
                    .iter()
                    .enumerate()
                    .map(|(index, check)| {
                        let (icon, color) = check_status_icon(&check.status, &t);
                        let url = check.url.clone();
                        h_flex()
                            .gap(px(8.))
                            .text_xs()
                            .child(Icon::new(icon).with_size(px(14.)).text_color(color))
                            .child(div().flex_1().min_w_0().truncate().child(check.name.clone()))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(t.muted_foreground)
                                    .child(logic::check_status_label(check)),
                            )
                            .children(url.map(|url| {
                                div()
                                    .id(ElementId::Name(format!("pr-check-details-{index}-{}", check.name).into()))
                                    .flex_none()
                                    .cursor_pointer()
                                    .text_color(t.primary)
                                    .hover(|s| s.underline())
                                    .child("Details")
                                    .on_click(move |_, _, cx| cx.open_url(&url))
                            }))
                            .into_any_element()
                    })
                    .collect();
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
                        } else if checks.is_empty() {
                            el.child(div().text_xs().text_color(t.muted_foreground).child("No checks reported"))
                        } else {
                            el.child(
                                v_flex()
                                    .id("pr-checks-list")
                                    .max_h(px(256.))
                                    .overflow_y_scroll()
                                    .gap(px(4.))
                                    .children(rows),
                            )
                        }
                    })
                    .when(collapsible && !stale, |el| {
                        let view = view.clone();
                        el.child(
                            div().mt(px(4.)).child(
                                ghost_button("pr-checks-show-all", &t)
                                    .child(if show_all { "Show less" } else { "Show all" })
                                    .on_click(move |_, _, cx| {
                                        view.update(cx, |this, cx| {
                                            if let Some(panel) = this.pull_requests.panel.as_mut() {
                                                panel.checks_all = !panel.checks_all;
                                            }
                                            cx.notify();
                                        })
                                    }),
                            ),
                        )
                    })
            })
            .into_any_element()
    }

    /// Linked threads: a count that lists them; picking one opens it.
    pub(super) fn render_pr_thread_count(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let linked = self.pull_requests.panel.as_ref()?.linked.clone()?;
        if linked.is_empty() {
            return None;
        }
        let view = cx.entity();
        let tip = format!("Linked from {} {}", linked.len(), if linked.len() == 1 { "thread" } else { "threads" });
        Some(
            ghost_button("pr-linked-threads", t)
                .tooltip(tip)
                .child(Icon::new(Lucide::MessageSquare).with_size(px(14.)))
                .child(linked.len().to_string())
                .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu: PopupMenu, window, _| {
                    menu = menu.label("Linked threads");
                    for thread in &linked {
                        let id = thread.id.clone();
                        menu =
                            menu.item(PopupMenuItem::new(thread.title.clone()).icon(Lucide::MessageSquare).on_click(
                                window.listener_for(&view, move |this, _, window, cx| {
                                    this.select_thread(id.clone(), window, cx)
                                }),
                            ));
                    }
                    menu
                })
                .into_any_element(),
        )
    }

    /// "Link to thread": a searchable list of active threads in this environment.
    pub(super) fn open_pr_thread_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = self.pull_requests.panel.as_ref().and_then(|p| p.detail.clone()) else { return };
        let term = logic::terminology(&detail.provider).0;
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search threads or projects..."));
        search.update(cx, |input, cx| input.focus(window, cx));
        let refresh = cx.subscribe_in(&search, window, |_, _, event: &InputEvent, window, _| {
            if matches!(event, InputEvent::Change) {
                window.refresh();
            }
        });
        if let Some(panel) = self.pull_requests.panel.as_mut() {
            panel._picker_search = Some(refresh);
        }
        let projects: HashMap<String, String> =
            self.shell.projects.iter().map(|p| (p.id.clone(), p.title.to_string())).collect();
        let mut threads: Vec<(String, String, String, String)> = self
            .shell
            .threads
            .iter()
            .filter(|t| t.archived_at.is_none())
            .map(|t| {
                (
                    t.id.clone(),
                    t.title.clone(),
                    projects.get(&t.project_id).cloned().unwrap_or_default(),
                    t.updated_at.clone(),
                )
            })
            .collect();
        threads.sort_by(|a, b| b.3.cmp(&a.3));
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let query = search.read(cx).value().to_lowercase();
            let t = *theme::tokens(cx);
            let matching: Vec<&(String, String, String, String)> = threads
                .iter()
                .filter(|(_, title, project, _)| format!("{title} {project}").to_lowercase().contains(&query))
                .collect();
            let rows: Vec<AnyElement> = if matching.is_empty() {
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
                matching
                    .into_iter()
                    .map(|(id, title, project, _)| {
                        let (id, view) = (id.clone(), view.clone());
                        h_flex()
                            .id(ElementId::Name(format!("pr-link-thread-{id}").into()))
                            .gap(px(8.))
                            .px(px(8.))
                            .py(px(6.))
                            .rounded(px(6.))
                            .text_sm()
                            .cursor_pointer()
                            .hover(|s| s.bg(t.accent))
                            .child(div().flex_1().min_w_0().truncate().child(title.clone()))
                            .child(div().flex_none().text_xs().text_color(t.muted_foreground).child(project.clone()))
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                let _ = view.update(cx, |this, cx| this.link_pr_to_thread(id.clone(), window, cx));
                            })
                            .into_any_element()
                    })
                    .collect()
            };
            dialog.entrance(DialogEntrance::None).title(format!("Link {term} to a thread")).child(
                v_flex()
                    .gap(px(8.))
                    .child(Input::new(&search))
                    .child(v_flex().id("pr-link-threads").max_h(px(320.)).overflow_y_scroll().children(rows)),
            )
        });
    }

    /// `thread.pull-request.link`, then the linked threads are read again.
    fn link_pr_to_thread(&mut self, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        let Some(detail) = self.pull_requests.panel.as_ref().and_then(|p| p.detail.clone()) else { return };
        let Some((host, repository, number)) = thread_key(&detail) else { return };
        let term = logic::terminology(&detail.provider).0;
        let command = json!({
            "type": "thread.pull-request.link", "commandId": super::super::super::new_id(), "threadId": thread_id,
            "host": host, "repository": repository, "number": number, "url": detail.url, "source": "manual",
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(_) => this.read_pr_extras(cx),
                Err(error) => toast_error(window, cx, format!("Could not link the {term}"), error.to_string()),
            });
        })
        .detach();
    }

    /// "Allow edits from maintainers" (`pullRequests.update` with `maintainerCanModify`).
    pub(super) fn save_pr_maintainer_edits(&mut self, value: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
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
                        .on_click(cx.listener(|this, _, _, cx| this.read_pr_extras(cx)))
                        .into_any_element(),
                ];
            }
            return Vec::new();
        };
        let plan = logic::stack_plan(&stack, detail.number);
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
        let mut out = vec![
            ghost_button("pr-stack", t)
                .tooltip(format!("View stack #{}, layer {position} of {}", stack.number, stack.layers.len()))
                .child(Icon::new(Lucide::Layers).with_size(px(14.)))
                .child(format!("{position}/{}", stack.layers.len()))
                .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu: PopupMenu, window, _| {
                    menu = menu.label(format!("Stack #{}", menu_stack.number));
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
                                this.open_pull_request_panel(reference.clone(), seed, cx)
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
                                    this.confirm_pr_stack(stack.clone(), "merge", method, window, cx)
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
                                    this.confirm_pr_stack(stack.clone(), "update-branch", method, window, cx)
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
                        this.confirm_pr_stack(stack.clone(), "merge", method, window, cx)
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
        let plan = logic::stack_plan(&stack, detail.number);
        let merge = action == "merge";
        let layers = if merge { plan.merge.clone() } else { plan.rebase.clone() };
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
        let Some(client) = self.client() else { return };
        let Some(panel) = self.pull_requests.panel.as_mut() else { return };
        if panel.pending.is_some() {
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
                this.read_pr_detail(cx);
                this.read_pr_extras(cx);
                this.read_pull_requests(false, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
