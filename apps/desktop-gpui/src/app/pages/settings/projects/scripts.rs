//! Project actions (project scripts): the pure logic of `apps/web/src/projectScripts.ts`,
//! `@t3tools/shared/projectScripts`, `lib/projectScriptKeybindings.ts`, and the writes of
//! `settings/useProjectScriptSettings.ts`, shared by Settings and the chat's script editor.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::{Context, Keystroke, Task, Window};
use serde_json::{Map, Value, json};

use crate::app::Workspace;
use crate::model::{ProjectScript, ProjectShell, ServerInfo, T3ProjectFile, T3ProjectFileScript};
use crate::rpc::RpcError;

const MAX_SCRIPT_ID_LENGTH: usize = 24;
const MAX_KEYBINDING_LENGTH: usize = 64;
const INVALID_KEYBINDING: &str = "Invalid keybinding.";

/// `SCRIPT_ICONS` in `projectScriptEditor.tsx`, in picker order.
pub(super) const SCRIPT_ICONS: [(&str, &str); 6] = [
    ("play", "Play"),
    ("test", "Test"),
    ("lint", "Lint"),
    ("configure", "Configure"),
    ("build", "Build"),
    ("debug", "Debug"),
];

/// `ScriptIcon`.
pub(super) fn script_icon(icon: &str) -> Lucide {
    match icon {
        "test" => Lucide::FlaskConical,
        "lint" => Lucide::ListChecks,
        "configure" => Lucide::Wrench,
        "build" => Lucide::Hammer,
        "debug" => Lucide::Bug,
        _ => Lucide::Play,
    }
}

/// The editor's form (`NewProjectScriptInput`).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ScriptInput {
    pub name: String,
    pub command: String,
    pub icon: String,
    pub run_on_worktree_create: bool,
    pub wait_for_setup: bool,
    pub run_on_settle: bool,
    /// `None` keeps the current shortcut; `Some(None)` clears it.
    pub keybinding: Option<Option<String>>,
    pub preview_url: Option<String>,
    pub auto_open_preview: bool,
}

impl Default for ScriptInput {
    /// `EMPTY_PROJECT_SCRIPT_INPUT`.
    fn default() -> Self {
        Self {
            name: String::new(),
            command: String::new(),
            icon: "play".into(),
            run_on_worktree_create: false,
            wait_for_setup: false,
            run_on_settle: false,
            keybinding: Some(None),
            preview_url: None,
            auto_open_preview: false,
        }
    }
}

impl ScriptInput {
    /// `editorRequestForScript`.
    pub fn from_script(script: &ProjectScript, keybindings: &Value) -> Self {
        let command = command_for_project_script(&script.id);
        Self {
            name: script.name.clone(),
            command: script.command.clone(),
            icon: script.icon.clone(),
            run_on_worktree_create: script.run_on_worktree_create,
            wait_for_setup: script.run_on_worktree_create && script.run_async == Some(false),
            run_on_settle: script.run_on_settle == Some(true),
            keybinding: Some(keybinding_value_for_command(keybindings, command.as_deref())),
            preview_url: script.preview_url.clone(),
            auto_open_preview: script.auto_open_preview == Some(true),
        }
    }

    /// `importFileScript` in `ProjectActionsSettings.tsx`.
    pub fn from_file_script(script: &T3ProjectFileScript) -> Self {
        let setup = script.run_on_worktree_create == Some(true);
        Self {
            name: script.name.clone(),
            command: script.command.clone(),
            icon: script.icon.clone().unwrap_or_else(|| "play".into()),
            run_on_worktree_create: setup,
            wait_for_setup: setup && script.run_async == Some(false),
            run_on_settle: script.run_on_settle == Some(true),
            keybinding: Some(None),
            preview_url: script.preview_url.clone(),
            auto_open_preview: script.preview_url.is_some() && script.auto_open_preview == Some(true),
        }
    }
}

