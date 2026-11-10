//! What this client keeps for a thread, cleared once the thread is deleted (`deleteThread` in
//! `useThreadActions.ts`): the server deletes the thread's side chats with it.

use gpui_kit::{Context, SharedString};
use serde_json::Value;

use super::composer::ComposerDraft;
use super::{Target, Workspace};

/// The side chats of `parent` in an `orchestration.getThreadFamily` reply.
pub(in crate::app) fn family_side_chats(family: &Value, parent: &str) -> Vec<String> {
    family["threads"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|thread| {
            let lineage = &thread["lineage"];
            lineage["sideChat"].as_bool() == Some(true) && lineage["parentThreadId"].as_str() == Some(parent)
        })
        .filter_map(|thread| thread["id"].as_str().map(str::to_owned))
        .collect()
}

impl Workspace {
    /// The side chats of `thread_id` the shell lists on `env`; read them before the delete.
    pub(in crate::app) fn listed_side_chats(&self, env: &str, thread_id: &str) -> Vec<String> {
        self.shell
            .threads
            .iter()
            .filter(|t| {
                t.environment_id.as_ref() == env
                    && t.lineage.side_chat == Some(true)
                    && t.lineage.parent_thread_id.as_deref() == Some(thread_id)
            })
            .map(|t| t.id.clone())
            .collect()
    }

    /// Whether `env` can list a thread's whole family (`shellThreadFamilies`), which a shell that
    /// leaves idle threads out needs to find every side chat.
    pub(in crate::app) fn lists_thread_families(&self, env: &str) -> bool {
        self.env_info(env).is_some_and(|info| info.config["shellThreadFamilies"].as_bool() == Some(true))
    }

    /// Drops `target`'s composer draft and lets its staged uploads go. Whichever of the delete
    /// reply and the `thread.deleted` event comes first does it.
    pub(in crate::app) fn drop_deleted_draft(&mut self, target: &Target, cx: &mut Context<Self>) {
        if self.target.as_ref() == Some(target) {
            for attachment in std::mem::take(&mut self.attachments) {
                self.release_upload(&attachment, cx);
            }
        }
        let attachments = match self.composer_drafts.remove(target) {
            Some(ComposerDraft::Prompt(_, attachments, _)) => attachments,
            Some(ComposerDraft::Question(question)) => question.prompt_attachments,
            None => Vec::new(),
        };
        for attachment in &attachments {
            self.release_upload(attachment, cx);
        }
        self.composer_selections.remove(target);
        // The restart-surviving store drops it too.
        self.drafts_changed(cx);
    }

    /// Clears a deleted thread's and its side chats' composer drafts, releasing their staged
    /// uploads, and resets the thread's terminal drawer.
    pub(in crate::app) fn forget_deleted_thread(
        &mut self,
        env: &SharedString,
        thread_id: &str,
        side_chats: &[String],
        cx: &mut Context<Self>,
    ) {
        for id in std::iter::once(thread_id).chain(side_chats.iter().map(String::as_str)) {
            self.drop_deleted_draft(&Target::Thread { env: env.clone(), id: id.to_owned() }, cx);
            self.side_chat.forget(env, id);
        }
        self.prune_requests();
        self.terminals.forget_thread(env, thread_id, cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_family_names_only_the_deleted_threads_own_side_chats() {
        let family = json!({"snapshotSequence": 3, "threads": [
            {"id": "root", "lineage": {}},
            {"id": "side", "lineage": {"sideChat": true, "parentThreadId": "root"}},
            {"id": "fork", "lineage": {"parentThreadId": "root", "relationshipToParent": "fork"}},
            {"id": "nested", "lineage": {"sideChat": true, "parentThreadId": "fork"}},
            {"id": "subagent", "lineage": {"parentThreadId": "root", "relationshipToParent": "subagent"}},
        ]});
        assert_eq!(family_side_chats(&family, "root"), ["side"]);
        assert_eq!(family_side_chats(&family, "fork"), ["nested"]);
        assert!(family_side_chats(&json!({}), "root").is_empty(), "an unreadable reply names none");
    }
}
