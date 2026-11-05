//! Settings → Projects: the picked project's name, icon, pin, archive, checkouts and
//! removal (`ProjectsSettings.tsx`, `ProjectSettingsPanel.tsx`, `useProjectGroupUpdate.ts`).
//!
//! Pending the foundation: the project comes from [`Workspace::select_project_control`]
//! until the settings scope picker supplies it (with its checkout), and `section`, `row`
//! and `reset_button` stand in for the shared row widgets. Nothing here is a scoped
//! setting row: name, icon, pin and archive write the project itself, and the grouping
//! rule is a client setting. The defaults and actions sections are scoped in the web.

mod actions;
mod defaults;
mod favicon_picker;
mod groups;
mod icon;
mod icon_picker;

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Context, ElementId, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window,
    div, px,
};
use serde_json::{Value, json};

use self::groups::{GroupingMode, GroupingSettings, Member, ProjectGroup};
use crate::app::{Target, Workspace, new_id};
use crate::theme::{self, Tokens};

/// What the panel remembers while Settings stays open.
pub(super) struct ProjectsPanel {
    /// The picked project group's key.
    selected: Option<String>,
    /// One checkout of it (the scope picker's `checkout`), by physical key.
    checkout: Option<String>,
    /// The last shown checkouts, so a grouping change can follow them to their new group.
    last_members: Vec<String>,
    grouping: GroupingSettings,
    name: Entity<InputState>,
    /// The `projectKey:displayName` the name field was filled from; the web keys the input by it.
    name_for: Option<String>,
    name_edited: bool,
    saving_icon: bool,
    organizing: bool,
    images: Entity<icon::Images>,
    subscriptions: Vec<Subscription>,
}

impl ProjectsPanel {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let images = cx.new(|_| icon::Images::default());
        let subscriptions = vec![cx.observe(&images, |_, _, cx| cx.notify())];
        Self {
            selected: None,
            checkout: None,
            last_members: Vec::new(),
            grouping: GroupingSettings::load(),
            name: cx.new(|cx| InputState::new(window, cx)),
            name_for: None,
            name_edited: false,
            saving_icon: false,
            organizing: false,
            images,
            subscriptions,
        }
    }
}

/// The picked project, scoped to one checkout when the scope says so.
struct Selection {
    group: ProjectGroup,
    /// The scoped checkouts; shared settings write these.
    members: Vec<Member>,
}

impl Selection {
    fn has_other_members(&self) -> bool {
        self.members.len() < self.group.members.len()
    }

    fn representative(&self) -> &Member {
        &self.members[0]
    }
}

pub(super) fn tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |window, cx| Tooltip::new(text.clone()).build(window, cx)
}

fn toast_error(window: &mut Window, cx: &mut App, title: impl Into<SharedString>, message: impl Into<SharedString>) {
    window.push_notification(Notification::error(message).title(title), cx);
}

