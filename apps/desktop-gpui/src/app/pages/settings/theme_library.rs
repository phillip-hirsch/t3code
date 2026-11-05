//! The Appearance page's color scheme tiles and theme cards (`ThemeLibrary` in `ThemeSettings.tsx`).

use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Div, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, hsla, linear_color_stop, linear_gradient, px, relative,
};

use super::widgets::subheading;
use crate::app::Workspace;
use crate::settings::{ThemePreference, contract};
use crate::theme::{self, Tokens};
use crate::themes::{self, CardId, Preview};

fn title(id: &str, fallback: &'static str) -> String {
    contract().search_item(id).map_or(fallback, |item| item.title.as_str()).to_owned()
}

impl Workspace {
    /// "Color scheme" tiles over the theme cards. Cards only mark their picked appearances, as
    /// on the web; the standard "T3 Code" card stands for no theme.
    pub(super) fn render_theme_library(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let choice = self.client_settings.theme_choice();
        let owners = [choice.owner(false), choice.owner(true)];
        let tiles = self.render_mode_tiles(&owners, &t, cx);
        let cards: Vec<AnyElement> = std::iter::once((None, "T3 Code"))
            .chain(themes::built_in().iter().map(|theme| (Some(theme.id.clone()), theme.label.as_str())))
            .map(|(card, label)| self.render_theme_card(card, label, &owners, &t, cx))
            .collect();
        v_flex()
            .id("theme")
            .gap(px(12.))
            .child(subheading(title("color-scheme", "Color scheme")))
            .child(tiles)
            .child(div().pt(px(20.)).child(subheading(title("theme", "Theme"))))
            .child(div().grid().grid_cols(3).gap(px(8.)).children(cards))
            .into_any_element()
    }

    /// System, Light and Dark tiles, each a wireframe in the theme that owns its appearance.
    fn render_mode_tiles(&self, owners: &[CardId; 2], t: &Tokens, cx: &mut Context<Self>) -> Div {
        let current = self.client_settings.theme;
        let [light, dark] =
            [Preview::of_card(owners[0].as_deref(), false), Preview::of_card(owners[1].as_deref(), true)];
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
                    let choice = themes::ThemeChoice { mode, ..this.client_settings.theme_choice() };
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

    /// `ThemeLibraryCard`: the card uses the theme; a circle gives it one appearance.
    fn render_theme_card(
        &self,
        card: CardId,
        label: &str,
        owners: &[CardId; 2],
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = card.clone().unwrap_or_else(|| "default".into());
        let circles = [false, true].map(|dark| {
            let picked = owners[dark as usize] == card;
            let colors = Preview::of_card(card.as_deref(), dark);
            let card = card.clone();
            div()
                .id(SharedString::from(format!("theme-{key}-{}", if dark { "dark" } else { "light" })))
                .relative()
                .size(px(68.))
                .p(px(6.))
                .rounded_full()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    let mut choice = this.client_settings.theme_choice();
                    choice.assign_half(dark, card.clone());
                    this.set_theme_choice(choice, window, cx);
                }))
                .child(preview_circle(&colors, dark, t))
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
        });
        v_flex()
            .id(SharedString::from(format!("theme-card-{key}")))
            .overflow_hidden()
            .rounded(px(12.))
            .border_1()
            .border_color(t.border.opacity(0.7))
            .bg(t.card.opacity(0.6))
            .cursor_pointer()
            .hover(|s| s.bg(t.accent.opacity(0.1)))
            .on_click(cx.listener(move |this, _, window, cx| {
                let mut choice = this.client_settings.theme_choice();
                choice.use_card(card.clone());
                this.set_theme_choice(choice, window, cx);
            }))
            .child(
                h_flex()
                    .min_h(px(64.))
                    .items_center()
                    .justify_center()
                    .gap(px(10.))
                    .px(px(12.))
                    .pt(px(12.))
                    .children(circles),
            )
            .child(
                div()
                    .px(px(12.))
                    .pb(px(12.))
                    .pt(px(8.))
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.foreground)
                    .child(label.to_owned()),
            )
            .into_any_element()
    }
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
