//! The Appearance page's Typography section (`TypographySettings.tsx`).

use gpui_kit::assets::IconName;
use gpui_kit::component::diff::{Diff, DiffChangeIndicator, DiffFile, DiffHunkSeparator, DiffState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{Icon, IndexPath, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, Hsla, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px, relative, rems, rgb,
};
use serde_json::{Map, Value, json};

use super::widgets::{SelectWidth, SettingRow, settings_section, switch};
use crate::app::Workspace;
use crate::app::git::{self, DiffColorScheme};
use crate::app::pages::settings::widgets::SettingsView;
use crate::app::right_panel::DiffTint;
use crate::app::terminal::TerminalTheme;
use crate::settings::{TYPOGRAPHY_ADVANCED_KEY, contract};
use crate::theme::{self, FONT_MONO, FONT_SANS_LABEL, Fonts, Tokens};

/// `FontFamilyPicker`'s `sm:w-44`; below `sm` it is `min-w-36 flex-1`.
const FAMILY_PICKER_WIDTH: f32 = 176.;
/// The size select's `w-22`, held to the trigger's own `min-w-36`.
const SIZE_SELECT_WIDTH: f32 = 144.;
/// Beside the title, `SettingsRow` widens every select trigger to `min-w-40`.
const SIZE_SELECT_BESIDE_WIDTH: f32 = 160.;

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
        let t = theme::tokens(cx);
        let muted = t.muted_foreground.opacity(0.6);
        let bp = t.breakpoint;
        // `ComboboxItem`: `min-h-8 sm:min-h-7` with `py-1`; the list row adds 2px a side, and a
        // fixed line keeps every family's row that tall whatever its metrics.
        bp.control_text(h_flex())
            .w_full()
            .min_w_0()
            .h(px(bp.pick(28., 24.)))
            .line_height(px(bp.pick(24., 20.)))
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

impl Workspace {
    /// `TypographySection`: one interface and one monospace font, or all four set separately.
    pub(super) fn render_typography(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let separate = self.client_settings.values.bool(TYPOGRAPHY_ADVANCED_KEY);
        let interface_family: SharedString = match view.str("fontFamilySans").trim() {
            "" => FONT_SANS_LABEL.into(),
            family => family.to_owned().into(),
        };
        let interface = |previews| FontRow {
            id: "interface-font",
            title: None,
            description: "Everything outside code blocks and the terminal.",
            family_key: "fontFamilySans",
            size_key: "fontSizeInterface",
            sizes: (12, 20),
            default_family: FONT_SANS_LABEL.into(),
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
        let mut items: Vec<AnyElement> = rows.into_iter().map(|row| self.font_row(view, row, window, cx)).collect();
        items.push(
            self.setting_row(
                view,
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
            view,
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
        let sizes: Vec<_> =
            (row.sizes.0..=row.sizes.1).map(|px| (px, SharedString::from(format!("{px} px")), px == size)).collect();
        let mut size_select = |width| {
            self.row_menu_select(
                format!("{}-size", row.id),
                sizes.clone(),
                format!("{size} px"),
                SelectWidth::Fixed(width),
                window,
                cx,
                move |this, px, window, cx| this.update_settings(json!({ size_key: px }), window, cx),
            )
        };
        let (stacked_size, beside_size) = (size_select(SIZE_SELECT_WIDTH), size_select(SIZE_SELECT_BESIDE_WIDTH));
        let below_sm = window.viewport_size().width < px(640.);
        // `min-w-36 flex-1 sm:w-44 sm:flex-none` on the picker's wrapper.
        let family_picker = |stacked: bool| {
            // `SelectButton size="sm"`; the popup is `w-72`, or the trigger's width when the
            // stacked trigger is the wider.
            let select = t
                .breakpoint
                .control_text(Select::new(&picker).small())
                .w_full()
                .h(px(t.breakpoint.pick(32., 28.)))
                .px(px(9.))
                .when(!(stacked && below_sm), |select| select.menu_width(px(288.)))
                .placeholder(row.default_family.clone())
                .search_placeholder("Search fonts…");
            div()
                .map(|el| if stacked && below_sm { el.flex_1().min_w(px(144.)) } else { el.w(px(FAMILY_PICKER_WIDTH)) })
                .child(select)
        };
        // `flex w-full flex-wrap sm:w-auto @min-[32rem]/settings-row:flex-nowrap`
        let control = h_flex().w_full().flex_wrap().gap(px(8.)).child(family_picker(true)).child(stacked_size);
        let control_beside = h_flex().gap(px(8.)).child(family_picker(false)).child(beside_size);
        let reset = (!family.is_empty() || size != size_default)
            .then(|| json!({ row.family_key: "", row.size_key: size_default }));
        let fonts = t.fonts;
        let diff = self.settings_ui.diff_preview.get_or_insert_with(|| diff_preview_state(cx)).clone();
        let scheme = DiffColorScheme::from_settings(&self.client_settings.values);
        let previews =
            v_flex().children(row.previews.iter().map(|preview| render_preview(*preview, &fonts, &t, &diff, scheme)));
        let setting = match row.title {
            Some(title) => SettingRow::titled(row.id, title),
            None => SettingRow::new(row.id),
        };
        let setting = setting
            .description(row.description)
            .setting(row.family_key)
            .setting(row.size_key)
            .control(control)
            .control_beside(control_beside)
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
fn render_preview(
    preview: Preview,
    fonts: &Fonts,
    t: &Tokens,
    diff: &Entity<DiffState>,
    scheme: DiffColorScheme,
) -> AnyElement {
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
            .child(prompt_preview(t))
            .into_any_element(),
        Preview::Code => {
            // `render_diff` sizes rows from the rem and the code font; Pierre pads the code 8px
            // below its last row.
            let row = fonts.interface_size * fonts.code_size / 16. * 1.6;
            // `--code-background`: `color-mix(in srgb, var(--card) 90%, var(--background))`.
            let code_background = t.background.blend(t.card.opacity(0.9));
            let preview = Diff::new(diff)
                .header_visible(false)
                .border_0()
                .bg(code_background)
                .hunk_separator(DiffHunkSeparator::LineInfo)
                .change_indicator(DiffChangeIndicator::Bars)
                .h(px(8. + row * DIFF_PREVIEW_ROWS))
                .text_size(px(fonts.code_size))
                .into_any_element();
            let preview = match scheme {
                DiffColorScheme::RedGreen => preview,
                DiffColorScheme::BlueOrange => DiffTint {
                    child: preview,
                    colors: (git::addition_color(t, scheme), git::deletion_color(t, scheme)),
                }
                .into_any_element(),
            };
            // Pierre's static HTML: no frame, its own header and no fold control.
            v_flex()
                .mt(px(4.))
                .mb(px(8.))
                .overflow_hidden()
                .bg(code_background)
                .child(diff_preview_header(fonts, t, scheme))
                .child(preview)
                .into_any_element()
        }
        Preview::Terminal => terminal_preview(fonts, t),
    }
}

/// Pierre's default file header for the preview patch (`createFileHeaderElement`): the
/// change icon, the path and `-1 +1`, in the shadow root's own colors. Those are the syntax
/// theme's `gitDecoration` colors (pierre-dark or pierre-light), and the app's diff colors once
/// blue-orange overrides them.
fn diff_preview_header(fonts: &Fonts, t: &Tokens, scheme: DiffColorScheme) -> impl IntoElement {
    let (modified, addition, deletion) =
        if t.dark { (0x009fff, 0x07c480, 0xff2e3f) } else { (0x009fff, 0x18a46c, 0xd52c36) };
    let (mut addition, mut deletion): (Hsla, Hsla) = (rgb(addition).into(), rgb(deletion).into());
    if scheme == DiffColorScheme::BlueOrange {
        (addition, deletion) = (git::addition_color(t, scheme), git::deletion_color(t, scheme));
    }
    // `min-height: calc(1lh + 3 * 8px)` at Pierre's 20px line height, `padding-inline: 16px`.
    h_flex()
        .min_h(px(44.))
        .px(px(16.))
        .gap(px(8.))
        .justify_between()
        .font_family(fonts.sans)
        .text_size(px(fonts.code_size))
        .line_height(px(20.))
        .text_color(t.foreground)
        .child(
            h_flex()
                .min_w_0()
                .gap(px(8.))
                .child(
                    gpui_kit::svg()
                        .path("icons/app/diffs/symbol-modified.svg")
                        .size(px(16.))
                        .flex_none()
                        .text_color(rgb(modified)),
                )
                .child(div().min_w_0().truncate().child("src/formatUser.ts")),
        )
        .child(
            h_flex()
                .flex_none()
                .gap(px(fonts.code_size * 0.6))
                .font_family(fonts.mono)
                .child(div().text_color(deletion).child("-1"))
                .child(div().text_color(addition).child("+1")),
        )
}

/// `DIFF_PREVIEW_PATCH`: one file, one hunk.
const DIFF_PREVIEW_PATCH: &str = "diff --git a/src/formatUser.ts b/src/formatUser.ts
--- a/src/formatUser.ts
+++ b/src/formatUser.ts
@@ -1,3 +1,3 @@
 export function formatUser(user: User) {
-  return user.name.toUpperCase();
+  return `${user.name} <${user.email}>`; // 0O 1lI
 }
";

/// The patch's rows in the unified layout.
const DIFF_PREVIEW_ROWS: f32 = 4.;

/// The code preview's diff, parsed once.
pub(super) fn diff_preview_state(cx: &mut App) -> Entity<DiffState> {
    let files = DiffFile::parse(DIFF_PREVIEW_PATCH).unwrap_or_default();
    cx.new(|cx| DiffState::new(files, cx))
}

/// One piece of `PROMPT_PREVIEW_TEXT` as the composer shows it.
enum PromptPiece {
    Text(&'static str),
    Skill(&'static str),
    File(&'static str),
}

/// `PROMPT_PREVIEW_TEXT`, with its `$frontend-design` skill and two Markdown file links.
const PROMPT_PREVIEW: &[PromptPiece] = &[
    PromptPiece::Text("Use "),
    PromptPiece::Skill("frontend-design"),
    PromptPiece::Text(" to fix the flaky test in "),
    PromptPiece::File("apps/web/src/terminal/ghostty/surface.test.ts"),
    PromptPiece::Text(" and align the header with "),
    PromptPiece::File("apps/web/src/components/settings/SettingsPanels.tsx"),
    PromptPiece::Text(" before shipping."),
];

/// The prompt, wrapping word by word, with the composer's chips (`ContextChip`: a 1em icon,
/// a 0.2em gap and a medium label in the kind's accent).
fn prompt_preview(t: &Tokens) -> impl IntoElement {
    let chip = |icon: Icon, accent: Hsla, label: &'static str| {
        h_flex()
            .gap(rems(0.2))
            .font_weight(FontWeight::MEDIUM)
            .text_color(accent)
            .child(icon.size(rems(1.)).flex_none())
            .child(label)
            .into_any_element()
    };
    let mut words: Vec<AnyElement> = Vec::new();
    for piece in PROMPT_PREVIEW {
        match piece {
            PromptPiece::Text(text) => {
                words.extend(text.split_inclusive(' ').map(|word| div().child(word).into_any_element()));
            }
            PromptPiece::Skill(name) => words.push(chip(Icon::new(IconName::Box), rgb(0xb261be).into(), name)),
            PromptPiece::File(path) => {
                let (asset, color) = crate::file_icons::file_icon(path, t.dark);
                let name = path.rsplit('/').next().unwrap_or(path);
                words.push(chip(Icon::empty().path(asset).text_color(color), rgb(0x0096af).into(), name));
            }
        }
    }
    h_flex().flex_wrap().items_center().whitespace_nowrap().children(words)
}

/// A styled stretch of the terminal transcript (its SGR attributes).
#[derive(Clone, Copy)]
struct Run {
    text: &'static str,
    color: Option<Hsla>,
    bold: bool,
    underline: bool,
    background: Option<Hsla>,
}

const fn plain(text: &'static str) -> Run {
    Run { text, color: None, bold: false, underline: false, background: None }
}

const fn tint(text: &'static str, color: Hsla) -> Run {
    Run { color: Some(color), ..plain(text) }
}

const fn bold(text: &'static str, color: Option<Hsla>) -> Run {
    Run { bold: true, color, ..plain(text) }
}

/// `TERMINAL_PREVIEW_TRANSCRIPT` in the terminal's palette, ending at a block cursor.
fn terminal_preview(fonts: &Fonts, t: &Tokens) -> AnyElement {
    let theme = TerminalTheme::new(t);
    let ansi = |index: usize| theme.palette[index];
    let (red, green, yellow, blue, cyan) = (ansi(1), ansi(2), ansi(3), ansi(4), ansi(6));
    // SGR 2, as the terminal grid dims its foreground.
    let fg = theme.foreground;
    let faint = gpui_kit::hsla(fg.h, fg.s, fg.l * 0.66, fg.a);
    let prompt = [
        bold("→", Some(green)),
        plain(" "),
        bold("t3code", Some(cyan)),
        plain(" "),
        bold("git:(", Some(blue)),
        bold("main", Some(red)),
        bold(")", Some(blue)),
        plain(" "),
        bold("✗", Some(yellow)),
        plain(" "),
    ];
    let link = |text| Run { underline: true, ..tint(text, cyan) };
    let lines: Vec<Vec<Run>> = vec![
        prompt.iter().copied().chain([plain("vpr dev")]).collect(),
        vec![],
        vec![
            plain("  "),
            bold("VITE", Some(green)),
            plain(" "),
            tint("v7.1.1", green),
            plain("  "),
            tint("ready in", faint),
            plain(" "),
            bold("1.24s", None),
        ],
        vec![],
        vec![
            plain("  "),
            tint("→", green),
            plain("  "),
            tint("Local:", faint),
            plain("    "),
            link("http://127.0.0.1:5173/"),
        ],
        vec![
            plain("  "),
            tint("→", green),
            plain("  "),
            tint("Network:", faint),
            plain("  "),
            link("http://192.168.1.24:5173/"),
        ],
        vec![],
        vec![
            plain("  "),
            tint("✓ 85 passed", green),
            plain("   "),
            tint("△ 2 warnings", yellow),
            plain("   "),
            tint("✗ 0 failed", red),
        ],
        vec![],
        vec![
            plain("  "),
            Run { background: Some(green), ..tint(" READY ", ansi(0)) },
            plain(" "),
            tint("watching for changes, press", faint),
            plain(" "),
            bold("q", None),
            plain(" "),
            tint("to quit", faint),
        ],
        vec![],
        prompt.iter().copied().chain([Run { background: Some(theme.cursor), ..plain(" ") }]).collect(),
    ];
    let line_height = px((fonts.terminal_size * 1.35).round());
    div()
        .mt(px(4.))
        .mb(px(8.))
        .rounded(px(8.))
        .border_1()
        .border_color(t.border)
        .overflow_hidden()
        // `CONTENT_PADDING`.
        .p(px(4.))
        .bg(theme.background)
        .font_family(fonts.terminal)
        .text_size(px(fonts.terminal_size))
        .line_height(line_height)
        .text_color(fg)
        .children(lines.into_iter().map(|runs| {
            h_flex().whitespace_nowrap().min_h(line_height).children(runs.into_iter().map(|run| {
                div()
                    .when_some(run.color, gpui_kit::Styled::text_color)
                    .when_some(run.background, gpui_kit::Styled::bg)
                    .when(run.bold, |el| el.font_weight(FontWeight::BOLD))
                    .when(run.underline, gpui_kit::Styled::underline)
                    .child(run.text)
            }))
        }))
        .into_any_element()
}
