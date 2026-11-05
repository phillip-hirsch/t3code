//! Settings › Worktrees (`WorktreesSettingsPanel` in `apps/web/src/components/settings/WorktreesSettings.tsx`,
//! `BranchNamingSettings.tsx`, and `WorktreesDirectoryRow`, `WorktreeCleanupSection` from
//! `StorageSettings.tsx`).
//!
//! Scoped in the web: every row is `serverScoped`. At project scope the web hides Worktree
//! location, rewords Submodules, and adds an Inherit/Off/Custom cleanup mode that writes the
//! project's `worktreeCleanup` instead of `storageCleanup`. Policy locks (`managedPaths`) are
//! the foundation's. This port edits the connected environment's settings only.

use gpui_kit::component::input::NumberInput;
use gpui_kit::component::{Sizable as _, h_flex};
use gpui_kit::{AnyElement, Context, IntoElement, ParentElement as _, Styled as _, Window, div, px};
use serde_json::{Value, json};

use super::pending::{
    Control, Row, SERVER_SCOPES, SettingRowSpec, advanced, muted, page, patch, reset_button, resolve_pasted_path,
    section, select, switch,
};
use crate::app::Workspace;
use crate::theme::tokens;

const ROWS: &[SettingRowSpec] = &[
    SettingRowSpec {
        id: "start-from-origin",
        label: "Start from remote",
        description: "Uses the latest commit of the base branch’s tracked remote branch, even if its name differs. Otherwise uses the same-named branch on origin. Uses your local branch if that remote branch is missing.",
        path: "newWorktreesStartFromOrigin",
        control: Control::Switch,
        default: Some("true"),
        scopes: SERVER_SCOPES,
    },
    SettingRowSpec {
        id: "branch-prefix",
        label: "Branch prefix",
        description: "For example, t3 or t3/ produces t3/add-search. Leave empty for no prefix.",
        path: "branchNamePrefix",
        control: Control::Text("No prefix"),
        default: Some("\"t3\""),
        scopes: SERVER_SCOPES,
    },
    SettingRowSpec {
        id: "branch-naming-instructions",
        label: "Branch naming instructions",
        description: "Appended to the naming prompt. The model returns the complete branch name; no prefix or suffix is added.",
        path: "branchNameInstructions",
        control: Control::Textarea("Use julius/ followed by the issue ID and a short description."),
        default: Some("\"\""),
        scopes: SERVER_SCOPES,
    },
    SettingRowSpec {
        id: "worktree-on-delete",
        label: "Delete worktrees with deleted threads",
        description: "Remove unused worktrees when active or archived threads are deleted. Worktrees with local changes are kept.",
        path: "storageCleanup.worktreeOnDelete",
        control: Control::Switch,
        default: None,
        scopes: SERVER_SCOPES,
    },
    SettingRowSpec {
        id: "worktree-on-merge",
        label: "Delete merged worktrees",
        description: "Remove worktrees whose pull request is merged and whose commits are included in the default branch.",
        path: "storageCleanup.worktreeOnMerge",
        control: Control::Switch,
        default: None,
        scopes: SERVER_SCOPES,
    },
    SettingRowSpec {
        id: "worktree-unchanged",
        label: "Delete unchanged worktrees",
        description: "Remove worktrees with no commits beyond the default branch.",
        path: "storageCleanup.worktreeUnchanged",
        control: Control::Switch,
        default: None,
        scopes: SERVER_SCOPES,
    },
];

fn spec(id: &str) -> &'static SettingRowSpec {
    ROWS.iter().find(|row| row.id == id).expect("settings row")
}

const SUBMODULES: &[(&str, &str)] = &[("recursive", "Recursive"), ("top-level", "Top level only"), ("none", "Skip")];
const NAMING: &[(&str, &str)] =
    &[("static", "Static prefix"), ("semantic", "Semantic prefix"), ("custom", "Custom instructions")];
const KEEP_WHEN: &[(&str, &str)] = &[
    ("any-local-files", "Any local files"),
    ("uncommitted-changes", "Uncommitted changes"),
    ("tracked-changes", "Edited tracked files"),
];

