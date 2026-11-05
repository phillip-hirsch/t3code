//! The theme library: the built-in palettes (`BUILT_IN_THEMES` in
//! `packages/shared/src/themePalettes.ts`, generated into `themes.json` by `tools/theme_data.mjs`),
//! the saved themes (`t3code:themes:v1`) and the light/dark pick rules of `useTheme.ts` and
//! `themePalette.ts`.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use gpui_kit::{Hsla, Rgba, rgb};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::settings::ThemePreference;
use crate::theme_color;

macro_rules! theme_colors {
    ($($field:ident = $role:literal),* $(,)?) => {
        /// The `ThemeColors` roles this client paints with.
        #[derive(Clone, Copy, Debug, PartialEq)]
        pub struct ThemeColors { $(pub $field: Hsla,)* }

        impl ThemeColors {
            /// Paints `roles`; a role that is not a color takes `fallback`'s.
            pub fn from_roles(roles: &Roles, fallback: Option<&ThemeColors>) -> Option<Self> {
                Some(Self { $($field: parse_color(roles.get($role)).or(fallback.map(|f| f.$field))?,)* })
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

fn parse_color(value: &str) -> Option<Hsla> {
    let [r, g, b, a] = theme_color::to_srgba(value)?;
    Some(Rgba { r, g, b, a }.into())
}

#[derive(Deserialize)]
struct Standard {
    roles: Vec<String>,
    reserved: Vec<String>,
    light: HashMap<String, String>,
    dark: HashMap<String, String>,
}

fn standard_data() -> &'static Standard {
    static DATA: OnceLock<Standard> = OnceLock::new();
    DATA.get_or_init(|| serde_json::from_str(include_str!("theme_standard.json")).expect("theme_standard.json"))
}

/// `THEME_COLOR_ROLES`, in file order.
pub fn role_names() -> &'static [String] {
    &standard_data().roles
}

/// `RESERVED_THEME_IDS`: appearance keywords, built-ins and legacy aliases.
pub fn is_reserved(id: &str) -> bool {
    standard_data().reserved.iter().any(|reserved| reserved == id)
}

/// A value for every `THEME_COLOR_ROLES` role, in role order. Values may be mid-edit text.
#[derive(Clone, Debug, PartialEq)]
pub struct Roles(Vec<String>);

impl Roles {
    fn from_map(map: &HashMap<String, String>) -> Option<Self> {
        role_names().iter().map(|role| map.get(role).cloned()).collect::<Option<_>>().map(Self)
    }

    /// `fallback` with every role of `overrides` that holds a color, canonicalized
    /// (`lenientThemeColorOverrides`).
    pub fn layered(fallback: &Roles, overrides: &Map<String, Value>) -> Self {
        let mut roles = fallback.clone();
        for (role, value) in overrides {
            if let Some(color) = value.as_str().and_then(theme_color::canonical) {
                roles.set(role, color);
            }
        }
        roles
    }

    pub fn get(&self, role: &str) -> &str {
        role_names().iter().position(|name| name == role).map_or("", |index| &self.0[index])
    }

    pub fn set(&mut self, role: &str, value: String) {
        if let Some(index) = role_names().iter().position(|name| name == role) {
            self.0[index] = value;
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &str)> {
        role_names().iter().map(String::as_str).zip(self.0.iter().map(String::as_str))
    }

    /// Every role as a canonical color, or why one is not (`decodeThemeColors`).
    pub fn canonical(&self) -> Result<Self, String> {
        self.iter()
            .map(|(role, value)| {
                theme_color::canonical(value).ok_or_else(|| {
                    format!("The color for \"{role}\" must be a literal CSS color such as oklch(0.62 0.2 280).")
                })
            })
            .collect::<Result<_, _>>()
            .map(Self)
    }

    pub fn to_json(&self) -> Value {
        Value::Object(self.iter().map(|(role, value)| (role.to_owned(), Value::from(value))).collect())
    }
}

/// `getDefaultThemeColors`: T3 Chat's palette, which theme files layer over, canonical.
pub fn default_roles(dark: bool) -> &'static Roles {
    static ROLES: OnceLock<[Roles; 2]> = OnceLock::new();
    &ROLES.get_or_init(|| {
        let theme = find_built_in("t3-chat").expect("t3-chat");
        [&theme.roles, theme.variant_roles.as_ref().expect("t3-chat dark")].map(|r| r.canonical().expect("t3-chat"))
    })[dark as usize]
}

/// `getStandardThemeColors`: the stock T3 Code palette (its dark canvas is black), canonical.
pub fn standard_roles(dark: bool) -> &'static Roles {
    static ROLES: OnceLock<[Roles; 2]> = OnceLock::new();
    &ROLES.get_or_init(|| {
        let data = standard_data();
        let mut dark = data.dark.clone();
        dark.insert("canvas".into(), "#000000".into());
        [&data.light, &dark].map(|map| Roles::from_map(map).and_then(|r| r.canonical().ok()).expect("standard"))
    })[dark as usize]
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

#[derive(Clone, Debug, PartialEq)]
pub struct Collection {
    pub id: String,
    pub label: String,
}

/// One palette (`ThemeDefinition`): its own appearance plus, optionally, the other one.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeDefinition {
    pub id: String,
    pub label: String,
    pub dark: bool,
    pub roles: Roles,
    pub variant_roles: Option<Roles>,
    pub colors: ThemeColors,
    pub variant: Option<ThemeColors>,
    pub collection: Option<Collection>,
    /// Guided-editor output: Duplicate and Edit open in the guided editor.
    pub managed: bool,
}

impl ThemeDefinition {
    /// None when a role is not a color.
    pub fn new(id: String, label: String, dark: bool, roles: Roles, variant_roles: Option<Roles>) -> Option<Self> {
        Some(Self {
            colors: ThemeColors::from_roles(&roles, None)?,
            variant: match &variant_roles {
                Some(roles) => Some(ThemeColors::from_roles(roles, None)?),
                None => None,
            },
            id,
            label,
            dark,
            roles,
            variant_roles,
            collection: None,
            managed: false,
        })
    }

    /// `getThemeColorsForMode`.
    pub fn colors_for(&self, dark: bool) -> Option<&ThemeColors> {
        if dark == self.dark { Some(&self.colors) } else { self.variant.as_ref() }
    }

    /// `singleAppearanceOf`: the one appearance a palette has, if it lacks the other.
    pub fn single_appearance(&self) -> Option<bool> {
        self.variant.is_none().then_some(self.dark)
    }

    /// `getThemeModes`: light first.
    pub fn modes(&self) -> Vec<bool> {
        [false, true].into_iter().filter(|dark| self.colors_for(*dark).is_some()).collect()
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
                let roles = Roles::from_map(&file.colors).expect("built-in theme roles");
                let variant = file.variants.get(other).map(|map| Roles::from_map(map).expect("variant roles"));
                ThemeDefinition::new(file.id, file.label, dark, roles, variant).expect("built-in theme colors")
            })
            .collect()
    })
}