fn build_project_script(id: &str, input: &ScriptInput) -> ProjectScript {
    ProjectScript {
        id: id.to_owned(),
        name: input.name.clone(),
        command: input.command.clone(),
        icon: input.icon.clone(),
        run_on_worktree_create: input.run_on_worktree_create,
        run_on_settle: input.run_on_settle.then_some(true),
        run_async: (input.run_on_worktree_create && input.wait_for_setup).then_some(false),
        preview_url: input.preview_url.clone(),
        auto_open_preview: input.preview_url.as_ref().map(|_| input.auto_open_preview),
    }
}

/// Only one script runs on worktree creation and one on settle; saving one claims the role.
fn release_claimed_roles(mut script: ProjectScript, saved: &ScriptInput) -> ProjectScript {
    if saved.run_on_worktree_create && script.run_on_worktree_create {
        script.run_on_worktree_create = false;
    }
    if saved.run_on_settle && script.run_on_settle == Some(true) {
        script.run_on_settle = Some(false);
    }
    script
}

fn normalize_script_id(value: &str) -> String {
    let mut cleaned = String::new();
    for c in value.trim().to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            cleaned.push(c);
        } else if !cleaned.ends_with('-') {
            cleaned.push('-');
        }
    }
    let cleaned = cleaned.trim_matches('-');
    if cleaned.is_empty() {
        return "script".into();
    }
    if cleaned.len() <= MAX_SCRIPT_ID_LENGTH {
        return cleaned.into();
    }
    let cut = cleaned[..MAX_SCRIPT_ID_LENGTH].trim_end_matches('-');
    if cut.is_empty() { "script".into() } else { cut.into() }
}

/// `commandForProjectScript`: `script.<id>.run` when the id fits `SCRIPT_RUN_COMMAND_PATTERN`.
pub(super) fn command_for_project_script(id: &str) -> Option<String> {
    let valid = !id.is_empty()
        && id.len() <= MAX_SCRIPT_ID_LENGTH
        && id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    valid.then(|| format!("script.{id}.run"))
}

pub(super) fn next_project_script_id<'a>(name: &str, existing: impl IntoIterator<Item = &'a str>) -> String {
    let taken: std::collections::HashSet<&str> = existing.into_iter().collect();
    let base = normalize_script_id(name);
    if !taken.contains(base.as_str()) {
        return base;
    }
    (2..10_000)
        .map(|suffix| {
            let candidate = format!("{base}-{suffix}");
            if candidate.len() <= MAX_SCRIPT_ID_LENGTH {
                candidate
            } else {
                let keep = MAX_SCRIPT_ID_LENGTH.saturating_sub(suffix.to_string().len() + 1).max(1);
                format!("{}-{suffix}", &base[..keep.min(base.len())])
            }
        })
        .find(|candidate| !taken.contains(candidate.as_str()))
        .unwrap_or(base)
}

fn scripts_at(value: Option<&Value>) -> Option<Vec<ProjectScript>> {
    serde_json::from_value(value?.clone()).ok()
}

fn override_entry<'a>(settings: &'a Value, project_id: &str) -> Option<&'a Map<String, Value>> {
    settings.get("projectSettingsOverrides")?.get(project_id)?.as_object()
}

/// `resolveProjectScripts`.
pub(in crate::app) fn resolve_project_scripts(settings: &Value, project: &ProjectShell) -> Vec<ProjectScript> {
    let defaults = || scripts_at(settings.get("defaultProjectScripts")).unwrap_or_default();
    if let Some(scripts) =
        override_entry(settings, &project.id).and_then(|entry| scripts_at(entry.get("defaultProjectScripts")))
    {
        return scripts;
    }
    if settings.get("projectSettingsFolded").and_then(Value::as_bool) == Some(true) {
        return defaults();
    }
    match settings.get("projectScriptOverrides").and_then(|o| o.get(&project.id)) {
        Some(Value::Null) => defaults(),
        Some(legacy) => scripts_at(Some(legacy)).unwrap_or_default(),
        None if !project.scripts.is_empty() => project.scripts.clone(),
        None => defaults(),
    }
}

/// Whether the project stores its own action list rather than inheriting the environment's.
pub(super) fn has_project_scripts_override(settings: &Value, project_id: &str) -> bool {
    override_entry(settings, project_id).is_some_and(|entry| entry.contains_key("defaultProjectScripts"))
}

