//! Add project's clone sources and flow (`buildAddProjectSourceGroups`,
//! `startAddProjectClone` and `submitAddProjectCloneFlow` in `CommandPalette.tsx`): pick Git URL
//! or a ready host, type the repository (a host's is looked up), then pick where it lands and
//! `projectClone.start` it. Its progress then shows in the clone toasts and composer banner.

use gpui_kit::assets::IconName;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{Context, SharedString, Window};
use serde_json::{Value, json};

use super::super::project_clone::paths;
use super::super::source_control::{self, Surface};
use super::super::{Workspace, new_id};
use super::{
    CloneFlow, CloneTarget, Description, Row, RowIcon, Run, View, is_explicit_relative, is_windows_absolute,
    is_windows_platform, local_folder_row, project_title_from_path, resolve_project_path,
};

const CLONE_TERMS: &[&str] = &["clone", "remote", "repository", "repo", "git"];
const SETUP_TERMS: &[&str] = &["clone", "remote", "repository", "repo", "git", "setup required"];
const NEEDS_GRANTS: &str = "This connection needs permission to write source control and add projects.";

fn toast_error(window: &mut Window, cx: &mut gpui_kit::App, title: &'static str, message: impl Into<SharedString>) {
    window.push_notification(Notification::error(message).title(title), cx);
}

/// `getDefaultCloneUrl`: the host's default transport.
fn default_clone_url(repository: &Value) -> Option<String> {
    let kind = repository["provider"].as_str().unwrap_or_default();
    let field = if source_control::clones_over_https(kind) { "url" } else { "sshUrl" };
    repository[field].as_str().map(str::to_owned)
}

impl Workspace {
    /// `projectClone.start`'s `permissionAtom`, and `orchestration:operate` to add the project.
    pub(super) fn can_clone_on(&self, environment: &str) -> bool {
        self.env_permits(environment, "projectClone.start", &Value::Null) && self.can_operate(environment)
    }

    /// The environment's discovery, once [`Self::load_palette_discovery`] answered for it.
    pub(super) fn palette_discovery(&self, environment: &str) -> Option<&Value> {
        let (env, discovery) = self.palette.as_ref()?.discovery.as_ref()?;
        (env == environment).then_some(discovery.as_ref()).flatten()
    }

    /// `sourceControlEnvironment.discovery` for the add-project environment, once per palette.
    pub(super) fn load_palette_discovery(&mut self, cx: &mut Context<Self>) {
        let Some(environment) = self.browse_environment() else { return };
        let Some(client) = self.env_client(&environment) else { return };
        let Some(palette) = &mut self.palette else { return };
        if palette.discovery.as_ref().is_some_and(|(env, _)| *env == environment) {
            return;
        }
        palette.discovery = Some((environment.clone(), None));
        palette._discovery_task = Some(cx.spawn(async move |this, cx| {
            let result = client.request("server.discoverSourceControl", json!({})).await;
            let _ = this.update(cx, |this, cx| {
                let Some((env, discovery)) = this.palette.as_mut().and_then(|p| p.discovery.as_mut()) else { return };
                if *env != environment {
                    return;
                }
                match result {
                    Ok(value) => *discovery = Some(value),
                    Err(error) => log::warn!("server.discoverSourceControl failed: {error}"),
                }
                cx.notify();
            });
        }));
    }

