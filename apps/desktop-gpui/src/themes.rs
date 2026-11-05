//! The theme library: the built-in palettes (`BUILT_IN_THEMES` in
//! `packages/shared/src/themePalettes.ts`, generated into `themes.json` by `tools/theme_data.mjs`)
//! and the light/dark pick rules of `useTheme.ts` and `themePalette.ts`.

use std::collections::HashMap;
use std::sync::OnceLock;

use gpui_kit::{Hsla, rgb};
use serde::Deserialize;

use crate::settings::ThemePreference;

macro_rules! theme_colors {
    ($($field:ident = $role:literal),* $(,)?) => {
        /// The `ThemeColors` roles this client paints with.
        #[derive(Clone, Copy, Debug, PartialEq)]
        pub struct ThemeColors { $(pub $field: Hsla,)* }

        impl ThemeColors {
            fn from_roles(roles: &HashMap<String, String>) -> Option<Self> {
                Some(Self { $($field: parse_color(roles.get($role)?)?,)* })
            }
        }
    };
}

theme_colors! {
    canvas = "canvas",
    text = "text",
    surface = "surface",
    surface_overlay = "surfaceOverlay",
    secondary = "secondary",
    muted = "muted",
    muted_foreground = "mutedForeground",
    accent = "accent",
    accent_surface = "accentSurface",
    message_surface = "messageSurface",
    message_action = "messageAction",
    message_action_foreground = "messageActionForeground",
    message_action_hover = "messageActionHover",
    border = "border",
    input = "input",
    focus = "focus",
    error = "error",
    error_foreground = "errorForeground",
    warning = "warning",
    warning_foreground = "warningForeground",
    secondary_label = "secondaryLabel",
    icon_muted = "iconMuted",
    sidebar = "sidebar",
    sidebar_foreground = "sidebarForeground",
    sidebar_muted_foreground = "sidebarMutedForeground",
    sidebar_row_hover = "sidebarRowHover",
    sidebar_row_active = "sidebarRowActive",
    sidebar_border = "sidebarBorder",
}

/// `#rrggbb` or `oklch(L C H)`, the two forms the built-in palettes use.
fn parse_color(value: &str) -> Option<Hsla> {
    if let Some(hex) = value.strip_prefix('#') {
        return (hex.len() == 6).then(|| u32::from_str_radix(hex, 16).ok()).flatten().map(|v| rgb(v).into());
    }
    let inner = value.strip_prefix("oklch(")?.strip_suffix(')')?;
    let [l, c, h] = inner.split_whitespace().map(str::parse::<f32>).collect::<Result<Vec<_>, _>>().ok()?[..] else {
        return None;
    };
    let (sin, cos) = h.to_radians().sin_cos();
    Some(crate::theme::srgb_from_oklab([l, c * cos, c * sin]).into())
}

#[derive(Deserialize)]
struct ThemeFile {
    id: String,
    label: String,
    appearance: String,
    colors: HashMap<String, String>,
    #[serde(default)]
    variants: HashMap<String, HashMap<String, String>>,
}

/// One palette (`ThemeDefinition`): its own appearance plus, optionally, the other one.
pub struct ThemeDefinition {
    pub id: String,
    pub label: String,
    pub dark: bool,
    pub colors: ThemeColors,
    pub variant: Option<ThemeColors>,
}

impl ThemeDefinition {
    /// `getThemeColorsForMode`.
    pub fn colors_for(&self, dark: bool) -> Option<&ThemeColors> {
        if dark == self.dark { Some(&self.colors) } else { self.variant.as_ref() }
    }

    /// `singleAppearanceOf`: the one appearance a palette has, if it lacks the other.
    pub fn single_appearance(&self) -> Option<bool> {
        self.variant.is_none().then_some(self.dark)
    }
}

pub fn built_in() -> &'static [ThemeDefinition] {
    static THEMES: OnceLock<Vec<ThemeDefinition>> = OnceLock::new();
    THEMES.get_or_init(|| {
        let files: Vec<ThemeFile> = serde_json::from_str(include_str!("themes.json")).expect("themes.json");
        files
            .into_iter()
            .map(|file| {
                let dark = file.appearance == "dark";
                let other = if dark { "light" } else { "dark" };
                ThemeDefinition {
                    colors: ThemeColors::from_roles(&file.colors).expect("built-in theme colors"),
                    variant: file.variants.get(other).map(|roles| ThemeColors::from_roles(roles).expect("variant")),
                    id: file.id,
                    label: file.label,
                    dark,
                }
            })
            .collect()
    })
}