fn find_built_in(id: &str) -> Option<&'static ThemeDefinition> {
    built_in().iter().find(|theme| theme.id == id)
}

/// The saved themes (`t3code:themes:v1`). Clones share the list; edits return a new library.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Library(Arc<Vec<ThemeDefinition>>);

impl Library {
    pub fn new(themes: Vec<ThemeDefinition>) -> Self {
        Self(Arc::new(themes))
    }

    pub fn custom(&self) -> &[ThemeDefinition] {
        &self.0
    }

    /// `getThemeDefinition`: built-ins win, then saved themes. `system`, `light` and `dark`
    /// name the standard palette, not a theme.
    pub fn find(&self, id: &str) -> Option<&ThemeDefinition> {
        find_built_in(id).or_else(|| self.custom_theme(id))
    }

    pub fn custom_theme(&self, id: &str) -> Option<&ThemeDefinition> {
        self.0.iter().find(|theme| theme.id == id)
    }

    /// `installCustomTheme`.
    pub fn install(&self, theme: ThemeDefinition) -> Result<Self, String> {
        if is_reserved(&theme.id) {
            return Err(format!("The theme id \"{}\" is reserved.", theme.id));
        }
        if self.find(&theme.id).is_some() {
            return Err(format!("A theme named \"{}\" is already installed.", theme.label));
        }
        let mut themes = (*self.0).clone();
        themes.push(theme);
        Ok(Self::new(themes))
    }

    /// `updateCustomTheme`.
    pub fn update(&self, theme: ThemeDefinition) -> Result<Self, String> {
        if is_reserved(&theme.id) {
            return Err(format!("The theme id \"{}\" is reserved.", theme.id));
        }
        let Some(index) = self.0.iter().position(|existing| existing.id == theme.id) else {
            return Err(format!("The theme \"{}\" is not installed.", theme.label));
        };
        let mut themes = (*self.0).clone();
        themes[index] = theme;
        Ok(Self::new(themes))
    }

