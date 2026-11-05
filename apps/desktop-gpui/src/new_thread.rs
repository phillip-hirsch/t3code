//! Where a new thread's workspace starts, as the web decides it for a draft:
//! `resolveNewThreadEnvMode` (`packages/shared/src/projectSettings.ts`, called from
//! `apps/web/src/hooks/useHandleNewThread.ts`), the base branch default in
//! `BranchToolbarBranchSelector.tsx`, and the launch `workspaceStrategy` built by
//! `startThreadTurn` in `packages/client-runtime/src/operations/commands.ts`.

use serde_json::{Value, json};

use crate::model::{ProjectShell, ServerInfo};
use crate::rpc::RpcClient;

/// `VCS_REF_LIST_LIMIT` in `apps/web/src/state/queries.ts`.
const REF_LIST_LIMIT: u32 = 100;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EnvMode {
    Local,
    Worktree,
}

impl EnvMode {
    fn parse(value: Option<&Value>) -> Option<Self> {
        match value?.as_str()? {
            "local" => Some(Self::Local),
            "worktree" => Some(Self::Worktree),
            _ => None,
        }
    }
}

/// Everything a draft launch needs from the shell and settings, captured before the send.
pub struct DraftWorkspace {
    /// `defaultThreadEnvMode` after project overrides; `None` inherits the project's t3.json.
    configured: Option<Configured>,
    workspace_root: String,
    new_projects_root: Option<String>,
    project_has_threads: bool,
    start_from_origin: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Configured {
    mode: EnvMode,
    /// Set by the environment, not the project.
    from_environment: bool,
}

impl DraftWorkspace {
    pub fn new(info: &ServerInfo, project: &ProjectShell, project_has_threads: bool) -> Self {
        let overrides = &info.project_settings_overrides[project.id.as_str()];
        // `resolveProjectOverrides`: stored overrides win over the legacy project field until folded.
        let project_mode = EnvMode::parse(overrides.get("defaultThreadEnvMode")).or_else(|| {
            let legacy = project.default_thread_env_mode.as_deref().map(Value::from);
            if info.project_settings_folded { None } else { EnvMode::parse(legacy.as_ref()) }
        });
        let configured = match project_mode {
            Some(mode) => Some(Configured { mode, from_environment: false }),
            None => EnvMode::parse(info.default_thread_env_mode.as_deref().map(Value::from).as_ref())
                .map(|mode| Configured { mode, from_environment: true }),
        };
        let start_from_origin = overrides
            .get("newWorktreesStartFromOrigin")
            .and_then(Value::as_bool)
            .or(info.new_worktrees_start_from_origin)
            .unwrap_or(true);
        Self {
            configured,
            workspace_root: project.workspace_root.clone(),
            new_projects_root: info.new_projects_root.clone(),
            project_has_threads,
            start_from_origin,
        }
    }

    /// The mode a fresh draft starts in, reading t3.json when no setting decides.
    pub async fn default_mode(&self, client: &RpcClient) -> EnvMode {
        let project_file = match self.configured {
            Some(_) => None,
            None => read_project_file_env_mode(client, &self.workspace_root).await,
        };
        self.env_mode(project_file)
    }

    pub fn start_from_origin(&self) -> bool {
        self.start_from_origin
    }

    /// `applyProjectFile` + `resolveNewThreadEnvMode`.
    fn env_mode(&self, project_file: Option<EnvMode>) -> EnvMode {
        // Unset everywhere, t3.json decides, then "local"; either way it is not the environment's choice.
        let Some(configured) = self.configured else { return project_file.unwrap_or(EnvMode::Local) };
        let Some(new_projects_root) = &self.new_projects_root else { return configured.mode };
        // New name-only projects use their checkout until the first thread.
        if configured.mode == EnvMode::Worktree
            && configured.from_environment
            && !self.project_has_threads
            && is_new_project_folder(new_projects_root, &self.workspace_root)
        {
            return EnvMode::Local;
        }
        configured.mode
    }
}

/// `vcs.listRefs` for a project's checkout, as the draft's workspace controls use it.
pub async fn list_refs(client: &RpcClient, cwd: &str) -> Result<Value, String> {
    client
        .request("vcs.listRefs", json!({"cwd": cwd, "limit": REF_LIST_LIMIT}))
        .await
        .map_err(|error| error.to_string())
}

/// What the draft's workspace controls picked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorkspaceChoice {
    pub mode: Option<EnvMode>,
    /// Worktree mode: the base branch. Local mode: the branch switched to.
    pub branch: Option<String>,
    /// A previous thread's worktree to reuse, with its branch.
    pub existing_worktree: Option<(String, Option<String>)>,
    pub start_from_origin: Option<bool>,
}

