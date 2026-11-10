//! The panel layout kept across launches, in `right-panel-state.json`: each thread's (or
//! draft's) tabs, active tab, open and maximized state (`rightPanelStore`'s persisted
//! `byThreadKey`, read back as `migratePersistedRightPanelState` does) and its width
//! (`t3code:preview-panel-width:<thread>`). Terminal tabs are left out: their groups live in
//! the terminal state, which a launch starts without.

use std::collections::HashMap;

use gpui_kit::{Context, Task};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{PanelState, Surface};
use crate::app::{Target, Workspace};

/// Changes this close together are saved once.
const SAVE_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

/// A [`Target`] as stored: the thread, or the project draft, with its environment.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub(super) enum StoredTarget {
    Thread { environment_id: String, thread_id: String },
    Draft { environment_id: String, project_id: String },
}

impl From<&Target> for StoredTarget {
    fn from(target: &Target) -> Self {
        match target {
            Target::Thread { env, id } => Self::Thread { environment_id: env.to_string(), thread_id: id.clone() },
            Target::Draft { env, project_id } => {
                Self::Draft { environment_id: env.to_string(), project_id: project_id.clone() }
            }
        }
    }
}

impl StoredTarget {
    pub(super) fn target(&self) -> Target {
        match self {
            Self::Thread { environment_id, thread_id } => {
                Target::Thread { env: environment_id.clone().into(), id: thread_id.clone() }
            }
            Self::Draft { environment_id, project_id } => {
                Target::Draft { env: environment_id.clone().into(), project_id: project_id.clone() }
            }
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

fn kept(surface: &Surface) -> bool {
    !matches!(surface, Surface::Terminal(_))
}

/// The panels worth keeping, in a stable order so an unchanged layout stores the same document.
pub(super) fn document(panels: &HashMap<Target, PanelState>) -> Value {
    let mut stored: Vec<StoredPanel> = panels
        .iter()
        .filter_map(|(target, panel)| {
            let surfaces: Vec<Value> =
                panel.surfaces.iter().filter(|s| kept(s)).filter_map(|s| serde_json::to_value(s).ok()).collect();
            let width = panel.width.filter(|width| width.is_finite());
            let stored = StoredPanel {
                target: target.into(),
                open: panel.open && !surfaces.is_empty(),
                active: panel.active.as_ref().filter(|s| kept(s)).and_then(|s| serde_json::to_value(s).ok()),
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

/// `migratePersistedRightPanelState`: unknown and terminal surfaces drop, the panel only opens
/// with a surface, and an open panel whose active tab is gone shows its first one.
pub(super) fn restored(document: &Value) -> HashMap<Target, PanelState> {
    let Some(panels) = document.get("panels").and_then(Value::as_array) else { return HashMap::new() };
    panels
        .iter()
        .filter_map(|value| {
            let stored: StoredPanel = serde_json::from_value(value.clone()).ok()?;
            let mut surfaces: Vec<Surface> = Vec::new();
            for surface in stored.surfaces.into_iter().filter_map(|s| serde_json::from_value::<Surface>(s).ok()) {
                if kept(&surface) && !surfaces.contains(&surface) {
                    surfaces.push(surface);
                }
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
            Some((stored.target.target(), panel))
        })
        .collect()
}

/// What the layout persistence keeps beside the panels.
pub(super) struct LayoutStore {
    writer: super::writer::Writer,
    /// The document last saved (or read), so an unchanged layout is not written again.
    persisted: Value,
    /// A panel was reached for change since the last save.
    pub(super) changed: bool,
    _save: Option<Task<()>>,
}

impl LayoutStore {
    /// The last launch's layout, and the store that keeps it.
    pub(super) fn restored() -> (Self, HashMap<Target, PanelState>) {
        let writer = super::writer::Writer::new("right panel layout", "right-panel-state.json");
        let panels = writer.read().map(|document| restored(&document)).unwrap_or_default();
        let store = Self { writer, persisted: document(&panels), changed: false, _save: None };
        (store, panels)
    }
}

impl Workspace {
    /// Saves the layout shortly after it changes: one save per [`SAVE_DELAY`], from the state
    /// at that time, so a resize drag writes once it settles rather than every frame.
    pub(super) fn persist_panel_layout(&mut self, cx: &mut Context<Self>) {
        let store = &mut self.right_panel.layout;
        if !store.changed || store._save.is_some() {
            return;
        }
        store._save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            let _ = this.update(cx, |this, _| {
                let document = document(&this.right_panel.panels);
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
                Surface::Terminal("terminal:1".into()),
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
        let draft = Target::Draft { env: "env".into(), project_id: "p".into() };
        let panels = HashMap::from([
            (thread("t"), panel),
            (draft.clone(), PanelState { width: Some(400.), ..PanelState::default() }),
            (thread("untouched"), PanelState { user_revision: 3, ..PanelState::default() }),
        ]);
        let stored = document(&panels);
        let text = serde_json::to_string(&stored).unwrap();
        let restored = restored(&serde_json::from_str(&text).unwrap());

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
                Surface::File("src/main.rs".into()),
                Surface::Attachment(attachment),
                Surface::PullRequest(reference),
                Surface::SideChat("side".into()),
                Surface::Browser("tab-1".into()),
            ],
            "terminal groups do not outlive the launch"
        );
        let Some(Surface::Attachment(file)) = &panel.active else { panic!("the attachment stays active") };
        assert_eq!((file.name.as_str(), file.mime.as_str(), file.size), ("notes.pdf", "application/pdf", 12));
        assert_eq!(restored[&draft].width, Some(400.));
        assert_eq!(document(&restored), stored, "the same layout stores the same document");
    }

    #[test]
    fn a_stored_layout_is_read_leniently() {
        let document = json!({"version": 1, "panels": [
            // Only a terminal and an unknown surface: nothing to show, so the panel stays shut.
            {"target": {"kind": "thread", "environmentId": "env", "threadId": "a"}, "open": true,
             "surfaces": [{"kind": "terminal", "value": "terminal:1"}, {"kind": "device"}], "width": 500.0},
            // The active tab was a terminal: the open panel shows its first tab.
            {"target": {"kind": "thread", "environmentId": "env", "threadId": "b"}, "open": true,
             "surfaces": [{"kind": "files"}, {"kind": "diff"}, {"kind": "files"}],
             "active": {"kind": "terminal", "value": "terminal:1"}},
            // A closed panel keeps no active tab it does not have.
            {"target": {"kind": "thread", "environmentId": "env", "threadId": "c"},
             "surfaces": [{"kind": "diff"}]},
            {"target": {"kind": "elsewhere"}},
            "garbage"
        ]});
        let panels = restored(&document);
        assert_eq!(panels.len(), 3);
        let a = &panels[&thread("a")];
        assert!(!a.open && a.surfaces.is_empty() && a.active.is_none());
        assert_eq!(a.width, Some(500.));
        let b = &panels[&thread("b")];
        assert!(b.open);
        assert_eq!(b.surfaces, [Surface::Files, Surface::Diff], "a repeated surface keeps its first place");
        assert_eq!(b.active, Some(Surface::Files));
        let c = &panels[&thread("c")];
        assert!(!c.open && c.active.is_none());
        assert!(restored(&json!("not a layout")).is_empty());
    }
}
