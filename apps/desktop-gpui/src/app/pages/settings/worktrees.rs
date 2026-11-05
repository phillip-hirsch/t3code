//! Settings › Worktrees (`WorktreesSettingsPanel` in `apps/web/src/components/settings/WorktreesSettings.tsx`,
//! `BranchNamingSettings.tsx`, and `WorktreesDirectoryRow`, `WorktreeCleanupSection` from
//! `StorageSettings.tsx`).
//!
//! Every row writes through the settings scope. Cleanup edits the environment's
//! `storageCleanup`; the web's project-scope Inherit/Off/Custom cleanup mode is not ported.

use gpui_kit::component::input::NumberInput;
use gpui_kit::component::{Sizable as _, h_flex};
use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use serde_json::{Value, json};

use super::super::fields::resolve_pasted_path;
use super::super::widgets::{SettingRow, SettingsView, note, settings_section, switch};
use crate::app::Workspace;
use crate::theme::tokens;

const SUBMODULES: &[(&str, &str)] = &[("recursive", "Recursive"), ("top-level", "Top level only"), ("none", "Skip")];
const NAMING: &[(&str, &str)] =
    &[("static", "Static prefix"), ("semantic", "Semantic prefix"), ("custom", "Custom instructions")];
const KEEP_WHEN: &[(&str, &str)] = &[
    ("any-local-files", "Any local files"),
    ("uncommitted-changes", "Uncommitted changes"),
    ("tracked-changes", "Edited tracked files"),
];

/// A `storageCleanup` field's patch; the server merges it into the stored object.
fn cleanup_patch(field: &str, value: Value) -> Value {
    json!({ "storageCleanup": { field: value } })
}