/// The base branch a worktree draft starts from: the picked one, else the
/// repository's default ref, else the current one.
pub fn default_base(refs: &Value) -> Option<&str> {
    let list = refs.get("refs").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let named = |flag: &str| {
        list.iter().find(|r| r.get(flag).and_then(Value::as_bool) == Some(true)).and_then(|r| r["name"].as_str())
    };
    named("isDefault").or_else(|| named("current"))
}

/// The launch's `workspaceStrategy` (`startThreadTurn` in `operations/commands.ts`).
pub fn launch_strategy(
    mode: EnvMode,
    choice: &WorkspaceChoice,
    refs: &Value,
    start_from_origin: bool,
) -> Result<Value, String> {
    if let Some((worktree_path, branch)) = &choice.existing_worktree {
        let mut strategy = json!({"type": "existing_worktree", "worktreePath": worktree_path});
        if let Some(branch) = branch {
            strategy["branch"] = branch.clone().into();
        }
        return Ok(strategy);
    }
    // Outside a repository there is nothing to branch from, so the thread runs in the checkout.
    if mode == EnvMode::Local || refs.get("isRepo").and_then(Value::as_bool) != Some(true) {
        let mut strategy = json!({"type": "root"});
        if let (EnvMode::Local, Some(branch)) = (mode, &choice.branch) {
            strategy["branch"] = branch.clone().into();
        }
        return Ok(strategy);
    }
    let base_ref = choice
        .branch
        .as_deref()
        .or_else(|| default_base(refs))
        .ok_or_else(|| "Select a base branch before sending in New worktree mode.".to_owned())?;
    let mut strategy = json!({"type": "worktree", "baseRef": base_ref});
    if start_from_origin {
        strategy["startFromOrigin"] = true.into();
    }
    Ok(strategy)
}

/// t3.json's `defaultThreadEnvMode`; a missing or unreadable file counts as unset, like the web.
async fn read_project_file_env_mode(client: &RpcClient, workspace_root: &str) -> Option<EnvMode> {
    let input = json!({"cwd": workspace_root, "relativePath": "t3.json", "allowMissing": true});
    let file = client.request("projects.readFile", input).await.ok()?;
    if file.get("exists").and_then(Value::as_bool) == Some(false) || file["truncated"].as_bool() == Some(true) {
        return None;
    }
    EnvMode::parse(parse_lenient_json(file["contents"].as_str()?)?.get("defaultThreadEnvMode"))
}

