//! The web app's default theme (`apps/web/src/index.css`), as GPUI colors.
//!
//! Alpha tokens stay translucent, as in CSS, so they composite over whatever
//! surface they sit on.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::{App, Global, Hsla, SharedString, Window, WindowAppearance, hsla, rgb};

use crate::settings::{Settings, TYPOGRAPHY_ADVANCED_KEY};
use crate::themes::{ThemeChoice, ThemeColors};

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
    /// `--glass-opacity`: the alpha of glass surfaces such as the composer.
    pub glass: f32,
    pub fonts: Fonts,
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
            glass: 0.8,
            fonts: Fonts::default(),
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
            glass: 0.8,
            fonts: Fonts::default(),
        }
    }
}

/// The client settings that restyle the tokens (`startClientAppearance` in `appInit.tsx`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Appearance {
    /// `appearanceContrast`, 50-200 percent.
    pub contrast: f32,
    /// `glassOpacity`, 40-100 percent.
    pub glass: f32,
    pub fonts: Fonts,
}

impl Default for Appearance {
    fn default() -> Self {
        Self { contrast: 100., glass: 80., fonts: Fonts::default() }
    }
}

impl Global for Appearance {}

impl Appearance {
    pub fn from_settings(settings: &Settings) -> Self {
        let percent = |key, default| settings.get(key).as_f64().map_or(default, |value| value as f32);
        Self {
            contrast: percent("appearanceContrast", 100.),
            glass: percent("glassOpacity", 80.),
            fonts: Fonts::from_settings(settings),
        }
    }
}

/// The typography settings as the app paints them (`applyAppearanceFontVariables` in
/// `appearanceFonts.ts`). Families are interned so the tokens stay `Copy`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fonts {
    /// `--font-sans`: everything outside code and the terminal.
    pub sans: &'static str,
    /// `--font-mono`: every monospace label, code block, diff and file preview.
    pub mono: &'static str,
    /// `--font-composer`: the prompt box.
    pub composer: &'static str,
    pub terminal: &'static str,
    /// The root font size, which every rem scales with.
    pub interface_size: f32,
    /// `--font-size-prompt`, in px.
    pub prompt_size: f32,
    /// `--font-size-code` and `--diffs-font-size`, in px.
    pub code_size: f32,
    pub terminal_size: f32,
}

impl Default for Fonts {
    /// `DEFAULT_CLIENT_SETTINGS`, without separate fonts.
    fn default() -> Self {
        Self {
            sans: FONT_SANS,
            mono: FONT_MONO,
            composer: FONT_SANS,
            terminal: FONT_MONO,
            interface_size: INTERFACE_SIZES.2,
            prompt_size: INTERFACE_SIZES.2 * 0.875,
            code_size: CODE_SIZES.2,
            terminal_size: CODE_SIZES.2,
        }
    }
}

/// `MIN_*_FONT_SIZE`, `MAX_*_FONT_SIZE` and the default, from `packages/contracts/src/settings.ts`.
const INTERFACE_SIZES: (f32, f32, f32) = (12., 20., 16.);
const PROMPT_SIZES: (f32, f32, f32) = (12., 20., 14.);
const CODE_SIZES: (f32, f32, f32) = (10., 18., 13.);
const TERMINAL_SIZES: (f32, f32, f32) = (8., 20., 12.);

impl Fonts {
    pub fn from_settings(settings: &Settings) -> Self {
        let family = |key: &str| first_family(settings.get(key).as_str().unwrap_or_default());
        let size = |key: &str, (min, max, fallback): (f32, f32, f32)| {
            settings.get(key).as_f64().map_or(fallback, |value| (value as f32).round().clamp(min, max))
        };
        let separate = settings.bool(TYPOGRAPHY_ADVANCED_KEY);
        let sans = family("fontFamilySans").unwrap_or(FONT_SANS);
        let mono = family("fontFamilyCode").unwrap_or(FONT_MONO);
        let interface_size = size("fontSizeInterface", INTERFACE_SIZES);
        let code_size = size("fontSizeCode", CODE_SIZES);
        Self {
            sans,
            mono,
            composer: separate.then(|| family("fontFamilyComposer")).flatten().unwrap_or(sans),
            // `resolveTerminalFontPreference`: the code font, or its own (default stack when unset).
            terminal: if separate { family("fontFamilyTerminal").unwrap_or(FONT_MONO) } else { mono },
            interface_size,
            // `var(--text-sm)` unless set separately.
            prompt_size: if separate { size("fontSizePrompt", PROMPT_SIZES) } else { interface_size * 0.875 },
            code_size,
            terminal_size: if separate { size("fontSizeTerminal", TERMINAL_SIZES) } else { code_size },
        }
    }

