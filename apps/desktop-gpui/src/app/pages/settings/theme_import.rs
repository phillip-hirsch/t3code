//! "Add a theme" (`ThemeImportDialog.tsx`, `ThemeSearchSection.tsx`): Open VSX community
//! themes, T3 Code or VS Code theme files, or pasted theme JSON.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, ExternalPaths, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, WeakEntity, Window, div, px, rems,
};

use super::theme_library::toast;
use crate::app::Workspace;
use crate::open_vsx::{self, Extension};
use crate::theme::{self, Tokens};
use crate::theme_file;
use crate::themes::{Library, ThemeChoice, ThemeDefinition};
use crate::vscode_theme::{humanize_theme_name, pair_themes, parse_theme_import, resolve_label_collisions};

/// Theme files are a few KB; anything bigger is not read (`MAX_THEME_FILE_BYTES`).
const MAX_THEME_FILE_BYTES: u64 = 256 * 1024;
const SUGGESTED_SEARCHES: [&str; 4] = ["Dracula", "Catppuccin", "Nord", "Tokyo Night"];
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(350);

fn byte_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024. * 1024.))
    } else if bytes >= 1024 {
        format!("{} KB", (bytes as f64 / 1024.).round())
    } else {
        format!("{bytes} bytes")
    }
}

/// A theme file's name and text, read at most one byte past the limit so a special or
/// growing file can't make the read unbounded.
fn read_theme_file(path: &std::path::Path) -> (String, Result<String, String>) {
    use std::io::Read as _;
    let name = path.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    if let Some(message) = std::fs::metadata(path).ok().and_then(|meta| oversized(meta.len())) {
        return (name, Err(message));
    }
    let mut text = String::new();
    let read = std::fs::File::open(path).and_then(|file| file.take(MAX_THEME_FILE_BYTES + 1).read_to_string(&mut text));
    let text = match read {
        Err(error) => Err(format!("Could not read {name} ({error}). Paste the JSON below instead.")),
        Ok(_) => oversized(text.len() as u64).map_or(Ok(text), Err),
    };
    (name, text)
}

/// `describeOversizedThemeFile`.
fn oversized(bytes: u64) -> Option<String> {
    (bytes > MAX_THEME_FILE_BYTES).then(|| {
        format!(
            "That file is {}. Theme files are only a few KB, so this one was not read (limit {}).",
            byte_size(bytes),
            byte_size(MAX_THEME_FILE_BYTES)
        )
    })
}

/// `Intl.NumberFormat` compact with one fraction digit: 1.2K, 34M.
fn compact(count: f64) -> String {
    let (value, suffix) = match count {
        c if c >= 1e9 => (c / 1e9, "B"),
        c if c >= 1e6 => (c / 1e6, "M"),
        c if c >= 1e3 => (c / 1e3, "K"),
        c => (c, ""),
    };
    let rounded = if value >= 100. || suffix.is_empty() { value.round() } else { (value * 10.).round() / 10. };
    format!("{}{suffix}", format!("{rounded:.1}").trim_end_matches(".0"))
}

/// `versionedCopy`: the theme under the file's name, else "<label> (n)", at a free id.
fn versioned_copy(
    theme: &ThemeDefinition,
    preferred: Option<&str>,
    library: &Library,
) -> Result<ThemeDefinition, String> {
    let free = |candidate: &ThemeDefinition| library.custom_theme(&candidate.id).is_none();
    if let Some(name) = preferred.filter(|name| name.to_lowercase() != theme.label.to_lowercase()) {
        let candidate = theme_file::renamed(theme, name)?;
        if free(&candidate) {
            return Ok(candidate);
        }
    }
    for copy in 1..100 {
        let suffix = format!(" ({copy})");
        let stem: String = theme.label.chars().take(48 - suffix.len()).collect();
        let candidate = theme_file::renamed(theme, &format!("{stem}{suffix}"))?;
        if free(&candidate) {
            return Ok(candidate);
        }
    }
    Err(format!("Too many copies of \"{}\".", theme.label))
}