fn shortcuts_for<'a>(keybindings: &'a Value, command: &'a str) -> impl DoubleEndedIterator<Item = &'a Value> {
    keybindings
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(move |binding| binding.get("command").and_then(Value::as_str) == Some(command))
        .filter_map(|binding| binding.get("shortcut"))
}

fn flag(shortcut: &Value, key: &str) -> bool {
    shortcut.get(key).and_then(Value::as_bool) == Some(true)
}

/// `keybindingValueForCommand`: the last rule for the command, as `mod+shift+k`.
pub(super) fn keybinding_value_for_command(keybindings: &Value, command: Option<&str>) -> Option<String> {
    let shortcut = shortcuts_for(keybindings, command?).next_back()?;
    let mut parts: Vec<&str> =
        [("modKey", "mod"), ("ctrlKey", "ctrl"), ("metaKey", "meta"), ("altKey", "alt"), ("shiftKey", "shift")]
            .into_iter()
            .filter(|(key, _)| flag(shortcut, key))
            .map(|(_, token)| token)
            .collect();
    let key = shortcut.get("key").and_then(Value::as_str)?;
    parts.push(match key {
        " " => "space",
        "escape" => "esc",
        key => key,
    });
    Some(parts.join("+"))
}

/// `formatShortcutKeyLabel`.
fn shortcut_key_label(key: &str) -> String {
    match key {
        " " => "Space".into(),
        "escape" => "Esc".into(),
        "arrowup" => "Up".into(),
        "arrowdown" => "Down".into(),
        "arrowleft" => "Left".into(),
        "arrowright" => "Right".into(),
        key if key.chars().count() == 1 => key.to_uppercase(),
        key => {
            let mut chars = key.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
        }
    }
}

/// `shortcutLabelForCommand` / `formatShortcutLabel` for this platform.
pub(in crate::app) fn shortcut_label_for_command(keybindings: &Value, command: Option<&str>) -> Option<String> {
    let shortcut = shortcuts_for(keybindings, command?).next_back()?;
    let key = shortcut_key_label(shortcut.get("key").and_then(Value::as_str)?);
    let mac = cfg!(target_os = "macos");
    let meta = flag(shortcut, "metaKey") || (flag(shortcut, "modKey") && mac);
    let ctrl = flag(shortcut, "ctrlKey") || (flag(shortcut, "modKey") && !mac);
    let (alt, shift) = (flag(shortcut, "altKey"), flag(shortcut, "shiftKey"));
    if mac {
        let glyph = |on: bool, glyph: &'static str| if on { glyph } else { "" };
        return Some(format!("{}{}{}{}{key}", glyph(ctrl, "⌃"), glyph(alt, "⌥"), glyph(shift, "⇧"), glyph(meta, "⌘")));
    }
    let mut parts: Vec<String> = [(ctrl, "Ctrl"), (alt, "Alt"), (shift, "Shift"), (meta, "Meta")]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, label)| label.to_owned())
        .collect();
    parts.push(key);
    Some(parts.join("+"))
}

/// `keybindingFromKeyboardEvent` for a GPUI keystroke; `None` for a bare modifier or an unsupported key.
pub(super) fn keybinding_from_keystroke(keystroke: &Keystroke) -> Option<String> {
    let key = keystroke.key.to_lowercase();
    let token = match key.as_str() {
        "space" | " " => "space".to_owned(),
        "escape" => "esc".to_owned(),
        "up" => "arrowup".to_owned(),
        "down" => "arrowdown".to_owned(),
        "left" => "arrowleft".to_owned(),
        "right" => "arrowright".to_owned(),
        "enter" | "tab" | "backspace" | "delete" | "home" | "end" | "pageup" | "pagedown" => key,
        k if k.chars().count() == 1 => key,
        k if k.len() <= 3 && k.starts_with('f') && k[1..].chars().all(|c| c.is_ascii_digit()) && k.len() > 1 => key,
        _ => return None,
    };
    let m = &keystroke.modifiers;
    let mut parts = Vec::new();
    if cfg!(target_os = "macos") {
        if m.platform {
            parts.push("mod");
        }
        if m.control {
            parts.push("ctrl");
        }
    } else {
        if m.control {
            parts.push("mod");
        }
        if m.platform {
            parts.push("meta");
        }
    }
    if m.alt {
        parts.push("alt");
    }
    if m.shift {
        parts.push("shift");
    }
    parts.push(&token);
    Some(parts.join("+"))
}