    /// A family no installed font has falls back to the default, as the CSS stack does.
    fn installed_or_default(mut self, cx: &mut App) -> Self {
        let installed = installed_fonts(cx);
        let known = |name: &str| name == FONT_SANS || installed.iter().any(|font| font.as_ref() == name);
        let sans = if known(self.sans) { self.sans } else { FONT_SANS };
        let mono = if known(self.mono) { self.mono } else { FONT_MONO };
        if self.composer == self.sans || !known(self.composer) {
            self.composer = sans;
        }
        if !known(self.terminal) {
            self.terminal = if self.terminal == self.mono { mono } else { FONT_MONO };
        }
        (self.sans, self.mono) = (sans, mono);
        self
    }
}

/// The first family of a CSS family list, unquoted (`cssFontFamilies`).
fn first_family(list: &str) -> Option<&'static str> {
    let name =
        list.split(',').map(|name| name.trim().trim_matches(['"', '\'']).trim()).find(|name| !name.is_empty())?;
    static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut names = NAMES.get_or_init(Mutex::default).lock().unwrap_or_else(|poison| poison.into_inner());
    Some(names.get(name).copied().unwrap_or_else(|| {
        let interned: &'static str = Box::leak(name.to_owned().into_boxed_str());
        names.insert(interned);
        interned
    }))
}

/// The platform's font families, listed once (`queryInstalledFontFamilies`).
struct InstalledFonts(Vec<SharedString>);

impl Global for InstalledFonts {}

pub fn installed_fonts(cx: &mut App) -> &[SharedString] {
    if cx.try_global::<InstalledFonts>().is_none() {
        let names = cx.text_system().all_font_names().into_iter().filter(|name| !name.starts_with('.'));
        cx.set_global(InstalledFonts(names.map(SharedString::from).collect()));
    }
    &cx.global::<InstalledFonts>().0
}

/// `isMonospaceFamily`: every probe glyph has the same advance.
pub fn is_monospace(family: &str, cx: &App) -> bool {
    let text_system = cx.text_system();
    let id = text_system.resolve_font(&gpui_kit::font(SharedString::from(family.to_owned())));
    let size = gpui_kit::px(32.);
    let advances: Vec<f32> = ['i', 'M', 'W', '0', '@', '#', '.']
        .into_iter()
        .filter_map(|glyph| text_system.advance(id, size, glyph).ok().map(|advance| f32::from(advance.width)))
        .collect();
    advances.windows(2).all(|pair| (pair[0] - pair[1]).abs() < 0.01)
}

impl Tokens {
    /// A theme library palette over the standard tokens (`html[data-theme-id]` in `index.css`).
    fn themed(mut self, c: &ThemeColors) -> Self {
        self.background = c.canvas;
        self.foreground = c.text;
        self.card = c.surface;
        self.popover = c.surface_overlay;
        self.primary = c.message_action;
        self.primary_foreground = c.message_action_foreground;
        self.primary_hover = c.message_action_hover;
        self.secondary = c.secondary;
        self.muted = c.muted;
        self.muted_foreground = c.muted_foreground;
        self.accent = c.accent_surface;
        self.message_surface = c.message_surface;
        self.border = c.border;
        self.input = c.input;
        self.ring = c.focus;
        self.error = c.error;
        self.error_foreground = c.error_foreground;
        self.warning = c.warning;
        self.warning_foreground = c.warning_foreground;
        self.sidebar = SidebarTokens {
            background: c.sidebar,
            foreground: c.sidebar_foreground,
            muted_foreground: c.sidebar_muted_foreground,
            secondary_label: c.secondary_label,
            border: c.sidebar_border,
            row_hover: c.sidebar_row_hover,
            row_active: c.sidebar_row_active,
            icon: c.icon_muted,
        };
        self
    }

    /// The tokens a theme choice paints with, before the contrast and glass settings.
    pub fn for_choice(choice: &ThemeChoice, system_dark: bool) -> Self {
        let (dark, colors) = choice.resolve(system_dark);
        let base = if dark { Self::dark() } else { Self::light() };
        colors.map_or(base, |colors| base.themed(&colors))
    }

