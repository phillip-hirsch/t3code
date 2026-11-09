//! Multiple-model sends (`ComposerModelPicker`'s `toggleModel`, `useMultipleModelSend.ts`): in a
//! new draft, Shift-picking models in the picker selects several, and a send starts one thread per
//! model, each in its own worktree off the draft's branch, in the background.

use std::collections::HashMap;

use futures::future::join_all;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{Context, SharedString, Window};
use serde_json::{Value, json};

use super::attachments::ATTACHMENT_ONLY_PROMPT;
use super::composer::ComposerDraft;
use super::{Target, Workspace, draft_title, new_id};
use crate::model::ModelSelection;

/// The models each draft starts, and the ids of launches that failed, kept for their retry.
#[derive(Default)]
pub(super) struct FanoutState {
    /// Only drafts with more than one model are here (`draftFanoutSelectionsAtom`).
    selections: HashMap<Target, Vec<ModelSelection>>,
    /// A failed launch may still have reached the server, so sending the same prompt to the same
    /// model again reuses its command and thread ids, which the server takes only once.
    retries: HashMap<RetryKey, Retry>,
}

/// A draft and one of its models.
type RetryKey = (Target, String, String);

#[derive(Clone, Debug, PartialEq)]
struct Retry {
    text: String,
    command_id: String,
    thread_id: String,
    message_id: String,
}

impl FanoutState {
    /// The ids a model's launch of `text` goes out with: a failed launch's again for the same prompt.
    fn launch_ids(&self, key: &RetryKey, text: &str) -> Retry {
        let failed = self.retries.get(key).filter(|retry| retry.text == text).cloned();
        failed.unwrap_or_else(|| Retry {
            text: text.to_owned(),
            command_id: new_id(),
            thread_id: new_id(),
            message_id: new_id(),
        })
    }

    /// Keeps a failed launch's ids for its retry; a started one needs none.
    fn settle(&mut self, key: RetryKey, failed: Option<Retry>) {
        match failed {
            Some(retry) => {
                self.retries.insert(key, retry);
            }
            None => {
                self.retries.remove(&key);
            }
        }
    }
}

/// What toggling `picked` leaves: several models, or the one the composer goes back to.
#[derive(Debug, PartialEq)]
pub(super) enum Toggled {
    Several(Vec<ModelSelection>),
    One(ModelSelection),
}

fn same_model(a: &ModelSelection, b: &ModelSelection) -> bool {
    a.instance_id == b.instance_id && a.model == b.model
}

/// `toggleModel`: adds or removes `picked`, starting from the composer's own model.
pub(super) fn toggle(current: Option<&[ModelSelection]>, active: &ModelSelection, picked: ModelSelection) -> Toggled {
    let mut next = current.map_or_else(|| vec![active.clone()], <[_]>::to_vec);
    if next.iter().any(|s| same_model(s, &picked)) {
        next.retain(|s| !same_model(s, &picked));
    } else {
        next.push(picked);
    }
    if next.len() > 1 { Toggled::Several(next) } else { Toggled::One(next.pop().unwrap_or_else(|| active.clone())) }
}

/// The trigger's label: two names, then how many more.
pub(super) fn label(names: &[String]) -> String {
    match names {
        [] => "Choose models".into(),
        [first] => first.clone(),
        [first, second, rest @ ..] => {
            let more = if rest.is_empty() { String::new() } else { format!(", {} more", rest.len()) };
            format!("{first}, {second}{more}")
        }
    }
}

/// `multipleModelSendBlock`: why a multiple-model send cannot start here.
#[derive(Debug, PartialEq)]
pub(super) enum SendBlock {
    ServerOutdated,
    NeedsGitThread,
}

pub(super) fn send_block(count: usize, supported: bool, git: bool, branch: Option<&str>) -> Option<SendBlock> {
    if !supported {
        Some(SendBlock::ServerOutdated)
    } else if !git || branch.is_none() || count == 0 {
        Some(SendBlock::NeedsGitThread)
    } else {
        None
    }
}