/// `getThemeDefinition`; `system`, `light` and `dark` name the standard palette, not a theme.
pub fn find(id: &str) -> Option<&'static ThemeDefinition> {
    built_in().iter().find(|theme| theme.id == id)
}

/// The theme a card stands for: `None` is the standard "T3 Code" card.
pub type CardId = Option<String>;

/// The per-appearance theme mix (`ThemeHalves`, `t3code:theme-halves:v1`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThemeHalves {
    pub light: Option<String>,
    pub dark: Option<String>,
}

impl ThemeHalves {
    pub fn get(&self, dark: bool) -> Option<&str> {
        if dark { self.dark.as_deref() } else { self.light.as_deref() }
    }

    fn set(&mut self, dark: bool, id: Option<String>) {
        *(if dark { &mut self.dark } else { &mut self.light }) = id;
    }
}

/// Everything the theme library stores: the appearance mode, the whole theme in use, and
/// the theme each appearance uses instead of it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThemeChoice {
    pub mode: ThemePreference,
    pub base: CardId,
    pub halves: ThemeHalves,
}

impl ThemeChoice {
    /// A half that names a theme with colors for its appearance (`parseThemeHalves`).
    fn half(&self, dark: bool) -> Option<&'static ThemeDefinition> {
        self.halves.get(dark).and_then(find).filter(|theme| theme.colors_for(dark).is_some())
    }

    fn base_theme(&self) -> Option<&'static ThemeDefinition> {
        self.base.as_deref().and_then(find)
    }

    /// `resolveThemeAppearance` then `resolveThemeHalf`: whether the app is dark, and the
    /// palette it paints with (`None` for the standard one).
    pub fn resolve(&self, system_dark: bool) -> (bool, Option<&'static ThemeColors>) {
        let dark = match self.mode {
            ThemePreference::System => system_dark,
            ThemePreference::Light => false,
            ThemePreference::Dark => true,
        };
        if let Some(theme) = self.half(dark) {
            return (dark, theme.colors_for(dark));
        }
        match self.base_theme() {
            Some(theme) if theme.colors_for(dark).is_none() => (theme.dark, Some(&theme.colors)),
            Some(theme) => (dark, theme.colors_for(dark)),
            None => (dark, None),
        }
    }

    /// The card that owns an appearance (`lightOwner` / `darkOwner`).
    pub fn owner(&self, dark: bool) -> CardId {
        self.half(dark).map(|theme| theme.id.clone()).or_else(|| self.base_theme().map(|theme| theme.id.clone()))
    }

    /// `setTheme`: a whole theme replaces the mix.
    pub fn use_theme(&mut self, base: CardId) {
        self.base = base.filter(|id| find(id).is_some());
        self.halves = ThemeHalves::default();
    }

    /// `assignHalf`: one appearance uses `card`. Restoring the standard palette for one
    /// appearance moves the themed base to the other appearance's half.
    pub fn assign_half(&mut self, dark: bool, card: CardId) {
        if card.is_none() && self.base_theme().is_some() {
            let other = self.halves.get(!dark).map(str::to_owned).or_else(|| self.base.clone());
            self.use_theme(None);
            self.halves.set(!dark, other);
            return;
        }
        self.halves.set(dark, card);
    }

    /// `onUse` on a card: a two-appearance theme becomes the whole theme; a one-appearance
    /// theme takes its own half.
    pub fn use_card(&mut self, card: CardId) {
        match card.as_deref().and_then(find).and_then(ThemeDefinition::single_appearance) {
            Some(dark) => self.assign_half(dark, card),
            None => self.use_theme(card),
        }
    }
}

/// The seven colors a theme card and wireframe show (`ThemeCardPreview`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preview {
    pub sidebar: Hsla,
    pub canvas: Hsla,
    pub surface: Hsla,
    pub accent_surface: Hsla,
    pub accent: Hsla,
    pub message_surface: Hsla,
    pub message_action: Hsla,
}

