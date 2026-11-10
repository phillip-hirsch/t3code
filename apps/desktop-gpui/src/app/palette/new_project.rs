//! New project (`startNewProject`, `submitNewProject` and the new-project groups in
//! `CommandPalette.tsx`, `CommandPaletteAddProject.tsx` and `useNewProject.ts`): the query is the
//! name, the machine is one with a new-projects folder, and a private repository on a ready host
//! is optional; then `projects.createNew`, `sourceControl.publishRepository` when asked, and a
//! draft in the new project.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use serde_json::{Value, json};

use super::super::project_clone::paths;
use super::super::source_control::{self, PublishTarget};
use super::super::{Workspace, keymap};
use super::{Description, Group, Row, RowIcon, Run, View, environment_row};
use crate::theme::Tokens;

const PUBLISH: &str = "sourceControl.publishRepository";
/// The publish toggle's row (`newProjectPublishToggleValue`).
const PUBLISH_KEY: &str = "new-project:publish";

/// `newProjectFlow` and the state beside it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct NewProjectFlow {
    pub(super) environment: SharedString,
    /// The machine whose sources opened the flow; `None` from the root's New project.
    sources_environment: Option<SharedString>,
    publishes: bool,
    /// The host picked for the repository (`newProjectPublishKind`).
    publish_kind: Option<&'static str>,
    /// `isCreatingNewProject`: `projects.createNew` is on its way.
    creating: bool,
}

/// The submit button's and footer's words (`createShortcutLabel`, `footerActionLabel`).
fn new_project_labels(highlighted: Option<&Run>, mac: bool) -> (String, &'static str) {
    let shortcut = match highlighted {
        Some(_) => format!("{} Enter", if mac { "\u{2318}" } else { "Ctrl" }),
        None => "Enter".to_owned(),
    };
    let action = match highlighted {
        None => "Create",
        Some(Run::NewProjectPublish) => "Toggle",
        Some(_) => "Select",
    };
    (shortcut, action)
}

/// `showNewProjectMachines`: several machines to choose from, or a choice that is no longer one.
fn shows_machines(options: &[SharedString], selected: &str) -> bool {
    options.len() > 1 || (!options.is_empty() && !options.iter().any(|id| id.as_ref() == selected))
}

impl Workspace {
    fn new_projects_root(&self, environment: &str) -> Option<String> {
        self.env_info(environment)?.new_projects_root.clone()
    }

    fn new_project_flow(&self) -> Option<&NewProjectFlow> {
        self.palette.as_ref().filter(|palette| palette.view == View::NewProject)?.new_project.as_ref()
    }

    /// `newProjectEnvironmentOptions`: the connected environments with a new-projects folder.
    fn new_project_environments(&self) -> Vec<super::AddEnvironment> {
        let mut options = self.add_project_environments();
        options.retain(|option| self.new_projects_root(&option.id).is_some());
        options
    }

    /// The root's "New project", while some machine can make one.
    pub(super) fn new_project_row(&self) -> Option<Row> {
        if self.new_project_environments().is_empty() {
            return None;
        }
        let mut row = Row::new("new-project", RowIcon::Lucide(IconName::FolderGit2), "New project", Run::NewProject);
        row.terms = &["new project", "create project", "empty", "repository", "repo", "git init"];
        Some(row)
    }

    /// "New project" first among `environment`'s sources, when it has a new-projects folder.
    pub(super) fn new_project_source_row(&self, environment: &SharedString) -> Option<Row> {
        self.new_projects_root(environment)?;
        let key = format!("add-project:{environment}:new");
        let run = Run::NewProjectIn(environment.clone());
        let mut row = Row::new(key, RowIcon::Lucide(IconName::FolderGit2), "New project", run);
        row.description = Some(Description::Text("Start a new Git repository from a name".into()));
        row.terms = &["new project", "create", "empty", "repository", "git init"];
        Some(row)
    }

