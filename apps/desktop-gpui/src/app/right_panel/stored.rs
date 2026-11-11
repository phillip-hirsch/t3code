//! The panel layout kept across launches, in `right-panel-state.json`: each thread's (or
//! draft's) tabs, active tab, open and maximized state (`rightPanelStore`'s persisted
//! `byThreadKey`, read back as `migratePersistedRightPanelState` does) and its width
//! (`t3code:preview-panel-width:<thread>`). A terminal tab keeps its group in the web's surface
//! fields (`terminalIds`, `activeTerminalId`, `splitDirection`) and the thread it runs under; a
//! file an earlier build wrote has no terminal tabs.

use std::collections::HashMap;

use gpui_kit::{Context, Task};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{PanelState, Surface};
use crate::app::terminal::StoredGroup;
use crate::app::{Target, Workspace};

/// Changes this close together are saved once.
const SAVE_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

/// A [`Target`] as stored: the thread, or the project draft, with its environment.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub(super) enum StoredTarget {
    Thread {
        environment_id: String,
        thread_id: String,
    },
    Draft {
        environment_id: String,
        project_id: String,
        #[serde(default)]
        draft_id: String,
    },
}

impl From<&Target> for StoredTarget {
    fn from(target: &Target) -> Self {
        match target {
            Target::Thread { env, id } => Self::Thread { environment_id: env.to_string(), thread_id: id.clone() },
            Target::Draft { env, project_id, draft } => Self::Draft {
                environment_id: env.to_string(),
                project_id: project_id.clone(),
                draft_id: draft.as_str().to_owned(),
            },
        }
    }
}

impl StoredTarget {
    pub(super) fn target(&self) -> Target {
        match self {
            Self::Thread { environment_id, thread_id } => {
                Target::Thread { env: environment_id.clone().into(), id: thread_id.clone() }
            }
            Self::Draft { environment_id, project_id, draft_id } => Target::draft(
                environment_id.clone().into(),
                project_id.clone(),
                crate::app::draft_sessions::DraftId::from_saved(draft_id.clone()),
            ),
        }
    }
}

#[expect(clippy::trivially_copy_pass_by_ref, reason = "`skip_serializing_if` passes a reference")]
fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredPanel {
    target: StoredTarget,
    #[serde(default, skip_serializing_if = "is_false")]
    open: bool,
    /// Read one by one, so a surface this build does not know drops alone.
    #[serde(default)]
    surfaces: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active: Option<Value>,
    #[serde(default, skip_serializing_if = "is_false")]
    maximized: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proactive_diff_run: Option<String>,
}

/// `surface` as stored; a terminal tab carries its group, and one without a group is not kept.
fn stored_surface(surface: &Surface, group: impl Fn(&str) -> Option<StoredGroup>) -> Option<Value> {
    let mut value = serde_json::to_value(surface).ok()?;
    if let Surface::Terminal(id) = surface {
        let Value::Object(fields) = serde_json::to_value(group(id)?).ok()? else { return None };
        value.as_object_mut()?.extend(fields);
    }
    Some(value)
}

/// The panels worth keeping, in a stable order so an unchanged layout stores the same document.
/// `group` is a terminal tab's group, by surface id.
pub(super) fn document(
    panels: &HashMap<Target, PanelState>,
    group: impl Fn(&str) -> Option<StoredGroup> + Copy,
) -> Value {
    let mut stored: Vec<StoredPanel> = panels
        .iter()
        .filter_map(|(target, panel)| {
            let surfaces: Vec<Value> = panel.surfaces.iter().filter_map(|s| stored_surface(s, group)).collect();
            let width = panel.width.filter(|width| width.is_finite());
            let active = panel.active.as_ref().filter(|active| match active {
                Surface::Terminal(id) => group(id).is_some(),
                _ => true,
            });
            let stored = StoredPanel {
                target: target.into(),
                open: panel.open && !surfaces.is_empty(),
                active: active.and_then(|s| serde_json::to_value(s).ok()),
                surfaces,
                maximized: panel.maximized,
                width,
                proactive_diff_run: panel.proactive_diff_run.clone(),
            };
            let empty = stored.surfaces.is_empty()
                && !stored.maximized
                && stored.width.is_none()
                && stored.proactive_diff_run.is_none();
            (!empty).then_some(stored)
        })
        .collect();
    stored.sort_by(|a, b| a.target.cmp(&b.target));
    json!({ "version": 1, "panels": stored })
}