/// One model's `orchestration.launchThread`: a fresh worktree off `base`, in the default mode.
struct Launch<'a> {
    command_id: &'a str,
    thread_id: &'a str,
    message_id: &'a str,
    project_id: &'a str,
    selection: &'a ModelSelection,
    runtime_mode: &'a str,
    base: &'a str,
    start_from_origin: bool,
    text: &'a str,
    attachments: &'a [Value],
    context: Option<&'a Value>,
}

impl Launch<'_> {
    fn input(&self) -> Value {
        let mut strategy = json!({"type": "worktree", "baseRef": self.base});
        if self.start_from_origin {
            strategy["startFromOrigin"] = true.into();
        }
        let mut initial = json!({"messageId": self.message_id, "text": self.text, "attachments": self.attachments});
        if let Some(context) = self.context {
            initial["context"] = context.clone();
        }
        json!({
            "commandId": self.command_id,
            "creationSource": "web",
            "threadId": self.thread_id,
            "projectId": self.project_id,
            "title": draft_title(self.text),
            "generateTitle": true,
            "modelSelection": self.selection.to_json(),
            "runtimeMode": self.runtime_mode,
            "interactionMode": "default",
            "workspaceStrategy": strategy,
            "initialMessage": initial,
        })
    }
}

impl Workspace {
    /// The open draft can start several models: its server prepares a worktree per thread.
    pub(super) fn fanout_supported(&self) -> bool {
        matches!(self.target, Some(Target::Draft { .. })) && self.server_info().capability("requiredWorktreeBootstrap")
    }

    /// The models the open draft starts, when it starts more than one.
    pub(super) fn model_fanout(&self) -> Option<&[ModelSelection]> {
        if !self.fanout_supported() {
            return None;
        }
        self.composer_ui.fanout.selections.get(self.target.as_ref()?).map(Vec::as_slice)
    }

    /// Back to one model, as a plain pick does.
    pub(super) fn clear_fanout(&mut self) {
        if let Some(target) = &self.target {
            self.composer_ui.fanout.selections.remove(target);
        }
    }

    /// A Shift-pick in the composer's picker.
    pub(super) fn toggle_fanout_model(&mut self, picked: ModelSelection, cx: &mut Context<Self>) {
        let (Some(target), Some(active)) = (self.target.clone(), self.composer_model()) else { return };
        match toggle(self.model_fanout(), &active, picked) {
            Toggled::Several(next) => {
                self.composer_ui.fanout.selections.insert(target, next);
            }
            Toggled::One(remaining) => {
                self.composer_ui.fanout.selections.remove(&target);
                if !same_model(&remaining, &active) {
                    self.pick_model(remaining, cx);
                }
            }
        }
        cx.notify();
    }

    /// The trigger's label for the selected models, as each one's trigger name.
    pub(super) fn fanout_label(&self, selections: &[ModelSelection]) -> String {
        let names: Vec<String> = selections
            .iter()
            .map(|selection| {
                let driver = self.server_info().provider(&selection.instance_id).map_or("", |p| p.driver.as_str());
                match self.server_info().model(selection) {
                    Some(model) if model.is_unavailable => {
                        format!("{} (Unavailable)", crate::model_label::trigger_model_name(model, driver))
                    }
                    Some(model) => crate::model_label::trigger_model_name(model, driver),
                    None => selection.model.clone(),
                }
            })
            .collect();
        label(&names)
    }

