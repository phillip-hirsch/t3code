//! The web app's default theme (`apps/web/src/index.css`), as GPUI colors.
//!
//! Alpha tokens stay translucent, as in CSS, so they composite over whatever
//! surface they sit on.

use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::{App, Global, Hsla, Window, WindowAppearance, hsla, rgb};

use crate::settings::ThemePreference;

pub const FONT_SANS: &str = "Inter";
pub const FONT_MONO: &str = "Consolas";

/// Static Inter 4.1 weights, the version `@fontsource-variable/inter` ships.
pub const FONTS: [&[u8]; 4] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Inter-Bold.ttf"),
];

#[derive(Clone, Copy)]
pub struct Tokens {
    pub dark: bool,
    pub background: Hsla,
    pub foreground: Hsla,
    pub card: Hsla,
    pub popover: Hsla,
    pub primary: Hsla,
    pub primary_foreground: Hsla,
    pub primary_hover: Hsla,
    pub secondary: Hsla,
    pub muted: Hsla,
    pub muted_foreground: Hsla,
    pub accent: Hsla,
    pub message_surface: Hsla,
    pub border: Hsla,
    pub input: Hsla,
    pub ring: Hsla,
    pub error: Hsla,
    pub error_foreground: Hsla,
    pub info: Hsla,
    pub info_foreground: Hsla,
    pub success_foreground: Hsla,
    pub warning: Hsla,
    pub warning_foreground: Hsla,
    pub indigo_foreground: Hsla,
    pub scrollbar_thumb: Hsla,
    pub scrollbar_thumb_hover: Hsla,
    pub sidebar: SidebarTokens,
}

/// The `[data-app-sidebar]` palette override (`index.css:1234-1278`).
#[derive(Clone, Copy)]
pub struct SidebarTokens {
    pub background: Hsla,
    pub foreground: Hsla,
    pub muted_foreground: Hsla,
    pub secondary_label: Hsla,
    pub border: Hsla,
    pub row_hover: Hsla,
    pub row_active: Hsla,
    pub icon: Hsla,
}

impl Global for Tokens {}

fn hex(value: u32) -> Hsla {
    rgb(value).into()
}

fn with_alpha(color: Hsla, alpha: f32) -> Hsla {
    Hsla { a: alpha, ..color }
}

fn white(alpha: f32) -> Hsla {
    hsla(0., 0., 1., alpha)
}

impl Tokens {
    pub fn dark() -> Self {
        let foreground = hex(0xf5f5f5);
        let sidebar_foreground = hex(0xf1f3f7);
        Self {
            dark: true,
            background: hex(0x000000),
            foreground,
            card: hex(0x111111),
            popover: hex(0x111111),
            primary: hex(0x346bf1),
            primary_foreground: hex(0xffffff),
            primary_hover: hex(0x2f60d9),
            secondary: white(0.03),
            muted: white(0.03),
            muted_foreground: hex(0xa1a1a1),
            accent: white(0.04),
            message_surface: hex(0x1e1e1e),
            border: white(0.08),
            input: white(0.11),
            ring: hex(0x346bf1),
            error: hex(0xfb414a),
            error_foreground: hex(0xff6467),
            info: hex(0x2b7fff),
            info_foreground: hex(0x51a2ff),
            success_foreground: hex(0x00d492),
            warning: hex(0xfe9a00),
            warning_foreground: hex(0xffb900),
            indigo_foreground: hex(0xa3b3ff),
            scrollbar_thumb: white(0.08),
            scrollbar_thumb_hover: white(0.12),
            sidebar: SidebarTokens {
                background: hex(0x000000),
                foreground: sidebar_foreground,
                muted_foreground: hex(0xa3a3a3),
                secondary_label: hex(0x8f8f8f),
                border: white(0.08),
                row_hover: with_alpha(sidebar_foreground, 0.08),
                row_active: with_alpha(sidebar_foreground, 0.11),
                icon: hex(0x8b8b8b),
            },
        }
    }