    /// `removeCustomThemes`.
    pub fn remove(&self, ids: &[String]) -> Self {
        Self::new(self.0.iter().filter(|theme| !ids.contains(&theme.id)).cloned().collect())
    }

    /// `replaceCustomThemeCollection`: the collection's themes in place of its old ones.
    pub fn replace_collection(&self, collection_id: &str, themes: Vec<ThemeDefinition>) -> Result<Self, String> {
        let in_collection = |theme: &ThemeDefinition| theme.collection.as_ref().is_some_and(|c| c.id == collection_id);
        let ids: std::collections::HashSet<_> = themes.iter().map(|theme| &theme.id).collect();
        if themes.is_empty() || ids.len() != themes.len() || !themes.iter().all(in_collection) {
            return Err("That theme collection is invalid.".into());
        }
        if let Some(conflict) = themes.iter().find(|theme| {
            is_reserved(&theme.id)
                || find_built_in(&theme.id).is_some()
                || self.0.iter().any(|existing| !in_collection(existing) && existing.id == theme.id)
        }) {
            return Err(format!("A theme named \"{}\" is already installed.", conflict.label));
        }
        let mut next = Vec::with_capacity(self.0.len() + themes.len());
        let mut replacement = Some(themes);
        for theme in self.0.iter() {
            if !in_collection(theme) {
                next.push(theme.clone());
            } else if let Some(themes) = replacement.take() {
                next.extend(themes);
            }
        }
        next.extend(replacement.into_iter().flatten());
        Ok(Self::new(next))
    }
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

    pub fn set(&mut self, dark: bool, id: Option<String>) {
        *(if dark { &mut self.dark } else { &mut self.light }) = id;
    }
}

/// Everything the theme library stores: the appearance mode, the whole theme in use, the
/// theme each appearance uses instead of it, and the saved themes they may name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThemeChoice {
    pub mode: ThemePreference,
    pub base: CardId,
    pub halves: ThemeHalves,
    pub library: Library,
}

impl ThemeChoice {
    /// A half that names a theme with colors for its appearance (`parseThemeHalves`).
    fn half(&self, dark: bool) -> Option<&ThemeDefinition> {
        self.halves.get(dark).and_then(|id| self.library.find(id)).filter(|theme| theme.colors_for(dark).is_some())
    }

    fn base_theme(&self) -> Option<&ThemeDefinition> {
        self.base.as_deref().and_then(|id| self.library.find(id))
    }

    /// `resolveThemeAppearance` then `resolveThemeHalf`: whether the app is dark, and the
    /// palette it paints with (`None` for the standard one).
    pub fn resolve(&self, system_dark: bool) -> (bool, Option<ThemeColors>) {
        let dark = match self.mode {
            ThemePreference::System => system_dark,
            ThemePreference::Light => false,
            ThemePreference::Dark => true,
        };
        if let Some(theme) = self.half(dark) {
            return (dark, theme.colors_for(dark).copied());
        }
        match self.base_theme() {
            Some(theme) if theme.colors_for(dark).is_none() => (theme.dark, Some(theme.colors)),
            Some(theme) => (dark, theme.colors_for(dark).copied()),
            None => (dark, None),
        }
    }

    /// The card that owns an appearance (`lightOwner` / `darkOwner`).
    pub fn owner(&self, dark: bool) -> CardId {
        self.half(dark).map(|theme| theme.id.clone()).or_else(|| self.base_theme().map(|theme| theme.id.clone()))
    }

    /// `setTheme`: a whole theme replaces the mix.
    pub fn use_theme(&mut self, base: CardId) {
        self.base = base.filter(|id| self.library.find(id).is_some());
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
        match card.as_deref().and_then(|id| self.library.find(id)).and_then(ThemeDefinition::single_appearance) {
            Some(dark) => self.assign_half(dark, card),
            None => self.use_theme(card),
        }
    }