impl Workspace {
    pub(super) fn render_settings_projects_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let panel = window.use_keyed_state("settings-projects-panel", cx, ProjectsPanel::new);
        self.watch_project_name(&panel, window, cx);
        let groups = self.settings_project_groups(&panel, cx);
        let selection = self.resolve_project_selection(&panel, &groups, cx);
        let tokens = *theme::tokens(cx);
        let body = match (&selection, panel.read(cx).selected.is_some()) {
            (Some(selection), _) => self.render_project_detail(&panel, selection, window, cx),
            (None, true) => notice("This project is no longer available.", &tokens),
            (None, false) if groups.is_empty() => {
                notice("Add a project from the sidebar to configure it here.", &tokens)
            }
            (None, false) => notice("Choose a project to manage its name, icon, checkouts and actions.", &tokens),
        };
        // `SettingsPageContainer`: readable width, `gap-6` on this page.
        div()
            .id("settings-projects")
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .mx_auto()
                    .w_full()
                    .max_w(px(896.))
                    .px_6()
                    .pt_6()
                    .pb_12()
                    .gap_6()
                    .child(self.select_project_control(&panel, &groups, cx))
                    .child(body),
            )
            .into_any_element()
    }

    /// Stand-in for the settings scope picker: a list of projects that picks one.
    /// The parent swaps this for the scope picker, which sets `selected` and `checkout`.
    fn select_project_control(
        &mut self,
        panel: &Entity<ProjectsPanel>,
        groups: &[ProjectGroup],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *theme::tokens(cx);
        let selected = panel.read(cx).selected.clone();
        let images = panel.read(cx).images.clone();
        let items: Vec<AnyElement> = groups
            .iter()
            .map(|group| {
                let key = group.key.clone();
                let active = selected.as_deref() == Some(key.as_str());
                let favicon = self.project_favicon(&group.members[0], &images, cx);
                let project = group.representative();
                let panel = panel.clone();
                h_flex()
                    .id(ElementId::Name(format!("settings-project-{key}").into()))
                    .h(px(28.))
                    .px_2()
                    .gap_2()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_sm()
                    .when(active, |el| el.bg(tokens.accent).font_weight(FontWeight::MEDIUM))
                    .hover(|s| s.bg(tokens.accent))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        panel.update(cx, |panel, cx| {
                            panel.selected = Some(key.clone());
                            panel.checkout = None;
                            cx.notify();
                        });
                    }))
                    .child(icon::project_icon(project.project_icon.as_ref(), &project.title, favicon, 16., &tokens))
                    .child(div().truncate().child(group.display_name.clone()))
                    .when(group.archived, |el| {
                        el.child(div().text_xs().text_color(tokens.muted_foreground).child("Archived"))
                    })
                    .into_any_element()
            })
            .collect();
        section(Some("Project"), vec![v_flex().p_1().gap_px().children(items).into_any_element()], &tokens)
    }

    fn settings_project_groups(&self, panel: &Entity<ProjectsPanel>, cx: &App) -> Vec<ProjectGroup> {
        groups::build_groups(
            &self.shell.projects,
            &panel.read(cx).grouping,
            self.server_info.environment_id.as_deref().unwrap_or_default(),
            self.server_info.scratch_workspace_root.as_deref(),
        )
    }

    /// `ProjectSettingsPanel`: the picked group and its scoped checkouts, following them to
    /// the group a grouping change moved them into.
    fn resolve_project_selection(
        &self,
        panel: &Entity<ProjectsPanel>,
        groups: &[ProjectGroup],
        cx: &mut Context<Self>,
    ) -> Option<Selection> {
        let state = panel.read(cx);
        let key = state.selected.clone()?;
        let checkout = state.checkout.clone();
        let scoped = |group: &ProjectGroup| -> Vec<Member> {
            group.members.iter().filter(|m| checkout.as_ref().is_none_or(|c| *c == m.physical_key)).cloned().collect()
        };
        let found = groups.iter().find(|group| group.key == key).map(|group| (group, scoped(group)));
        let (group, members) = match found {
            Some((group, members)) if !members.is_empty() => (group, members),
            _ => {
                let group = groups::successor(groups, &state.last_members)?;
                let members = scoped(group);
                if members.is_empty() {
                    return None;
                }
                (group, members)
            }
        };
        let member_keys: Vec<String> = members.iter().map(|m| m.physical_key.clone()).collect();
        if group.key != key || member_keys != state.last_members {
            let group_key = group.key.clone();
            panel.update(cx, |panel, _| {
                panel.selected = Some(group_key);
                panel.last_members = member_keys;
            });
        }
        Some(Selection { group: group.clone(), members })
    }

    /// The name field writes on blur or Enter, and only after an edit (`renameGroup`).
    fn watch_project_name(&mut self, panel: &Entity<ProjectsPanel>, window: &mut Window, cx: &mut Context<Self>) {
        if panel.read(cx).subscriptions.len() > 1 {
            return;
        }
        let input = panel.read(cx).name.clone();
        let watched = panel.clone();
        let subscription =
            cx.subscribe_in(&input, window, move |this, state, event: &InputEvent, window, cx| match event {
                InputEvent::Change => watched.update(cx, |panel, _| panel.name_edited = true),
                // Enter commits like the web's blur; clearing focus here emits no `Blur`.
                InputEvent::PressEnter { .. } | InputEvent::Blur => {
                    let edited = watched.update(cx, |panel, _| std::mem::take(&mut panel.name_edited));
                    let title = state.read(cx).value().trim().to_owned();
                    this.rename_project(&watched, title, edited, window, cx);
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        window.blur(cx);
                    }
                }
                InputEvent::Focus => {}
            });
        panel.update(cx, |panel, _| panel.subscriptions.push(subscription));
    }

    fn rename_project(
        &mut self,
        panel: &Entity<ProjectsPanel>,
        title: String,
        was_edited: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let groups = self.settings_project_groups(panel, cx);
        let Some(selection) = self.resolve_project_selection(panel, &groups, cx) else { return };
        if title.is_empty() {
            window.push_notification(Notification::warning("Project title cannot be empty"), cx);
            return;
        }
        let titles: Vec<&str> = selection.members.iter().map(|m| m.project.title.as_str()).collect();
        if !groups::title_needs_update(&titles, &title, was_edited) {
            return;
        }
        let ids = member_ids(&selection.members);
        self.update_projects(ids, json!({"title": title}), "Failed to rename project", window, cx, |_, _| {});
    }

    /// `useProjectGroupUpdate`: writes one update to each checkout in turn and reports the
    /// first failure. `done` runs either way.
    fn update_projects(
        &mut self,
        members: Vec<(String, String)>,
        update: Value,
        failure_title: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) {
        let Some(client) = self.client() else {
            toast_error(window, cx, failure_title, "Connect the selected environment and try again.");
            done(self, cx);
            return;
        };
        let several = members.len() > 1;
        let environment =
            self.server_info.environment_label.clone().unwrap_or_else(|| "the current environment".into());
        cx.spawn_in(window, async move |this, cx| {
            let mut failure = None;
            for (id, _) in &members {
                let mut command = json!({"type": "project.update", "commandId": new_id(), "projectId": id});
                if let (Some(command), Value::Object(fields)) = (command.as_object_mut(), update.clone()) {
                    command.extend(fields);
                }
                if let Err(error) = client.request("projects.mutate", command).await {
                    failure = Some(error.to_string());
                    break;
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(error) = failure {
                    let title =
                        if several { format!("{failure_title} on {environment}") } else { failure_title.into() };
                    toast_error(window, cx, title, error);
                }
                done(this, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn set_project_icon(
        &mut self,
        panel: &Entity<ProjectsPanel>,
        members: Vec<(String, String)>,
        update: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if panel.read(cx).saving_icon {
            return;
        }
        panel.update(cx, |panel, cx| {
            panel.saving_icon = true;
            cx.notify();
        });
        let panel = panel.clone();
        self.update_projects(members, update, "Failed to update project icon", window, cx, move |_, cx| {
            panel.update(cx, |panel, cx| {
                panel.saving_icon = false;
                cx.notify();
            });
        });
    }

    /// Pin and archive belong to the whole project, even when one checkout is picked.
    fn organize_project(
        &mut self,
        panel: &Entity<ProjectsPanel>,
        members: Vec<(String, String)>,
        update: Value,
        failure_title: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        panel.update(cx, |panel, cx| {
            panel.organizing = true;
            cx.notify();
        });
        let panel = panel.clone();
        self.update_projects(members, update, failure_title, window, cx, move |_, cx| {
            panel.update(cx, |panel, cx| {
                panel.organizing = false;
                cx.notify();
            });
        });
    }

    /// `removeMembers`: confirm, then delete each checkout entry with its threads.
    fn remove_project_members(
        &mut self,
        panel: &Entity<ProjectsPanel>,
        selection: &Selection,
        members: Vec<Member>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let refs: Vec<&Member> = members.iter().collect();
        let (whole_group, message) = groups::plan_removal(
            &refs,
            selection.members.len(),
            selection.has_other_members(),
            &selection.group.display_name,
            self.server_info.environment_label.as_deref(),
            &self.shell.threads,
        );
        let clear_selection = whole_group && !selection.has_other_members();
        let (title, description) = message.split_once('\n').unwrap_or((&message, ""));
        let (title, description) = (SharedString::from(title.to_owned()), SharedString::from(description.to_owned()));
        let workspace = cx.weak_entity();
        let panel = panel.clone();
        let ids = member_ids(&members);
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (workspace, panel, ids) = (workspace.clone(), panel.clone(), ids.clone());
            alert
                .entrance(DialogEntrance::None)
                .title(title.clone())
                .description(description.clone())
                .confirm()
                .ok_text("Remove")
                .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let (panel, ids) = (panel.clone(), ids.clone());
                    let _ =
                        workspace.update(cx, |this, cx| this.delete_projects(panel, ids, clear_selection, window, cx));
                    true
                })
        });
    }

    fn delete_projects(
        &mut self,
        panel: Entity<ProjectsPanel>,
        members: Vec<(String, String)>,
        clear_selection: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client() else { return };
        cx.spawn_in(window, async move |this, cx| {
            for (id, title) in &members {
                let command = json!({"type": "project.delete", "commandId": new_id(), "projectId": id, "force": true});
                let result = client.request("projects.mutate", command).await;
                let failed = this.update_in(cx, |this, window, cx| {
                    if let Err(error) = result {
                        toast_error(window, cx, format!("Failed to remove \"{title}\""), error.to_string());
                        return true;
                    }
                    // The project's draft goes with it.
                    let draft = Target::Draft { project_id: id.clone() };
                    this.composer_drafts.remove(&draft);
                    if this.target.as_ref() == Some(&draft) {
                        this.target = None;
                    }
                    false
                });
                if failed.unwrap_or(true) {
                    return;
                }
            }
            if clear_selection {
                let _ = panel.update(cx, |panel, cx| {
                    panel.selected = None;
                    panel.checkout = None;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn set_grouping_rule(
        &mut self,
        panel: &Entity<ProjectsPanel>,
        physical_key: String,
        mode: Option<GroupingMode>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let saved = panel.update(cx, |panel, cx| {
            panel.grouping.set_override(&physical_key, mode);
            cx.notify();
            panel.grouping.save()
        });
        if let Err(error) = saved {
            log::error!("failed to save checkout grouping rule: {error}");
            toast_error(window, cx, "Could not save grouping rule", error.to_string());
        }
    }

    /// The project's own favicon when it shows one (no custom icon).
    fn project_favicon(
        &self,
        member: &Member,
        images: &Entity<icon::Images>,
        cx: &mut Context<Self>,
    ) -> Option<std::sync::Arc<gpui_kit::Image>> {
        if member.project.project_icon.as_ref().is_some_and(|icon| !icon.is_null()) {
            return None;
        }
        let resource = icon::favicon_resource(&member.project.workspace_root, member.project.favicon_path.as_deref());
        let (client, endpoint) = (self.client(), self.endpoint.clone());
        images.update(cx, |images, cx| images.get(resource, client, endpoint, cx))
    }

    /// The file picker's "Open in Explorer" needs the server on this machine.
    fn can_pick_external_favicon(&self, members: &[Member]) -> bool {
        let local = self.server.is_some()
            || self.endpoint.as_ref().is_some_and(|endpoint| {
                let host = endpoint.http_base.split("://").nth(1).unwrap_or_default();
                ["127.0.0.1", "localhost", "[::1]"].iter().any(|local| host.starts_with(local))
            });
        local && members.iter().all(|m| favicon_picker::can_pick_external(&m.project.workspace_root))
    }

    fn render_project_detail(
        &mut self,
        panel: &Entity<ProjectsPanel>,
        selection: &Selection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *theme::tokens(cx);
        let group = &selection.group;
        let representative = selection.representative().clone();
        let state = panel.read(cx);
        let (saving_icon, organizing, images, name) =
            (state.saving_icon, state.organizing, state.images.clone(), state.name.clone());

        // `key={projectKey:displayName}`: a new project or name refills the field.
        let name_key = format!("{}:{}", group.key, group.display_name);
        if state.name_for.as_deref() != Some(name_key.as_str()) {
            let display_name = group.display_name.clone();
            name.update(cx, |input, cx| input.set_value(display_name, window, cx));
            panel.update(cx, |panel, _| {
                panel.name_for = Some(name_key);
                panel.name_edited = false;
            });
        }

        let ids = member_ids(&selection.members);
        let all_ids = member_ids(&group.members);
        let can_reset_scratch_title = selection.members.iter().all(|m| m.is_scratch)
            && selection.members.iter().any(|m| m.project.title != groups::SCRATCH_PROJECT_TITLE);
        let icon_customized = selection.members.iter().any(|m| !groups::has_default_icon(m));
        let favicon = self.project_favicon(&representative, &images, cx);

        let name_reset = can_reset_scratch_title.then(|| {
            let ids = ids.clone();
            reset_button("reset-project-name", false, &tokens).on_click(cx.listener(move |this, _, window, cx| {
                let update = json!({"title": groups::SCRATCH_PROJECT_TITLE});
                this.update_projects(ids.clone(), update, "Failed to rename project", window, cx, |_, _| {});
            }))
        });
        let icon_reset = icon_customized.then(|| {
            let (ids, panel) = (ids.clone(), panel.clone());
            let update = groups::default_icon_update(representative.is_scratch);
            reset_button("reset-project-icon", saving_icon, &tokens).on_click(cx.listener(
                move |this, _, window, cx| this.set_project_icon(&panel, ids.clone(), update.clone(), window, cx),
            ))
        });

        let icon_controls = {
            let open_icons = {
                let (ids, panel) = (ids.clone(), panel.clone());
                let current = representative.project.project_icon.clone();
                let title = representative.project.title.clone();
                cx.listener(move |_, _, window, cx| {
                    let workspace = cx.weak_entity();
                    let (ids, panel) = (ids.clone(), panel.clone());
                    let on_select: icon_picker::OnSelect = Rc::new(move |icon, window, cx| {
                        let update = json!({"faviconPath": null, "projectIcon": icon});
                        let _ = workspace
                            .update(cx, |this, cx| this.set_project_icon(&panel, ids.clone(), update, window, cx));
                    });
                    let picker =
                        cx.new(|cx| icon_picker::IconPicker::new(current.as_ref(), &title, on_select, window, cx));
                    // No exit transition: here it can stall, and the closing dialog keeps eating clicks.
                    window.open_dialog(cx, move |dialog, _, _| {
                        dialog
                            .entrance(DialogEntrance::None)
                            .title("Choose project icon")
                            .w(px(512.))
                            .child(picker.clone())
                    });
                })
            };
            let open_files = {
                let (ids, panel, images) = (ids.clone(), panel.clone(), images.clone());
                let cwd = representative.project.workspace_root.clone();
                let project_name: SharedString = group.display_name.clone().into();
                let pick_external = self.can_pick_external_favicon(&selection.members);
                cx.listener(move |this, _, window, cx| {
                    let workspace = cx.weak_entity();
                    let (ids, panel) = (ids.clone(), panel.clone());
                    let on_pick: favicon_picker::OnPick = Rc::new(move |path, window, cx| {
                        let update = json!({"faviconPath": path, "projectIcon": null});
                        let _ = workspace
                            .update(cx, |this, cx| this.set_project_icon(&panel, ids.clone(), update, window, cx));
                    });
                    let (client, endpoint) = (this.client(), this.endpoint.clone());
                    let (cwd, project_name, images) = (cwd.clone(), project_name.clone(), images.clone());
                    let picker = cx.new(|cx| {
                        favicon_picker::FaviconPicker::new(
                            cwd,
                            project_name,
                            client,
                            endpoint,
                            images,
                            pick_external,
                            on_pick,
                            window,
                            cx,
                        )
                    });
                    favicon_picker::open_picker(window, cx, picker);
                })
            };
            h_flex()
                .gap_2()
                .child(icon::project_icon(
                    representative.project.project_icon.as_ref(),
                    &representative.project.title,
                    favicon,
                    f32::from(icon::PANEL_ICON),
                    &tokens,
                ))
                .child(
                    Button::new("choose-project-icon")
                        .outline()
                        .small()
                        .label("Choose icon")
                        .disabled(saving_icon)
                        .on_click(open_icons),
                )
                .child(
                    Button::new("choose-project-icon-file")
                        .outline()
                        .small()
                        .label("Choose file")
                        .disabled(saving_icon)
                        .on_click(open_files),
                )
        };

        let mut overview = vec![
            row(
                "Name",
                Some("The shared name for this project group in the sidebar and thread lists.".into()),
                name_reset.map(IntoElement::into_any_element),
                Some(div().w(px(256.)).child(Input::new(&name).small()).into_any_element()),
                None,
                &tokens,
            ),
            row(
                "Project icon",
                Some(
                    icon::describe(
                        representative.project.project_icon.as_ref(),
                        representative.project.favicon_path.as_deref(),
                    )
                    .into(),
                ),
                icon_reset.map(IntoElement::into_any_element),
                Some(icon_controls.into_any_element()),
                None,
                &tokens,
            ),
        ];
        if self.server_info.project_organization {
            let pin = {
                let (panel, all_ids) = (panel.clone(), all_ids.clone());
                Switch::new("pin-project").checked(group.pinned).disabled(organizing).on_click(cx.listener(
                    move |this, checked: &bool, window, cx| {
                        let title = if *checked { "Failed to pin project" } else { "Failed to unpin project" };
                        this.organize_project(&panel, all_ids.clone(), json!({"pinned": *checked}), title, window, cx);
                    },
                ))
            };
            let archived = group.archived;
            let archive = {
                let (panel, all_ids) = (panel.clone(), all_ids.clone());
                Button::new("archive-project")
                    .outline()
                    .small()
                    .label(if archived { "Unarchive" } else { "Archive" })
                    .disabled(organizing)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let title = if archived { "Failed to unarchive project" } else { "Failed to archive project" };
                        this.organize_project(
                            &panel,
                            all_ids.clone(),
                            json!({"archived": !archived}),
                            title,
                            window,
                            cx,
                        );
                    }))
            };
            overview.push(row(
                "Pin",
                Some("Pinned projects come first in project pickers.".into()),
                None,
                Some(pin.into_any_element()),
                None,
                &tokens,
            ));
            overview.push(row(
                "Archive",
                Some(
                    if archived {
                        "Hidden from project pickers. Threads and files are kept."
                    } else {
                        "Hides the project from project pickers. Threads and files are kept."
                    }
                    .into(),
                ),
                None,
                Some(archive.into_any_element()),
                None,
                &tokens,
            ));
        }

        let checkouts = self.render_checkouts(panel, selection, window, cx);
        let danger = self.render_project_danger(panel, selection, cx);
        let alert = h_flex()
            .gap_2()
            .px(px(14.))
            .py_3()
            .rounded(px(12.))
            .border_1()
            .border_color(tokens.info.opacity(0.32))
            .bg(tokens.info.opacity(0.04))
            .text_sm()
            .child(Icon::new(IconName::Info).size(px(16.)).text_color(tokens.info))
            .child("Can't find a setting? Keep this project picked above and hop to any other settings page.");

        v_flex()
            .gap_6()
            .child(alert)
            .child(section(None, overview, &tokens))
            .children(self.render_project_defaults(&selection.group, window, cx))
            .children(self.render_project_actions(&selection.group, window, cx))
            .child(checkouts)
            .child(danger)
            .into_any_element()
    }

    fn render_checkouts(
        &mut self,
        panel: &Entity<ProjectsPanel>,
        selection: &Selection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *theme::tokens(cx);
        let several = selection.members.len() > 1;
        let grouping = panel.read(cx).grouping.clone();
        let environment = self.server_info.environment_label.clone().unwrap_or_else(|| "Environment".into());
        let mut rows = vec![
            div()
                .px_4()
                .py(px(10.))
                .text_xs()
                .text_color(tokens.muted_foreground)
                .child(
                    "Grouping rule decides how each checkout joins project groups in the sidebar. \
                     Changing it can move a checkout to a different project.",
                )
                .into_any_element(),
        ];
        // One environment: each checkout is its own row, titled by the environment.
        for member in &selection.members {
            let selected = grouping.overrides.get(&member.physical_key).copied();
            let label = match selected {
                Some(mode) => mode.label().to_owned(),
                None => format!("Default ({})", grouping.mode.label()),
            };
            let menu = {
                let (panel, key) = (panel.clone(), member.physical_key.clone());
                let workspace = cx.weak_entity();
                Button::new(ElementId::Name(format!("grouping-{}", member.physical_key).into()))
                    .outline()
                    .small()
                    .w(px(288.))
                    .child(
                        h_flex()
                            .w_full()
                            .justify_between()
                            .child(label)
                            .child(Icon::new(IconName::ChevronDown).size(px(14.)).text_color(tokens.muted_foreground)),
                    )
                    .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, _, _| {
                        let choices = std::iter::once(None).chain(GroupingMode::ALL.map(Some));
                        for choice in choices {
                            let (panel, key, workspace) = (panel.clone(), key.clone(), workspace.clone());
                            let text = choice.map_or("Use global default", GroupingMode::label);
                            menu = menu.item(PopupMenuItem::new(text).checked(choice == selected).on_click(
                                move |_, window, cx| {
                                    let key = key.clone();
                                    let _ = workspace
                                        .update(cx, |this, cx| this.set_grouping_rule(&panel, key, choice, window, cx));
                                },
                            ));
                        }
                        menu
                    })
            };
            let remove = several.then(|| {
                let (panel, member) = (panel.clone(), member.clone());
                let scoped = Selection { group: selection.group.clone(), members: selection.members.clone() };
                Button::new(ElementId::Name(format!("remove-checkout-{}", member.physical_key).into()))
                    .outline()
                    .small()
                    .label("Remove")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.remove_project_members(&panel, &scoped, vec![member.clone()], window, cx)
                    }))
            });
            rows.push(row(
                &environment,
                Some(member.project.workspace_root.clone().into()),
                None,
                Some(h_flex().gap_2().child(menu).children(remove).into_any_element()),
                None,
                &tokens,
            ));
        }
        section(Some("Checkouts"), rows, &tokens)
    }

    fn render_project_danger(
        &mut self,
        panel: &Entity<ProjectsPanel>,
        selection: &Selection,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *theme::tokens(cx);
        let count = selection.members.len();
        let (title, description, label) = if selection.has_other_members() {
            (
                "Remove checkout".to_owned(),
                "Deletes the selected machine's checkout entries and their threads. Other machines and files on disk are not touched.".to_owned(),
                "Remove checkout",
            )
        } else if count > 1 {
            (
                "Remove this project everywhere".to_owned(),
                format!(
                    "Deletes all {count} checkout entries and their threads on every machine. Files on disk are not touched."
                ),
                "Remove all entries",
            )
        } else {
            (
                "Remove project".to_owned(),
                "Deletes the project entry and its threads. Files on disk are not touched.".to_owned(),
                "Remove project",
            )
        };
        let remove = {
            let panel = panel.clone();
            let selection = Selection { group: selection.group.clone(), members: selection.members.clone() };
            Button::new("remove-project")
                .danger()
                .outline()
                .small()
                .icon(Icon::new(IconName::Trash).size(px(16.)))
                .label(label)
                .on_click(cx.listener(move |this, _, window, cx| {
                    let members = selection.members.clone();
                    this.remove_project_members(&panel, &selection, members, window, cx)
                }))
        };
        section(
            Some("Danger"),
            vec![row(&title, Some(description.into()), None, Some(remove.into_any_element()), None, &tokens)],
            &tokens,
        )
    }
}