pub(in crate::app) struct ThemeImport {
    workspace: WeakEntity<Workspace>,
    query: Entity<InputState>,
    sort: &'static str,
    results: Option<Vec<Extension>>,
    search_error: Option<String>,
    searching: bool,
    installing: Option<String>,
    /// The query and sort the shown results belong to.
    shown: Option<(String, &'static str)>,
    json: Entity<TextareaState>,
    file_name: Option<String>,
    error: Option<String>,
    reading: bool,
    drop_target: bool,
    conflicts: Option<Vec<ThemeDefinition>>,
    debounce: Option<Task<()>>,
    request: Option<Task<()>>,
    install: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ThemeImport {
    fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search themes..."));
        let placeholder =
            "{\n  \"version\": 1,\n  \"name\": \"Aurora\",\n  \"appearance\": \"light\",\n  \"colors\": { ... }\n}";
        let json = cx.new(|cx| TextareaState::new(window, cx).rows(8).placeholder(placeholder));
        let subscriptions = vec![
            cx.subscribe(&query, |this, _, event: &InputEvent, cx| match event {
                InputEvent::Change => this.query_changed(cx),
                InputEvent::PressEnter { .. } if !this.searching && this.installing.is_none() => this.search_now(cx),
                _ => {}
            }),
            cx.subscribe(&json, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        query.update(cx, |input, cx| input.focus(window, cx));
        Self {
            workspace,
            query,
            sort: "downloadCount",
            results: None,
            search_error: None,
            searching: false,
            installing: None,
            shown: None,
            json,
            file_name: None,
            error: None,
            reading: false,
            drop_target: false,
            conflicts: None,
            debounce: None,
            request: None,
            install: None,
            _subscriptions: subscriptions,
        }
    }

    fn library(&self, cx: &gpui_kit::App) -> Library {
        self.workspace.upgrade().map(|w| w.read(cx).client_settings.themes.clone()).unwrap_or_default()
    }

    fn query(&self, cx: &gpui_kit::App) -> String {
        self.query.read(cx).value().trim().to_owned()
    }

    /// `setQuery`: typing waits for a pause; clearing drops the results.
    fn query_changed(&mut self, cx: &mut Context<Self>) {
        self.debounce = None;
        if self.query(cx).is_empty() {
            if self.installing.is_none() {
                self.request = None;
                (self.results, self.search_error, self.searching, self.shown) = (None, None, false, None);
            }
            cx.notify();
            return;
        }
        self.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let _ = this.update(cx, ThemeImport::settle);
        }));
    }

    /// Searches unless the shown results already belong to this query and sort.
    fn settle(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        if self.installing.is_some() || query.is_empty() || self.shown.as_ref() == Some(&(query, self.sort)) {
            return;
        }
        self.search_now(cx);
    }

