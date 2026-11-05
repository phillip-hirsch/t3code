//! The Appearance page's Typography section (`TypographySettings.tsx`).

use gpui_kit::component::notification::Notification;
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{IndexPath, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Hsla, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px, relative,
};
use serde_json::{Map, Value, json};

use super::widgets::{SettingRow, settings_section, switch};
use crate::app::Workspace;
use crate::settings::{TYPOGRAPHY_ADVANCED_KEY, contract};
use crate::theme::{self, FONT_MONO, FONT_SANS, Fonts, Tokens};

/// A font row's family picker, rebuilt when its default family changes.
pub(super) struct FontPicker {
    state: Entity<SelectState<SearchableVec<FontItem>>>,
    default_family: SharedString,
    _subscription: Subscription,
}

/// One entry of `FontFamilyPicker`: an installed family, or the default (stored as "").
#[derive(Clone)]
pub(super) struct FontItem {
    family: SharedString,
    label: SharedString,
}

impl SelectItem for FontItem {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &SharedString {
        &self.family
    }

    /// The default entry is listed only while nothing is searched.
    fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        if self.family.is_empty() { query.is_empty() } else { self.label.to_lowercase().contains(&query) }
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = theme::tokens(cx).muted_foreground.opacity(0.6);
        h_flex()
            .w_full()
            .min_w_0()
            .justify_between()
            .gap(px(8.))
            .child(div().min_w_0().truncate().font_family(self.label.clone()).child(self.label.clone()))
            .when(self.family.is_empty(), |el| {
                el.child(div().flex_none().text_size(px(10.)).text_color(muted).child("default"))
            })
    }
}

#[derive(Clone, Copy)]
enum Preview {
    Prompt,
    Code,
    Terminal,
}

/// `FontFamilySettingsRow`'s props.
struct FontRow {
    id: &'static str,
    title: Option<&'static str>,
    description: &'static str,
    family_key: &'static str,
    size_key: &'static str,
    /// `MIN_*_FONT_SIZE` and `MAX_*_FONT_SIZE`.
    sizes: (i64, i64),
    default_family: SharedString,
    monospace: bool,
    previews: &'static [Preview],
}

const PROMPT_PREVIEW_TEXT: &str = "Use $frontend-design to fix the flaky test in surface.test.ts and align the header \
     with SettingsPanels.tsx before shipping.";

impl Workspace {
    /// `TypographySection`: one interface and one monospace font, or all four set separately.
    pub(super) fn render_typography(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let view = self.settings_view();
        let separate = self.client_settings.values.bool(TYPOGRAPHY_ADVANCED_KEY);
        let interface_family: SharedString = match view.str("fontFamilySans").trim() {
            "" => FONT_SANS.into(),
            family => family.to_owned().into(),
        };
        let interface = |previews| FontRow {
            id: "interface-font",
            title: None,
            description: "Everything outside code blocks and the terminal.",
            family_key: "fontFamilySans",
            size_key: "fontSizeInterface",
            sizes: (12, 20),
            default_family: FONT_SANS.into(),
            monospace: false,
            previews,
        };
        let code = |title, description, previews| FontRow {
            id: "code-font",
            title,
            description,
            family_key: "fontFamilyCode",
            size_key: "fontSizeCode",
            sizes: (10, 18),
            default_family: FONT_MONO.into(),
            monospace: true,
            previews,
        };
        let rows = if separate {
            vec![
                interface(&[]),
                FontRow {
                    id: "prompt-font",
                    title: None,
                    description: "Only the box you write prompts in. Mono works well here.",
                    family_key: "fontFamilyComposer",
                    size_key: "fontSizePrompt",
                    sizes: (12, 20),
                    default_family: interface_family,
                    monospace: false,
                    previews: &[Preview::Prompt],
                },
                code(None, "Code blocks, diffs, and file previews.", &[Preview::Code]),
                FontRow {
                    id: "terminal-font",
                    title: None,
                    description: "Terminal output, independent from code blocks and diffs.",
                    family_key: "fontFamilyTerminal",
                    size_key: "fontSizeTerminal",
                    sizes: (8, 20),
                    default_family: FONT_MONO.into(),
                    monospace: true,
                    previews: &[Preview::Terminal],
                },
            ]
        } else {
            vec![
                interface(&[Preview::Prompt]),
                code(
                    Some("Monospace font"),
                    "Code blocks, diffs, file previews, and the terminal.",
                    &[Preview::Code, Preview::Terminal],
                ),
            ]
        };
        let mut items: Vec<AnyElement> = rows.into_iter().map(|row| self.font_row(&view, row, window, cx)).collect();
        items.push(
            self.setting_row(
                &view,
                SettingRow::new("word-wrap")
                    .description("Wrap long lines in code blocks, diffs, and file previews by default.")
                    .setting("wordWrap")
                    .switch(),
                window,
                cx,
            ),
        );
        let separate_switch =
            switch(&"separate-fonts".into(), separate, false, &t).on_click(cx.listener(move |this, _, window, cx| {
                let patch = Map::from_iter([(TYPOGRAPHY_ADVANCED_KEY.to_owned(), Value::Bool(!separate))]);
                this.client_settings.apply(&patch);
                theme::set_appearance(theme::Appearance::from_settings(&this.client_settings.values), window, cx);
                cx.notify();
            }));
        // `FontSmoothingRow` is macOS-only, and GPUI has no grayscale smoothing to switch to.
        let advanced = [self.setting_row(
            &view,
            SettingRow::new("separate-fonts")
                .description("Set the prompt, code, and terminal fonts separately instead of one monospace font.")
                .control(separate_switch),
            window,
            cx,
        )];
        items.push(self.advanced_fold("typography", advanced, cx));
        settings_section("typography", "Typography", items, &t)
    }