    /// `handleConfirmRemoveTheme`: the pick once `removed` themes are gone. A removed base
    /// falls back to the standard palette; a removed half is cleared.
    pub fn without(&self, removed: &[String]) -> Self {
        let mut next = self.clone();
        let gone = |id: Option<&str>| id.is_some_and(|id| removed.iter().any(|r| r == id));
        if gone(self.base.as_deref()) {
            next.base = None;
        }
        for dark in [false, true] {
            if gone(self.halves.get(dark)) {
                next.halves.set(dark, None);
            }
        }
        next
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

    pub fn of(colors: &ThemeColors) -> Self {
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
    pub fn of_card(card: Option<&str>, dark: bool, library: &Library) -> Self {
        match card.and_then(|id| library.find(id)) {
            Some(theme) => Self::of(theme.colors_for(dark).unwrap_or(&theme.colors)),
            None => Self::standard(dark),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(mode: ThemePreference, base: Option<&str>) -> ThemeChoice {
        ThemeChoice { mode, base: base.map(str::to_owned), ..Default::default() }
    }

    fn colors(id: &str, dark: bool) -> Option<ThemeColors> {
        find_built_in(id).unwrap().colors_for(dark).copied()
    }

    #[test]
    fn built_ins_parse_with_both_appearances() {
        let ids: Vec<_> = built_in().iter().map(|theme| theme.id.as_str()).collect();
        assert_eq!(ids, ["t3-chat", "grove", "ocean", "ember", "iris"]);
        assert!(built_in().iter().all(|theme| theme.single_appearance().is_none()));
        // oklch(1 0 0) is white; T3 Chat's light overlay surface.
        let white = find_built_in("t3-chat").unwrap().colors.surface_overlay.to_rgb();
        assert!((white.r - 1.).abs() < 1e-3 && (white.g - 1.).abs() < 1e-3 && (white.b - 1.).abs() < 1e-3);
        assert_eq!(parse_color("#ff0000").map(|c| c.to_rgb().r), Some(1.));
        assert_eq!(parse_color("var(--x)"), None);
        assert_eq!(role_names().len(), 61);
        assert!(is_reserved("dark") && is_reserved("t3-grove") && !is_reserved("aurora"));
        assert_eq!(standard_roles(true).get("canvas"), "oklch(0 0 0)");
    }

    #[test]
    fn halves_win_over_the_base_for_their_appearance() {
        let mut picked = choice(ThemePreference::System, Some("grove"));
        assert_eq!(picked.resolve(true), (true, colors("grove", true)));
        picked.assign_half(true, Some("ocean".into()));
        assert_eq!(picked.resolve(true).1, colors("ocean", true));
        assert_eq!(picked.resolve(false).1, colors("grove", false));
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
        assert_eq!(light.resolve(false).1, colors("ember", false));
        assert_eq!(picked.resolve(true), (true, None));
        // A whole theme clears the mix; the standard card clears everything.
        picked.use_card(Some("iris".into()));
        assert_eq!((picked.base.as_deref(), &picked.halves), (Some("iris"), &ThemeHalves::default()));
        picked.use_card(None);
        assert_eq!(picked, choice(ThemePreference::Dark, None));
    }

    fn saved(id: &str, dark: bool) -> ThemeDefinition {
        ThemeDefinition::new(id.into(), id.into(), dark, default_roles(dark).clone(), None).unwrap()
    }

    #[test]
    fn saved_themes_resolve_and_one_appearance_themes_take_their_half() {
        let library = Library::default().install(saved("aurora", true)).unwrap();
        assert_eq!(
            library.install(saved("aurora", true)).unwrap_err(),
            "A theme named \"aurora\" is already installed."
        );
        assert!(library.install(saved("grove", true)).is_err(), "built-in ids are taken");
        assert!(library.install(saved("system", true)).unwrap_err().contains("reserved"));
        let mut picked = ThemeChoice { library: library.clone(), ..Default::default() };
        picked.use_card(Some("aurora".into()));
        assert_eq!((picked.base.as_deref(), picked.halves.get(true)), (None, Some("aurora")));
        assert_eq!(picked.resolve(true).1, Some(library.custom()[0].colors));
        // Removing it clears the half it held.
        assert_eq!(picked.without(&["aurora".into()]).halves, ThemeHalves::default());
        assert!(library.remove(&["aurora".into()]).custom().is_empty());
    }

    #[test]
    fn a_collection_replaces_its_themes_in_place() {
        let collection = Collection { id: "open-vsx:a.b".into(), label: "A".into() };
        let member = |id: &str| ThemeDefinition { collection: Some(collection.clone()), ..saved(id, false) };
        let library = Library::new(vec![saved("first", false), member("old"), saved("last", false)]);
        let next = library.replace_collection(&collection.id, vec![member("new-1"), member("new-2")]).unwrap();
        let ids: Vec<_> = next.custom().iter().map(|theme| theme.id.as_str()).collect();
        assert_eq!(ids, ["first", "new-1", "new-2", "last"]);
        assert!(library.replace_collection(&collection.id, vec![member("first")]).is_err(), "id taken outside");
        assert!(library.replace_collection(&collection.id, vec![saved("loose", false)]).is_err());
    }
}