/// `decodeProjectScriptKeybindingRule`: the trimmed key, `None` when blank.
pub(super) fn decode_keybinding(keybinding: Option<&str>, command: Option<&str>) -> Result<Option<String>, String> {
    let Some(key) = keybinding.map(str::trim).filter(|key| !key.is_empty()) else { return Ok(None) };
    if command.is_none() || key.chars().count() > MAX_KEYBINDING_LENGTH {
        return Err(INVALID_KEYBINDING.into());
    }
    Ok(Some(key.to_owned()))
}

/// One project's settings write plus the shortcut requests that follow it, in order.
#[derive(Debug, PartialEq)]
pub(super) struct ScriptsWrite {
    pub patch: Value,
    pub bindings: Vec<(&'static str, Value)>,
}

/// `persist`/`writeTargets` in `useProjectScriptSettings.ts`, for one project.
/// `next` of `None` drops the project's list so it inherits the environment's again.
pub(super) fn plan_scripts_write(
    info: &ServerInfo,
    projects: &[ProjectShell],
    project: &ProjectShell,
    next: Option<Vec<ProjectScript>>,
    script_id: Option<&str>,
    keybinding: Option<Option<&str>>,
) -> Result<ScriptsWrite, String> {
    let settings: &Value = &info.settings;
    let defaults = scripts_at(settings.get("defaultProjectScripts")).unwrap_or_default();
    let current = resolve_project_scripts(settings, project);
    let effective = next.clone().unwrap_or_else(|| defaults.clone());
    let patch = if info.capability("projectSettingsOverrides") {
        let mut entry = override_entry(settings, &project.id).cloned().unwrap_or_default();
        match &next {
            Some(scripts) => {
                entry.insert("defaultProjectScripts".into(), json!(scripts));
            }
            None => {
                entry.remove("defaultProjectScripts");
            }
        }
        let entry = if entry.is_empty() && next.is_none() { Value::Null } else { Value::Object(entry) };
        json!({"projectSettingsOverrides": {project.id.clone(): entry}})
    } else {
        json!({"projectScriptOverrides": {project.id.clone(): next}})
    };

    let changed: Vec<String> = match script_id {
        Some(id) => vec![id.to_owned()],
        None => current.iter().filter(|s| !effective.iter().any(|n| n.id == s.id)).map(|s| s.id.clone()).collect(),
    };
    let mut bindings = Vec::new();
    for id in changed {
        let kept = effective.iter().any(|script| script.id == id);
        if keybinding.is_none() && kept {
            continue;
        }
        let command = command_for_project_script(&id);
        let previous = keybinding_value_for_command(info.keybindings_json(), command.as_deref())
            .map(|value| decode_keybinding(Some(&value), command.as_deref()))
            .transpose()?
            .flatten();
        let wanted = decode_keybinding(keybinding.flatten(), command.as_deref())?;
        let in_other_override =
            settings.get("projectSettingsOverrides").and_then(Value::as_object).is_some_and(|overrides| {
                overrides.iter().any(|(project_id, entry)| {
                    project_id != &project.id
                        && scripts_at(entry.get("defaultProjectScripts")).is_some_and(|s| s.iter().any(|s| s.id == id))
                })
            });
        let retained_elsewhere = !next.as_ref().is_some_and(|scripts| scripts.iter().any(|s| s.id == id))
            && (defaults.iter().any(|s| s.id == id)
                || in_other_override
                || projects.iter().any(|other| {
                    other.id != project.id && resolve_project_scripts(settings, other).iter().any(|s| s.id == id)
                }));
        let Some(command) = command else { continue };
        match (wanted, previous) {
            (Some(key), previous) => {
                let mut input = json!({"key": key, "command": command});
                if let Some(previous) = previous.filter(|previous| previous != &key) {
                    input["replace"] = json!({"key": previous, "command": command});
                }
                bindings.push(("server.upsertKeybinding", input));
            }
            (None, Some(previous)) if !retained_elsewhere => {
                bindings.push(("server.removeKeybinding", json!({"key": previous, "command": command})));
            }
            (None, _) => {}
        }
    }
    Ok(ScriptsWrite { patch, bindings })
}

/// Every script id the environment knows, so a new one never collides (`submit`).
fn known_script_ids(settings: &Value, projects: &[ProjectShell], project: &ProjectShell) -> Vec<String> {
    let mut ids: Vec<String> = projects.iter().flat_map(|p| p.scripts.iter().map(|s| s.id.clone())).collect();
    ids.extend(scripts_at(settings.get("defaultProjectScripts")).unwrap_or_default().into_iter().map(|s| s.id));
    if let Some(overrides) = settings.get("projectSettingsOverrides").and_then(Value::as_object) {
        for entry in overrides.values() {
            ids.extend(scripts_at(entry.get("defaultProjectScripts")).unwrap_or_default().into_iter().map(|s| s.id));
        }
    }
    ids.extend(project.scripts.iter().map(|s| s.id.clone()));
    ids
}

/// The list after saving `input` as `script_id` (a new script when `None`), and the id it saved under.
pub(super) fn scripts_after_save(
    settings: &Value,
    projects: &[ProjectShell],
    project: &ProjectShell,
    script_id: Option<&str>,
    input: &ScriptInput,
) -> (String, Vec<ProjectScript>) {
    let ids = known_script_ids(settings, projects, project);
    let id = script_id
        .map(str::to_owned)
        .unwrap_or_else(|| next_project_script_id(&input.name, ids.iter().map(String::as_str)));
    let saved = build_project_script(&id, input);
    let mut scripts: Vec<ProjectScript> = resolve_project_scripts(settings, project)
        .into_iter()
        .map(|script| if script.id == id { saved.clone() } else { release_claimed_roles(script, input) })
        .collect();
    if script_id.is_none() {
        scripts.push(saved);
    }
    (id, scripts)
}

/// Scripts `t3.json` declares that the project does not have yet, by command or name.
pub(super) fn importable_scripts<'a>(
    file: &'a T3ProjectFile,
    scripts: &[ProjectScript],
) -> Vec<&'a T3ProjectFileScript> {
    file.scripts()
        .iter()
        .filter(|candidate| {
            !scripts.iter().any(|script| {
                script.command == candidate.command || script.name.to_lowercase() == candidate.name.to_lowercase()
            })
        })
        .collect()
}