    fn font_row(
        &mut self,
        view: &super::widgets::SettingsView,
        row: FontRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let family = view.str(row.family_key).trim().to_owned();
        let size_default = contract().default_value(row.size_key).as_i64().unwrap_or(row.sizes.0);
        let size = view.value(row.size_key).as_i64().unwrap_or(size_default);
        let picker = self.font_picker(&row, &family, window, cx);
        let size_key = row.size_key;
        let sizes =
            (row.sizes.0..=row.sizes.1).map(|px| (px, SharedString::from(format!("{px} px")), px == size)).collect();
        let size_select = self.menu_select(
            format!("{}-size", row.id),
            sizes,
            format!("{size} px"),
            88.,
            cx,
            move |this, px, window, cx| this.update_settings(json!({ size_key: px }), window, cx),
        );
        let control = h_flex()
            .gap(px(8.))
            .child(
                Select::new(&picker)
                    .small()
                    .w(px(176.))
                    .menu_width(px(288.))
                    .placeholder(row.default_family.clone())
                    .search_placeholder("Search fonts…"),
            )
            .child(size_select);
        let reset = (!family.is_empty() || size != size_default)
            .then(|| json!({ row.family_key: "", row.size_key: size_default }));
        let fonts = t.fonts;
        let previews = v_flex().children(row.previews.iter().map(|preview| render_preview(*preview, &fonts, &t)));
        let setting = match row.title {
            Some(title) => SettingRow::titled(row.id, title),
            None => SettingRow::new(row.id),
        };
        let setting = setting
            .description(row.description)
            .setting(row.family_key)
            .setting(row.size_key)
            .control(control)
            .reset(reset);
        let setting = if row.previews.is_empty() { setting } else { setting.body(previews) };
        self.setting_row(view, setting, window, cx)
    }

