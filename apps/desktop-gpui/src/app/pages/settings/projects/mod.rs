//! Settings → Projects: the project group picked in the scope sentence, with its name, icon,
//! pin, archive, new-thread defaults, actions, checkouts and removal (`ProjectsSettings.tsx`,
//! `ProjectSettingsPanel.tsx`, `useProjectGroupUpdate.ts`). Name, icon, pin and archive write
//! every checkout's project record; the defaults are scoped setting rows; the grouping rule
//! is a client setting.

mod actions;
mod favicon_picker;
pub(in crate::app) mod groups;
pub(in crate::app) mod icon;
mod icon_picker;
mod script_editor;
pub(in crate::app) mod scripts;

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use serde_json::{Value, json};

use self::groups::{GroupingMode, GroupingSettings, Member, ProjectGroup};
use crate::app::pages::settings::connections::ui::tooltip;
use crate::app::pages::settings::widgets::{RowFrame, SettingsView, card, reset_button, settings_section};
use crate::app::{Workspace, new_id};
use crate::rpc::RpcClient;
use crate::settings::scope::ScopeKind;
use crate::theme::{self, Tokens};

/// What the panel remembers while Settings stays open.
pub(super) struct ProjectsPanel {
    /// The last shown group's key and checkouts, so a grouping change can follow them.
    last: Option<(String, Vec<String>)>,
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
            last: None,
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

fn toast_error(window: &mut Window, cx: &mut App, title: impl Into<SharedString>, message: impl Into<SharedString>) {
    window.push_notification(Notification::error(message).title(title), cx);
}

impl Workspace {
    pub(super) fn render_settings_projects_panel(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = window.use_keyed_state("settings-projects-panel", cx, ProjectsPanel::new);
        self.watch_project_name(&panel, window, cx);
        let regrouped;
        let view = if self.follow_regrouped_project(&panel, view, cx) {
            regrouped = self.settings_view();
            &regrouped
        } else {
            view
        };
        let t = *theme::tokens(cx);
        let group = self.scoped_project_group(&view);
        let detail = group.is_some();
        let content = match (group, &view.scope.kind) {
            (Some(group), _) => self.render_project_detail(&panel, &view, &group, window, cx),
            (None, ScopeKind::Unavailable { message }) => vec![self.render_scope_unavailable(message, &t, cx)],
            (None, _) => vec![self.render_project_choices(&view, &t, cx)],
        };
        // `SettingsPageContainer`, with `gap-6` around a project's detail.
        self.settings_page_with_gap("projects", &view, if detail { 24. } else { 32. }, content, cx)
    }

    /// Settings' project groups (`useSettingsProjectGroups`), grouped by this device's rule.
    pub(in crate::app) fn settings_project_groups(&self) -> Vec<ProjectGroup> {
        groups::build_groups(
            &self.shell.projects,
            &GroupingSettings::from_settings(&self.client_settings.values),
            &self.scratch_roots(),
        )
    }

    /// The group the scope picked, while it resolves.
    fn scoped_project_group(&self, view: &SettingsView) -> Option<ProjectGroup> {
        let key = self.settings_ui.scope.project.as_ref().filter(|_| view.scope.is_project())?;
        view.groups.iter().find(|group| &group.key == key).cloned()
    }

    /// `ProjectSettingsPanel`: when a grouping change moves the picked checkouts to another
    /// group, the scope follows them there. True when it moved.
    fn follow_regrouped_project(&mut self, panel: &Entity<ProjectsPanel>, view: &SettingsView, cx: &mut App) -> bool {
        let Some(key) = self.settings_ui.scope.project.clone() else { return false };
        if let Some(group) = view.groups.iter().find(|group| group.key == key) {
            let members: Vec<String> = group.members.iter().map(|m| m.physical_key.clone()).collect();
            if panel.read(cx).last.as_ref().is_none_or(|last| last.0 != key || last.1 != members) {
                panel.update(cx, |panel, _| panel.last = Some((key, members)));
            }
            return false;
        }
        let successor = panel
            .read(cx)
            .last
            .as_ref()
            .filter(|(last_key, _)| *last_key == key)
            .and_then(|(_, members)| groups::successor(&view.groups, members));
        let Some(successor) = successor else { return false };
        self.settings_ui.scope = self.settings_ui.scope.with_project(Some(successor.key.clone()));
        true
    }