    fn search_now(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        if self.installing.is_some() || query.is_empty() {
            return;
        }
        self.debounce = None;
        (self.search_error, self.searching) = (None, true);
        let sort = self.sort;
        self.request = Some(cx.spawn(async move |this, cx| {
            let result = open_vsx::search(query.clone(), sort).await;
            let _ = this.update(cx, |this, cx| {
                this.searching = false;
                match result {
                    Ok(results) => (this.results, this.shown) = (Some(results), Some((query, sort))),
                    Err(error) => (this.results, this.shown, this.search_error) = (None, None, Some(error)),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn pick_suggestion(&mut self, suggestion: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if self.query(cx) == suggestion {
            self.search_now(cx);
            return;
        }
        self.query.update(cx, |input, cx| input.set_value(suggestion, window, cx));
        self.query_changed(cx);
    }

    /// `handleInstall`: an installed collection asks before it is replaced.
    fn install(&mut self, extension: Extension, confirmed: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.search_error = None;
        let library = self.library(cx);
        let updated =
            library.custom().iter().any(|t| t.collection.as_ref().is_some_and(|c| c.id == extension.collection_id));
        if updated && !confirmed {
            self.confirm_update(extension, window, cx);
            return;
        }
        (self.request, self.searching, self.installing) = (None, false, Some(extension.id.clone()));
        self.install = Some(cx.spawn_in(window, async move |this, cx| {
            let result = open_vsx::import(extension.clone()).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.installing = None;
                let installed = result.and_then(|themes| {
                    let library = this.library(cx).replace_collection(&extension.collection_id, themes.clone())?;
                    Ok((library, themes))
                });
                match installed {
                    Ok((library, themes)) => {
                        this.imported_many(library, &themes, updated, window, cx);
                        window.close_dialog(cx);
                        return;
                    }
                    Err(error) => this.search_error = Some(error),
                }
                this.settle(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn confirm_update(&mut self, extension: Extension, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.weak_entity();
        let title = format!("Update “{}”?", extension.name);
        window.open_dialog(cx, move |dialog, _, cx| {
            let (view, extension) = (view.clone(), extension.clone());
            let t = theme::tokens(cx);
            dialog
                .entrance(DialogEntrance::None)
                .title(title.clone())
                .child(div().text_sm().text_color(t.muted_foreground).child(
                    "This replaces its installed variants, including any local edits. Variants no longer in the extension will be removed.",
                ))
                .footer(
                    h_flex()
                        .justify_end()
                        .gap(px(8.))
                        .child(Button::new("theme-update-cancel").outline().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                        .child(Button::new("theme-update-confirm").primary().label("Update theme").on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            let extension = extension.clone();
                            let _ = view.update(cx, |this, cx| this.install(extension, true, window, cx));
                        })),
                )
        });
    }

    /// `onImportedMany`: stores the themes and says which were added or updated.
    fn imported_many(
        &mut self,
        library: Library,
        themes: &[ThemeDefinition],
        updated: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = self.workspace.update(cx, |workspace, cx| {
            let choice = ThemeChoice { library: library.clone(), ..workspace.client_settings.theme_choice() };
            workspace.store_themes(library, choice, window, cx);
        });
        let verb = if updated { "updated" } else { "added" };
        let title = match themes {
            [theme] => format!("{} {verb}", theme.label),
            themes => format!("{} themes {verb}", themes.len()),
        };
        let labels: Vec<&str> = themes.iter().map(|theme| theme.label.as_str()).collect();
        toast(window, cx, title, labels.join(", "));
    }

    /// `onImported`: a one-appearance theme becomes that appearance's theme; others the theme.
    fn imported(&mut self, library: Library, theme: &ThemeDefinition, window: &mut Window, cx: &mut Context<Self>) {
        let modes = theme.modes();
        let _ = self.workspace.update(cx, |workspace, cx| {
            let mut choice = ThemeChoice { library: library.clone(), ..workspace.client_settings.theme_choice() };
            match modes[..] {
                [dark] => choice.assign_half(dark, Some(theme.id.clone())),
                _ => choice.use_theme(Some(theme.id.clone())),
            }
            workspace.store_themes(library, choice, window, cx);
        });
        let description = match modes[..] {
            [dark] => format!("It’s now your {} theme.", if dark { "dark" } else { "light" }),
            _ => "It’s now active.".to_owned(),
        };
        toast(window, cx, format!("{} added", theme.label), description);
    }

    fn choose_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked =
            cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let _ = this.update_in(cx, |this, window, cx| this.read_files(paths, window, cx));
        })
        .detach();
    }

    /// `readThemeFiles`: one file fills the editor; several install at once. The reads run off
    /// the UI thread.
    fn read_files(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.drop_target = false;
        if paths.is_empty() || self.reading {
            return cx.notify();
        }
        self.reading = true;
        cx.notify();
        let read = cx.background_spawn(async move { paths.iter().map(|path| read_theme_file(path)).collect() });
        cx.spawn_in(window, async move |this, cx| {
            let files: Vec<(String, Result<String, String>)> = read.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.reading = false;
                match <[_; 1]>::try_from(files) {
                    Ok([(name, Ok(text))]) => {
                        this.json.update(cx, |json, cx| json.set_value(text, window, cx));
                        (this.file_name, this.error) = (Some(name), None);
                    }
                    Ok([(_, Err(error))]) => this.error = Some(error),
                    Err(files) => this.install_batch(files, window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `installThemeBatch`.
    fn install_batch(
        &mut self,
        files: Vec<(String, Result<String, String>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut failures = Vec::new();
        let mut parsed = Vec::new();
        for (name, text) in files {
            match text.and_then(|text| parse_theme_import(&text)) {
                Ok(theme) => parsed.push((theme, Some(name))),
                Err(error) if error.starts_with("That file is") => failures.push(format!("{name}: too large")),
                Err(error) => failures.push(format!("{name}: {error}")),
            }
        }
        let mut library = self.library(cx);
        let (mut installed, mut conflicting) = (Vec::new(), Vec::new());
        for theme in pair_themes(resolve_label_collisions(parsed), None) {
            if library.custom_theme(&theme.id).is_some() {
                conflicting.push(theme);
                continue;
            }
            match library.install(theme.clone()) {
                Ok(next) => {
                    library = next;
                    installed.push(theme);
                }
                Err(error) => failures.push(format!("{}: {error}", theme.label)),
            }
        }
        if !installed.is_empty() {
            self.imported_many(library, &installed, false, window, cx);
        }
        self.error = (!failures.is_empty()).then(|| failures.join("; "));
        if !conflicting.is_empty() {
            self.conflicts = Some(conflicting);
        } else if !installed.is_empty() && failures.is_empty() {
            window.close_dialog(cx);
        }
    }

    /// `resolveConflicts`: update the installed themes, or keep both under new names.
    fn resolve_conflicts(&mut self, update: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(conflicts) = self.conflicts.take() else { return };
        let preferred = match (&conflicts[..], &self.file_name) {
            ([_], Some(name)) => {
                Some(humanize_theme_name(name.rsplit_once('.').map_or(name.as_str(), |(stem, _)| stem)))
            }
            _ => None,
        };
        let mut library = self.library(cx);
        let (mut resolved, mut failures) = (Vec::new(), Vec::new());
        for theme in conflicts {
            let result = if update {
                let collection = library.custom_theme(&theme.id).and_then(|existing| existing.collection.clone());
                let theme = ThemeDefinition { collection: collection.or(theme.collection.clone()), ..theme.clone() };
                library.update(theme.clone()).map(|next| (next, theme))
            } else {
                versioned_copy(&theme, preferred.as_deref(), &library)
                    .and_then(|copy| library.install(copy.clone()).map(|next| (next, copy)))
            };
            match result {
                Ok((next, theme)) => {
                    library = next;
                    resolved.push(theme);
                }
                Err(error) => failures.push(format!("{}: {error}", theme.label)),
            }
        }
        if !resolved.is_empty() {
            self.imported_many(library, &resolved, update, window, cx);
        }
        if !failures.is_empty() {
            self.error = Some(self.error.iter().cloned().chain(failures).collect::<Vec<_>>().join("; "));
        } else if self.error.is_none() {
            window.close_dialog(cx);
        }
        cx.notify();
    }

    /// `handleSubmit`: the pasted or loaded JSON.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let json = self.json.read(cx).value().to_string();
        if let Some(message) = oversized(json.len() as u64) {
            self.error = Some(message);
            cx.notify();
            return;
        }
        let library = self.library(cx);
        match parse_theme_import(&json) {
            Ok(theme) if library.custom_theme(&theme.id).is_some() => {
                (self.error, self.conflicts) = (None, Some(vec![theme]));
            }
            Ok(theme) => match library.install(theme.clone()) {
                Ok(library) => {
                    self.imported(library, &theme, window, cx);
                    window.close_dialog(cx);
                    return;
                }
                Err(error) => self.error = Some(error),
            },
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }
}

impl Render for ThemeImport {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let divider = h_flex()
            .gap(px(12.))
            .child(div().h(px(1.)).flex_1().bg(t.border))
            .child(div().text_size(rems(0.6875)).text_color(t.muted_foreground).child("OR IMPORT A FILE"))
            .child(div().h(px(1.)).flex_1().bg(t.border));
        let body = match &self.conflicts {
            Some(conflicts) => self.render_conflicts(conflicts, &t, cx),
            None => self.render_file_section(&t, cx),
        };
        v_flex()
            .id("theme-import")
            .max_h(window.viewport_size().height - px(160.))
            .overflow_y_scroll()
            .pr(px(4.))
            .gap(px(16.))
            .child(self.render_search(&t, cx))
            .child(divider)
            .child(body)
            .children(self.error.clone().map(|error| alert(error, &t)))
    }
}

fn alert(message: String, t: &Tokens) -> AnyElement {
    div()
        .rounded(px(8.))
        .border_1()
        .border_color(t.error.opacity(0.32))
        .bg(t.error.opacity(0.04))
        .px(px(12.))
        .py(px(10.))
        .text_sm()
        .text_color(t.error_foreground)
        .child(message)
        .into_any_element()
}

impl ThemeImport {
    fn render_search(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.installing.is_some();
        let icon = if self.searching { IconName::LoaderCircle } else { IconName::Search };
        let mut controls = h_flex().flex_wrap().items_center().justify_between().gap(px(8.)).child(
            h_flex()
                .min_w_0()
                .flex_wrap()
                .items_center()
                .gap(px(6.))
                .child(div().text_xs().text_color(t.muted_foreground).child("Popular"))
                .children(SUGGESTED_SEARCHES.map(|suggestion| {
                    Button::new(SharedString::from(format!("theme-suggest-{suggestion}")))
                        .ghost()
                        .xsmall()
                        .label(suggestion)
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, window, cx| this.pick_suggestion(suggestion, window, cx)))
                })),
        );
        if self.results.as_ref().is_some_and(|results| !results.is_empty()) {
            let label = open_vsx::SORTS.iter().find(|(value, _)| *value == self.sort).map_or("", |(_, label)| label);
            let view = cx.entity();
            let current = self.sort;
            controls = controls.child(
                h_flex().gap(px(8.)).child(div().text_xs().text_color(t.muted_foreground).child("Sort")).child(
                    super::widgets::select_trigger(&"theme-sort".into(), label, 160., t)
                        .disabled(busy)
                        .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, window, _| {
                            for (value, label) in open_vsx::SORTS {
                                menu = menu.item(PopupMenuItem::new(*label).checked(current == *value).on_click(
                                    window.listener_for(&view, move |this, _, _, cx| {
                                        this.sort = value;
                                        if this.debounce.is_none() {
                                            this.settle(cx);
                                        }
                                        cx.notify();
                                    }),
                                ));
                            }
                            menu
                        }),
                ),
            );
        }
        let results: Option<AnyElement> = match (&self.results, self.searching) {
            (None, true) => Some(
                h_flex()
                    .min_h(px(80.))
                    .justify_center()
                    .gap(px(8.))
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .child(Icon::new(IconName::LoaderCircle).size(px(16.)))
                    .child("Searching themes...")
                    .into_any_element(),
            ),
            (Some(results), _) if results.is_empty() => Some(
                v_flex()
                    .min_h(px(160.))
                    .items_center()
                    .justify_center()
                    .rounded(px(16.))
                    .border_1()
                    .border_dashed()
                    .border_color(t.border)
                    .child(
                        div().text_sm().font_weight(FontWeight::MEDIUM).child("No supported open-source themes found"),
                    )
                    .child(div().mt(px(4.)).text_xs().text_color(t.muted_foreground).child("Try a broader search."))
                    .into_any_element(),
            ),
            (Some(results), _) => {
                let library = self.library(cx);
                Some(
                    div()
                        .grid()
                        .grid_cols(2)
                        .gap(px(8.))
                        .children(results.iter().map(|extension| self.render_extension(extension, &library, t, cx)))
                        .into_any_element(),
                )
            }
            (None, false) => None,
        };
        v_flex()
            .gap(px(12.))
            .child(
                v_flex().child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Search community themes")).child(
                    div()
                        .mt(px(2.))
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .child("Find open-source themes from Open VSX."),
                ),
            )
            .child(Input::new(&self.query).large().prefix(Icon::new(icon).size(px(16.)).text_color(t.muted_foreground)))
            .when(!self.searching || self.results.is_some(), |el| el.child(controls))
            .children(self.search_error.clone().map(|error| alert(error, t)))
            .children(results)
            .into_any_element()
    }

    fn render_extension(
        &self,
        extension: &Extension,
        library: &Library,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let installing = self.installing.as_deref() == Some(extension.id.as_str());
        let installed = library
            .custom()
            .iter()
            .any(|theme| theme.collection.as_ref().is_some_and(|c| c.id == extension.collection_id));
        let (action, progress) = if installed { ("Update", "Updating") } else { ("Install", "Installing") };
        let icon = match (installing, installed) {
            (true, _) => IconName::LoaderCircle,
            (false, true) => IconName::RefreshCw,
            (false, false) => IconName::PackagePlus,
        };
        let pick = extension.clone();
        let source = extension.source_url.clone();
        v_flex()
            .id(SharedString::from(format!("theme-extension-{}", extension.id)))
            .min_w_0()
            .gap(px(12.))
            .p(px(12.))
            .rounded(px(12.))
            .border_1()
            .border_color(t.border.opacity(0.7))
            .bg(t.card.opacity(0.6))
            .hover(|s| s.bg(t.accent.opacity(0.2)))
            .child(
                h_flex()
                    .min_w_0()
                    .gap(px(12.))
                    .child(
                        div()
                            .size(px(40.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(8.))
                            .bg(t.muted)
                            .text_color(t.muted_foreground)
                            .child(Icon::new(IconName::Palette).size(px(16.))),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(extension.name.clone()),
                            )
                            .child(div().truncate().text_xs().text_color(t.muted_foreground).child(format!(
                                "{} · {} downloads",
                                extension.publisher,
                                compact(extension.download_count)
                            ))),
                    ),
            )
            .child(
                div()
                    .min_h(px(32.))
                    .max_h(px(32.))
                    .overflow_hidden()
                    .text_xs()
                    .line_height(px(16.))
                    .text_color(t.muted_foreground)
                    .child(if extension.description.is_empty() {
                        "A community color theme for your editor.".to_owned()
                    } else {
                        extension.description.clone()
                    }),
            )
            .child(
                h_flex()
                    .mt_auto()
                    .justify_between()
                    .gap(px(8.))
                    .child(div().children(source.map(|url| {
                        Button::new(SharedString::from(format!("theme-source-{}", extension.id)))
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::ExternalLink))
                            .tooltip(format!("View source for {}", extension.name))
                            .on_click(move |_, _, cx| cx.open_url(&url))
                    })))
                    .child(
                        Button::new(SharedString::from(format!("theme-install-{}", extension.id)))
                            .outline()
                            .xsmall()
                            .icon(Icon::new(icon))
                            .label(if installing { format!("{progress}...") } else { action.to_owned() })
                            .disabled(self.installing.is_some())
                            .on_click(
                                cx.listener(move |this, _, window, cx| this.install(pick.clone(), false, window, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_file_section(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let json_empty = self.json.read(cx).value().trim().is_empty();
        let drop_zone = h_flex()
            .id("theme-file-drop")
            .flex_wrap()
            .justify_between()
            .gap(px(12.))
            .px(px(12.))
            .py(px(12.))
            .rounded(px(12.))
            .border_1()
            .border_dashed()
            .map(|el| {
                if self.drop_target {
                    el.border_color(t.ring).bg(t.accent.opacity(0.2))
                } else {
                    el.border_color(t.border.opacity(0.8)).bg(t.muted.opacity(0.2))
                }
            })
            .on_drag_move(cx.listener(|this, _: &gpui_kit::DragMoveEvent<ExternalPaths>, _, cx| {
                if !this.drop_target {
                    this.drop_target = true;
                    cx.notify();
                }
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.read_files(paths.paths().to_vec(), window, cx);
            }))
            .child(
                v_flex().min_w_0().child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Theme file")).child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .child(self.file_name.clone().unwrap_or_else(|| "Drop T3 Code or VS Code .json files".into())),
                ),
            )
            .child(
                Button::new("theme-choose-files")
                    .outline()
                    .small()
                    .icon(Icon::new(IconName::Download))
                    .label(if self.reading { "Reading…" } else { "Choose files" })
                    .disabled(self.reading)
                    .on_click(cx.listener(|this, _, window, cx| this.choose_files(window, cx))),
            );
        v_flex()
            .gap(px(16.))
            .child(drop_zone)
            .child(
                v_flex()
                    .gap(px(8.))
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Theme JSON"))
                    .child(Textarea::new(&self.json).font_family(t.fonts.mono).text_xs()),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        Button::new("theme-import-cancel")
                            .ghost()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("theme-import-add")
                            .primary()
                            .icon(Icon::new(IconName::Plus))
                            .label("Add theme")
                            .disabled(json_empty || self.reading)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_conflicts(&self, conflicts: &[ThemeDefinition], t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let labels: Vec<&str> = conflicts.iter().map(|theme| theme.label.as_str()).collect();
        v_flex()
            .gap(px(12.))
            .child(
                v_flex()
                    .p(px(12.))
                    .rounded(px(12.))
                    .border_1()
                    .border_color(t.border.opacity(0.7))
                    .bg(t.muted.opacity(0.2))
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Already installed"))
                    .child(div().mt(px(4.)).text_xs().text_color(t.muted_foreground).child(labels.join(", "))),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .child(
                        Button::new("theme-conflict-update")
                            .primary()
                            .small()
                            .label("Update existing")
                            .on_click(cx.listener(|this, _, window, cx| this.resolve_conflicts(true, window, cx))),
                    )
                    .child(
                        Button::new("theme-conflict-copy")
                            .outline()
                            .small()
                            .label("Keep both")
                            .on_click(cx.listener(|this, _, window, cx| this.resolve_conflicts(false, window, cx))),
                    )
                    .child(Button::new("theme-conflict-back").ghost().small().label("Back").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.conflicts = None;
                            cx.notify();
                        },
                    )))
                    .child(
                        Button::new("theme-conflict-cancel")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
            )
            .into_any_element()
    }
}

impl Workspace {
    /// "Add theme": a fresh dialog session each time; closing it drops its searches.
    pub(in crate::app) fn open_theme_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = cx.weak_entity();
        let import = cx.new(|cx| ThemeImport::new(workspace, window, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            dialog.entrance(DialogEntrance::None).w(px(768.)).title("Add a theme").child(import.clone())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_files_read_up_to_the_limit() {
        let dir = std::env::temp_dir().join(format!("t3-gpui-theme-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (small, big) = (dir.join("small.json"), dir.join("big.json"));
        std::fs::write(&small, "{}").unwrap();
        std::fs::write(&big, vec![b' '; 300 * 1024]).unwrap();
        assert_eq!(read_theme_file(&small), ("small.json".to_owned(), Ok("{}".to_owned())));
        assert!(read_theme_file(&big).1.unwrap_err().contains("300 KB"));
        assert!(read_theme_file(&dir.join("missing.json")).1.unwrap_err().starts_with("Could not read missing.json"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sizes_and_counts_read_like_the_web() {
        assert_eq!(oversized(1024), None);
        assert_eq!(
            oversized(300 * 1024).unwrap(),
            "That file is 300 KB. Theme files are only a few KB, so this one was not read (limit 256 KB)."
        );
        assert_eq!(byte_size(3 * 1024 * 1024 / 2), "1.5 MB");
        assert_eq!(
            (compact(950.), compact(1234.), compact(45_600_000.), compact(120_400.)),
            ("950".into(), "1.2K".into(), "45.6M".into(), "120K".into())
        );
    }

    #[test]
    fn keep_both_names_the_copy_after_the_file_then_numbers_it() {
        let theme = theme_file::parse_theme_file(&serde_json::json!({
            "version": 1, "name": "Aurora", "appearance": "dark", "colors": {"canvas": "#101014"},
        }))
        .unwrap();
        let library = Library::default().install(theme.clone()).unwrap();
        assert_eq!(versioned_copy(&theme, Some("Aurora Night"), &library).unwrap().label, "Aurora Night");
        assert_eq!(versioned_copy(&theme, None, &library).unwrap().label, "Aurora (1)");
        let library = library.install(versioned_copy(&theme, None, &library).unwrap()).unwrap();
        assert_eq!(versioned_copy(&theme, Some("aurora"), &library).unwrap().label, "Aurora (2)");
    }
}