fn label_of(options: &[(&str, &'static str)], id: &str) -> &'static str {
    options.iter().find(|(option, _)| *option == id).map_or("", |(_, label)| label)
}

fn options(options: &[(&str, &str)]) -> Vec<(gpui_kit::SharedString, gpui_kit::SharedString)> {
    options.iter().map(|(id, label)| (id.to_string().into(), label.to_string().into())).collect()
}

impl Workspace {
    pub(super) fn render_worktrees_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        let mut creation = Vec::new();
        if self.server_info.capabilities["worktreesDirectory"] == true {
            creation.push(self.worktrees_directory_row(window, cx));
        }
        creation.push(self.render_spec(spec("start-from-origin"), window, cx).render(&t));
        let open = window.use_keyed_state("settings-worktree-creation-advanced", cx, |_, _| false);
        let rows = if *open.read(cx) { vec![self.submodules_row(cx)] } else { Vec::new() };
        creation.push(advanced("worktree-creation-advanced", *open.read(cx), rows, &t, move |_, _, cx| {
            open.update(cx, |open, cx| {
                *open = !*open;
                cx.notify();
            })
        }));
        page(
            "settings-worktrees",
            [
                section("Creation", None, creation, &t),
                section("Branches", None, self.branch_naming_rows(window, cx), &t),
                self.worktree_cleanup_section(window, cx),
            ],
        )
    }

    fn worktrees_directory_row(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        let saved = self.setting("worktreesDirectory").as_str().unwrap_or_default().to_owned();
        let field = self.text_field(
            "worktrees-directory",
            &saved,
            "Default",
            false,
            Some(resolve_pasted_path),
            window,
            cx,
            |this, text, cx| this.update_settings(json!({ "worktreesDirectory": text }), cx),
        );
        Row::new("storage-worktrees-location", "Worktree location")
            .description("Folder where new worktrees are created, on any drive, such as D:\\worktrees or ~/worktrees. Existing worktrees stay where they are. Leave empty to use the T3 home folder.")
            .reset((!saved.is_empty()).then(|| {
                reset_button("worktree-location", cx.listener(|this, _, _, cx| this.update_settings(json!({ "worktreesDirectory": "" }), cx)))
            }))
            .control(field.sized(px(288.)))
            .render(&t)
    }

    /// `WorktreeSubmodulesRow`: unset follows t3.json, then recursive.
    fn submodules_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        let stored = self.setting("worktreeSubmodules").as_str().map(str::to_owned);
        let effective = stored.clone().unwrap_or_else(|| "recursive".into());
        Row::new("worktree-submodules", "Submodules")
            .description("How new worktrees populate git submodules. Projects and their t3.json can override it.")
            .reset(stored.is_some().then(|| {
                reset_button(
                    "worktree-submodules",
                    cx.listener(|this, _, _, cx| this.update_settings(json!({ "worktreeSubmodules": null }), cx)),
                )
            }))
            .control(select(
                "worktree-submodules",
                label_of(SUBMODULES, &effective),
                options(SUBMODULES),
                Some(effective.into()),
                cx,
                |this, value, cx| this.update_settings(json!({ "worktreeSubmodules": value.as_ref() }), cx),
            ))
            .render(&t)
    }

    fn branch_naming_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = *tokens(cx);
        let mode = self.setting("branchNamingMode").as_str().unwrap_or("static").to_owned();
        let naming = Row::new("worktree-branch-naming", "Worktree branch naming")
            .description("Choose how new worktree branches are named from your first message.")
            .status((mode == "semantic").then_some(
                "The model chooses a prefix that describes the work, such as feat/add-search, fix/login-timeout, or refactor/auth.",
            ))
            .reset((mode != "static").then(|| {
                reset_button("branch-naming", cx.listener(|this, _, _, cx| this.update_settings(json!({ "branchNamingMode": "static" }), cx)))
            }))
            .control(select("branch-naming", label_of(NAMING, &mode), options(NAMING), Some(mode.clone().into()), cx, |this, value, cx| {
                this.update_settings(json!({ "branchNamingMode": value.as_ref() }), cx)
            }))
            .render(&t);
        let mut rows = vec![naming];
        match mode.as_str() {
            "static" => rows.push(self.render_spec(spec("branch-prefix"), window, cx).render(&t)),
            "custom" => rows.push(self.render_spec(spec("branch-naming-instructions"), window, cx).render(&t)),
            _ => {}
        }
        rows
    }

    /// `WorktreeCleanupSection` at environment scope.
    fn worktree_cleanup_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        if self.server_info.capabilities["storageCleanup"] != true {
            let notice = muted(
                "Update the selected environments to use storage cleanup, or choose a machine that supports it.",
                &t,
            )
            .px(px(16.))
            .py(px(10.))
            .text_xs();
            return section("Cleanup", None, [notice.into_any_element()], &t);
        }
        let mut rows = vec![
            self.render_spec(spec("worktree-on-delete"), window, cx).render(&t),
            self.render_spec(spec("worktree-on-merge"), window, cx).render(&t),
            Row::new("worktree-after-days", "Delete inactive worktrees")
                .description("Remove worktrees after their threads have been inactive for this many days. Branches and thread history are kept.")
                .control(self.retention_control("worktreeAfterDays", 1, window, cx))
                .render(&t),
        ];
        let open = window.use_keyed_state("settings-worktree-cleanup-advanced", cx, |_, _| false);
        let mut folded = Vec::new();
        if *open.read(cx) {
            if self.server_info.capabilities["settledWorktreeCleanup"] == true {
                folded.push(
                    Row::new("worktree-settled-after-days", "Delete settled worktrees")
                        .description("Remove worktrees this many days after their thread settles. Set 0 days to remove them at the next cleanup. Branches and thread history are kept.")
                        .control(self.retention_control("worktreeSettledAfterDays", 0, window, cx))
                        .render(&t),
                );
            }
            folded.push(self.render_spec(spec("worktree-unchanged"), window, cx).render(&t));
            if self.server_info.capabilities["storageCleanupRun"] == true {
                folded.push(self.keep_when_row(cx));
            }
        }
        rows.push(advanced("worktree-cleanup-advanced", *open.read(cx), folded, &t, move |_, _, cx| {
            open.update(cx, |open, cx| {
                *open = !*open;
                cx.notify();
            })
        }));
        section("Cleanup", None, rows, &t)
    }

    /// `RetentionControl`: a day count with a switch; off shows "Off" and turning on starts at 8.
    fn retention_control(
        &mut self,
        key: &'static str,
        minimum: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *tokens(cx);
        let path: &'static str = if key == "worktreeAfterDays" {
            "storageCleanup.worktreeAfterDays"
        } else {
            "storageCleanup.worktreeSettledAfterDays"
        };
        let days = self.setting(path).as_i64();
        let value: AnyElement = match days {
            Some(days) => {
                let field = self.number_field(key, days, minimum, 3650, window, cx, move |this, days, cx| {
                    this.update_settings(patch(path, json!(days)), cx)
                });
                div()
                    .w(px(128.))
                    .child(
                        NumberInput::new(&field)
                            .small()
                            .suffix(div().text_xs().text_color(t.muted_foreground).child("days")),
                    )
                    .into_any_element()
            }
            None => div().text_xs().text_color(t.muted_foreground).child("Off").into_any_element(),
        };
        h_flex()
            .gap(px(12.))
            .child(value)
            .child(switch(key, days.is_some(), cx, move |this, on, cx| {
                this.update_settings(patch(path, if on { json!(8) } else { Value::Null }), cx)
            }))
            .into_any_element()
    }

    fn keep_when_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        let keep = self.setting("storageCleanup.worktreeKeepWhen").as_str().unwrap_or("uncommitted-changes").to_owned();
        Row::new("storage-worktree-keep-when", "Keep worktrees with local changes")
            .description(match keep.as_str() {
                "any-local-files" => "Includes ignored files, except node_modules.",
                "tracked-changes" => "Deletes untracked and ignored files, including .env.",
                _ => "Keeps edits and untracked files. Deletes ignored files, including .env.",
            })
            .control(select(
                "keep-when",
                label_of(KEEP_WHEN, &keep),
                options(KEEP_WHEN),
                Some(keep.into()),
                cx,
                |this, value, cx| {
                    this.update_settings(patch("storageCleanup.worktreeKeepWhen", json!(value.as_ref())), cx)
                },
            ))
            .render(&t)
    }
}
