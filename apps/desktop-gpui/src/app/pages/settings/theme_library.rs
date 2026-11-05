//! The Appearance page's color scheme tiles and theme cards (`ThemeLibrary` in `ThemeSettings.tsx`).

use std::collections::{HashMap, HashSet};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Div, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, hsla, linear_color_stop, linear_gradient,
    px, relative,
};

use super::widgets::subheading;
use crate::app::Workspace;
use crate::app::theme_editor::Session;
use crate::settings::{ThemePreference, contract};
use crate::theme::{self, Tokens};
use crate::themes::{self, CardId, Preview, ThemeChoice, ThemeDefinition};

fn title(id: &str, fallback: &'static str) -> String {
    contract().search_item(id).map_or(fallback, |item| item.title.as_str()).to_owned()
}

/// Card state the web keeps in component state: each collection's shown variant, the open
/// variant fan, and the removal being confirmed.
#[derive(Default)]
pub(in crate::app) struct LibraryUi {
    variant: HashMap<String, usize>,
    fan: Option<(String, bool)>,
    removal: Option<Removal>,
}

struct Removal {
    theme: ThemeDefinition,
    collection: Vec<ThemeDefinition>,
    selected: HashSet<String>,
}

/// What a card's click does (`onUse`).
#[derive(Clone)]
enum Use {
    Standard,
    Theme(String),
    /// `selectCollectionDefaults`: the first light and dark variants of the collection with
    /// this key.
    Collection(String),
}

/// One `ThemeLibraryCard`, borrowing the saved themes it shows.
struct Card<'a> {
    key: String,
    label: String,
    card: CardId,
    on_use: Use,
    previews: Vec<(bool, Preview)>,
    duplicate: Session,
    /// A saved theme: Edit, Export and Remove.
    custom: Option<&'a ThemeDefinition>,
}

fn previews_of(theme: &ThemeDefinition) -> Vec<(bool, Preview)> {
    theme.modes().into_iter().map(|dark| (dark, Preview::of(theme.colors_for(dark).unwrap_or(&theme.colors)))).collect()
}

fn tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> gpui_kit::AnyView {
    let text = text.into();
    move |window, cx| Tooltip::new(text.clone()).build(window, cx)
}

/// `collectionVariantLabels`: each variant's label without the words all of them share.
fn variant_labels(themes: &[&ThemeDefinition]) -> Vec<String> {
    let words: Vec<Vec<&str>> = themes.iter().map(|theme| theme.label.split_whitespace().collect()).collect();
    let Some(first) = words.first() else { return Vec::new() };
    let shared = first
        .iter()
        .enumerate()
        .position(|(i, word)| words.iter().any(|w| w.get(i).is_none_or(|w| !w.eq_ignore_ascii_case(word))));
    let prefix = shared.unwrap_or(first.len().saturating_sub(1));
    themes
        .iter()
        .zip(&words)
        .map(|(theme, words)| {
            let short = words.get(prefix..).unwrap_or_default().join(" ");
            if short.trim().is_empty() { theme.label.clone() } else { short }
        })
        .collect()
}

/// Saved themes grouped by collection, in library order (`customThemeCollections`).
fn collections(themes: &[ThemeDefinition]) -> Vec<(String, Vec<&ThemeDefinition>)> {
    let mut groups: Vec<(String, Vec<&ThemeDefinition>)> = Vec::new();
    for theme in themes {
        let key = match &theme.collection {
            Some(collection) => format!("collection:{}", collection.id),
            None => format!("theme:{}", theme.id),
        };
        match groups.iter_mut().find(|(id, _)| *id == key) {
            Some((_, group)) => group.push(theme),
            None => groups.push((key, vec![theme])),
        }
    }
    groups
}

/// The saved collection with this key, as `collections` groups it.
fn collection<'a>(themes: &'a [ThemeDefinition], key: &str) -> Vec<&'a ThemeDefinition> {
    collections(themes).into_iter().find(|(id, _)| id == key).map(|(_, group)| group).unwrap_or_default()
}