impl Workspace {
    pub(super) fn render_worktrees_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        let view = self.settings_view();
        let directory = self.server_info.capability("worktreesDirectory").then(|| {
            let saved = view.str("worktreesDirectory").to_owned();
            self.text_field(
                "worktrees-directory",
                &saved,
                "Default",
                false,
                Some(resolve_pasted_path),
                window,
                cx,
                |this, text, window, cx| this.update_settings(json!({ "worktreesDirectory": text }), window, cx),
            )
            .sized(px(288.))
        });
        let submodules = view.str("worktreeSubmodules");
        let submodules = if submodules.is_empty() { "recursive" } else { submodules };
        let submodules = self.value_select(
            "worktree-submodules",
            "worktreeSubmodules",
            &json!(submodules),
            SUBMODULES.iter().map(|(id, label)| (json!(id), (*label).into())).collect(),
            SUBMODULES.iter().find(|(id, _)| *id == submodules).map_or("", |(_, label)| label),
            176.,
            cx,
        );
        let mode = view.str("branchNamingMode").to_owned();
        let cleanup = self.cleanup_rows(&view, window, cx);
        let mut row = |this: &mut Self, row: SettingRow| this.setting_row(&view, row, window, cx);
        let mut creation = Vec::new();
        if let Some(directory) = directory {
            creation.push(row(
                self,
                SettingRow::new("storage-worktrees-location")
                    .description("Folder where new worktrees are created, on any drive, such as D:\\worktrees or ~/worktrees. Existing worktrees stay where they are. Leave empty to use the T3 home folder.")
                    .setting("worktreesDirectory")
                    .control(directory),
            ));
        }
        creation.push(row(
            self,
            SettingRow::new("start-from-origin")
                .description("Uses the latest commit of the base branch’s tracked remote branch, even if its name differs. Otherwise uses the same-named branch on origin. Uses your local branch if that remote branch is missing.")
                .setting("newWorktreesStartFromOrigin")
                .switch(),
        ));
        // `WorktreeSubmodulesRow`: unset follows t3.json, then recursive.
        let submodules = row(
            self,
            SettingRow::new("worktree-submodules")
                .description("How new worktrees populate git submodules. Projects and their t3.json can override it.")
                .setting("worktreeSubmodules")
                .control(submodules),
        );
        let mut branches = vec![row(
            self,
            SettingRow::new("worktree-branch-naming")
                .description("Choose how new worktree branches are named from your first message.")
                .status((mode == "semantic").then_some(
                    "The model chooses a prefix that describes the work, such as feat/add-search, fix/login-timeout, or refactor/auth.",
                ))
                .setting("branchNamingMode")
                .select(NAMING, 176.),
        )];
        match mode.as_str() {
            "static" => branches.push(row(
                self,
                SettingRow::titled("branch-prefix", "Branch prefix")
                    .description("For example, t3 or t3/ produces t3/add-search. Leave empty for no prefix.")
                    .setting("branchNamePrefix")
                    .text("No prefix", 224.),
            )),
            "custom" => branches.push(row(
                self,
                SettingRow::titled("branch-naming-instructions", "Branch naming instructions")
                    .description("Appended to the naming prompt. The model returns the complete branch name; no prefix or suffix is added.")
                    .setting("branchNameInstructions")
                    .textarea("Use julius/ followed by the issue ID and a short description."),
            )),
            _ => {}
        }
        creation.push(self.advanced_fold("worktree-creation", [submodules], cx));
        let sections = [
            settings_section("creation", "Creation", creation, &t),
            settings_section("branches", "Branches", branches, &t),
            settings_section("cleanup", "Cleanup", cleanup, &t),
        ];
        self.settings_page("worktrees", &view, sections, cx)
    }

    /// `WorktreeCleanupSection` at environment scope.
    fn cleanup_rows(&mut self, view: &SettingsView, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = *tokens(cx);
        let info = &self.server_info;
        if !info.capability("storageCleanup") {
            return vec![note(
                "Update the selected environments to use storage cleanup, or choose a machine that supports it.",
                &t,
            )];
        }
        let (settled, run) = (info.capability("settledWorktreeCleanup"), info.capability("storageCleanupRun"));
        let cleanup = view.value("storageCleanup").clone();
        let toggle = |field: &'static str, cx: &mut Context<Self>| {
            let on = cleanup[field] == true;
            switch(&field.into(), on, false, &t)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.update_settings(cleanup_patch(field, json!(!on)), window, cx)
                }))
                .into_any_element()
        };
        let on_delete = toggle("worktreeOnDelete", cx);
        let on_merge = toggle("worktreeOnMerge", cx);
        let unchanged = toggle("worktreeUnchanged", cx);
        let after = self.worktree_retention(&cleanup, "worktreeAfterDays", 1, window, cx);
        let settled_after =
            settled.then(|| self.worktree_retention(&cleanup, "worktreeSettledAfterDays", 0, window, cx));
        let keep = cleanup["worktreeKeepWhen"].as_str().unwrap_or("uncommitted-changes").to_owned();
        let keep_select = run.then(|| {
            let options = KEEP_WHEN.iter().map(|(id, label)| (*id, (*label).into(), *id == keep)).collect();
            let label = KEEP_WHEN.iter().find(|(id, _)| *id == keep).map_or("", |(_, label)| label);
            self.menu_select("keep-when", options, label, 208., cx, |this, value: &str, window, cx| {
                this.update_settings(cleanup_patch("worktreeKeepWhen", json!(value)), window, cx)
            })
        });
        let mut row = |this: &mut Self, row: SettingRow| this.setting_row(view, row, window, cx);
        let mut rows = vec![
            row(
                self,
                SettingRow::titled("worktree-on-delete", "Delete worktrees with deleted threads")
                    .description("Remove unused worktrees when active or archived threads are deleted. Worktrees with local changes are kept.")
                    .control(on_delete),
            ),
            row(
                self,
                SettingRow::titled("worktree-on-merge", "Delete merged worktrees")
                    .description("Remove worktrees whose pull request is merged and whose commits are included in the default branch.")
                    .control(on_merge),
            ),
            row(
                self,
                SettingRow::titled("worktree-after-days", "Delete inactive worktrees")
                    .description("Remove worktrees after their threads have been inactive for this many days. Branches and thread history are kept.")
                    .control(after),
            ),
        ];
        let mut folded = Vec::new();
        if let Some(settled_after) = settled_after {
            folded.push(row(
                self,
                SettingRow::titled("worktree-settled-after-days", "Delete settled worktrees")
                    .description("Remove worktrees this many days after their thread settles. Set 0 days to remove them at the next cleanup. Branches and thread history are kept.")
                    .control(settled_after),
            ));
        }
        folded.push(row(
            self,
            SettingRow::titled("worktree-unchanged", "Delete unchanged worktrees")
                .description("Remove worktrees with no commits beyond the default branch.")
                .control(unchanged),
        ));
        if let Some(keep_select) = keep_select {
            folded.push(row(
                self,
                SettingRow::new("storage-worktree-keep-when")
                    .description(match keep.as_str() {
                        "any-local-files" => "Includes ignored files, except node_modules.",
                        "tracked-changes" => "Deletes untracked and ignored files, including .env.",
                        _ => "Keeps edits and untracked files. Deletes ignored files, including .env.",
                    })
                    .control(keep_select),
            ));
        }
        rows.push(self.advanced_fold("worktree-cleanup", folded, cx));
        rows
    }

    /// `RetentionControl`: a day count with a switch; off shows "Off" and turning on starts at 8.
    fn worktree_retention(
        &mut self,
        cleanup: &Value,
        field: &'static str,
        minimum: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *tokens(cx);
        let days = cleanup[field].as_i64();
        let value: AnyElement = match days {
            Some(days) => {
                let input =
                    self.number_field(field, days, (minimum, 3650, 1), window, cx, move |this, days, window, cx| {
                        this.update_settings(cleanup_patch(field, json!(days)), window, cx)
                    });
                div()
                    .w(px(128.))
                    .child(
                        NumberInput::new(&input)
                            .small()
                            .suffix(div().text_xs().text_color(t.muted_foreground).child("days")),
                    )
                    .into_any_element()
            }
            None => div().text_xs().text_color(t.muted_foreground).child("Off").into_any_element(),
        };
        let on = days.is_some();
        h_flex()
            .gap(px(12.))
            .child(value)
            .child(switch(&field.into(), on, false, &t).on_click(cx.listener(move |this, _, window, cx| {
                this.update_settings(cleanup_patch(field, if on { Value::Null } else { json!(8) }), window, cx)
            })))
            .into_any_element()
    }
}
