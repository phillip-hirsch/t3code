//! The Settings modal (`apps/web/src/routes/_chat.settings.tsx`, an `AppRouteModal`
//! over the whole window with `SettingsSidebarNav` and one section at a time).

mod agents;
mod chat;
mod code;
mod connections;
mod diffs;
mod general;
mod keybindings;
mod placeholder;
mod snap_shot;
mod system;

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};

use crate::app::Workspace;
use crate::theme;

/// The sections in sidebar order (`SETTINGS_NAV_GROUPS`, `SETTINGS_SECTION_LABELS` in
/// `apps/web/src/components/settings/settingsSearch.ts`). A `None` heading is set apart by space alone.
pub(in crate::app) const SETTINGS: &[(Option<&str>, &[(&str, &str)])] = &[
    (
        None,
        &[
            ("general", "General"),
            ("appearance", "Appearance"),
            ("chat", "Chat"),
            ("diffs", "Diffs & pull requests"),
            ("keybindings", "Keybindings"),
            ("snap-shot", "SnapShots"),
        ],
    ),
    (
        Some("Agents"),
        &[
            ("providers", "Providers"),
            ("threads", "Threads"),
            ("browser", "Browser"),
            ("devices", "Devices"),
            ("scheduled-tasks", "Scheduled tasks"),
        ],
    ),
    (Some("Code"), &[("projects", "Projects"), ("source-control", "Source control"), ("worktrees", "Worktrees")]),
    (Some("Connections"), &[("connections", "Environments"), ("access", "Sharing & access"), ("secrets", "Secrets")]),
    (None, &[("maintenance", "Maintenance"), ("labs", "Labs"), ("about", "About"), ("archived", "Archive")]),
];

impl Workspace {
    /// The modal layer, drawn over everything while Settings is open.
    pub(in crate::app) fn render_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let section: SharedString = self.settings.clone()?;
        let tokens = *theme::tokens(cx);
        Some(
            div()
                .id("settings-modal")
                .absolute()
                .inset_0()
                .occlude()
                .bg(tokens.background)
                .p_4()
                .flex()
                .gap_4()
                // Placeholder navigation; the frame and its look are ported from `SettingsSidebarNav`.
                .child(div().flex().flex_col().gap_1().children(
                    SETTINGS.iter().flat_map(|(_, items)| items.iter()).map(|(id, label)| {
                        let id: &'static str = id;
                        div()
                            .id(SharedString::from(format!("settings-nav-{id}")))
                            .cursor_pointer()
                            .text_color(if section.as_ref() == id {
                                tokens.foreground
                            } else {
                                tokens.muted_foreground
                            })
                            .on_click(cx.listener(move |this, _, _, cx| this.open_settings(id, cx)))
                            .child(*label)
                    }),
                ))
                .child(div().flex_1().child(self.render_settings_section(&section, window, cx)))
                .into_any_element(),
        )
    }

    /// One section's content.
    fn render_settings_section(&mut self, section: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match section {
            "general" => self.render_settings_general(window, cx),
            "appearance" => self.render_settings_appearance(window, cx),
            "chat" => self.render_settings_chat(window, cx),
            "diffs" => self.render_settings_diffs(window, cx),
            "keybindings" => self.render_settings_keybindings(window, cx),
            "snap-shot" => self.render_settings_snap_shot(window, cx),
            "providers" => self.render_settings_providers(window, cx),
            "threads" => self.render_settings_threads(window, cx),
            "browser" => self.render_settings_browser(window, cx),
            "devices" => self.render_settings_devices(window, cx),
            "scheduled-tasks" => self.render_settings_scheduled_tasks(window, cx),
            "projects" => self.render_settings_projects(window, cx),
            "source-control" => self.render_settings_source_control(window, cx),
            "worktrees" => self.render_settings_worktrees(window, cx),
            "connections" => self.render_settings_connections(window, cx),
            "access" => self.render_settings_access(window, cx),
            "secrets" => self.render_settings_secrets(window, cx),
            "maintenance" => self.render_settings_maintenance(window, cx),
            "labs" => self.render_settings_labs(window, cx),
            "about" => self.render_settings_about(window, cx),
            "archived" => self.render_settings_archived(window, cx),
            _ => self.render_settings_general(window, cx),
        }
    }
}