pub(in crate::app) fn toast(window: &mut Window, cx: &mut App, title: String, description: String) {
    window.push_notification(Notification::success(description).title(title), cx);
}

impl Workspace {
    /// "Color scheme" tiles over the theme cards. Cards only mark their picked appearances, as
    /// on the web; the standard "T3 Code" card stands for no theme.
    pub(super) fn render_theme_library(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let choice = self.client_settings.theme_choice();
        let owners = [choice.owner(false), choice.owner(true)];
        let tiles = self.render_mode_tiles(&owners, &t, cx);
        let initial_dark = t.dark;
        let seed = |id: Option<&str>, label: &str| Session::duplicate(id, label, initial_dark);
        let mut cards = vec![Card {
            key: "default".into(),
            label: "T3 Code".into(),
            card: None,
            on_use: Use::Standard,
            previews: vec![(false, Preview::standard(false)), (true, Preview::standard(true))],
            duplicate: seed(None, "T3 Code"),
            custom: None,
        }];
        cards.extend(themes::built_in().iter().map(|theme| Card {
            key: theme.id.clone(),
            label: theme.label.clone(),
            card: Some(theme.id.clone()),
            on_use: Use::Theme(theme.id.clone()),
            previews: previews_of(theme),
            duplicate: seed(Some(&theme.id), &theme.label),
            custom: None,
        }));
        let mut rendered: Vec<AnyElement> =
            cards.into_iter().map(|card| self.render_theme_card(card, &owners, None, &t, cx)).collect();
        for (key, group) in collections(self.client_settings.themes.custom()) {
            let index = self.settings_ui.theme_library.variant.get(&key).copied().unwrap_or_else(|| {
                group.iter().position(|theme| owners.iter().any(|o| o.as_deref() == Some(&theme.id))).unwrap_or(0)
            });
            let theme = group[index.min(group.len() - 1)];
            let on_use = if group.len() == 1 { Use::Theme(theme.id.clone()) } else { Use::Collection(key.clone()) };
            let card = Card {
                key: key.clone(),
                label: theme.label.clone(),
                card: Some(theme.id.clone()),
                on_use,
                previews: previews_of(theme),
                duplicate: seed(Some(&theme.id), &theme.label),
                custom: Some(theme),
            };
            let fan = (group.len() > 1).then_some(group.as_slice());
            rendered.push(self.render_theme_card(card, &owners, fan, &t, cx));
        }
        let create_seed = owners[usize::from(initial_dark)].clone();
        let header = h_flex()
            .min_h(px(32.))
            .pt(px(20.))
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .child(subheading(title("theme", "Theme")))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        Button::new("theme-create")
                            .outline()
                            .xsmall()
                            .icon(Icon::new(IconName::Paintbrush))
                            .label("Create theme")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let session = Session { seed: create_seed.clone(), ..Session::new(initial_dark) };
                                this.open_theme_editor(session, window, cx);
                            })),
                    )
                    .child(
                        Button::new("theme-add")
                            .outline()
                            .xsmall()
                            .icon(Icon::new(IconName::Plus))
                            .label("Add theme")
                            .on_click(cx.listener(|this, _, window, cx| this.open_theme_import(window, cx))),
                    ),
            );
        v_flex()
            .id("theme")
            .gap(px(12.))
            .child(subheading(title("color-scheme", "Color scheme")))
            .child(tiles)
            .child(header)
            .child(div().grid().grid_cols(3).gap(px(8.)).children(rendered))
            .into_any_element()
    }

    /// System, Light and Dark tiles, each a wireframe in the theme that owns its appearance.
    fn render_mode_tiles(&self, owners: &[CardId; 2], t: &Tokens, cx: &mut Context<Self>) -> Div {
        let current = self.client_settings.theme;
        let library = &self.client_settings.themes;
        let [light, dark] =
            [false, true].map(|dark| Preview::of_card(owners[usize::from(dark)].as_deref(), dark, library));
        let tile = |mode: ThemePreference, label: &'static str| {
            let active = current == mode;
            let wireframe = match mode {
                ThemePreference::System => wireframe(&[(&light, Some(false)), (&dark, Some(true))], t),
                ThemePreference::Light => wireframe(&[(&light, None)], t),
                ThemePreference::Dark => wireframe(&[(&dark, None)], t),
            };
            v_flex()
                .id(SharedString::from(format!("color-scheme-{}", mode.id())))
                .flex_1()
                .gap(px(6.))
                .p(px(8.))
                .rounded(px(12.))
                .cursor_pointer()
                .map(|el| {
                    if active {
                        el.border_2().border_color(t.ring).bg(t.accent.opacity(0.3)).p(px(7.))
                    } else {
                        el.border_1()
                            .border_color(t.border.opacity(0.7))
                            .bg(t.card.opacity(0.6))
                            .hover(|s| s.bg(t.accent.opacity(0.1)))
                    }
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    let choice = ThemeChoice { mode, ..this.client_settings.theme_choice() };
                    this.set_theme_choice(choice, window, cx);
                }))
                .child(wireframe)
                .child(
                    div()
                        .text_center()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if active { t.foreground } else { t.muted_foreground })
                        .child(label),
                )
        };
        h_flex()
            .gap(px(12.))
            .child(tile(ThemePreference::System, "System"))
            .child(tile(ThemePreference::Light, "Light"))
            .child(tile(ThemePreference::Dark, "Dark"))
    }

    fn use_card_action(&mut self, on_use: &Use, window: &mut Window, cx: &mut Context<Self>) {
        let mut choice = self.client_settings.theme_choice();
        match on_use {
            Use::Standard => choice.use_theme(None),
            Use::Theme(id) => choice.use_card(Some(id.clone())),
            Use::Collection(key) => {
                let themes = collection(self.client_settings.themes.custom(), key);
                for dark in [false, true] {
                    if let Some(theme) = themes.iter().find(|theme| theme.colors_for(dark).is_some()) {
                        choice.assign_half(dark, Some(theme.id.clone()));
                    }
                }
                self.settings_ui.theme_library.variant.insert(key.clone(), 0);
            }
        }
        self.set_theme_choice(choice, window, cx);
    }

    fn pick_half(&mut self, dark: bool, card: CardId, window: &mut Window, cx: &mut Context<Self>) {
        let mut choice = self.client_settings.theme_choice();
        choice.assign_half(dark, card);
        self.set_theme_choice(choice, window, cx);
    }

    /// `ThemeLibraryCard`: the card uses the theme; a circle gives it one appearance. Saved
    /// themes also edit, export and remove; a collection's circles fan out its variants.
    fn render_theme_card(
        &self,
        card: Card,
        owners: &[CardId; 2],
        fan: Option<&[&ThemeDefinition]>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = card.key.clone();
        let circles = match fan {
            Some(themes) => self.render_variant_fan(&key, themes, owners, t, cx),
            None => h_flex()
                .min_h(px(64.))
                .items_center()
                .justify_center()
                .gap(px(10.))
                .px(px(12.))
                .pt(px(12.))
                .children(card.previews.iter().map(|(dark, colors)| {
                    let dark = *dark;
                    let target = card.card.clone();
                    let mode = if dark { "dark" } else { "light" };
                    let picked = owners[usize::from(dark)] == card.card;
                    circle_button(SharedString::from(format!("theme-{key}-{mode}")), colors, dark, picked, t)
                        .tooltip(tip(format!("Use {} for {mode} mode", card.label)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.pick_half(dark, target.clone(), window, cx);
                        }))
                        .into_any_element()
                }))
                .into_any_element(),
        };
        let card_tip: SharedString = if fan.is_some() {
            "Use the first variants for light and dark".into()
        } else if card.previews.len() > 1 {
            "Use for both light and dark".into()
        } else {
            let mode = if card.previews.first().is_some_and(|p| p.0) { "dark" } else { "light" };
            format!("Use for {mode} mode only").into()
        };
        let label = match (&card.custom, fan) {
            (Some(theme), Some(_)) => theme.collection.as_ref().map_or(card.label.clone(), |c| c.label.clone()),
            _ => card.label.clone(),
        };
        let icon_button = |id: &str, icon: IconName, text: &'static str| {
            Button::new(SharedString::from(format!("theme-{id}-{key}")))
                .ghost()
                .xsmall()
                .icon(Icon::new(icon))
                .tooltip(text)
        };
        let duplicate = card.duplicate.clone();
        let mut actions = vec![
            icon_button("duplicate", IconName::Copy, "Duplicate theme")
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.open_theme_editor(duplicate.clone(), window, cx);
                }))
                .into_any_element(),
        ];
        if let Some(theme) = card.custom {
            let edit = Session { editing: Some(theme.id.clone()), ..Session::new(t.dark) };
            let (export, remove) = (theme.id.clone(), (theme.id.clone(), key.clone()));
            actions.push(
                icon_button("edit", IconName::PenLine, "Edit theme")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_theme_editor(edit.clone(), window, cx);
                    }))
                    .into_any_element(),
            );
            actions.push(
                icon_button("export", IconName::Upload, "Export theme file")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        if let Some(theme) = this.client_settings.themes.custom().iter().find(|t| t.id == export) {
                            export_theme(theme, window, cx);
                        }
                    }))
                    .into_any_element(),
            );
            actions.push(
                icon_button("remove", IconName::Trash, if fan.is_some() { "Remove themes" } else { "Remove theme" })
                    .text_color(t.error_foreground)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        let (id, key) = &remove;
                        let themes = this.client_settings.themes.custom();
                        let group: Vec<ThemeDefinition> = collection(themes, key).into_iter().cloned().collect();
                        if let Some(theme) = group.iter().find(|theme| &theme.id == id).cloned() {
                            this.confirm_remove_theme(theme, group, window, cx);
                        }
                    }))
                    .into_any_element(),
            );
        }
        let on_use = card.on_use;
        v_flex()
            .id(SharedString::from(format!("theme-card-{key}")))
            .overflow_hidden()
            .rounded(px(12.))
            .border_1()
            .border_color(t.border.opacity(0.7))
            .bg(t.card.opacity(0.6))
            .cursor_pointer()
            .hover(|s| s.bg(t.accent.opacity(0.1)))
            .tooltip(tip(card_tip))
            .on_click(cx.listener(move |this, _, window, cx| this.use_card_action(&on_use, window, cx)))
            .child(circles)
            .child(
                h_flex()
                    .gap(px(8.))
                    .px(px(12.))
                    .pb(px(12.))
                    .pt(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t.foreground)
                            .child(label),
                    )
                    .child(h_flex().flex_none().gap(px(4.)).children(actions)),
            )
            .into_any_element()
    }

    /// A collection card's variant picker: a root circle per appearance at the picked variant
    /// that fans out the others on hover (all at once; no motion).
    fn render_variant_fan(
        &self,
        key: &str,
        themes: &[&ThemeDefinition],
        owners: &[CardId; 2],
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let labels = variant_labels(themes);
        let open = self.settings_ui.theme_library.fan.as_ref().filter(|(k, _)| k == key).map(|(_, dark)| *dark);
        let mut layer = div().id(SharedString::from(format!("theme-fan-{key}"))).relative().h(px(80.)).on_hover(
            cx.listener(|this, hovered: &bool, _, cx| {
                if !hovered && this.settings_ui.theme_library.fan.take().is_some() {
                    cx.notify();
                }
            }),
        );
        for dark in [false, true] {
            let options: Vec<(usize, &ThemeDefinition)> =
                themes.iter().copied().enumerate().filter(|(_, theme)| theme.colors_for(dark).is_some()).collect();
            let active = |theme: &ThemeDefinition| owners[usize::from(dark)].as_deref() == Some(theme.id.as_str());
            let Some(&(selected_index, selected)) = options.iter().find(|(_, theme)| active(theme)).or(options.first())
            else {
                continue;
            };
            let offset = if dark { 52. } else { -52. };
            let mode = if dark { "Dark" } else { "Light" };
            let colors = Preview::of(selected.colors_for(dark).unwrap_or(&selected.colors));
            let selected_label = labels[selected_index].clone();
            let (fan_key, hover_key, root_id) = (key.to_owned(), key.to_owned(), selected.id.clone());
            layer = layer.child(
                div().absolute().top(px(8.)).left(relative(0.5)).ml(px(offset - 28.)).child(
                    circle_button(
                        SharedString::from(format!("theme-fan-{key}-{mode}")),
                        &colors,
                        dark,
                        active(selected),
                        t,
                    )
                    .size(px(56.))
                    .p_0()
                    .tooltip(tip(format!("{mode}: {selected_label}")))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered && this.settings_ui.theme_library.fan != Some((hover_key.clone(), dark)) {
                            this.settings_ui.theme_library.fan = Some((hover_key.clone(), dark));
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.settings_ui.theme_library.variant.insert(fan_key.clone(), selected_index);
                        this.pick_half(dark, Some(root_id.clone()), window, cx);
                    })),
                ),
            );
            layer = layer.child(
                h_flex()
                    .absolute()
                    .bottom_0()
                    .left(relative(0.5))
                    .ml(px(offset - 48.))
                    .w(px(96.))
                    .justify_center()
                    .gap(px(4.))
                    .text_size(gpui_kit::rems(0.6875))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.foreground)
                    .child(div().min_w_0().truncate().child(selected_label))
                    .when(options.len() > 1, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .rounded_full()
                                .bg(t.muted)
                                .px(px(4.))
                                .text_size(gpui_kit::rems(0.625))
                                .text_color(t.muted_foreground)
                                .child(format!("+{}", options.len() - 1)),
                        )
                    }),
            );
            if open != Some(dark) || options.len() < 2 {
                continue;
            }
            for (position, (theme_index, theme)) in options.iter().enumerate() {
                let progress = position as f32 / (options.len() - 1) as f32 - 0.5;
                let (x, y) = (offset + progress * 68., progress.abs() * 10.);
                let colors = Preview::of(theme.colors_for(dark).unwrap_or(&theme.colors));
                let (fan_key, id, index) = (key.to_owned(), theme.id.clone(), *theme_index);
                layer = layer.child(
                    div()
                        .id(SharedString::from(format!("theme-fan-{key}-{mode}-{}", theme.id)))
                        .absolute()
                        .top(px(4. + y))
                        .left(relative(0.5))
                        .ml(px(x - 14.))
                        .size(px(28.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(t.background)
                        .shadow_sm()
                        .border_2()
                        .border_color(if active(theme) { t.ring } else { t.border.opacity(0.7) })
                        .child(preview_circle(&colors, dark, t).size(px(22.)).border_1())
                        .tooltip(tip(format!("Use {} for {} mode", labels[index], mode.to_lowercase())))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.settings_ui.theme_library.variant.insert(fan_key.clone(), index);
                            this.pick_half(dark, Some(id.clone()), window, cx);
                        })),
                );
            }
        }
        layer.into_any_element()
    }

    /// The removal confirmation; a collection lists its variants to pick from.
    fn confirm_remove_theme(
        &mut self,
        theme: ThemeDefinition,
        collection: Vec<ThemeDefinition>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = if collection.len() > 1 { HashSet::new() } else { HashSet::from([theme.id.clone()]) };
        self.settings_ui.theme_library.removal = Some(Removal { theme, collection, selected });
        let workspace = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let t = *theme::tokens(cx);
            let Some(removal) = &workspace.read(cx).settings_ui.theme_library.removal else { return dialog };
            let many = removal.collection.len() > 1;
            let label = removal.theme.collection.as_ref().map_or(&removal.theme.label, |c| &c.label);
            let (title, description) = if many {
                (
                    format!("Remove themes from “{label}”?"),
                    "Select the variants you want to remove. You can restore them by importing the extension again.",
                )
            } else {
                (
                    format!("Remove “{}”?", removal.theme.label),
                    "You can bring it back anytime by importing its JSON file.",
                )
            };
            let count = removal.selected.len();
            let tiles: Vec<AnyElement> = if many {
                removal.collection.iter().map(|variant| removal_tile(variant, removal, &workspace, &t)).collect()
            } else {
                Vec::new()
            };
            let confirm = workspace.clone();
            let remove_label = match (many, count) {
                (false, _) => "Remove theme".to_owned(),
                (true, 0) => "Remove selected".to_owned(),
                (true, count) => format!("Remove selected ({count})"),
            };
            dialog
                .entrance(DialogEntrance::None)
                .title(title)
                .child(div().text_sm().text_color(t.muted_foreground).child(description))
                .when(many, |dialog| {
                    dialog.child(
                        div()
                            .id("remove-theme-variants")
                            .mt(px(8.))
                            .max_h(px(288.))
                            .overflow_y_scroll()
                            .grid()
                            .grid_cols(2)
                            .gap(px(8.))
                            .children(tiles),
                    )
                })
                .footer(
                    h_flex()
                        .justify_end()
                        .gap(px(8.))
                        .child(
                            Button::new("remove-theme-cancel")
                                .outline()
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("remove-theme-confirm")
                                .with_variant(ButtonVariant::Danger)
                                .disabled(count == 0)
                                .label(remove_label)
                                .on_click(move |_, window, cx| {
                                    confirm.update(cx, |this, cx| this.remove_selected_themes(window, cx));
                                    window.close_dialog(cx);
                                }),
                        ),
                )
        });
    }

    /// `handleConfirmRemoveTheme`: a removed base falls back to the standard palette and a
    /// removed half is cleared, then the themes go.
    fn remove_selected_themes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(removal) = self.settings_ui.theme_library.removal.take() else { return };
        let ids: Vec<String> = removal.selected.into_iter().collect();
        if ids.is_empty() {
            return;
        }
        let library = self.client_settings.themes.remove(&ids);
        let choice = ThemeChoice { library: library.clone(), ..self.client_settings.theme_choice().without(&ids) };
        self.store_themes(library, choice, window, cx);
    }
}