    /// `openNewProjectFlow`: on the first machine that can make one.
    pub(super) fn open_new_project_flow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(option) = self.new_project_environments().into_iter().next() {
            self.start_new_project(option.id, None, window, cx);
        }
    }

    /// `startNewProject`.
    pub(super) fn start_new_project(
        &mut self,
        environment: SharedString,
        sources_environment: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(palette) = &mut self.palette else { return };
        palette.add_environment = Some(environment.clone());
        palette.clone = None;
        palette.new_project = Some(NewProjectFlow {
            environment,
            sources_environment,
            publishes: false,
            publish_kind: None,
            creating: false,
        });
        self.push_palette_view(View::NewProject, window, cx);
    }

    /// `popView` leaving the flow: back on the sources it came from, else on no machine.
    pub(super) fn leave_new_project(&mut self) {
        let Some(palette) = &mut self.palette else { return };
        let Some(flow) = palette.new_project.take() else { return };
        palette.add_environment = if palette.stack.is_empty() { None } else { flow.sources_environment };
    }

    /// `switchNewProjectEnvironment`.
    pub(super) fn switch_new_project_environment(&mut self, environment: SharedString, cx: &mut Context<Self>) {
        let Some(palette) = &mut self.palette else { return };
        let Some(flow) = palette.new_project.as_mut().filter(|flow| !flow.creating) else { return };
        flow.environment = environment.clone();
        palette.add_environment = Some(environment);
        self.load_palette_discovery(cx);
        cx.notify();
    }

    pub(super) fn toggle_new_project_publish(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.palette.as_mut().and_then(|p| p.new_project.as_mut()).filter(|f| !f.creating) else {
            return;
        };
        flow.publishes = !flow.publishes;
        cx.notify();
    }

    fn set_new_project_publish_kind(&mut self, kind: &'static str, cx: &mut Context<Self>) {
        let Some(flow) = self.palette.as_mut().and_then(|p| p.new_project.as_mut()).filter(|f| !f.creating) else {
            return;
        };
        flow.publish_kind = Some(kind);
        cx.notify();
    }

    /// `showExistingProjectSources`: Add existing project shows the machine's sources, going
    /// back to them when the flow started there.
    pub(super) fn show_existing_project_sources(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(flow) = self.new_project_flow().cloned() else { return };
        if flow.sources_environment.as_ref() == Some(&flow.environment) {
            return self.pop_palette_view(window, cx);
        }
        if flow.sources_environment.is_some()
            && let Some(palette) = &mut self.palette
        {
            // Off the other machine's sources too, so Back from these goes where those did.
            palette.stack.pop();
        }
        self.pop_palette_view(window, cx);
        self.open_add_project_sources(flow.environment, window, cx);
    }

    /// `getNewProjectPublishTargets` for the flow's machine; none where this connection cannot
    /// publish, since `sourceControl.publishRepository` needs `source-control:write`.
    fn new_project_publish_targets(&self, environment: &str) -> Vec<PublishTarget> {
        if !self.env_permits(environment, PUBLISH, &Value::Null) {
            return Vec::new();
        }
        source_control::new_project_publish_targets(self.palette_discovery(environment))
    }

    /// The flow's groups: its machines, the repository option, and Add existing project.
    pub(super) fn new_project_groups(&self, query: &str) -> Vec<Group> {
        let Some(flow) = self.new_project_flow() else { return Vec::new() };
        let options = self.new_project_environments();
        let ids: Vec<SharedString> = options.iter().map(|option| option.id.clone()).collect();
        let mut groups = Vec::new();
        if shows_machines(&ids, &flow.environment) {
            let rows = options
                .into_iter()
                .map(|option| {
                    let (key, run) =
                        (format!("new-project:environment:{}", option.id), Run::NewProjectMachine(option.id.clone()));
                    let checked = option.id == flow.environment;
                    let mut row = environment_row(option, key, run);
                    row.checked = checked;
                    row.disabled = flow.creating;
                    row
                })
                .collect();
            groups.push(Group { label: "Environments", rows });
        }
        let targets = self.new_project_publish_targets(&flow.environment);
        let target = source_control::new_project_publish_target(&targets, flow.publish_kind);
        let root = self.new_projects_root(&flow.environment);
        if let (Some(target), Some(root)) = (target, root) {
            let label = target.host.label;
            let name = query.trim();
            let title = if targets.len() > 1 {
                "Create private repository".to_owned()
            } else {
                format!("Create private repository on {label}")
            };
            let description = if name.is_empty() {
                target.owner.clone().unwrap_or_else(|| format!("Your {label} account"))
            } else {
                source_control::new_project_repository(
                    target.owner.as_deref(),
                    &paths::new_project_path_preview(&root, name),
                )
            };
            let mut row = Row::new(PUBLISH_KEY, RowIcon::Host(target.host.kind), title, Run::NewProjectPublish);
            row.description = Some(Description::Text(description.into()));
            row.toggle = Some(flow.publishes);
            row.disabled = flow.creating;
            groups.push(Group { label: "Options", rows: vec![row] });
        }
        let mut existing = Row::new(
            "new-project:existing",
            RowIcon::Lucide(IconName::FolderPlus),
            "Add existing project",
            Run::NewProjectExisting,
        );
        existing.description = Some(Description::Text("Open a folder or clone a repository".into()));
        existing.chevron = true;
        groups.push(Group { label: "", rows: vec![existing] });
        groups
    }

    /// `canSubmitNewProject`; `projects.createNew` also needs `orchestration:operate` there.
    fn can_submit_new_project(&self, name: &str) -> bool {
        self.new_project_flow().is_some_and(|flow| {
            !name.trim().is_empty()
                && !flow.creating
                && self.env_client(&flow.environment).is_some()
                && self.can_operate(&flow.environment)
        })
    }

    /// `submitNewProject` and `useNewProject`: creates the project and opens a draft in it. A
    /// failure keeps the palette open on the name.
    pub(super) fn submit_new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.palette_query(cx).trim().to_owned();
        if !self.can_submit_new_project(&name) {
            return;
        }
        let Some(flow) = self.new_project_flow().cloned() else { return };
        let Some(client) = self.env_client(&flow.environment) else { return };
        let publish_to = flow
            .publishes
            .then(|| {
                let targets = self.new_project_publish_targets(&flow.environment);
                source_control::new_project_publish_target(&targets, flow.publish_kind).cloned()
            })
            .flatten();
        if let Some(flow) = self.palette.as_mut().and_then(|p| p.new_project.as_mut()) {
            flow.creating = true;
        }
        cx.notify();
        let environment = flow.environment;
        // Detached: the project is made whether or not the palette stays up.
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("projects.createNew", json!({"name": name})).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(flow) = this.palette.as_mut().and_then(|p| p.new_project.as_mut()) {
                    flow.creating = false;
                }
                cx.notify();
                let created = match result {
                    Ok(created) => created,
                    Err(error) => {
                        let toast = Notification::error(error_message(&error.message()));
                        return window.push_notification(toast.title("Could not create the project"), cx);
                    }
                };
                let (Some(project_id), Some(root)) = (created["projectId"].as_str(), created["workspaceRoot"].as_str())
                else {
                    let toast = Notification::error("An error occurred.").title("Could not create the project");
                    return window.push_notification(toast, cx);
                };
                let toast = match created["commitError"].as_str() {
                    None => Notification::success(root.to_owned()).title(format!("Created {name}")),
                    Some(commit_error) => Notification::warning(format!("{commit_error} The project is in {root}."))
                        .title(format!("Created {name} without a first commit")),
                };
                window.push_notification(toast, cx);
                if let Some(target) = publish_to {
                    this.publish_new_repository(environment.clone(), root.to_owned(), target, window, cx);
                }
                this.close_palette(window, cx);
                this.settings = None;
                // The shell may not list the new project yet.
                this.start_draft_in(environment, project_id.to_owned(), window, cx);
            });
        })
        .detach();
    }

    /// `publishNewRepository`: a private repository for the new project; its failure says how to
    /// try again.
    fn publish_new_repository(
        &mut self,
        environment: SharedString,
        workspace_root: String,
        target: PublishTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = target.host.label;
        let failed = move |message: String, window: &mut Window, cx: &mut gpui_kit::App| {
            let description =
                format!("{} Use Publish Repository in the Git menu to try again.", error_message(&message));
            let toast = Notification::error(description).title(format!("Could not create the {label} repository"));
            window.push_notification(toast, cx);
        };
        let input = json!({
            "cwd": workspace_root,
            "provider": target.host.kind,
            "repository": source_control::new_project_repository(target.owner.as_deref(), &workspace_root),
            "visibility": "private",
        });
        if !self.env_permits(&environment, PUBLISH, &input) {
            return failed("This connection needs permission to write source control.".to_owned(), window, cx);
        }
        let Some(client) = self.env_client(&environment) else {
            return failed("This environment is not connected.".to_owned(), window, cx);
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request(PUBLISH, input).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Err(error) => failed(error.message(), window, cx),
                Ok(published) => {
                    let name = published["repository"]["nameWithOwner"].as_str().unwrap_or_default().to_owned();
                    window.push_notification(Notification::success(name).title(format!("Published to {label}")), cx);
                    if let Some(remote_url) = published["remoteUrl"].as_str() {
                        this.suggest_app_access(&environment, remote_url, window, cx);
                    }
                }
            });
        })
        .detach();
    }

    /// `NewProjectSummary`: the name and where it goes, above the flow's rows.
    pub(super) fn render_new_project_summary(&self, query: &str, t: &Tokens) -> Option<AnyElement> {
        let flow = self.new_project_flow()?;
        let root = self.new_projects_root(&flow.environment)?;
        let name = query.trim();
        let options = self.new_project_environments();
        let ids: Vec<SharedString> = options.iter().map(|option| option.id.clone()).collect();
        let machine = shows_machines(&ids, &flow.environment).then(|| {
            options
                .iter()
                .find(|option| option.id == flow.environment)
                .map_or_else(|| self.location_label(&flow.environment).to_string(), |option| option.label.clone())
        });
        let mut description = if name.is_empty() {
            format!("Goes in {root}")
        } else {
            format!("Creates {}", paths::new_project_path_preview(&root, name))
        };
        if let Some(machine) = machine {
            description.push_str(" on ");
            description.push_str(&machine);
        }
        let title: SharedString = if name.is_empty() { "New project".into() } else { name.to_owned().into() };
        let muted = t.muted_foreground;
        Some(
            div()
                .flex_none()
                .p(px(8.))
                .pb_0()
                .child(
                    h_flex()
                        .min_h(px(32.))
                        .items_center()
                        .gap(px(8.))
                        .px(px(8.))
                        .py(px(6.))
                        .child(
                            div()
                                .flex_none()
                                .self_start()
                                .h(px(20.))
                                .pt(px(2.))
                                .child(Icon::new(IconName::FolderGit2).size(px(16.)).text_color(muted)),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().truncate().text_sm().text_color(t.foreground).child(title))
                                .child(div().truncate().text_xs().text_color(muted.opacity(0.85)).child(description)),
                        ),
                )
                .into_any_element(),
        )
    }

    /// `CommandPaletteSubmitButton` for the flow: Create, or Creating while it runs.
    pub(super) fn render_new_project_submit(
        &self,
        query: &str,
        highlighted: Option<&Run>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let flow = self.new_project_flow()?;
        let (shortcut, _) = new_project_labels(highlighted, keymap::MAC);
        let can_operate = self.can_operate(&flow.environment);
        let tooltip =
            if can_operate { format!("Create ({shortcut})") } else { "This connection cannot add projects.".into() };
        let button = Button::new("palette-new-project-submit")
            .outline()
            .xsmall()
            .tab_stop(false)
            .label(if flow.creating { "Creating" } else { "Create" })
            .child(super::super::model_picker::kbd(&shortcut, t))
            .disabled(!self.can_submit_new_project(query))
            .tooltip(tooltip)
            .on_click(cx.listener(|this, _, window, cx| this.submit_new_project(window, cx)));
        Some(div().absolute().right(px(2.)).top_0().bottom_0().flex().items_center().child(button).into_any_element())
    }

    /// The footer's Enter label for the flow.
    pub(super) fn new_project_footer_label(&self, highlighted: Option<&Run>) -> Option<&'static str> {
        self.new_project_flow()?;
        Some(new_project_labels(highlighted, keymap::MAC).1)
    }

    /// `NewProjectPublishHostPicker`: which host the repository goes to, while there are several.
    /// It keeps its clicks from the row, which toggles.
    pub(super) fn render_publish_host_picker(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let flow = self.new_project_flow()?;
        let targets = self.new_project_publish_targets(&flow.environment);
        if targets.len() < 2 {
            return None;
        }
        let selected = source_control::new_project_publish_target(&targets, flow.publish_kind)?.host;
        let kinds: Vec<&'static str> = targets.iter().map(|target| target.host.kind).collect();
        let view = cx.entity();
        let muted = t.muted_foreground;
        let trigger = Button::new("new-project-publish-host")
            .ghost()
            .xsmall()
            .label(selected.label)
            .dropdown_caret(true)
            .disabled(flow.creating)
            .tooltip("Host for the new repository");
        let picker = trigger.dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, window, _| {
            kinds.iter().fold(menu, |menu, &kind| {
                let host = source_control::definition(kind);
                let item = PopupMenuItem::element(move |_, _| {
                    h_flex().gap(px(6.)).items_center().child(host.mark(14., muted)).child(host.label)
                })
                .checked(kind == selected.kind)
                .on_click(
                    window.listener_for(&view, move |this, _, _, cx| this.set_new_project_publish_kind(kind, cx)),
                );
                menu.item(item)
            })
        });
        Some(
            div()
                .id("new-project-publish-host-wrap")
                .flex_none()
                .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(picker)
                .into_any_element(),
        )
    }
}

/// `errorMessage`: an empty message reads "An error occurred.".
fn error_message(message: &str) -> String {
    if message.trim().is_empty() { "An error occurred.".to_owned() } else { message.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_creates_unless_a_row_is_highlighted_which_then_takes_the_modifier() {
        assert_eq!(new_project_labels(None, false), ("Enter".to_owned(), "Create"));
        assert_eq!(new_project_labels(Some(&Run::NewProjectPublish), false), ("Ctrl Enter".to_owned(), "Toggle"));
        assert_eq!(new_project_labels(Some(&Run::NewProjectExisting), true), ("\u{2318} Enter".to_owned(), "Select"));
    }

    #[test]
    fn machines_show_when_there_is_a_choice_or_the_pick_went_away() {
        let ids = |list: &[&str]| list.iter().map(|id| SharedString::from((*id).to_owned())).collect::<Vec<_>>();
        assert!(!shows_machines(&ids(&["a"]), "a"));
        assert!(shows_machines(&ids(&["a", "b"]), "a"));
        assert!(shows_machines(&ids(&["b"]), "a"), "the picked machine lost its folder or connection");
        assert!(!shows_machines(&ids(&[]), "a"));
    }
}