    /// `buildAddProjectSourceGroups`: New project where the machine has a folder for them, Local
    /// folder, Git URL, then each host, ready ones first; one that is not set up is listed dimmed
    /// with Setup Required.
    pub(super) fn add_project_source_rows(&self, environment: SharedString) -> Vec<Row> {
        let discovery = self.palette_discovery(&environment);
        let mut rows: Vec<Row> = self.new_project_source_row(&environment).into_iter().collect();
        rows.push(local_folder_row(environment.clone()));
        let url = Run::CloneFrom(environment.clone(), None);
        let mut row =
            Row::new(format!("add-project:{environment}:url"), RowIcon::Lucide(IconName::Link), "Git URL", url);
        row.description = Some(Description::Text("Clone from a remote URL".into()));
        row.terms = CLONE_TERMS;
        row.data_terms = vec!["Git URL".into()];
        rows.push(row);
        for host in source_control::sorted_by_readiness(discovery, Surface::AddProject) {
            let readiness = source_control::palette_readiness(discovery, host.kind);
            let run = Run::CloneFrom(environment.clone(), Some(host.kind));
            let key = format!("add-project:{environment}:{}", host.kind);
            let mut row = Row::new(key, RowIcon::Host(host.kind), format!("{} repository", host.label), run);
            row.description = Some(Description::Text(format!("Clone {} {}", host.label, host.path_hint).into()));
            row.data_terms = vec![host.label.into()];
            row.terms = CLONE_TERMS;
            if !readiness.ready {
                row.disabled = true;
                row.terms = SETUP_TERMS;
                let hint = readiness
                    .hint
                    .unwrap_or_else(|| "Open Settings -> Source control to configure this provider.".into());
                row.setup = Some(hint.into());
            }
            rows.push(row);
        }
        rows
    }

    /// `startAddProjectClone`: the repository field for `source` on `environment`.
    pub(super) fn start_add_project_clone(
        &mut self,
        environment: SharedString,
        source: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(palette) = &mut self.palette else { return };
        palette.add_environment = Some(environment.clone());
        palette.clone = Some(CloneFlow { environment, source, confirm: None });
        palette.clone_pending = false;
        self.push_palette_view(View::Clone, window, cx);
    }

    /// The flow while it is still the one `flow` started from.
    fn clone_flow_is(&self, flow: &CloneFlow) -> bool {
        self.palette.as_ref().is_some_and(|palette| palette.view == View::Clone && palette.clone.as_ref() == Some(flow))
    }

