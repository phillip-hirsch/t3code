//! Projects settings. STANDALONE STAND-IN: the project panel agent owns this file; at merge keep
//! theirs and add the `mod` lines below, then call `render_project_defaults` and
//! `render_project_actions` from their panel.

mod actions;
mod defaults;
mod rows;
mod script_editor;
mod scripts;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, px,
};

use crate::app::Workspace;

impl Workspace {
    /// A bare project picker over this slice's sections, for testing them before the panel lands.
    pub(super) fn render_projects_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let picked = window.use_keyed_state("projects-settings-picked", cx, |_, _| None::<String>);
        let projects: Vec<(String, String)> = self
            .shell
            .projects
            .iter()
            .filter(|p| p.archived_at.is_none())
            .map(|p| (p.id.clone(), p.title.clone()))
            .collect();
        let current = picked
            .read(cx)
            .clone()
            .filter(|id| projects.iter().any(|(p, _)| p == id))
            .or_else(|| projects.first().map(|(id, _)| id.clone()));
        let picker = h_flex().flex_wrap().gap(px(6.)).children(projects.iter().map(|(id, title)| {
            let (id, picked) = (id.clone(), picked.clone());
            let button = Button::new(SharedString::from(format!("pick-project-{id}"))).xsmall().label(title.clone());
            if current.as_deref() == Some(id.as_str()) { button.primary() } else { button.outline() }.on_click(
                move |_, _, cx| {
                    picked.update(cx, |picked, cx| {
                        *picked = Some(id.clone());
                        cx.notify();
                    })
                },
            )
        }));
        let mut body = v_flex().gap(px(32.)).child(picker);
        if let Some(project_id) = current {
            body = body
                .child(self.render_project_defaults(&project_id, window, cx))
                .child(self.render_project_actions(&project_id, window, cx));
        }
        gpui_kit::div()
            .id("projects-settings")
            .size_full()
            .overflow_y_scroll()
            .child(body.max_w(px(832.)).mx_auto().py(px(24.)))
            .into_any_element()
    }
}