    /// The `--contrast-*` tokens (`index.css`): below 100% text fades toward its surface and
    /// borders toward transparent; above it text moves toward black (light) or white
    /// (dark) and borders toward the text color.
    fn with_appearance(mut self, appearance: Appearance) -> Self {
        let base = appearance.contrast.min(100.) / 100.;
        let boost = (appearance.contrast - 100.).max(0.) / 100.;
        let target = if self.dark { white(1.) } else { hsla(0., 0., 0., 1.) };
        let text = |color: Hsla, surface: Hsla| mix_oklab(mix_oklab(surface, color, base), target, boost);
        let line = |color: Hsla, ink: Hsla| mix_srgb(color.opacity(base), ink, boost / 4.);
        let (foreground, sidebar_foreground) = (self.foreground, self.sidebar.foreground);
        self.foreground = text(foreground, self.background);
        self.muted_foreground = text(self.muted_foreground, self.background);
        self.border = line(self.border, foreground);
        self.input = line(self.input, foreground);
        let sidebar = &mut self.sidebar;
        sidebar.foreground = text(sidebar_foreground, sidebar.background);
        sidebar.muted_foreground = text(sidebar.muted_foreground, sidebar.background);
        sidebar.secondary_label = text(sidebar.secondary_label, self.background);
        sidebar.icon = text(sidebar.icon, self.background);
        sidebar.border = line(sidebar.border, sidebar_foreground);
        if self.dark {
            sidebar.row_hover = with_alpha(sidebar.foreground, 0.08);
            sidebar.row_active = with_alpha(sidebar.foreground, 0.11);
        }
        self.glass = appearance.glass / 100.;
        self.fonts = appearance.fonts;
        self
    }
}

/// CSS `color-mix(in srgb, from, to t)`, with premultiplied alpha.
fn mix_srgb(from: Hsla, to: Hsla, t: f32) -> Hsla {
    let (a, b) = (from.to_rgb(), to.to_rgb());
    let alpha = a.a * (1. - t) + b.a * t;
    if alpha <= 0. {
        return gpui_kit::transparent_black();
    }
    let channel = |x: f32, y: f32| (x * a.a * (1. - t) + y * b.a * t) / alpha;
    gpui_kit::Rgba { r: channel(a.r, b.r), g: channel(a.g, b.g), b: channel(a.b, b.b), a: alpha }.into()
}

/// CSS `color-mix(in oklab, from, to t)` for opaque colors.
pub(crate) fn mix_oklab(from: Hsla, to: Hsla, t: f32) -> Hsla {
    if t <= 0. {
        return from;
    }
    let (a, b) = (oklab(from.to_rgb()), oklab(to.to_rgb()));
    let lab = [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t);
    let mut rgb = srgb_from_oklab(lab);
    rgb.a = from.a + (to.a - from.a) * t;
    rgb.into()
}

fn oklab(color: gpui_kit::Rgba) -> [f32; 3] {
    let linear = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    let (r, g, b) = (linear(color.r), linear(color.g), linear(color.b));
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

pub(crate) fn srgb_from_oklab([l, a, b]: [f32; 3]) -> gpui_kit::Rgba {
    let l_ = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m_ = (l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s_ = (l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    let gamma = |c: f32| {
        let c = c.clamp(0., 1.);
        if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1. / 2.4) - 0.055 }
    };
    gpui_kit::Rgba {
        r: gamma(4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_94 * s_),
        g: gamma(-1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_38 * s_),
        b: gamma(-0.004_196_086_3 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_),
        a: 1.,
    }
}

/// Applies the contrast, glass and typography settings now and on every later `sync`.
pub fn set_appearance(appearance: Appearance, window: &mut Window, cx: &mut App) {
    if cx.try_global::<Appearance>() == Some(&appearance) {
        return;
    }
    cx.set_global(appearance);
    sync(window, cx);
    window.refresh();
}

pub fn tokens(cx: &App) -> &Tokens {
    cx.global::<Tokens>()
}

/// The theme library pick: the appearance mode, the theme and the light/dark mix.
#[derive(Clone, Default)]
struct Preference(ThemeChoice);

impl Global for Preference {}

/// Applies the theme library pick now and on every later `sync`.
pub fn set_preference(choice: ThemeChoice, window: &mut Window, cx: &mut App) {
    cx.set_global(Preference(choice));
    sync(window, cx);
    window.refresh();
}

/// The theme editor's draft (`applyThemeColorPreview`): its colors and appearance paint the
/// app until it is cleared, over the stored pick.
#[derive(Clone, Copy, Default)]
struct EditorPreview(Option<(bool, ThemeColors)>);

impl Global for EditorPreview {}

pub fn set_editor_preview(preview: Option<(bool, ThemeColors)>, window: &mut Window, cx: &mut App) {
    if cx.try_global::<EditorPreview>().and_then(|p| p.0) == preview {
        return;
    }
    cx.set_global(EditorPreview(preview));
    sync(window, cx);
    window.refresh();
}

pub fn system_dark(window: &Window) -> bool {
    matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark)
}