/// `parseLenientJsonGetter` in `packages/shared/src/schemaJson.ts`: JSON plus comments and trailing commas.
fn parse_lenient_json(input: &str) -> Option<Value> {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut comma = false;
    while let Some(c) = chars.next() {
        match c {
            '/' if chars.peek() == Some(&'/') => {
                while chars.next_if(|c| *c != '\n' && *c != '\r').is_some() {}
                out.push(' ');
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = ' ';
                for c in chars.by_ref() {
                    if previous == '*' && c == '/' {
                        break;
                    }
                    previous = c;
                }
                out.push(' ');
            }
            c if c.is_whitespace() => out.push(c),
            // A comma waits for the next token; it is dropped before a closing bracket.
            ',' => {
                if std::mem::replace(&mut comma, true) {
                    out.push(',');
                }
            }
            '}' | ']' => {
                comma = false;
                out.push(c);
            }
            c => {
                if std::mem::take(&mut comma) {
                    out.push(',');
                }
                out.push(c);
                if c == '"' {
                    while let Some(c) = chars.next() {
                        out.push(c);
                        match c {
                            '\\' => out.extend(chars.next()),
                            '"' => break,
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    serde_json::from_str(&out).ok()
}

/// A direct child of `root`, compared like `normalizeProjectPathForComparison`.
fn is_new_project_folder(root: &str, folder: &str) -> bool {
    let (root, folder) = (normalize_path(root), normalize_path(folder));
    let separator = if root.contains('\\') { '\\' } else { '/' };
    let prefix = if root.ends_with(separator) { root } else { format!("{root}{separator}") };
    folder.strip_prefix(&prefix).is_some_and(|rest| !rest.is_empty() && !rest.contains(separator))
}

fn normalize_path(path: &str) -> String {
    let path = path.trim();
    let trimmed = if path.starts_with('/') { path.trim_end_matches('/') } else { path.trim_end_matches(['/', '\\']) };
    // Roots like "/" and "C:\" keep their separator.
    let path = if trimmed.is_empty() || trimmed.ends_with(':') { path } else { trimmed };
    let bytes = path.as_bytes();
    let windows = (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':') || path.starts_with("\\\\");
    if windows { path.replace('/', "\\").to_lowercase() } else { path.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(settings: Value) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.apply(&json!({"type": "snapshot", "config": {
            "providers": [], "settings": settings, "newProjectsRoot": "C:\\Users\\me\\T3 Projects",
        }}))
        .unwrap();
        info
    }

    fn project(root: &str, legacy: Option<&str>) -> ProjectShell {
        serde_json::from_value(json!({
            "id": "p", "title": "P", "workspaceRoot": root, "defaultThreadEnvMode": legacy,
        }))
        .unwrap()
    }

    fn mode(settings: Value, project: &ProjectShell, has_threads: bool, file: Option<EnvMode>) -> EnvMode {
        DraftWorkspace::new(&info(settings), project, has_threads).env_mode(file)
    }

    #[test]
    fn env_mode_follows_settings_then_project_then_t3_json() {
        use EnvMode::*;
        let repo = project("H:/code/repo", None);
        assert_eq!(mode(json!({}), &repo, true, None), Local);
        assert_eq!(mode(json!({}), &repo, true, Some(Worktree)), Worktree);
        assert_eq!(mode(json!({"defaultThreadEnvMode": "worktree"}), &repo, true, Some(Local)), Worktree);
        let overridden = json!({
            "defaultThreadEnvMode": "worktree",
            "projectSettingsOverrides": {"p": {"defaultThreadEnvMode": "local"}},
        });
        assert_eq!(mode(overridden, &repo, true, None), Local);
        // The legacy project field applies until settings fold it into the overrides.
        let legacy = project("H:/code/repo", Some("worktree"));
        assert_eq!(mode(json!({}), &legacy, true, None), Worktree);
        assert_eq!(mode(json!({"projectSettingsFolded": true}), &legacy, true, None), Local);
    }

    #[test]
    fn new_project_folders_start_local_until_their_first_thread() {
        use EnvMode::*;
        let worktree = json!({"defaultThreadEnvMode": "worktree"});
        let fresh = project("c:/users/me/t3 projects/demo/", None);
        assert_eq!(mode(worktree.clone(), &fresh, false, None), Local);
        assert_eq!(mode(worktree.clone(), &fresh, true, None), Worktree);
        let nested = project("C:\\Users\\me\\T3 Projects\\demo\\app", None);
        assert_eq!(mode(worktree.clone(), &nested, false, None), Worktree);
        // A project's own choice is not overruled.
        let mut chosen = worktree;
        chosen["projectSettingsOverrides"] = json!({"p": {"defaultThreadEnvMode": "worktree"}});
        assert_eq!(mode(chosen, &fresh, false, None), Worktree);
    }

    #[test]
    fn launch_strategy_follows_the_workspace_controls() {
        use EnvMode::*;
        let refs = json!({"isRepo": true, "refs": [
            {"name": "feature", "current": true, "isDefault": false},
            {"name": "main", "current": false, "isDefault": true},
        ]});
        let none = WorkspaceChoice::default();
        assert_eq!(
            launch_strategy(Worktree, &none, &refs, true).unwrap(),
            json!({"type": "worktree", "baseRef": "main", "startFromOrigin": true})
        );
        let picked = WorkspaceChoice { branch: Some("feature".into()), ..none.clone() };
        assert_eq!(
            launch_strategy(Worktree, &picked, &refs, false).unwrap(),
            json!({"type": "worktree", "baseRef": "feature"})
        );
        assert_eq!(launch_strategy(Local, &picked, &refs, true).unwrap(), json!({"type": "root", "branch": "feature"}));
        assert_eq!(launch_strategy(Local, &none, &refs, true).unwrap(), json!({"type": "root"}));
        let reuse = WorkspaceChoice { existing_worktree: Some(("C:/wt/a".into(), Some("a".into()))), ..none.clone() };
        assert_eq!(
            launch_strategy(Worktree, &reuse, &refs, true).unwrap(),
            json!({"type": "existing_worktree", "worktreePath": "C:/wt/a", "branch": "a"})
        );
        assert!(launch_strategy(Worktree, &none, &json!({"isRepo": true, "refs": []}), false).is_err());
        assert_eq!(launch_strategy(Worktree, &none, &json!({"isRepo": false}), true).unwrap(), json!({"type": "root"}));
    }

    #[test]
    fn project_files_allow_comments_and_trailing_commas() {
        let text = "{\n  // where threads start\n  \"defaultThreadEnvMode\": \"worktree\", /* a // b */\n  \"scripts\": [\"a,]\", ],\n}";
        let value = parse_lenient_json(text).unwrap();
        assert_eq!(value, json!({"defaultThreadEnvMode": "worktree", "scripts": ["a,]"]}));
        assert_eq!(parse_lenient_json("{\"url\": \"http://x/*y*/\"}").unwrap()["url"], "http://x/*y*/");
    }
}