/// One variant in the collection removal grid, a checkbox in its corner.
fn removal_tile(
    variant: &ThemeDefinition,
    removal: &Removal,
    workspace: &gpui_kit::Entity<Workspace>,
    t: &Tokens,
) -> AnyElement {
    let checked = removal.selected.contains(&variant.id);
    let (id, workspace) = (variant.id.clone(), workspace.clone());
    v_flex()
        .id(SharedString::from(format!("remove-theme-{}", variant.id)))
        .relative()
        .min_h(px(112.))
        .items_center()
        .justify_center()
        .gap(px(8.))
        .p(px(12.))
        .rounded(px(8.))
        .border_1()
        .cursor_pointer()
        .map(|el| {
            if checked {
                el.border_color(t.ring).bg(t.accent.opacity(0.2))
            } else {
                el.border_color(t.border.opacity(0.7)).bg(t.muted.opacity(0.25)).hover(|s| s.bg(t.muted.opacity(0.4)))
            }
        })
        .child(
            div()
                .absolute()
                .top(px(8.))
                .right(px(8.))
                .size(px(16.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .border_1()
                .border_color(if checked { t.primary } else { t.input })
                .bg(if checked { t.primary } else { t.background })
                .when(checked, |el| {
                    el.child(Icon::new(IconName::Check).size(px(12.)).text_color(t.primary_foreground))
                }),
        )
        .child(h_flex().min_h(px(48.)).gap(px(4.)).children(previews_of(variant).into_iter().map(|(dark, colors)| {
            div()
                .size(px(44.))
                .flex()
                .items_center()
                .justify_center()
                .child(preview_circle(&colors, dark, t).size(px(42.)))
        })))
        .child(
            div()
                .max_w_full()
                .truncate()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.foreground)
                .child(variant.label.clone()),
        )
        .on_click(move |_, _, cx| {
            workspace.update(cx, |this, cx| {
                if let Some(removal) = &mut this.settings_ui.theme_library.removal
                    && !removal.selected.remove(&id)
                {
                    removal.selected.insert(id.clone());
                }
                cx.notify();
            });
        })
        .into_any_element()
}

/// `downloadThemeFile`: saves `<id>.json` where the person picks.
fn export_theme(theme: &ThemeDefinition, window: &mut Window, cx: &mut App) {
    let contents = match crate::theme_file::serialize_theme_file(theme) {
        Ok(contents) => contents,
        Err(error) => {
            window.push_notification(Notification::error(error).title("Could not export theme"), cx);
            return;
        }
    };
    let downloads = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|home| std::path::PathBuf::from(home).join("Downloads"))
        .unwrap_or_default();
    let picked = cx.prompt_for_new_path(&downloads, Some(&format!("{}.json", theme.id)));
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            if let Err(error) = std::fs::write(&path, contents) {
                let message = format!("Could not write {}: {error}", path.display());
                let _ = cx.update(|window, cx| {
                    window.push_notification(Notification::error(message).title("Could not export theme"), cx);
                });
            }
        })
        .detach();
}