    /// Starts one background thread per selected model; the draft stays, and a model that fails
    /// stays selected with the prompt given back.
    pub(super) fn submit_fanout(
        &mut self,
        text: String,
        attachments: Vec<Value>,
        context: Option<Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(origin @ Target::Draft { .. }) = self.target.clone() else { return };
        let Target::Draft { project_id, .. } = &origin else { return };
        let Some(selections) = self.model_fanout().map(<[_]>::to_vec) else { return };
        let base = self.branch_trigger().0.map(str::to_owned);
        let supported = self.server_info().capability("requiredWorktreeBootstrap");
        match send_block(selections.len(), supported, self.is_git_repo(), base.as_deref()) {
            Some(SendBlock::ServerOutdated) => {
                self.error = Some("Update this server before starting multiple models.".into());
                cx.notify();
                return;
            }
            Some(SendBlock::NeedsGitThread) => {
                let note = Notification::warning(
                    "Multiple models need a new thread in a Git project. Each gets its own worktree.",
                )
                .title("Choose models and a base branch");
                window.push_notification(note, cx);
                return;
            }
            None => {}
        }
        let (Some(client), Some(base)) = (self.client(), base) else { return };
        // The composer's own model carries its options; the others go with their defaults.
        let current = self.composer_model();
        let mut targets = Vec::with_capacity(selections.len());
        for selection in &selections {
            let ready = self
                .server_info()
                .provider(&selection.instance_id)
                .is_some_and(|p| p.enabled && p.availability.as_deref() != Some("unavailable") && p.status == "ready");
            if !ready {
                self.error = Some(format!("Provider for {} is unavailable.", selection.model).into());
                cx.notify();
                return;
            }
            targets.push(current.clone().filter(|c| same_model(c, selection)).unwrap_or_else(|| selection.clone()));
        }
        let message_text = if text.is_empty() { ATTACHMENT_ONLY_PROMPT.to_owned() } else { text.clone() };
        let (runtime_mode, start_from_origin) = (self.runtime_mode(), self.draft_start_from_origin());
        let launches: Vec<(ModelSelection, Retry, Value)> = targets
            .into_iter()
            .map(|selection| {
                let key = (origin.clone(), selection.instance_id.clone(), selection.model.clone());
                let retry = self.composer_ui.fanout.launch_ids(&key, &message_text);
                let input = Launch {
                    command_id: &retry.command_id,
                    thread_id: &retry.thread_id,
                    message_id: &retry.message_id,
                    project_id,
                    selection: &selection,
                    runtime_mode: &runtime_mode,
                    base: &base,
                    start_from_origin,
                    text: &message_text,
                    attachments: &attachments,
                    context: context.as_ref(),
                }
                .input();
                (selection, retry, input)
            })
            .collect();
        let sent_attachments = std::mem::take(&mut self.attachments);
        self.error = None;
        self.sending = true;
        self.set_composer_text("", window, cx);
        self.release_typing_hold(false, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let requests =
                launches.iter().map(|(.., input)| client.request("orchestration.launchThread", input.clone()));
            let results = join_all(requests).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                let mut failed = Vec::new();
                let mut started = 0;
                for ((selection, retry, _), result) in launches.into_iter().zip(results) {
                    let key = (origin.clone(), selection.instance_id.clone(), selection.model.clone());
                    match result {
                        Ok(_) => {
                            this.composer_ui.fanout.settle(key, None);
                            started += 1;
                        }
                        Err(error) => {
                            let note = Notification::error(error.to_string())
                                .title(SharedString::from(format!("Could not start {}", selection.model)));
                            window.push_notification(note, cx);
                            this.composer_ui.fanout.settle(key, Some(retry));
                            failed.push(selection);
                        }
                    }
                }
                if started > 0 {
                    let noun = if started == 1 { "thread" } else { "threads" };
                    let note = Notification::success("").title(format!("Started {started} {noun} in background"));
                    window.push_notification(note, cx);
                    this.record_prompt_sent(window, cx);
                }
                if !failed.is_empty() {
                    // Only the failed models stay picked, unless the picks changed meanwhile.
                    let fanout = &mut this.composer_ui.fanout.selections;
                    if fanout.get(&origin) == Some(&selections) {
                        fanout.insert(origin.clone(), failed);
                    }
                    if !text.is_empty() {
                        this.give_back_prompt(&origin, text, window, cx);
                    }
                    if this.target.as_ref() == Some(&origin) {
                        this.attachments.extend(sent_attachments);
                    } else if let Some(ComposerDraft::Prompt(_, kept)) = this.composer_drafts.get_mut(&origin) {
                        kept.extend(sent_attachments);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(instance: &str, slug: &str) -> ModelSelection {
        ModelSelection { instance_id: instance.into(), model: slug.into(), options: None }
    }

    #[test]
    fn shift_picks_add_and_remove_models_down_to_one() {
        let active = model("claude", "opus");
        let added = toggle(None, &active, model("codex", "gpt"));
        assert_eq!(added, Toggled::Several(vec![active.clone(), model("codex", "gpt")]));
        let Toggled::Several(two) = added else { unreachable!() };
        let three = toggle(Some(&two), &active, model("codex", "mini"));
        assert_eq!(three, Toggled::Several(vec![active.clone(), model("codex", "gpt"), model("codex", "mini")]));
        // Removing down to one model leaves the composer on it.
        assert_eq!(toggle(Some(&two), &active, active.clone()), Toggled::One(model("codex", "gpt")));
        // The composer's own model, toggled alone, stays.
        assert_eq!(toggle(None, &active, active.clone()), Toggled::One(active));
    }

    #[test]
    fn a_failed_launch_retries_the_same_prompt_with_its_ids() {
        let mut state = FanoutState::default();
        let draft = Target::Draft { env: "e".into(), project_id: "p".into() };
        let key = (draft, "codex".to_owned(), "gpt".to_owned());
        let first = state.launch_ids(&key, "Fix it");
        assert_ne!(state.launch_ids(&key, "Fix it"), first, "nothing failed yet");
        state.settle(key.clone(), Some(first.clone()));
        assert_eq!(state.launch_ids(&key, "Fix it"), first, "the server takes the same ids once");
        assert_ne!(state.launch_ids(&key, "Fix it now").thread_id, first.thread_id, "a new prompt is a new thread");
        state.settle(key.clone(), None);
        assert_ne!(state.launch_ids(&key, "Fix it"), first);
    }

    #[test]
    fn the_trigger_names_two_models_then_counts_the_rest() {
        let names = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(label(&[]), "Choose models");
        assert_eq!(label(&names(&["Opus"])), "Opus");
        assert_eq!(label(&names(&["Opus", "GPT-5"])), "Opus, GPT-5");
        assert_eq!(label(&names(&["Opus", "GPT-5", "Sonnet", "Mini"])), "Opus, GPT-5, 2 more");
    }

    #[test]
    fn several_models_need_a_git_draft_on_a_server_that_prepares_worktrees() {
        assert_eq!(send_block(2, false, true, Some("main")), Some(SendBlock::ServerOutdated));
        assert_eq!(send_block(2, true, false, Some("main")), Some(SendBlock::NeedsGitThread));
        assert_eq!(send_block(2, true, true, None), Some(SendBlock::NeedsGitThread));
        assert_eq!(send_block(0, true, true, Some("main")), Some(SendBlock::NeedsGitThread));
        assert_eq!(send_block(2, true, true, Some("main")), None);
    }

    #[test]
    fn each_model_launches_in_its_own_worktree_off_the_branch() {
        let selection = model("codex", "gpt");
        let launch = |start_from_origin| {
            Launch {
                command_id: "c1",
                thread_id: "t1",
                message_id: "m1",
                project_id: "p1",
                selection: &selection,
                runtime_mode: "full-access",
                base: "main",
                start_from_origin,
                text: "Fix the bug",
                attachments: &[json!({"id": "a1"})],
                context: None,
            }
            .input()
        };
        assert_eq!(
            launch(false),
            json!({
                "commandId": "c1", "creationSource": "web", "threadId": "t1", "projectId": "p1",
                "title": "Fix the bug", "generateTitle": true, "modelSelection": selection.to_json(),
                "runtimeMode": "full-access", "interactionMode": "default",
                "workspaceStrategy": {"type": "worktree", "baseRef": "main"},
                "initialMessage": {"messageId": "m1", "text": "Fix the bug", "attachments": [{"id": "a1"}]},
            })
        );
        assert_eq!(launch(true)["workspaceStrategy"]["startFromOrigin"], true);
    }
}