    /// `FontFamilyPicker` over the installed families, kept on the saved value.
    fn font_picker(
        &mut self,
        row: &FontRow,
        family: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<SelectState<SearchableVec<FontItem>>> {
        let selected = SharedString::from(family.to_owned());
        if let Some(picker) = self.settings_ui.font_pickers.get(row.family_key)
            && picker.default_family == row.default_family
        {
            let state = picker.state.clone();
            if state.read(cx).selected_value() != Some(&selected) {
                state.update(cx, |state, cx| state.set_selected_value(&selected, window, cx));
            }
            return state;
        }
        let mut items = vec![FontItem { family: SharedString::default(), label: row.default_family.clone() }];
        items.extend(
            theme::installed_fonts(cx).iter().map(|family| FontItem { family: family.clone(), label: family.clone() }),
        );
        let index = items.iter().position(|item| item.family == selected).map(IndexPath::new);
        let state = cx.new(|cx| SelectState::new(SearchableVec::new(items), index, window, cx).searchable(true));
        let (key, monospace) = (row.family_key, row.monospace);
        let subscription = cx.subscribe_in(&state, window, move |this, state, event: &SelectEvent<_>, window, cx| {
            let SelectEvent::Confirm(Some(family)) = event else { return };
            if monospace && !family.is_empty() && !theme::is_monospace(family, cx) {
                let saved = SharedString::from(this.settings_view().str(key).trim().to_owned());
                state.update(cx, |state, cx| state.set_selected_value(&saved, window, cx));
                let notice =
                    Notification::error("Code and terminal need a fixed-width font, so the current font was kept.")
                        .title(format!("\"{family}\" isn't monospace"));
                window.push_notification(notice, cx);
                return;
            }
            this.update_settings(json!({ key: family.as_ref() }), window, cx);
        });
        let picker = FontPicker {
            state: state.clone(),
            default_family: row.default_family.clone(),
            _subscription: subscription,
        };
        self.settings_ui.font_pickers.insert(row.family_key, picker);
        state
    }
}

/// `PromptFontPreview`, `CodeFontPreview` and `TerminalFontPreview`, as static samples.
fn render_preview(preview: Preview, fonts: &Fonts, t: &Tokens) -> AnyElement {
    let frame = || div().mt(px(4.)).mb(px(8.)).rounded(px(8.)).border_1().border_color(t.border).overflow_hidden();
    match preview {
        Preview::Prompt => frame()
            .bg(t.background)
            .px(px(12.))
            .py(px(8.))
            .font_family(fonts.composer)
            .text_size(px(fonts.prompt_size))
            .line_height(relative(1.5))
            .text_color(t.foreground)
            .child(PROMPT_PREVIEW_TEXT)
            .into_any_element(),
        Preview::Code => {
            let line = |sign: char, text: &'static str| {
                let tint: Option<Hsla> = match sign {
                    '+' => Some(t.success_foreground.opacity(0.12)),
                    '-' => Some(t.error_foreground.opacity(0.12)),
                    _ => None,
                };
                h_flex()
                    .px(px(12.))
                    .gap(px(12.))
                    .whitespace_nowrap()
                    .when_some(tint, |el, tint| el.bg(tint))
                    .child(div().w(px(8.)).text_color(t.muted_foreground).child(sign.to_string()))
                    .child(text)
            };
            frame()
                .py(px(6.))
                .bg(if t.dark { t.input.opacity(0.32) } else { t.secondary })
                .font_family(fonts.mono)
                .text_size(px(fonts.code_size))
                .line_height(relative(1.6))
                .text_color(t.foreground)
                .child(div().px(px(12.)).pb(px(4.)).text_color(t.muted_foreground).child("src/formatUser.ts"))
                .child(line(' ', "export function formatUser(user: User) {"))
                .child(line('-', "  return user.name.toUpperCase();"))
                .child(line('+', "  return `${user.name} <${user.email}>`; // 0O 1lI"))
                .child(line(' ', "}"))
                .into_any_element()
        }
        Preview::Terminal => {
            let (green, cyan, blue, red, yellow, dim) = (
                t.success_foreground,
                t.info_foreground,
                t.indigo_foreground,
                t.error_foreground,
                t.warning_foreground,
                t.muted_foreground,
            );
            let prompt: [(&'static str, Option<Hsla>); 7] = [
                ("→ ", Some(green)),
                ("t3code ", Some(cyan)),
                ("git:(", Some(blue)),
                ("main", Some(red)),
                (") ", Some(blue)),
                ("✗ ", Some(yellow)),
                ("", None),
            ];
            let lines: Vec<Vec<(&'static str, Option<Hsla>)>> = vec![
                prompt.iter().copied().chain([("vpr dev", None)]).collect(),
                vec![],
                vec![("  VITE ", Some(green)), ("v7.1.1  ", Some(green)), ("ready in ", Some(dim)), ("1.24s", None)],
                vec![],
                vec![("  →  ", Some(green)), ("Local:    ", Some(dim)), ("http://127.0.0.1:5173/", Some(cyan))],
                vec![("  →  ", Some(green)), ("Network:  ", Some(dim)), ("http://192.168.1.24:5173/", Some(cyan))],
                vec![],
                vec![("  ✓ 85 passed   ", Some(green)), ("△ 2 warnings   ", Some(yellow)), ("✗ 0 failed", Some(red))],
                vec![],
                vec![
                    ("  READY ", Some(green)),
                    ("watching for changes, press ", Some(dim)),
                    ("q", None),
                    (" to quit", Some(dim)),
                ],
                vec![],
                prompt.to_vec(),
            ];
            frame()
                .p(px(4.))
                .bg(if t.dark { t.background } else { t.card })
                .font_family(fonts.terminal)
                .text_size(px(fonts.terminal_size))
                .line_height(px((fonts.terminal_size * 1.35).round()))
                .text_color(t.foreground)
                .children(lines.into_iter().map(|segments| {
                    h_flex().whitespace_nowrap().min_h(px((fonts.terminal_size * 1.35).round())).children(
                        segments
                            .into_iter()
                            .map(|(text, color)| div().when_some(color, |el, c| el.text_color(c)).child(text)),
                    )
                }))
                .into_any_element()
        }
    }
}