    /// `submitAddProjectCloneFlow`'s repository step: a URL goes straight on, a host's
    /// repository is looked up first.
    pub(super) fn submit_clone_repository(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.palette_query(cx);
        let Some(palette) = &self.palette else { return };
        let Some(flow) = palette.clone.clone().filter(|flow| flow.confirm.is_none()) else { return };
        let Some(client) = self.env_client(&flow.environment) else {
            let label = self.location_label(&flow.environment);
            return toast_error(window, cx, "Environment unavailable", format!("{label} is not connected."));
        };
        let raw = query.trim().to_owned();
        if raw.is_empty() || palette.clone_pending {
            return;
        }
        if let Some(message) = paths::github_incomplete_owner_message(flow.source, &raw) {
            return toast_error(window, cx, "Repository name incomplete", message);
        }
        let Some(kind) = flow.source else {
            let remote_url = paths::normalize_pasted_clone_url(&raw);
            let target = CloneTarget { input: raw.clone(), repository: None, remote_url };
            return self.confirm_clone(target, &raw, window, cx);
        };
        if let Some(palette) = &mut self.palette {
            palette.clone_pending = true;
        }
        cx.notify();
        let input = json!({"provider": kind, "repository": raw});
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = client.request("sourceControl.lookupRepository", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                // Back, a closed palette or another source leaves the answer unused.
                if !this.clone_flow_is(&flow) {
                    return;
                }
                if let Some(palette) = &mut this.palette {
                    palette.clone_pending = false;
                }
                cx.notify();
                let repository = match result {
                    Ok(repository) => repository,
                    Err(error) => return toast_error(window, cx, "Repository lookup failed", error.message()),
                };
                let (Some(name), Some(url), Some(remote_url)) =
                    (repository["nameWithOwner"].as_str(), repository["url"].as_str(), default_clone_url(&repository))
                else {
                    return toast_error(window, cx, "Repository lookup failed", "An error occurred.");
                };
                let target =
                    CloneTarget { input: raw, repository: Some((name.to_owned(), url.to_owned())), remote_url };
                this.confirm_clone(target, name, window, cx);
            });
        });
        if let Some(palette) = &mut self.palette {
            palette._clone_task = Some(task);
        }
    }

    /// The destination step, starting at the environment's base folder plus the clone's folder.
    fn confirm_clone(&mut self, target: CloneTarget, repository: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(environment) = self.palette.as_ref().and_then(|p| p.clone.as_ref()).map(|f| f.environment.clone())
        else {
            return;
        };
        let folder = paths::clone_directory_path(&target.remote_url, self.ghq_layout(&environment), repository);
        let destination = paths::clone_destination_path(&self.add_project_base(&environment), &folder);
        let Some(palette) = &mut self.palette else { return };
        let Some(flow) = &mut palette.clone else { return };
        flow.confirm = Some(target);
        palette.highlight = None;
        self.set_palette_query(&destination, window, cx);
    }

    /// `submitAddProjectCloneFlow`'s destination step: `projectClone.start`, then a draft in the
    /// new project while the clone runs.
    pub(super) fn submit_clone_destination(&mut self, raw: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(palette) = &self.palette else { return };
        let Some(flow) = palette.clone.clone() else { return };
        let Some(target) = flow.confirm.clone() else { return };
        let environment = flow.environment.clone();
        let Some(client) = self.env_client(&environment) else {
            let label = self.location_label(&environment);
            return toast_error(window, cx, "Environment unavailable", format!("{label} is not connected."));
        };
        if !self.can_clone_on(&environment) {
            return toast_error(window, cx, "Clone unavailable", NEEDS_GRANTS);
        }
        let raw = raw.trim();
        if raw.is_empty() || palette.clone_pending {
            return;
        }
        let platform = self.env_info(&environment).and_then(|info| info.platform_os.as_deref());
        if is_windows_absolute(raw) && platform.is_some_and(|os| !is_windows_platform(os)) {
            return toast_error(window, cx, "Clone failed", "Windows-style paths are only supported on Windows.");
        }
        let cwd = self.browse_cwd();
        if is_explicit_relative(raw) && cwd.is_none() {
            return toast_error(window, cx, "Clone failed", "Relative paths require an active project.");
        }
        let destination = resolve_project_path(raw, cwd.as_deref());
        if destination.is_empty() {
            return;
        }
        let project_id = new_id();
        let input = json!({
            "projectId": project_id,
            "title": project_title_from_path(&destination),
            "createdAt": crate::timestamp::iso(crate::timestamp::now_ms()),
            "remoteUrl": target.remote_url,
            "destinationPath": destination,
        });
        if !self.env_permits(&environment, "projectClone.start", &input) {
            return toast_error(window, cx, "Clone unavailable", NEEDS_GRANTS);
        }
        if let Some(palette) = &mut self.palette {
            palette.clone_pending = true;
        }
        cx.notify();
        // Detached: the clone runs whether or not the palette stays up.
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("projectClone.start", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let current = this.clone_flow_is(&flow);
                if current && let Some(palette) = &mut this.palette {
                    palette.clone_pending = false;
                }
                cx.notify();
                if let Err(error) = result {
                    return toast_error(window, cx, "Clone failed", error.message());
                }
                this.suggest_app_access(&environment, &target.remote_url, window, cx);
                // Only a palette still on this clone moves on to the new project.
                if current {
                    this.close_palette(window, cx);
                    this.settings = None;
                    this.start_draft_in(environment, project_id, window, cx);
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::default_clone_url;

    #[test]
    fn a_looked_up_repository_clones_over_its_hosts_default_transport() {
        let repository =
            |provider: &str| json!({"provider": provider, "url": "https://h/o/r", "sshUrl": "git@h:o/r.git"});
        assert_eq!(default_clone_url(&repository("github")).as_deref(), Some("https://h/o/r"));
        assert_eq!(default_clone_url(&repository("forgejo")).as_deref(), Some("https://h/o/r"));
        assert_eq!(default_clone_url(&repository("gitlab")).as_deref(), Some("git@h:o/r.git"));
        assert_eq!(default_clone_url(&repository("azure-devops")).as_deref(), Some("git@h:o/r.git"));
    }
}