/// Terminal tab groups, each with its target and surface id.
pub(super) type RestoredGroups = Vec<(Target, String, StoredGroup)>;

/// `migratePersistedRightPanelState`: unknown surfaces and terminal tabs without a valid group
/// drop, the panel only opens with a surface, and an open panel whose active tab is gone shows
/// its first one. The terminal tabs' groups come back beside the panels.
pub(super) fn restored(document: &Value) -> (HashMap<Target, PanelState>, RestoredGroups) {
    let mut groups = Vec::new();
    let Some(panels) = document.get("panels").and_then(Value::as_array) else { return (HashMap::new(), groups) };
    let panels = panels
        .iter()
        .filter_map(|value| {
            let stored: StoredPanel = serde_json::from_value(value.clone()).ok()?;
            let target = stored.target.target();
            let mut surfaces: Vec<Surface> = Vec::new();
            for value in stored.surfaces {
                let Ok(surface) = serde_json::from_value::<Surface>(value.clone()) else { continue };
                if surfaces.contains(&surface) {
                    continue;
                }
                if let Surface::Terminal(id) = &surface {
                    let group = serde_json::from_value::<StoredGroup>(value).ok().and_then(|g| g.restored(&target));
                    let Some(group) = group else { continue };
                    groups.push((target.clone(), id.clone(), group));
                }
                surfaces.push(surface);
            }
            let open = stored.open && !surfaces.is_empty();
            let active = stored
                .active
                .and_then(|s| serde_json::from_value::<Surface>(s).ok())
                .filter(|active| surfaces.contains(active))
                .or_else(|| surfaces.first().cloned().filter(|_| open));
            let panel = PanelState {
                open,
                surfaces,
                active,
                maximized: stored.maximized,
                width: stored.width.filter(|width| width.is_finite() && *width > 0.),
                proactive_diff_run: stored.proactive_diff_run,
                ..PanelState::default()
            };
            Some((target, panel))
        })
        .collect();
    (panels, groups)
}

/// `groups`' group for `surface`.
fn group_in(groups: &RestoredGroups, surface: &str) -> Option<StoredGroup> {
    groups.iter().find(|(_, id, _)| id == surface).map(|(_, _, group)| group.clone())
}

/// What the layout persistence keeps beside the panels.
pub(super) struct LayoutStore {
    writer: super::writer::Writer,
    /// The document last saved (or read), so an unchanged layout is not written again.
    persisted: Value,
    /// A panel was reached for change since the last save.
    pub(super) changed: bool,
    _save: Option<Task<()>>,
    /// The last launch's terminal tab groups, until the terminals take them.
    groups: RestoredGroups,
}

impl LayoutStore {
    /// The last launch's layout, and the store that keeps it.
    pub(super) fn restored() -> (Self, HashMap<Target, PanelState>) {
        let writer = super::writer::Writer::new("right panel layout", "right-panel-state.json");
        let (panels, groups) = writer.read().map(|document| restored(&document)).unwrap_or_default();
        let persisted = document(&panels, |surface| group_in(&groups, surface));
        let store = Self { writer, persisted, changed: false, _save: None, groups };
        (store, panels)
    }
}

impl Workspace {
    /// Hands the last launch's terminal tab groups to the terminals.
    pub(in crate::app) fn restore_terminal_surfaces(&mut self) {
        for (target, surface, group) in std::mem::take(&mut self.right_panel.layout.groups) {
            self.terminals.restore_group(&target, surface, group);
        }
    }