/// Installs the tokens for the preferred appearance and points the
/// component library's palette at them. Call again when the appearance changes.
pub fn sync(window: &mut Window, cx: &mut App) {
    let choice = cx.try_global::<Preference>().map(|preference| preference.0.clone()).unwrap_or_default();
    let mut appearance = cx.try_global::<Appearance>().copied().unwrap_or_default();
    appearance.fonts = appearance.fonts.installed_or_default(cx);
    let base = match cx.try_global::<EditorPreview>().and_then(|preview| preview.0) {
        Some((dark, colors)) => if dark { Tokens::dark() } else { Tokens::light() }.themed(&colors),
        None => Tokens::for_choice(&choice, system_dark(window)),
    };
    let tokens = base.with_appearance(appearance);
    let (dark, fonts) = (tokens.dark, tokens.fonts);
    Theme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
    Theme::update(cx, |theme| {
        theme.font_family = fonts.sans.into();
        theme.mono_font_family = fonts.mono.into();
        // gpui-component's root sets the window's rem from this, like the web's `html` font size.
        theme.font_size = gpui_kit::px(fonts.interface_size);
        theme.mono_font_size = gpui_kit::px(fonts.code_size);
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
        colors.button_primary = tokens.primary;
        colors.button_primary_hover = tokens.primary_hover;
        colors.button_primary_active = tokens.primary_hover;
        colors.button_primary_foreground = tokens.primary_foreground;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_of(color: Hsla) -> (u32, f32) {
        let rgb = color.to_rgb();
        let byte = |c: f32| (c * 255.).round() as u32;
        ((byte(rgb.r) << 16) | (byte(rgb.g) << 8) | byte(rgb.b), (rgb.a * 1000.).round() / 1000.)
    }

    #[test]
    fn contrast_and_glass_follow_the_web_tokens() {
        let at = |contrast, dark| {
            let base = if dark { Tokens::dark() } else { Tokens::light() };
            base.with_appearance(Appearance { contrast, glass: 64., ..Appearance::default() })
        };
        let default = at(100., true);
        assert_eq!(hex_of(default.foreground), hex_of(Tokens::dark().foreground), "100% changes nothing");
        assert_eq!(hex_of(default.border), hex_of(Tokens::dark().border));
        assert_eq!(default.glass, 0.64);
        // 50%: text halfway to its surface (oklab), borders at half alpha.
        let low = at(50., true);
        assert_eq!(hex_of(low.border).1, 0.04);
        let low_text = hex_of(low.foreground).0 & 0xff;
        assert_eq!(low_text, 0x5f, "oklab midpoint of #f5f5f5 and black");
        // 200%: dark text all the way to white, light text to black.
        assert_eq!(hex_of(at(200., true).muted_foreground).0, 0xffffff);
        assert_eq!(hex_of(at(200., false).foreground).0, 0x000000);
    }

    #[test]
    fn fonts_follow_the_typography_settings() {
        let fonts = |stored: serde_json::Value| {
            Fonts::from_settings(&Settings::layered(&crate::settings::contract().client_defaults, &stored))
        };
        assert_eq!(fonts(serde_json::Value::Null), Fonts::default());
        // One monospace font for code and the terminal unless set separately; the prompt is `text-sm`.
        let simple = fonts(serde_json::json!({
            "fontFamilyCode": " \"JetBrains Mono\", monospace",
            "fontFamilyTerminal": "Cascadia Mono",
            "fontSizeInterface": 18,
            "fontSizeCode": 99,
        }));
        assert_eq!((simple.mono, simple.terminal, simple.composer), ("JetBrains Mono", "JetBrains Mono", FONT_SANS));
        assert_eq!(
            (simple.interface_size, simple.prompt_size, simple.code_size, simple.terminal_size),
            (18., 15.75, 18., 18.)
        );
        let separate = fonts(serde_json::json!({
            TYPOGRAPHY_ADVANCED_KEY: true,
            "fontFamilyCode": "Courier New",
            "fontFamilyTerminal": "Cascadia Mono",
            "fontSizePrompt": 16,
            "fontSizeTerminal": 11,
        }));
        assert_eq!((separate.terminal, separate.composer), ("Cascadia Mono", FONT_SANS));
        let unset = fonts(serde_json::json!({ TYPOGRAPHY_ADVANCED_KEY: true, "fontFamilyCode": "Courier New" }));
        assert_eq!(unset.terminal, FONT_MONO, "an unset terminal font is the default stack, not the code font");
        assert_eq!((separate.prompt_size, separate.terminal_size), (16., 11.));
    }

    #[test]
    fn a_theme_repaints_the_tokens_it_maps() {
        let grove = crate::themes::ThemeChoice { base: Some("grove".into()), ..Default::default() };
        let colors = crate::themes::built_in()[1].colors_for(true).unwrap();
        let tokens = Tokens::for_choice(&grove, true);
        assert!(tokens.dark);
        assert_eq!(hex_of(tokens.background), hex_of(colors.canvas));
        assert_eq!(hex_of(tokens.primary), hex_of(colors.message_action));
        assert_eq!(hex_of(tokens.sidebar.row_active), hex_of(colors.sidebar_row_active));
        assert_eq!(
            hex_of(Tokens::for_choice(&Default::default(), false).background),
            hex_of(Tokens::light().background)
        );
    }
}