/// A theme circle with the picked ring and the sun or moon badge.
fn circle_button(id: SharedString, colors: &Preview, dark: bool, picked: bool, t: &Tokens) -> gpui_kit::Stateful<Div> {
    div()
        .id(id)
        .relative()
        .size(px(68.))
        .p(px(6.))
        .rounded_full()
        .cursor_pointer()
        .child(preview_circle(colors, dark, t))
        .when(picked, |el| {
            el.child(div().absolute().inset_0().rounded_full().border_2().border_color(t.ring)).child(
                div()
                    .absolute()
                    .bottom(px(2.))
                    .right(px(2.))
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(t.border.opacity(0.7))
                    .bg(t.background)
                    .text_color(t.foreground)
                    .shadow_sm()
                    .child(Icon::new(if dark { IconName::Moon } else { IconName::Sun }).size(px(12.))),
            )
        })
}

/// `ThemePreviewCircle`: the canvas lifted toward white or black, with the accent and the
/// action color fading in from opposite edges (GPUI has no radial gradients).
fn preview_circle(colors: &Preview, dark: bool, t: &Tokens) -> Div {
    let target: Hsla = if dark { gpui_kit::rgb(0x09090b).into() } else { hsla(0., 0., 1., 1.) };
    let base = theme::mix_oklab(colors.canvas, target, 0.2);
    // `THEME_PREVIEW_RENDER_SPECS`: the accent sits top right in light mode, bottom left in dark.
    let (accent_angle, action_angle) = if dark { (225., 45.) } else { (45., 225.) };
    let layer = |angle: f32, color: Hsla, start: f32| {
        div().absolute().inset_0().rounded_full().bg(linear_gradient(
            angle,
            linear_color_stop(color.opacity(0.), start),
            linear_color_stop(color, 1.),
        ))
    };
    let edge: Hsla = if dark { hsla(0., 0., 1., 0.14) } else { hsla(0., 0., 0., 0.1) };
    div()
        .relative()
        .size(px(56.))
        .flex_none()
        .rounded_full()
        .border_2()
        .border_color(t.background)
        .bg(base)
        .child(layer(accent_angle, colors.accent.opacity(0.85), 0.35))
        .child(layer(action_angle, colors.message_action.opacity(0.45), 0.5))
        .child(div().absolute().inset_0().rounded_full().border_1().border_color(edge))
}

