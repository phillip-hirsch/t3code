//! `ProjectIconPickerDialog`: a Lucide icon, an emoji, or a monogram, with a color.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use serde_json::{Value, json};

use super::icon;
use crate::theme;

const DEFAULT_ICON: &str = "folder-code";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Lucide,
    Emoji,
    Monogram,
}

pub(super) type OnSelect = Rc<dyn Fn(Value, &mut Window, &mut App)>;

pub(super) struct IconPicker {
    mode: Mode,
    icon_name: String,
    color: &'static str,
    emoji: String,
    automatic_monogram: String,
    query: Entity<InputState>,
    letters: Entity<InputState>,
    custom_emoji: Entity<InputState>,
    grid_scroll: ScrollHandle,
    on_select: OnSelect,
    _subscriptions: Vec<Subscription>,
}

impl IconPicker {
    /// The draft starts from `current` each time the dialog opens.
    pub(super) fn new(
        current: Option<&Value>,
        project_name: &str,
        on_select: OnSelect,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let current = current.filter(|icon| !icon.is_null());
        let field = |key: &str| current.and_then(|icon| icon[key].as_str()).map(str::to_owned);
        let kind = field("kind");
        let (automatic_monogram, automatic_color) = icon::automatic_identity(project_name);
        let mode = match kind.as_deref() {
            Some("emoji") => Mode::Emoji,
            Some("monogram") => Mode::Monogram,
            _ => Mode::Lucide,
        };
        let color = match kind.as_deref() {
            Some("lucide" | "monogram") => field("color"),
            _ => None,
        };
        let color = color.and_then(|c| icon::color_names().find(|n| *n == c)).unwrap_or(automatic_color);
        let letters_value = if mode == Mode::Monogram { field("text") } else { None };
        let letters_value = letters_value.unwrap_or_else(|| automatic_monogram.clone());
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search all Lucide icons"));
        let letters = cx.new(|cx| InputState::new(window, cx).default_value(letters_value));
        let custom_emoji = cx.new(|cx| InputState::new(window, cx).placeholder("Paste an emoji"));
        let notify =
            |this: &mut Self, _: &Entity<InputState>, event: &InputEvent, _: &mut Window, cx: &mut Context<Self>| {
                if matches!(event, InputEvent::Change) {
                    this.grid_scroll.set_offset(Default::default());
                    cx.notify();
                }
            };
        let subscriptions = vec![
            cx.subscribe_in(&query, window, notify),
            cx.subscribe_in(&letters, window, notify),
            cx.subscribe_in(&custom_emoji, window, |this, state, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    if let Some(emoji) = icon::first_emoji(&state.read(cx).value()) {
                        this.emoji = emoji;
                    }
                    cx.notify();
                }
            }),
        ];
        Self {
            mode,
            icon_name: if kind.as_deref() == Some("lucide") { field("name") } else { None }
                .unwrap_or_else(|| DEFAULT_ICON.into()),
            color,
            emoji: if mode == Mode::Emoji { field("emoji") } else { None }.unwrap_or_else(|| "💻".into()),
            automatic_monogram,
            query,
            letters,
            custom_emoji,
            grid_scroll: ScrollHandle::new(),
            on_select,
            _subscriptions: subscriptions,
        }
    }

    fn monogram(&self, cx: &App) -> String {
        self.letters.read(cx).value().trim().to_uppercase()
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let monogram = self.monogram(cx);
        let selection = match self.mode {
            Mode::Monogram if !icon::is_monogram_text(&monogram) => return,
            Mode::Monogram => json!({"kind": "monogram", "text": monogram, "color": self.color}),
            Mode::Lucide => json!({"kind": "lucide", "name": self.icon_name, "color": self.color}),
            Mode::Emoji => json!({"kind": "emoji", "emoji": self.emoji}),
        };
        window.close_dialog(cx);
        (self.on_select)(selection, window, cx);
    }

    fn segment(&self, mode: Mode, label: &'static str, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = *theme::tokens(cx);
        let selected = self.mode == mode;
        div()
            .id(label)
            .flex_1()
            .h(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .cursor_pointer()
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .text_color(if selected { tokens.foreground } else { tokens.muted_foreground })
            .when(selected, |el| el.bg(tokens.background).shadow_xs())
            .when(!selected, |el| el.hover(|s| s.text_color(tokens.foreground)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.mode = mode;
                cx.notify();
            }))
            .child(label)
    }

    fn colors(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = *theme::tokens(cx);
        v_flex()
            .child(
                div()
                    .mb_2()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(tokens.muted_foreground)
                    .child("Color"),
            )
            .child(h_flex().flex_wrap().gap(px(6.)).children(icon::color_names().map(|name| {
                let selected = self.color == name;
                div()
                    .id(SharedString::from(format!("icon-color-{name}")))
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(if selected {
                        tokens.foreground.opacity(0.64)
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .cursor_pointer()
                    .tooltip(super::tooltip(icon::capitalized(name)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.color = name;
                        cx.notify();
                    }))
                    .child(div().size(px(16.)).rounded_full().bg(icon::swatch_color(name)))
            })))
    }

    /// A tile in the 10-column icon or emoji grid (`aspect-square`, `rounded-md`).
    fn tile(id: SharedString, selected: bool, tokens: &theme::Tokens) -> gpui_kit::Stateful<gpui_kit::Div> {
        div()
            .id(id)
            .h(px(44.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .border_1()
            .border_color(if selected { tokens.border } else { gpui_kit::transparent_black() })
            .when(selected, |el| el.bg(tokens.accent))
            .hover(|s| s.bg(tokens.accent))
            .cursor_pointer()
    }

    fn grid(&self, tiles: Vec<gpui_kit::AnyElement>) -> impl IntoElement + use<> {
        // `grid-cols-10 gap-1` inside `max-h-64`.
        let rows: Vec<_> = {
            let mut tiles = tiles.into_iter().peekable();
            let mut rows = Vec::new();
            while tiles.peek().is_some() {
                let row: Vec<_> = tiles.by_ref().take(10).collect();
                let pad = 10 - row.len();
                rows.push(
                    h_flex()
                        .gap_1()
                        .children(row.into_iter().map(|tile| div().flex_1().min_w_0().child(tile)))
                        .children((0..pad).map(|_| div().flex_1())),
                );
            }
            rows
        };
        div()
            .id("icon-grid")
            .max_h(px(256.))
            .overflow_y_scroll()
            .track_scroll(&self.grid_scroll)
            .child(v_flex().gap_1().p(px(2.)).children(rows))
    }

    fn lucide_body(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = *theme::tokens(cx);
        let names = icon::filter_icon_names(&self.query.read(cx).value());
        let color = icon::icon_color(self.color, &tokens);
        let tiles = names
            .iter()
            .map(|name| {
                let name: &'static str = name;
                Self::tile(SharedString::from(format!("icon-{name}")), self.icon_name == name, &tokens)
                    .text_color(color)
                    .tooltip(super::tooltip(icon::icon_label(name)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.icon_name = name.to_owned();
                        cx.notify();
                    }))
                    .children(icon::lucide_icon(name).map(|icon| Icon::new(icon).size(px(20.))))
                    .into_any_element()
            })
            .collect();
        v_flex().gap_4().child(Input::new(&self.query).small()).child(self.grid(tiles)).when(names.is_empty(), |el| {
            el.child(div().py_8().text_center().text_sm().text_color(tokens.muted_foreground).child("No icons found."))
        })
    }

    fn monogram_body(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = *theme::tokens(cx);
        let monogram = self.monogram(cx);
        let preview = if icon::is_monogram_text(&monogram) { monogram } else { self.automatic_monogram.clone() };
        h_flex().gap_4().py_2().child(icon::monogram(&preview, self.color, 48., &tokens)).child(
            v_flex()
                .flex_1()
                .gap_2()
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Letters"))
                .child(Input::new(&self.letters).small())
                .child(div().text_xs().text_color(tokens.muted_foreground).child("One or two letters or numbers.")),
        )
    }

    fn emoji_body(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = *theme::tokens(cx);
        let tiles = icon::EMOJIS
            .iter()
            .map(|(emoji, label)| {
                let emoji: &'static str = emoji;
                Self::tile(SharedString::from(format!("emoji-{label}")), self.emoji == emoji, &tokens)
                    .text_xl()
                    .tooltip(super::tooltip(*label))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.emoji = emoji.to_owned();
                        cx.notify();
                    }))
                    .child(emoji)
                    .into_any_element()
            })
            .collect();
        v_flex().gap_4().child(self.grid(tiles)).child(
            v_flex()
                .child(
                    div()
                        .mb_2()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(tokens.muted_foreground)
                        .child("Or paste any emoji"),
                )
                .child(Input::new(&self.custom_emoji).small()),
        )
    }
}

impl Render for IconPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *theme::tokens(cx);
        let invalid = self.mode == Mode::Monogram && !icon::is_monogram_text(&self.monogram(cx));
        v_flex()
            .gap_4()
            .child(div().text_sm().text_color(tokens.muted_foreground).child("Choose an icon, emoji, or monogram."))
            .child(
                h_flex()
                    .p(px(2.))
                    .gap(px(2.))
                    .rounded(px(8.))
                    .bg(cx.theme().muted)
                    .child(self.segment(Mode::Lucide, "Icons", cx))
                    .child(self.segment(Mode::Emoji, "Emoji", cx))
                    .child(self.segment(Mode::Monogram, "Monogram", cx)),
            )
            .when(self.mode != Mode::Emoji, |el| el.child(self.colors(cx)))
            .map(|el| match self.mode {
                Mode::Lucide => el.child(self.lucide_body(cx)),
                Mode::Monogram => el.child(self.monogram_body(cx)),
                Mode::Emoji => el.child(self.emoji_body(cx)),
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .pt_2()
                    .child(
                        Button::new("icon-picker-cancel")
                            .outline()
                            .small()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("icon-picker-save")
                            .primary()
                            .small()
                            .label("Save icon")
                            .disabled(invalid)
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}