    pub fn light() -> Self {
        let foreground = hex(0x18181b);
        Self {
            dark: false,
            background: hex(0xfcfcfc),
            foreground,
            card: hex(0xffffff),
            popover: hex(0xffffff),
            primary: hex(0x1b4ed8),
            primary_foreground: hex(0xffffff),
            primary_hover: hex(0x3160db),
            secondary: hex(0xfafafa),
            muted: hex(0xfafafa),
            muted_foreground: hex(0x71717b),
            accent: hex(0xf4f4f5),
            message_surface: hex(0xebebed),
            border: hex(0xe4e4e7),
            input: hex(0xd4d4d8),
            ring: hex(0x1b4ed8),
            error: hex(0xfb2c36),
            error_foreground: hex(0xc10007),
            info: hex(0x2b7fff),
            info_foreground: hex(0x1447e6),
            success_foreground: hex(0x007a55),
            warning: hex(0xfe9a00),
            warning_foreground: hex(0xbb4d00),
            indigo_foreground: hex(0x4f39f6),
            scrollbar_thumb: hex(0xd9d9d9),
            scrollbar_thumb_hover: hex(0xbfbfbf),
            sidebar: SidebarTokens {
                background: hex(0xfafafa),
                foreground,
                muted_foreground: hex(0x71717b),
                secondary_label: hex(0x71717b),
                border: hex(0xe4e4e7),
                row_hover: hex(0xfcfcfc),
                row_active: hex(0xffffff),
                icon: hex(0x86858e),
            },
        }
    }
}

pub fn tokens(cx: &App) -> &Tokens {
    cx.global::<Tokens>()
}

/// The Appearance setting: follow the system, or force light or dark.
#[derive(Clone, Copy, Default)]
struct Preference(ThemePreference);

impl Global for Preference {}

/// Applies the Appearance setting now and on every later `sync`.
pub fn set_preference(preference: ThemePreference, window: &mut Window, cx: &mut App) {
    cx.set_global(Preference(preference));
    sync(window, cx);
    window.refresh();
}

/// Installs the tokens for the preferred appearance and points the
/// component library's palette at them. Call again when the appearance changes.
pub fn sync(window: &mut Window, cx: &mut App) {
    let dark = match cx.try_global::<Preference>().copied().unwrap_or_default().0 {
        ThemePreference::Light => false,
        ThemePreference::Dark => true,
        ThemePreference::System => {
            matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark)
        }
    };
    let tokens = if dark { Tokens::dark() } else { Tokens::light() };
    Theme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
    Theme::update(cx, |theme| {
        theme.font_family = FONT_SANS.into();
        theme.mono_font_family = FONT_MONO.into();
        theme.radius = gpui_kit::px(8.);
        theme.radius_lg = gpui_kit::px(12.);
        let colors = &mut theme.colors;
        colors.background = tokens.background;
        colors.foreground = tokens.foreground;
        colors.border = tokens.border;
        colors.input = tokens.input;
        colors.ring = tokens.ring;
        colors.caret = tokens.foreground;
        colors.selection = with_alpha(tokens.primary, 0.35);
        colors.primary = tokens.primary;
        colors.primary_hover = tokens.primary_hover;
        colors.primary_active = tokens.primary_hover;
        colors.primary_foreground = tokens.primary_foreground;
        colors.secondary = tokens.secondary;
        colors.secondary_hover = tokens.accent;
        colors.secondary_active = tokens.accent;
        colors.secondary_foreground = tokens.foreground;
        colors.muted = tokens.muted;
        colors.muted_foreground = tokens.muted_foreground;
        colors.accent = tokens.accent;
        colors.accent_foreground = tokens.foreground;
        colors.popover = tokens.popover;
        colors.popover_foreground = tokens.foreground;
        colors.list_hover = tokens.accent;
        colors.list_active = tokens.accent;
        colors.danger = tokens.error;
        colors.danger_foreground = tokens.primary_foreground;
        colors.warning = tokens.warning;
        colors.info = tokens.info;
        colors.link = tokens.info_foreground;
        colors.scrollbar = gpui_kit::transparent_black();
        colors.scrollbar_thumb = tokens.scrollbar_thumb;
        colors.scrollbar_thumb_hover = tokens.scrollbar_thumb_hover;
        colors.sidebar = tokens.sidebar.background;
        colors.sidebar_foreground = tokens.sidebar.foreground;
        colors.sidebar_border = tokens.sidebar.border;
        colors.sidebar_accent = tokens.sidebar.row_active;
        colors.sidebar_accent_foreground = tokens.sidebar.foreground;
    });
    cx.set_global(tokens);
}