/// The message a toast or form shows for a failed request (`error.message`).
pub(super) fn rpc_message(error: &RpcError) -> String {
    match error {
        RpcError::Failure(value) => value.get("message").and_then(Value::as_str).map(str::to_owned),
        _ => None,
    }
    .unwrap_or_else(|| error.to_string())
}

impl Workspace {
    fn project(&self, project_id: &str) -> Option<&ProjectShell> {
        self.shell.projects.iter().find(|project| project.id == project_id)
    }

    /// Writes the project's settings, then reconciles its shortcuts; both land in `server_info` at once.
    fn write_project_scripts(
        &mut self,
        project_id: &str,
        next: impl FnOnce(&Value, &ProjectShell) -> Option<Vec<ProjectScript>>,
        script_id: Option<&str>,
        keybinding: Option<Option<&str>>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let Some(client) = self.client() else { return Task::ready(Err("Not connected.".into())) };
        let Some(project) = self.project(project_id) else { return Task::ready(Err("Project not found.".into())) };
        let next = next(&self.server_info.settings, project);
        let write =
            match plan_scripts_write(&self.server_info, &self.shell.projects, project, next, script_id, keybinding) {
                Ok(write) => write,
                Err(error) => return Task::ready(Err(error)),
            };
        cx.spawn(async move |this, cx| {
            let settings = client
                .request("server.updateSettings", json!({"patch": write.patch}))
                .await
                .map_err(|e| rpc_message(&e))?;
            let _ = this.update(cx, |this, cx| {
                let _ = this.server_info.apply(&json!({"type": "settingsUpdated", "payload": {"settings": settings}}));
                cx.notify();
            });
            for (method, input) in write.bindings {
                let result = client.request(method, input).await.map_err(|e| rpc_message(&e))?;
                let _ = this.update(cx, |this, cx| {
                    let event =
                        json!({"type": "keybindingsUpdated", "payload": {"keybindings": result["keybindings"]}});
                    let _ = this.server_info.apply(&event);
                    cx.notify();
                });
            }
            Ok(())
        })
    }