/// (project id, title) for each checkout, as the update and delete loops need them.
fn member_ids(members: &[Member]) -> Vec<(String, String)> {
    members.iter().map(|m| (m.project.id.clone(), m.project.title.clone())).collect()
}

/// `SettingsScopeNotice`, centered under the picker.
fn notice(text: &'static str, tokens: &Tokens) -> AnyElement {
    div().py_8().text_center().text_sm().text_color(tokens.muted_foreground).child(text).into_any_element()
}

/// `SettingsSection` (grouped): a heading, then rows in a bordered card divided by hairlines.
fn section(title: Option<&str>, rows: Vec<AnyElement>, tokens: &Tokens) -> AnyElement {
    let count = rows.len();
    v_flex()
        .gap(px(10.))
        .children(title.map(|title| {
            h_flex()
                .min_h(px(28.))
                .px_4()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(tokens.foreground)
                .child(title.to_owned())
        }))
        .child(
            v_flex()
                .rounded(px(12.))
                .border_1()
                .border_color(tokens.border.opacity(0.6))
                .bg(tokens.card.opacity(0.4))
                .children(rows.into_iter().enumerate().map(move |(index, row)| {
                    div()
                        .when(index > 0 && index < count, |el| el.border_t_1().border_color(tokens.border.opacity(0.5)))
                        .child(row)
                })),
        )
        .into_any_element()
}