impl Preview {
    /// `STANDARD_THEME_PREVIEW_COLORS`.
    pub fn standard(dark: bool) -> Self {
        let hex = |value: u32| -> Hsla { rgb(value).into() };
        if dark {
            Self {
                sidebar: hex(0x0f0f10),
                canvas: hex(0x0a0a0a),
                surface: hex(0x121212),
                accent_surface: hex(0x27272a),
                accent: hex(0x1c1c1f),
                message_surface: hex(0x27272a),
                message_action: hex(0x8b9cff),
            }
        } else {
            Self {
                sidebar: hex(0xfafafa),
                canvas: hex(0xfcfcfc),
                surface: hex(0xffffff),
                accent_surface: hex(0xf4f4f5),
                accent: hex(0xf4f4f5),
                message_surface: hex(0xe4e4e7),
                message_action: hex(0x4f46e5),
            }
        }
    }

    fn of(colors: &ThemeColors) -> Self {
        Self {
            sidebar: colors.sidebar,
            canvas: colors.canvas,
            surface: colors.surface,
            accent_surface: colors.accent_surface,
            accent: colors.accent,
            message_surface: colors.message_surface,
            message_action: colors.message_action,
        }
    }

    /// `pickColors`: the card's colors for `dark`, else its only appearance's.
    pub fn of_card(card: Option<&str>, dark: bool) -> Self {
        match card.and_then(find) {
            Some(theme) => Self::of(theme.colors_for(dark).unwrap_or(&theme.colors)),
            None => Self::standard(dark),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(mode: ThemePreference, base: Option<&str>) -> ThemeChoice {
        ThemeChoice { mode, base: base.map(str::to_owned), halves: ThemeHalves::default() }
    }

    #[test]
    fn built_ins_parse_with_both_appearances() {
        let ids: Vec<_> = built_in().iter().map(|theme| theme.id.as_str()).collect();
        assert_eq!(ids, ["t3-chat", "grove", "ocean", "ember", "iris"]);
        assert!(built_in().iter().all(|theme| theme.single_appearance().is_none()));
        // oklch(1 0 0) is white; T3 Chat's light overlay surface.
        let white = find("t3-chat").unwrap().colors.surface_overlay.to_rgb();
        assert!((white.r - 1.).abs() < 1e-3 && (white.g - 1.).abs() < 1e-3 && (white.b - 1.).abs() < 1e-3);
        assert_eq!(parse_color("#ff0000").map(|c| c.to_rgb().r), Some(1.));
        assert_eq!(parse_color("rgb(1 2 3)"), None);
    }

    #[test]
    fn halves_win_over_the_base_for_their_appearance() {
        let mut picked = choice(ThemePreference::System, Some("grove"));
        assert_eq!(picked.resolve(true), (true, find("grove").unwrap().colors_for(true)));
        picked.assign_half(true, Some("ocean".into()));
        assert_eq!(picked.resolve(true).1, find("ocean").unwrap().colors_for(true));
        assert_eq!(picked.resolve(false).1, find("grove").unwrap().colors_for(false));
        assert_eq!((picked.owner(false), picked.owner(true)), (Some("grove".into()), Some("ocean".into())));
        // An unknown half is ignored, as `parseThemeHalves` drops it.
        picked.halves.dark = Some("gone".into());
        assert_eq!(picked.owner(true), Some("grove".into()));
    }

    #[test]
    fn restoring_the_standard_palette_for_one_appearance_keeps_the_theme_on_the_other() {
        let mut picked = choice(ThemePreference::Dark, Some("ember"));
        picked.assign_half(true, None);
        assert_eq!(picked.base, None);
        assert_eq!(picked.halves, ThemeHalves { light: Some("ember".into()), dark: None });
        let light = ThemeChoice { mode: ThemePreference::Light, ..picked.clone() };
        assert_eq!(light.resolve(false).1, find("ember").unwrap().colors_for(false));
        assert_eq!(picked.resolve(true), (true, None));
        // A whole theme clears the mix; the standard card clears everything.
        picked.use_card(Some("iris".into()));
        assert_eq!((picked.base.as_deref(), &picked.halves), (Some("iris"), &ThemeHalves::default()));
        picked.use_card(None);
        assert_eq!(picked, choice(ThemePreference::Dark, None));
    }
}