    /// The scope's message, with a way back to every environment (`ProjectsSettings`).
    fn render_scope_unavailable(&mut self, message: &str, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let show_all = self.settings_ui.scope.machine.is_some().then(|| {
            Button::new("settings-scope-show-all")
                .outline()
                .xsmall()
                .icon(Icon::new(IconName::Layers))
                .label("Show all environments")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.settings_ui.scope = this.settings_ui.scope.with_machine(None);
                    cx.notify();
                }))
        });
        v_flex()
            .items_start()
            .gap_3()
            .child(div().text_sm().text_color(t.muted_foreground).child(message.to_owned()))
            .children(show_all)
            .into_any_element()
    }

    /// `SettingsScopeNotice target="project"`: the prompt and a row per project group.
    fn render_project_choices(&mut self, view: &SettingsView, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let rows: Vec<AnyElement> = view
            .groups
            .iter()
            .map(|group| {
                let project = group.representative();
                let shared = view.groups.iter().any(|o| o.key != group.key && o.display_name == group.display_name);
                let secondary = group
                    .archived
                    .then(|| "Archived".to_owned())
                    .into_iter()
                    .chain(shared.then(|| view.label_of(&project.environment_id).to_owned()))
                    .chain([project.workspace_root.clone()])
                    .collect::<Vec<_>>()
                    .join(" · ");
                let key = group.key.clone();
                h_flex()
                    .id(ElementId::Name(format!("settings-project-{}", group.key).into()))
                    .w_full()
                    .px_4()
                    .py_3()
                    .gap_3()
                    .cursor_pointer()
                    .hover(|s| s.bg(t.accent.opacity(0.5)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings_ui.scope = this.settings_ui.scope.with_project(Some(key.clone()));
                        cx.notify();
                    }))
                    .child(icon::project_icon(
                        project.project_icon.as_ref(),
                        &project.title,
                        self.favicon(project),
                        16.,
                        t,
                    ))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(group.display_name.clone()),
                            )
                            .child(div().truncate().text_xs().text_color(t.muted_foreground).child(secondary)),
                    )
                    .child(Icon::new(IconName::ChevronRight).size(px(16.)).text_color(t.muted_foreground))
                    .into_any_element()
            })
            .collect();
        v_flex()
            .gap_3()
            .child(
                div()
                    .px_4()
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .child("Choose a project to manage its name, icon, checkouts and actions."),
            )
            .child(card(rows, t))
            .into_any_element()
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
                    this.rename_project(title, edited, window, cx);
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        window.blur(cx);
                    }
                }
                InputEvent::Focus => {}
            });
        panel.update(cx, |panel, _| panel.subscriptions.push(subscription));
    }

    fn rename_project(&mut self, title: String, was_edited: bool, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.settings_view();
        let Some(group) = self.scoped_project_group(&view) else { return };
        if title.is_empty() {
            window.push_notification(Notification::warning("Project title cannot be empty"), cx);
            return;
        }
        let titles: Vec<&str> = group.members.iter().map(|m| m.project.title.as_str()).collect();
        if !groups::title_needs_update(&titles, &title, was_edited) {
            return;
        }
        let ids = member_ids(&group.members);
        self.update_projects(ids, json!({"title": title}), "Failed to rename project", window, cx, |_, _| {});
    }

    /// Each checkout with its own environment's socket (none while that one is disconnected)
    /// and that environment's name: a group can span machines.
    fn member_targets(&self, members: Vec<(String, String)>) -> Vec<(String, String, Option<RpcClient>, String)> {
        members
            .into_iter()
            .map(|(id, title)| {
                let environment = self.project_environment(&id);
                let label = environment.as_deref().and_then(|e| self.env_info(e)?.environment_label.clone());
                let client = environment.and_then(|e| self.env_client(&e));
                (id, title, client, label.unwrap_or_else(|| "the current environment".into()))
            })
            .collect()
    }

    /// `useProjectGroupUpdate`: writes one update to each checkout in turn, each on its own
    /// environment, and reports the first failure. `done` runs either way.
    fn update_projects(
        &mut self,
        members: Vec<(String, String)>,
        update: Value,
        failure_title: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) {
        let several = members.len() > 1;
        let targets = self.member_targets(members);
        cx.spawn_in(window, async move |this, cx| {
            let mut failure = None;
            for (id, _, client, environment) in &targets {
                let Some(client) = client else {
                    let error = format!("Connect {environment} and try again.");
                    failure = Some((environment.clone(), error));
                    break;
                };
                let mut command = json!({"type": "project.update", "commandId": new_id(), "projectId": id});
                if let (Some(command), Value::Object(fields)) = (command.as_object_mut(), update.clone()) {
                    command.extend(fields);
                }
                if let Err(error) = client.request("projects.mutate", command).await {
                    failure = Some((environment.clone(), error.message()));
                    break;
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some((environment, error)) = failure {
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
    /// Removes one checkout of the group with this key, or all of them, looked up at the click
    /// so rows don't hold copies of their group.
    fn remove_group_members(&mut self, key: &str, checkout: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.settings_view();
        let Some(group) = view.groups.iter().find(|group| group.key == key) else { return };
        let members: Vec<Member> =
            group.members.iter().filter(|m| checkout.is_none_or(|c| m.physical_key == c)).cloned().collect();
        self.remove_project_members(group, members, window, cx);
    }

    fn remove_project_members(
        &mut self,
        group: &ProjectGroup,
        members: Vec<Member>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let refs: Vec<&Member> = members.iter().collect();
        // One environment: the scope always holds every checkout of the group.
        let (whole_group, message) = groups::plan_removal(
            &refs,
            group.members.len(),
            false,
            &group.display_name,
            self.settings_info().environment_label.as_deref(),
            &self.shell.threads,
        );
        let (title, description) = message.split_once('\n').unwrap_or((&message, ""));
        let (title, description) = (SharedString::from(title.to_owned()), SharedString::from(description.to_owned()));
        let workspace = cx.weak_entity();
        let ids = member_ids(&members);
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (workspace, ids) = (workspace.clone(), ids.clone());
            alert
                .entrance(DialogEntrance::None)
                .title(title.clone())
                .description(description.clone())
                .confirm()
                .ok_text("Remove")
                .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let ids = ids.clone();
                    let _ = workspace.update(cx, |this, cx| this.delete_projects(ids, whole_group, window, cx));
                    true
                })
        });
    }

    fn delete_projects(
        &mut self,
        members: Vec<(String, String)>,
        clear_selection: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let targets = self.member_targets(members);
        cx.spawn_in(window, async move |this, cx| {
            for (id, title, client, environment) in &targets {
                let result = match client {
                    Some(client) => {
                        let command =
                            json!({"type": "project.delete", "commandId": new_id(), "projectId": id, "force": true});
                        client.request("projects.mutate", command).await.map_err(|error| error.message())
                    }
                    None => Err(format!("Connect {environment} and try again.")),
                };
                let failed = this.update_in(cx, |this, window, cx| {
                    if let Err(error) = result {
                        toast_error(window, cx, format!("Failed to remove \"{title}\""), error);
                        return true;
                    }
                    // The project's draft goes with it.
                    if let Some(draft) = this.draft_target(id) {
                        this.composer_drafts.remove(&draft);
                        if this.target.as_ref() == Some(&draft) {
                            this.target = None;
                        }
                    }
                    false
                });
                if failed.unwrap_or(true) {
                    return;
                }
            }
            if clear_selection {
                let _ = this.update(cx, |this, cx| {
                    this.settings_ui.scope = this.settings_ui.scope.with_project(None);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// `withProjectGroupingOverride`, saved to this device's settings.
    fn set_grouping_rule(
        &mut self,
        physical_key: String,
        mode: Option<GroupingMode>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut grouping = GroupingSettings::from_settings(&self.client_settings.values);
        grouping.set_override(&physical_key, mode);
        self.update_settings(serde_json::Value::Object(grouping.overrides_patch()), window, cx);
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
        let id = &member.project.id;
        let (client, endpoint) = (self.project_client(id), self.project_endpoint(id).cloned());
        images.update(cx, |images, cx| images.get(resource, client, endpoint, cx))
    }

    /// The file picker's "Open in Explorer" needs each checkout's server on this machine.
    fn can_pick_external_favicon(&self, members: &[Member]) -> bool {
        let local = |member: &Member| {
            let id = &member.project.id;
            (self.server.is_some() && self.project_environment(id).as_ref() == self.primary_environment())
                || self.project_endpoint(id).is_some_and(|endpoint| {
                    let host = endpoint.http_base.split("://").nth(1).unwrap_or_default();
                    ["127.0.0.1", "localhost", "[::1]"].iter().any(|local| host.starts_with(local))
                })
        };
        members.iter().all(|m| local(m) && favicon_picker::can_pick_external(&m.project.workspace_root))
    }

    fn render_project_detail(
        &mut self,
        panel: &Entity<ProjectsPanel>,
        view: &SettingsView,
        group: &ProjectGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let tokens = *theme::tokens(cx);
        let representative = group.members[0].clone();
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

        let ids = member_ids(&group.members);
        let can_reset_scratch_title = group.members.iter().all(|m| m.is_scratch)
            && group.members.iter().any(|m| m.project.title != groups::SCRATCH_PROJECT_TITLE);
        let icon_customized = group.members.iter().any(|m| !groups::has_default_icon(m));
        let favicon = self.project_favicon(&representative, &images, cx);

        let name_reset = can_reset_scratch_title.then(|| {
            let ids = ids.clone();
            reset_button(&"reset-project-name".into(), "Reset to default", cx).on_click(cx.listener(
                move |this, _, window, cx| {
                    let update = json!({"title": groups::SCRATCH_PROJECT_TITLE});
                    this.update_projects(ids.clone(), update, "Failed to rename project", window, cx, |_, _| {});
                },
            ))
        });
        let icon_reset = icon_customized.then(|| {
            let (ids, panel) = (ids.clone(), panel.clone());
            let update = groups::default_icon_update(representative.is_scratch);
            reset_button(&"reset-project-icon".into(), "Reset to default", cx).disabled(saving_icon).on_click(
                cx.listener(move |this, _, window, cx| {
                    this.set_project_icon(&panel, ids.clone(), update.clone(), window, cx)
                }),
            )
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
                let representative_id = representative.project.id.clone();
                let project_name: SharedString = group.display_name.clone().into();
                let pick_external = self.can_pick_external_favicon(&group.members);
                cx.listener(move |this, _, window, cx| {
                    let workspace = cx.weak_entity();
                    let (ids, panel) = (ids.clone(), panel.clone());
                    let on_pick: favicon_picker::OnPick = Rc::new(move |path, window, cx| {
                        let update = json!({"faviconPath": path, "projectIcon": null});
                        let _ = workspace
                            .update(cx, |this, cx| this.set_project_icon(&panel, ids.clone(), update, window, cx));
                    });
                    let client = this.project_client(&representative_id);
                    let endpoint = this.project_endpoint(&representative_id).cloned();
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
            RowFrame::new("Name")
                .description("The shared name for this project group in the sidebar and thread lists.")
                .reset(name_reset.map(IntoElement::into_any_element))
                .control(div().w(px(256.)).child(Input::new(&name).small()).into_any_element())
                .render(&tokens)
                .into_any_element(),
            RowFrame::new("Project icon")
                .description(icon::describe(
                    representative.project.project_icon.as_ref(),
                    representative.project.favicon_path.as_deref(),
                ))
                .reset(icon_reset.map(IntoElement::into_any_element))
                .control(icon_controls.into_any_element())
                .render(&tokens)
                .into_any_element(),
        ];
        if self.settings_info().capability("projectOrganization") {
            let pin = {
                let (panel, all_ids) = (panel.clone(), ids.clone());
                Switch::new("pin-project").checked(group.pinned).disabled(organizing).on_click(cx.listener(
                    move |this, checked: &bool, window, cx| {
                        let title = if *checked { "Failed to pin project" } else { "Failed to unpin project" };
                        this.organize_project(&panel, all_ids.clone(), json!({"pinned": *checked}), title, window, cx);
                    },
                ))
            };
            let archived = group.archived;
            let archive = {
                let (panel, all_ids) = (panel.clone(), ids.clone());
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
            overview.push(
                RowFrame::new("Pin")
                    .description("Pinned projects come first in project pickers.")
                    .control(pin.into_any_element())
                    .render(&tokens)
                    .into_any_element(),
            );
            overview.push(
                RowFrame::new("Archive")
                    .description(if archived {
                        "Hidden from project pickers. Threads and files are kept."
                    } else {
                        "Hides the project from project pickers. Threads and files are kept."
                    })
                    .control(archive.into_any_element())
                    .render(&tokens)
                    .into_any_element(),
            );
        }

        let checkouts = self.render_checkouts(group, cx);
        let danger = self.render_project_danger(group, cx);
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

        let project_id = group.representative().id.clone();
        vec![
            alert.into_any_element(),
            card(overview, &tokens),
            self.new_threads_section(view, false, window, cx),
            self.render_project_actions(&project_id, window, cx),
            checkouts,
            danger,
        ]
    }

    fn render_checkouts(&mut self, group: &ProjectGroup, cx: &mut Context<Self>) -> AnyElement {
        let tokens = *theme::tokens(cx);
        let several = group.members.len() > 1;
        let grouping = GroupingSettings::from_settings(&self.client_settings.values);
        let environment = self.settings_info().environment_label.clone().unwrap_or_else(|| "Environment".into());
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
        for member in &group.members {
            let selected = grouping.overrides.get(&member.physical_key).copied();
            let label = match selected {
                Some(mode) => mode.label().to_owned(),
                None => format!("Default ({})", grouping.mode.label()),
            };
            let menu =
                {
                    let key = member.physical_key.clone();
                    let workspace = cx.weak_entity();
                    Button::new(ElementId::Name(format!("grouping-{}", member.physical_key).into()))
                        .outline()
                        .small()
                        .w(px(288.))
                        .child(
                            h_flex().w_full().justify_between().child(label).child(
                                Icon::new(IconName::ChevronDown).size(px(14.)).text_color(tokens.muted_foreground),
                            ),
                        )
                        .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, _, _| {
                            let choices = std::iter::once(None).chain(GroupingMode::ALL.map(Some));
                            for choice in choices {
                                let (key, workspace) = (key.clone(), workspace.clone());
                                let text = choice.map_or("Use global default", GroupingMode::label);
                                menu = menu.item(PopupMenuItem::new(text).checked(choice == selected).on_click(
                                    move |_, window, cx| {
                                        let key = key.clone();
                                        let _ = workspace
                                            .update(cx, |this, cx| this.set_grouping_rule(key, choice, window, cx));
                                    },
                                ));
                            }
                            menu
                        })
                };
            let remove = several.then(|| {
                let (key, checkout) = (group.key.clone(), member.physical_key.clone());
                Button::new(ElementId::Name(format!("remove-checkout-{checkout}").into()))
                    .outline()
                    .small()
                    .label("Remove")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.remove_group_members(&key, Some(&checkout), window, cx)
                    }))
            });
            rows.push(
                RowFrame::new(environment.clone())
                    .description(member.project.workspace_root.clone())
                    .control(h_flex().gap_2().child(menu).children(remove).into_any_element())
                    .render(&tokens)
                    .into_any_element(),
            );
        }
        settings_section("checkouts", "Checkouts", rows, &tokens)
    }

    fn render_project_danger(&mut self, group: &ProjectGroup, cx: &mut Context<Self>) -> AnyElement {
        let tokens = *theme::tokens(cx);
        let count = group.members.len();
        let (title, description, label) = if count > 1 {
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
            let key = group.key.clone();
            Button::new("remove-project")
                .danger()
                .outline()
                .small()
                .icon(Icon::new(IconName::Trash).size(px(16.)))
                .label(label)
                .on_click(cx.listener(move |this, _, window, cx| this.remove_group_members(&key, None, window, cx)))
        };
        settings_section(
            "danger",
            "Danger",
            vec![
                RowFrame::new(title.clone())
                    .description(description)
                    .control(remove.into_any_element())
                    .render(&tokens)
                    .into_any_element(),
            ],
            &tokens,
        )
    }
}

/// (project id, title) for each checkout, as the update and delete loops need them.
fn member_ids(members: &[Member]) -> Vec<(String, String)> {
    members.iter().map(|m| (m.project.id.clone(), m.project.title.clone())).collect()
}