/// `SettingsRow`: title and reset slot over the description, the control on the right.
fn row(
    title: &str,
    description: Option<SharedString>,
    reset: Option<AnyElement>,
    control: Option<AnyElement>,
    children: Option<AnyElement>,
    tokens: &Tokens,
) -> AnyElement {
    v_flex()
        .px_4()
        .map(|el| if children.is_some() { el.pt_3().pb_1() } else { el.py_3() })
        .child(
            h_flex()
                .gap_8()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(
                            h_flex()
                                .min_h(px(20.))
                                .gap(px(6.))
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(tokens.foreground)
                                        .child(title.to_owned()),
                                )
                                .child(div().size(px(20.)).flex().items_center().justify_center().children(reset)),
                        )
                        .children(description.map(|description| {
                            div()
                                .max_w(px(672.))
                                .text_xs()
                                .line_height(px(18.))
                                .text_color(tokens.muted_foreground)
                                .child(description)
                        })),
                )
                .children(control.map(|control| h_flex().flex_none().justify_end().gap_2().child(control))),
        )
        .children(children)
        .into_any_element()
}

/// `SettingResetButton`: a micro ghost button with the undo arrow.
fn reset_button(id: &'static str, disabled: bool, tokens: &Tokens) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .text_color(tokens.muted_foreground)
        .when(disabled, |el| el.opacity(0.64))
        .when(!disabled, |el| el.cursor_pointer().hover(|s| s.bg(tokens.accent).text_color(tokens.foreground)))
        .tooltip(tooltip("Reset to default"))
        .child(Icon::new(IconName::Undo2).size(px(12.)))
}
