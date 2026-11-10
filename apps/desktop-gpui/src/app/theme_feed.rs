//! The theme environment's published palettes and default theme (`useEnvironmentTheme.ts`,
//! `useDefaultTheme.ts`): which environment supplies them, repainting when they change, and
//! adopting its default theme once per generation.

use std::sync::Arc;

use gpui_kit::{Context, Task, Window};
use serde_json::Value;

use super::Workspace;
use super::sessions::EnvironmentSession;
use crate::theme;

#[derive(Default)]
pub(super) struct ThemeFeed {
    /// `getHostEnvironmentId`: the environment a service sharing this T3 home runs, read once.
    host_environment: Option<String>,
    /// The published set the library holds, so a resent, unchanged set repaints nothing.
    painted: Value,
    _lookup: Option<Task<()>>,
}

/// `createThemeEnvironmentIdAtom`: of the sessions' ids (the primary first), the primary once
/// its id is known, else the session of the service sharing this T3 home.
fn theme_environment<'a>(mut ids: impl Iterator<Item = Option<&'a str>>, host: Option<&str>) -> Option<usize> {
    if ids.next()?.is_some() {
        return Some(0);
    }
    let host = host?;
    ids.position(|id| id == Some(host)).map(|index| index + 1)
}

/// `defaultThemeGeneration`: the set time tells a reassertion of the same theme apart.
fn default_theme_generation(theme: &str, set_at: &str) -> String {
    if set_at.is_empty() { theme.to_owned() } else { format!("{theme}@{set_at}") }
}

/// `defaultThemeToApply`: the generation to adopt, if any. The setting and the palette it
/// names arrive separately, so one that does not resolve yet waits for the next change.
fn default_theme_to_apply(theme: &str, set_at: &str, applied: Option<&str>, resolves: bool) -> Option<String> {
    if theme.is_empty() {
        return None;
    }
    let generation = default_theme_generation(theme, set_at);
    (applied != Some(generation.as_str()) && resolves).then_some(generation)
}

impl Workspace {
    fn theme_session(&self) -> Option<&EnvironmentSession> {
        let ids = self.sessions().iter().map(|session| session.id.as_deref());
        self.sessions().get(theme_environment(ids, self.theme_feed.host_environment.as_deref())?)
    }

    /// Reads the host environment's id off the UI thread, then follows its feed if it is the
    /// theme environment.
    pub(super) fn start_theme_feed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let read = cx.background_executor().spawn(async {
            let path = crate::settings::userdata_dir()?.join("environment-id");
            let id = std::fs::read_to_string(path).ok()?;
            Some(id.trim().to_owned()).filter(|id| !id.is_empty())
        });
        self.theme_feed._lookup = Some(cx.spawn_in(window, async move |this, cx| {
            let host = read.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.theme_feed.host_environment = host;
                this.sync_theme_feed(window, cx);
            });
        }));
    }

    /// Puts the theme environment's published palettes in the library, repainting when they
    /// changed, then adopts its default theme.
    pub(super) fn sync_theme_feed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let painted = &self.theme_feed.painted;
        let changed = match self.theme_session().map(|session| &session.server_info.environment_themes) {
            Some(themes) => (themes != painted).then(|| themes.clone()),
            None => (!painted.is_null()).then_some(Value::Null),
        };
        if let Some(themes) = changed {
            let published = Arc::from(crate::theme_file::published_definitions(&themes));
            self.client_settings.themes = self.client_settings.themes.with_published(published);
            self.theme_feed.painted = themes;
            // The palette paints from the pick as it was set, so new colors land only on a repaint.
            theme::set_preference(self.client_settings.theme_choice(), window, cx);
            self.sync_composer_marks(cx);
            cx.notify();
        }
        self.adopt_default_theme(window, cx);
    }

    /// `adoptDefaultTheme`: applies the theme environment's `defaultTheme` once per generation.
    fn adopt_default_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((environment, settings)) =
            self.theme_session().and_then(|s| Some((s.id.clone()?, &s.server_info.settings)))
        else {
            return;
        };
        let text = |key| settings.get(key).as_str().unwrap_or_default();
        let (default_theme, set_at) = (text("defaultTheme").to_owned(), text("defaultThemeSetAt"));
        let resolves = self.client_settings.themes.find(&default_theme).is_some();
        let applied = self.client_settings.applied_default_theme(&environment);
        let Some(generation) = default_theme_to_apply(&default_theme, set_at, applied, resolves) else { return };
        let mut choice = self.client_settings.theme_choice();
        if !choice.adopt(&default_theme) {
            return;
        }
        self.client_settings.adopt_default_theme(choice.clone(), &environment, generation);
        theme::set_preference(choice, window, cx);
        self.sync_composer_marks(cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_theme_applies_once_per_generation_after_it_resolves() {
        assert_eq!(default_theme_generation("aurora", ""), "aurora");
        assert_eq!(default_theme_to_apply("aurora", "t1", None, true).as_deref(), Some("aurora@t1"));
        assert_eq!(default_theme_to_apply("aurora", "t1", Some("aurora@t1"), true), None, "already adopted");
        assert_eq!(
            default_theme_to_apply("aurora", "t2", Some("aurora@t1"), true).as_deref(),
            Some("aurora@t2"),
            "setting the same theme again applies again"
        );
        assert_eq!(default_theme_to_apply("aurora", "t1", None, false), None, "waits for its palette");
        assert_eq!(default_theme_to_apply("", "t1", None, true), None, "an empty default clears nothing");
    }

    #[test]
    fn the_primary_supplies_themes_else_the_host_service() {
        let pick = |ids: &[Option<&str>], host| theme_environment(ids.iter().copied(), host);
        assert_eq!(pick(&[Some("primary"), Some("host")], Some("host")), Some(0));
        let without_primary = [None, Some("saved"), Some("host")];
        assert_eq!(pick(&without_primary, Some("host")), Some(2));
        assert_eq!(pick(&without_primary, Some("elsewhere")), None);
        assert_eq!(pick(&without_primary, None), None);
    }
}