    /// Saves the editor's form as `script_id`, or as a new script when `None`.
    pub(super) fn save_project_script(
        &mut self,
        project_id: &str,
        script_id: Option<&str>,
        input: &ScriptInput,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let Some(project) = self.project(project_id) else { return Task::ready(Err("Project not found.".into())) };
        let (id, scripts) =
            scripts_after_save(&self.server_info.settings, &self.shell.projects, project, script_id, input);
        let keybinding = input.keybinding.as_ref().map(Option::as_deref);
        self.write_project_scripts(project_id, |_, _| Some(scripts), Some(&id), keybinding, cx)
    }

    pub(super) fn delete_project_script(
        &mut self,
        project_id: &str,
        script_id: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let id = script_id.to_owned();
        self.write_project_scripts(
            project_id,
            |settings, project| {
                Some(resolve_project_scripts(settings, project).into_iter().filter(|s| s.id != id).collect())
            },
            Some(script_id),
            Some(None),
            cx,
        )
    }

    /// The row's reset: the project inherits the environment's actions again.
    pub(super) fn reset_project_scripts(
        &mut self,
        project_id: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        self.write_project_scripts(project_id, |_, _| None, None, None, cx)
    }

    /// Shows a failed write as a toast, like `reportScriptFailure`.
    pub(super) fn report_scripts_failure(
        &self,
        task: Task<Result<(), String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |_, cx| {
            if let Err(message) = task.await {
                let _ = cx.update(|window, cx| {
                    use gpui_kit::component::WindowExt as _;
                    use gpui_kit::component::notification::Notification;
                    window.push_notification(Notification::error(message).title("Failed to save project actions"), cx);
                });
            }
        })
        .detach();
    }

    /// The checkout's `t3.json`, read once per root (`useT3ProjectFileState`).
    pub(super) fn t3_project_file(&mut self, workspace_root: &str, cx: &mut Context<Self>) -> &T3ProjectFile {
        if !self.t3_project_files.contains_key(workspace_root) {
            self.t3_project_files.insert(workspace_root.to_owned(), T3ProjectFile::Loading);
            if let Some(client) = self.client() {
                let root = workspace_root.to_owned();
                let input = json!({"cwd": root, "relativePath": "t3.json", "allowMissing": true});
                cx.spawn(async move |this, cx| {
                    let result = client.request("projects.readFile", input).await;
                    let file = match result {
                        Ok(read) if read["exists"] != false && read["truncated"] != true => {
                            read["contents"].as_str().map(T3ProjectFile::parse).unwrap_or(T3ProjectFile::Missing)
                        }
                        _ => T3ProjectFile::Missing,
                    };
                    let _ = this.update(cx, |this, cx| {
                        this.t3_project_files.insert(root, file);
                        cx.notify();
                    });
                })
                .detach();
            }
        }
        &self.t3_project_files[workspace_root]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(id: &str) -> ProjectScript {
        ProjectScript {
            id: id.into(),
            name: id.into(),
            command: format!("run {id}"),
            icon: "play".into(),
            run_on_worktree_create: false,
            run_on_settle: None,
            run_async: None,
            preview_url: None,
            auto_open_preview: None,
        }
    }

    fn project(id: &str, scripts: Vec<ProjectScript>) -> ProjectShell {
        serde_json::from_value(json!({"id": id, "title": id, "workspaceRoot": format!("/{id}"), "scripts": scripts}))
            .unwrap()
    }

    fn info(settings: Value, keybindings: Value) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.settings = crate::settings::Settings::layered(&Default::default(), &settings);
        info.config = serde_json::json!({"keybindings": keybindings});
        info.capabilities = serde_json::json!({"projectSettingsOverrides": true});
        info
    }

    fn binding(command: &str, key: &str, mod_key: bool, shift: bool) -> Value {
        json!({"command": command, "shortcut": {"key": key, "modKey": mod_key, "shiftKey": shift,
            "ctrlKey": false, "metaKey": false, "altKey": false}})
    }

    #[test]
    fn script_ids_normalize_and_avoid_collisions() {
        assert_eq!(next_project_script_id("Run Tests!", []), "run-tests");
        assert_eq!(next_project_script_id("  ", []), "script");
        assert_eq!(next_project_script_id("Test", ["test", "test-2"]), "test-3");
        let long = next_project_script_id("a very long action name that keeps going", []);
        assert_eq!(long, "a-very-long-action-name");
        assert_eq!(
            next_project_script_id("a very long action name that keeps going", [long.as_str()]),
            "a-very-long-action-nam-2"
        );
        assert_eq!(command_for_project_script("lint"), Some("script.lint.run".into()));
        assert_eq!(command_for_project_script("-lint"), None);
    }

    #[test]
    fn built_scripts_only_carry_the_flags_that_apply() {
        let input = ScriptInput {
            name: "Dev".into(),
            command: "bun dev".into(),
            wait_for_setup: true,
            preview_url: Some("http://localhost:5173".into()),
            auto_open_preview: true,
            ..Default::default()
        };
        let built = serde_json::to_value(build_project_script("dev", &input)).unwrap();
        assert_eq!(
            built,
            json!({"id": "dev", "name": "Dev", "command": "bun dev", "icon": "play", "runOnWorktreeCreate": false,
                "previewUrl": "http://localhost:5173", "autoOpenPreview": true})
        );
        let setup = ScriptInput { run_on_worktree_create: true, wait_for_setup: true, run_on_settle: true, ..input };
        let built = serde_json::to_value(build_project_script("dev", &setup)).unwrap();
        assert_eq!(built["async"], false);
        assert_eq!(built["runOnSettle"], true);
    }

    #[test]
    fn project_overrides_win_over_legacy_and_record_scripts() {
        let shell = project("p", vec![script("record")]);
        let scripts =
            |settings: Value| resolve_project_scripts(&settings, &shell).into_iter().map(|s| s.id).collect::<Vec<_>>();
        let defaults = json!([script("default")]);
        assert_eq!(scripts(json!({"defaultProjectScripts": defaults})), ["record"]);
        assert_eq!(scripts(json!({"defaultProjectScripts": defaults, "projectSettingsFolded": true})), ["default"]);
        assert_eq!(
            scripts(json!({"defaultProjectScripts": defaults, "projectScriptOverrides": {"p": null}})),
            ["default"]
        );
        let overridden = json!({"projectSettingsOverrides": {"p": {"defaultProjectScripts": [script("mine")]}}});
        assert_eq!(scripts(overridden), ["mine"]);
    }

    #[test]
    fn saving_a_setup_script_releases_the_role_and_appends() {
        let mut setup = script("setup");
        setup.run_on_worktree_create = true;
        let shell = project("p", vec![]);
        let settings = json!({"projectSettingsOverrides": {"p": {"defaultProjectScripts": [setup]}}});
        let input = ScriptInput {
            name: "Setup".into(),
            command: "bun i".into(),
            run_on_worktree_create: true,
            ..Default::default()
        };
        let (id, scripts) = scripts_after_save(&settings, std::slice::from_ref(&shell), &shell, None, &input);
        assert_eq!(id, "setup-2");
        assert_eq!(
            scripts.iter().map(|s| (s.id.as_str(), s.run_on_worktree_create)).collect::<Vec<_>>(),
            [("setup", false), ("setup-2", true)]
        );
    }

    #[test]
    fn writes_keep_other_override_keys_and_replace_a_changed_shortcut() {
        let shell = project("p", vec![]);
        let settings = json!({"projectSettingsOverrides": {"p": {"defaultThreadEnvMode": "worktree", "defaultProjectScripts": [script("lint")]}}});
        let info = info(settings, json!([binding("script.lint.run", "l", true, false)]));
        let write = plan_scripts_write(
            &info,
            std::slice::from_ref(&shell),
            &shell,
            Some(vec![script("lint")]),
            Some("lint"),
            Some(Some("mod+shift+l")),
        )
        .unwrap();
        assert_eq!(
            write.patch,
            json!({"projectSettingsOverrides": {"p": {"defaultThreadEnvMode": "worktree", "defaultProjectScripts": [script("lint")]}}})
        );
        assert_eq!(
            write.bindings,
            [(
                "server.upsertKeybinding",
                json!({"key": "mod+shift+l", "command": "script.lint.run", "replace": {"key": "mod+l", "command": "script.lint.run"}})
            )]
        );
    }

    #[test]
    fn deleting_removes_the_shortcut_unless_another_project_keeps_the_script() {
        let alone = project("p", vec![]);
        let settings = json!({"projectSettingsOverrides": {"p": {"defaultProjectScripts": [script("lint")]}}});
        let keys = json!([binding("script.lint.run", "l", true, false)]);
        let write = plan_scripts_write(
            &info(settings.clone(), keys.clone()),
            std::slice::from_ref(&alone),
            &alone,
            Some(vec![]),
            Some("lint"),
            Some(None),
        )
        .unwrap();
        assert_eq!(
            write.bindings,
            [("server.removeKeybinding", json!({"key": "mod+l", "command": "script.lint.run"}))]
        );

        let other = project("q", vec![script("lint")]);
        let projects = [alone.clone(), other];
        let write =
            plan_scripts_write(&info(settings, keys), &projects, &alone, Some(vec![]), Some("lint"), Some(None))
                .unwrap();
        assert!(write.bindings.is_empty());
    }

    #[test]
    fn resetting_drops_the_list_and_an_emptied_entry() {
        let shell = project("p", vec![]);
        let settings = json!({"projectSettingsOverrides": {"p": {"defaultProjectScripts": [script("lint")]}}, "defaultProjectScripts": [script("lint")]});
        let write =
            plan_scripts_write(&info(settings, json!([])), std::slice::from_ref(&shell), &shell, None, None, None)
                .unwrap();
        assert_eq!(write.patch, json!({"projectSettingsOverrides": {"p": null}}));
        assert!(write.bindings.is_empty());
    }

    #[test]
    fn shortcuts_round_trip_between_rules_values_and_labels() {
        let keys = json!([binding("script.dev.run", "d", true, true), binding("script.x.run", "escape", false, false)]);
        assert_eq!(keybinding_value_for_command(&keys, Some("script.dev.run")).as_deref(), Some("mod+shift+d"));
        assert_eq!(keybinding_value_for_command(&keys, Some("script.x.run")).as_deref(), Some("esc"));
        if !cfg!(target_os = "macos") {
            assert_eq!(shortcut_label_for_command(&keys, Some("script.dev.run")).as_deref(), Some("Ctrl+Shift+D"));
            let keystroke = Keystroke::parse("ctrl-shift-k").unwrap();
            assert_eq!(keybinding_from_keystroke(&keystroke).as_deref(), Some("mod+shift+k"));
            assert_eq!(keybinding_from_keystroke(&Keystroke::parse("alt-up").unwrap()).as_deref(), Some("alt+arrowup"));
        }
        assert_eq!(keybinding_from_keystroke(&Keystroke::parse("f12").unwrap()).as_deref(), Some("f12"));
        assert_eq!(decode_keybinding(Some("  "), Some("script.a.run")), Ok(None));
        assert!(decode_keybinding(Some("k"), None).is_err());
    }

    #[test]
    fn t3_file_scripts_already_present_are_not_importable() {
        let file = T3ProjectFile::parse(
            r#"{"scripts": [{"name": "Lint", "command": "bun lint"}, {"name": "Dev", "command": "bun dev", "icon": "debug"}]}"#,
        );
        let mut lint = script("lint");
        lint.name = "lint".into();
        let names: Vec<&str> = importable_scripts(&file, &[lint]).iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Dev"]);
        assert_eq!(
            T3ProjectFile::parse(r#"{"scripts": [{"name": "x", "command": "y", "icon": "nope"}]}"#),
            T3ProjectFile::Invalid
        );
        assert_eq!(T3ProjectFile::parse("{oops"), T3ProjectFile::Invalid);
        let imported = ScriptInput::from_file_script(&file.scripts()[1]);
        assert_eq!((imported.icon.as_str(), imported.keybinding), ("debug", Some(None)));
    }
}