    /// Saves the layout shortly after it changes: one save per [`SAVE_DELAY`], from the state
    /// at that time, so a resize drag writes once it settles rather than every frame.
    pub(super) fn persist_panel_layout(&mut self, cx: &mut Context<Self>) {
        if self.terminals.take_panel_changed() {
            self.right_panel.layout.changed = true;
        }
        let store = &mut self.right_panel.layout;
        if !store.changed || store._save.is_some() {
            return;
        }
        store._save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            let _ = this.update(cx, |this, _| {
                let document = document(&this.right_panel.panels, |surface| this.terminals.stored_group(surface));
                let store = &mut this.right_panel.layout;
                store._save = None;
                store.changed = false;
                if document != store.persisted {
                    store.persisted = document.clone();
                    store.writer.save(document);
                }
            });
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(id: &str) -> Target {
        Target::Thread { env: "env".into(), id: id.to_owned() }
    }

    fn group(value: Value) -> StoredGroup {
        serde_json::from_value(value).unwrap()
    }

    fn no_groups(_: &str) -> Option<StoredGroup> {
        None
    }

    #[test]
    fn a_thread_layout_survives_a_launch() {
        let attachment = crate::app::files::AttachmentFile {
            id: "att-1".into(),
            name: "notes.pdf".into(),
            mime: "application/pdf".into(),
            size: 12,
        };
        let reference = crate::pull_request_detail::Reference {
            environment_id: "env".into(),
            project_id: "p".into(),
            host: Some("github.com".into()),
            repository: "o/r".into(),
            number: 7,
        };
        let panel = PanelState {
            open: true,
            surfaces: vec![
                Surface::Diff,
                Surface::Terminal("terminal-closed".into()),
                Surface::Terminal("terminal-split".into()),
                Surface::File("src/main.rs".into()),
                Surface::Attachment(attachment.clone()),
                Surface::PullRequest(reference.clone()),
                Surface::SideChat("side".into()),
                Surface::Browser("tab-1".into()),
            ],
            active: Some(Surface::Attachment(attachment.clone())),
            maximized: true,
            width: Some(612.),
            proactive_diff_run: Some("run-3".into()),
            user_revision: 9,
            ..PanelState::default()
        };
        let draft = Target::draft("env".into(), "p".into(), crate::app::draft_sessions::DraftId::for_test("d1"));
        let panels = HashMap::from([
            (thread("t"), panel),
            (draft.clone(), PanelState { width: Some(400.), ..PanelState::default() }),
            (thread("untouched"), PanelState { user_revision: 3, ..PanelState::default() }),
        ]);
        let split = group(json!({"threadId": "t", "terminalIds": ["term-2", "term-3"],
            "activeTerminalId": "term-3", "splitDirection": "vertical"}));
        let stored = document(&panels, |surface| (surface == "terminal-split").then(|| split.clone()));
        let text = serde_json::to_string(&stored).unwrap();
        let (restored, groups) = restored(&serde_json::from_str(&text).unwrap());

        assert_eq!(restored.len(), 2, "a panel left as it started is not kept");
        let panel = &restored[&thread("t")];
        assert!(panel.open && panel.maximized);
        assert_eq!(panel.width, Some(612.));
        assert_eq!(panel.proactive_diff_run.as_deref(), Some("run-3"));
        assert_eq!(panel.user_revision, 0);
        assert_eq!(
            panel.surfaces,
            [
                Surface::Diff,
                Surface::Terminal("terminal-split".into()),
                Surface::File("src/main.rs".into()),
                Surface::Attachment(attachment),
                Surface::PullRequest(reference),
                Surface::SideChat("side".into()),
                Surface::Browser("tab-1".into()),
            ],
            "a terminal tab whose group is gone is not kept"
        );
        assert_eq!(groups, [(thread("t"), "terminal-split".to_owned(), split)], "the tab's split group");
        let Some(Surface::Attachment(file)) = &panel.active else { panic!("the attachment stays active") };
        assert_eq!((file.name.as_str(), file.mime.as_str(), file.size), ("notes.pdf", "application/pdf", 12));
        assert_eq!(restored[&draft].width, Some(400.));
        assert_eq!(
            document(&restored, |surface| group_in(&groups, surface)),
            stored,
            "the same layout stores the same document"
        );
        assert_eq!(document(&restored, no_groups)["panels"][0]["surfaces"].as_array().unwrap().len(), 6);
    }

    #[test]
    fn a_stored_layout_is_read_leniently() {
        let document = json!({"version": 1, "panels": [
            // Only a terminal without its group and an unknown surface: nothing to show, so the
            // panel stays shut.
            {"target": {"kind": "thread", "environmentId": "env", "threadId": "a"}, "open": true,
             "surfaces": [{"kind": "terminal", "value": "terminal:1"}, {"kind": "device"}], "width": 500.0},
            // The active tab was a terminal: the open panel shows its first tab.
            {"target": {"kind": "thread", "environmentId": "env", "threadId": "b"}, "open": true,
             "surfaces": [{"kind": "files"}, {"kind": "diff"}, {"kind": "files"}],
             "active": {"kind": "terminal", "value": "terminal:1"}},
            // A closed panel keeps no active tab it does not have.
            {"target": {"kind": "thread", "environmentId": "env", "threadId": "c"},
             "surfaces": [{"kind": "diff"}]},
            // Terminal tabs: repeated and unknown active ids are cleaned up; one naming another
            // thread or holding no terminals drops.
            {"target": {"kind": "thread", "environmentId": "env", "threadId": "d"}, "open": true,
             "surfaces": [
                {"kind": "terminal", "value": "terminal-1", "threadId": "d",
                 "terminalIds": ["term-1", "term-2", "term-1"], "activeTerminalId": "gone"},
                {"kind": "terminal", "value": "terminal-2", "threadId": "other",
                 "terminalIds": ["term-3"], "activeTerminalId": "term-3"},
                {"kind": "terminal", "value": "terminal-3", "threadId": "d",
                 "terminalIds": [], "activeTerminalId": "term-4"}],
             "active": {"kind": "terminal", "value": "terminal-2"}},
            {"target": {"kind": "elsewhere"}},
            "garbage"
        ]});
        let (panels, groups) = restored(&document);
        assert_eq!(panels.len(), 4);
        let a = &panels[&thread("a")];
        assert!(!a.open && a.surfaces.is_empty() && a.active.is_none());
        assert_eq!(a.width, Some(500.));
        let b = &panels[&thread("b")];
        assert!(b.open);
        assert_eq!(b.surfaces, [Surface::Files, Surface::Diff], "a repeated surface keeps its first place");
        assert_eq!(b.active, Some(Surface::Files));
        let c = &panels[&thread("c")];
        assert!(!c.open && c.active.is_none());
        let d = &panels[&thread("d")];
        assert_eq!(d.surfaces, [Surface::Terminal("terminal-1".into())]);
        assert_eq!(d.active, Some(Surface::Terminal("terminal-1".into())));
        let expected =
            group(json!({"threadId": "d", "terminalIds": ["term-1", "term-2"], "activeTerminalId": "term-1"}));
        assert_eq!(groups, [(thread("d"), "terminal-1".to_owned(), expected)]);
        let (panels, groups) = restored(&json!("not a layout"));
        assert!(panels.is_empty() && groups.is_empty());
    }

    #[test]
    fn a_drafts_terminal_tab_keeps_the_thread_it_will_launch_with() {
        let draft = Target::draft("env".into(), "p".into(), crate::app::draft_sessions::DraftId::for_test("d1"));
        let tab = json!({"kind": "terminal", "value": "terminal-1", "threadId": "future",
            "terminalIds": ["term-1"], "activeTerminalId": "term-1"});
        let document =
            json!({"version": 1, "panels": [{"target": StoredTarget::from(&draft), "open": true, "surfaces": [tab]}]});
        let (panels, groups) = restored(&document);
        assert!(panels[&draft].open);
        let mut terminals = crate::app::terminal::Terminals::default();
        for (target, surface, group) in groups {
            terminals.restore_group(&target, surface, group);
        }
        assert_eq!(
            terminals.stored_group("terminal-1").map(|g| serde_json::to_value(g).unwrap()),
            Some(json!({
                "threadId": "future", "terminalIds": ["term-1"], "activeTerminalId": "term-1", "splitDirection": "horizontal"
            }))
        );
    }
}