/// `ThemeWireframe`: a miniature app per pane; two panes split the tile down the middle.
fn wireframe(panes: &[(&Preview, Option<bool>)], t: &Tokens) -> Div {
    div()
        .relative()
        .w_full()
        .h(px(140.))
        .overflow_hidden()
        .rounded(px(8.))
        .border_1()
        .border_color(t.border.opacity(0.6))
        .children(panes.iter().map(|(colors, half)| {
            match half {
                None => wireframe_pane(colors).into_any_element(),
                // The left half and the right half, each 1px short of the middle.
                Some(right) => div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .w(relative(0.5))
                    .overflow_hidden()
                    .map(|el| if *right { el.right_0() } else { el.left_0() })
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .w(relative(2.))
                            .map(|el| if *right { el.right_0() } else { el.left_0() })
                            .child(wireframe_pane(colors)),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .w(px(1.))
                            .bg(t.border.opacity(0.6))
                            .map(|el| if *right { el.left_0() } else { el.right_0() }),
                    )
                    .into_any_element(),
            }
        }))
}

fn wireframe_pane(c: &Preview) -> Div {
    let line: Hsla = hsla(0., 0., 0.5, 0.25);
    let block = |left: f32, top: f32, width: f32, height: f32, color: Hsla| {
        div()
            .absolute()
            .left(relative(left))
            .top(relative(top))
            .w(relative(width))
            .h(relative(height))
            .rounded(px(6.))
            .bg(color)
    };
    let menu_row = |row: usize, dot: Hsla| {
        h_flex()
            .absolute()
            .left(relative(0.11))
            .right(relative(0.11))
            .top(relative(0.1 + row as f32 * 0.3))
            .h(relative(0.2))
            .gap(px(4.))
            .child(div().size(px(5.)).rounded_full().bg(dot.opacity(0.55)))
            .child(div().h(px(4.)).w(relative(0.52)).rounded(px(2.)).bg(line))
    };
    div()
        .absolute()
        .inset_0()
        .bg(c.canvas)
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(relative(0.22))
                .bg(c.sidebar)
                .border_r_1()
                .border_color(line),
        )
        .child(block(0.03, 0.08, 0.16, 0.08, c.surface).border_1().border_color(line))
        .child(block(0.03, 0.22, 0.16, 0.07, c.accent_surface))
        .child(block(0.03, 0.32, 0.16, 0.07, c.message_surface.opacity(0.7)))
        .child(block(0.03, 0.42, 0.16, 0.07, c.message_surface.opacity(0.5)))
        .child(block(0.48, 0.11, 0.24, 0.09, c.message_surface).rounded(px(8.)))
        .child(block(0.27, 0.28, 0.34, 0.05, line).rounded(px(2.)))
        .child(block(0.27, 0.38, 0.26, 0.05, line).rounded(px(2.)))
        .child(
            h_flex()
                .absolute()
                .bottom(relative(0.08))
                .left(relative(0.26))
                .right(relative(0.06))
                .h(relative(0.15))
                .px(px(4.))
                .justify_between()
                .rounded(px(6.))
                .bg(c.surface)
                .border_1()
                .border_color(line)
                .child(div().h(px(4.)).w(relative(0.34)).rounded_full().bg(line.opacity(0.7)))
                .child(div().size(px(11.)).rounded_full().bg(c.message_action)),
        )
        .child(
            div()
                .absolute()
                .right(relative(0.05))
                .top(relative(0.08))
                .w(relative(0.2))
                .h(relative(0.46))
                .rounded(px(8.))
                .bg(c.surface)
                .border_1()
                .border_color(line)
                .child(menu_row(0, gpui_kit::rgb(0x34d399).into()))
                .child(menu_row(1, c.message_action))
                .child(menu_row(2, gpui_kit::rgb(0xfbbf24).into())),
        )
}
