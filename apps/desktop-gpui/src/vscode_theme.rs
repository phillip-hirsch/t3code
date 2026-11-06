//! VS Code color themes as T3 Code palettes (`vscodeThemeImport.ts`), plus the JSONC reading
//! the import and Open VSX share.

use serde_json::{Map, Value, json};

use crate::theme_color::{self, Rgb, contrast, is_dark, readable_on, rgb_hex, vivid};
use crate::theme_file::{self, THEME_FILE_VERSION, parse_theme_file, theme_id_from_name};
use crate::themes::{self, ThemeDefinition};

/// `jsonc-parser`'s `parse` with trailing commas allowed: comments and trailing commas out,
/// then strict JSON.
pub fn parse_jsonc(source: &str) -> Option<Value> {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let (mut i, mut in_string) = (0, false);
    let mut pending_comma: Option<usize> = None;
    while i < bytes.len() {
        let c = bytes[i];
        let len = source[i..].chars().next().map_or(1, char::len_utf8);
        if in_string {
            let end = if c == b'\\' { i + 2 } else { i + len };
            out.push_str(source.get(i..end.min(bytes.len()))?);
            in_string = c != b'"';
            i = end;
            continue;
        }
        match c {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = source[i + 2..].find("*/").map(|end| i + 2 + end + 2)?;
                continue;
            }
            b'}' | b']' if pending_comma.is_some() => {
                out.remove(pending_comma.take()?);
            }
            b',' => pending_comma = Some(out.len()),
            c if !c.is_ascii_whitespace() => pending_comma = None,
            _ => {}
        }
        in_string = c == b'"';
        if c == b'"' {
            pending_comma = None;
        }
        out.push_str(&source[i..i + len]);
        i += len;
    }
    serde_json::from_str(&out).ok()
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rgba {
    r: f64,
    g: f64,
    b: f64,
    a: f64,
}

impl Rgba {
    fn rgb(self) -> Rgb {
        Rgb { r: self.r, g: self.g, b: self.b }
    }
}

fn decode_gamma(value: f64) -> f64 {
    if value <= 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
}

fn encode_gamma(value: f64) -> f64 {
    let c = value.clamp(0., 1.);
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1. / 2.4) - 0.055 }
}

fn number_or_percent(part: &str) -> Option<f64> {
    let value = match part.strip_suffix('%') {
        Some(percent) => percent.parse::<f64>().ok()? / 100.,
        None => part.parse::<f64>().ok()?,
    };
    value.is_finite().then_some(value)
}

/// `color(display-p3 r g b / a)` and `color(srgb ...)`.
fn parse_color_function(value: &str) -> Option<Rgba> {
    let body = value.strip_prefix("color(")?.strip_suffix(')')?.trim();
    let (space, rest) = body.split_once(char::is_whitespace)?;
    let space = space.to_ascii_lowercase();
    let (channels, alpha) = match rest.split_once('/') {
        Some((channels, alpha)) => (channels, number_or_percent(alpha.trim())?),
        None => (rest, 1.),
    };
    let channels: Vec<f64> = channels.split_whitespace().map(number_or_percent).collect::<Option<_>>()?;
    let [red, green, blue] = channels[..] else { return None };
    let a = alpha.clamp(0., 1.);
    match space.as_str() {
        "srgb" => Some(Rgba { r: red * 255., g: green * 255., b: blue * 255., a }),
        "display-p3" => {
            let [r, g, b] = [red, green, blue].map(decode_gamma);
            let srgb = [
                1.2249401762805 * r - 0.2249401762805 * g,
                -0.042056961239 * r + 1.042056961239 * g,
                -0.0196375547643 * r - 0.0786360655012 * g + 1.0982736202656 * b,
            ]
            .map(|channel| encode_gamma(channel) * 255.);
            Some(Rgba { r: srgb[0], g: srgb[1], b: srgb[2], a })
        }
        _ => None,
    }
}

fn parse_vscode_color(value: &Value) -> Option<Rgba> {
    let trimmed = value.as_str()?.trim();
    if trimmed.starts_with("color(") {
        return parse_color_function(trimmed);
    }
    let hex = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if !matches!(hex.len(), 3 | 4 | 6 | 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |s: &str| u8::from_str_radix(s, 16).ok().map(f64::from);
    let short = |i: usize| byte(&hex[i..=i].repeat(2));
    if hex.len() <= 4 {
        let a = if hex.len() == 4 { short(3)? / 255. } else { 1. };
        return Some(Rgba { r: short(0)?, g: short(1)?, b: short(2)?, a });
    }
    let a = if hex.len() == 8 { byte(&hex[6..8])? / 255. } else { 1. };
    Some(Rgba { r: byte(&hex[0..2])?, g: byte(&hex[2..4])?, b: byte(&hex[4..6])?, a })
}

/// Composites a translucent VS Code overlay onto its surface; our roles are opaque.
fn flatten_over(color: Rgba, base: Rgb) -> String {
    if color.a >= 1. {
        return rgb_hex(color.rgb());
    }
    let mixed = |c: f64, b: f64| c * color.a + b * (1. - color.a);
    rgb_hex(Rgb { r: mixed(color.r, base.r), g: mixed(color.g, base.g), b: mixed(color.b, base.b) })
}

fn hex_to_rgb(value: &str) -> Rgb {
    let hex = theme_color::to_hex(value).unwrap_or_else(|| value.to_owned());
    parse_vscode_color(&Value::from(hex)).map_or(Rgb { r: 0., g: 0., b: 0. }, Rgba::rgb)
}

/// `isVsCodeThemeFile`: workbench colors (dotted keys) or token colors, not a T3 Code file.
pub fn is_vscode_theme(value: &Value) -> bool {
    let Some(object) = value.as_object() else { return false };
    if object.get("version") == Some(&json!(THEME_FILE_VERSION)) {
        return false;
    }
    let workbench = object.get("colors").and_then(Value::as_object).is_some_and(|c| c.keys().any(|k| k.contains('.')));
    workbench || object.get("tokenColors").is_some_and(Value::is_array)
}

/// `parseThemeImport`: a T3 Code theme file or a VS Code color theme.
pub fn parse_theme_import(source: &str) -> Result<ThemeDefinition, String> {
    let Some(value) = parse_jsonc(source) else { return Err("That theme file is not valid JSON.".into()) };
    if is_vscode_theme(&value) {
        return parse_vscode_theme(&value);
    }
    let strict: Value = serde_json::from_str(source).map_err(|error| error.to_string())?;
    parse_theme_file(&strict)
}

/// `humanizeThemeName`: `one-dark_pro` reads "One Dark Pro"; names with spaces stay.
pub fn humanize_theme_name(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.contains(char::is_whitespace) || !trimmed.contains(['-', '_', '.']) {
        return trimmed.to_owned();
    }
    trimmed
        .split(['-', '_', '.'])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn resolve_name(value: &Value) -> String {
    for candidate in [&value["displayName"], &value["name"]] {
        if let Some(candidate) = candidate.as_str() {
            let humanized = humanize_theme_name(candidate);
            if !humanized.is_empty() {
                return humanized.chars().take(48).collect();
            }
        }
    }
    "VS Code theme".into()
}

/// `parseVsCodeThemeFile`: a palette seeded from the editor background and an accent, with
/// every workbench color that maps onto a role and stays readable.
pub fn parse_vscode_theme(value: &Value) -> Result<ThemeDefinition, String> {
    let Some(object) = value.as_object() else { return Err("Theme files must contain a JSON object.".into()) };
    let empty = Map::new();
    let colors = object.get("colors").and_then(Value::as_object).unwrap_or(&empty);
    let pick = |keys: &[&str]| keys.iter().find_map(|key| colors.get(*key).and_then(parse_vscode_color));
    let solid_over = |base: Rgb, keys: &[&str]| pick(keys).map(|color| flatten_over(color, base));

    let Some(canvas_color) = pick(&["editor.background", "editorPane.background"]) else {
        return Err(
            "That VS Code theme has no \"editor.background\" color, so there is nothing to build a palette from."
                .into(),
        );
    };
    let canvas = canvas_color.rgb();
    let dark = match value["type"].as_str().map(str::to_ascii_lowercase).as_deref() {
        Some("light" | "hc-light") => false,
        Some("dark" | "hc-black") => true,
        _ => is_dark(canvas),
    };
    let canvas_hex = rgb_hex(canvas);
    let raised_candidate = solid_over(canvas, &["editorWidget.background", "dropdown.background"]);
    let stands_apart = |first: &str, second: &str| contrast(hex_to_rgb(first), hex_to_rgb(second)) >= 1.1;
    let apart_from_surfaces = |candidate: &str| {
        stands_apart(candidate, &canvas_hex)
            && raised_candidate.as_deref().is_none_or(|raised| stands_apart(candidate, raised))
    };

    let mut accent = None;
    for key in [
        "focusBorder",
        "button.background",
        "textLink.foreground",
        "activityBarBadge.background",
        "progressBar.background",
        "badge.background",
    ] {
        let Some(candidate) = colors.get(key).and_then(parse_vscode_color) else { continue };
        let candidate_hex = flatten_over(candidate, canvas);
        if apart_from_surfaces(&candidate_hex) {
            accent = Some((candidate, candidate_hex));
            break;
        }
    }
    let standard = themes::standard_roles(dark);
    let (accent_color, accent_hex) = accent.unwrap_or_else(|| {
        let standard_accent = theme_color::to_hex(standard.get("accent")).unwrap_or_default();
        let hex = [standard_accent.as_str(), "#ffffff", "#000000"]
            .into_iter()
            .find(|candidate| apart_from_surfaces(candidate))
            .unwrap_or(&standard_accent)
            .to_owned();
        (parse_vscode_color(&Value::from(hex.as_str())).unwrap_or(Rgba { r: 0., g: 0., b: 0., a: 1. }), hex)
    });

    let muted_accent = flatten_over(Rgba { a: 0.2, ..accent_color }, canvas);
    let derived = vivid(dark, &canvas_hex, &muted_accent);
    let d = |role: &str| derived.get(role).to_owned();
    let sidebar_hex =
        solid_over(canvas, &["sideBar.background", "activityBar.background"]).unwrap_or_else(|| d("sidebar"));
    let sidebar = hex_to_rgb(&sidebar_hex);
    let terminal_hex =
        solid_over(canvas, &["terminal.background", "panel.background"]).unwrap_or_else(|| d("terminalBackground"));
    let terminal = hex_to_rgb(&terminal_hex);

    let readable_over = |surface: &str, fallback: String, keys: &[&str]| {
        let surface_rgb = hex_to_rgb(surface);
        let readable = |candidate: &str| contrast(hex_to_rgb(candidate), surface_rgb) >= 4.5;
        match solid_over(surface_rgb, keys) {
            Some(specified) if readable(&specified) => specified,
            _ if readable(&fallback) => fallback,
            _ => if is_dark(surface_rgb) { "#ffffff" } else { "#000000" }.to_owned(),
        }
    };
    // Status colors also fill destructive buttons with white text: a dim one is lightened
    // along its hue; one hidden in the canvas has no hue to keep.
    let readable_status = |fallback: String, keys: &[&str]| {
        let base =
            solid_over(canvas, keys).filter(|specified| stands_apart(specified, &canvas_hex)).unwrap_or(fallback);
        readable_on(&base, &canvas_hex, 4.5)
    };

    let surface_raised = raised_candidate.clone().unwrap_or_else(|| d("surfaceRaised"));
    let action = solid_over(canvas, &["button.background"])
        .filter(|button| stands_apart(button, &canvas_hex) && stands_apart(button, &surface_raised))
        .unwrap_or_else(|| accent_hex.clone());
    let standard_input = standard.get("input").to_owned();
    let input_candidates =
        [d("input"), d("surfaceRaised"), standard_input, "#000000".into(), "#ffffff".into(), "#808080".into()];
    let mut input = input_candidates
        .into_iter()
        .find(|candidate| stands_apart(candidate, &canvas_hex) && stands_apart(candidate, &action))
        .unwrap_or_else(|| "#808080".into());
    for key in ["input.background", "input.border"] {
        if let Some(candidate) = solid_over(canvas, &[key])
            && stands_apart(&candidate, &canvas_hex)
            && stands_apart(&candidate, &action)
        {
            input = candidate;
            break;
        }
    }
    let search_match =
        solid_over(canvas, &["editor.findMatchHighlightBackground"]).unwrap_or_else(|| d("searchMatchBackground"));
    let search_active =
        solid_over(canvas, &["editor.findMatchBackground"]).unwrap_or_else(|| d("searchMatchActiveBackground"));

    let overrides: Vec<(&str, String)> = vec![
        ("canvas", canvas_hex.clone()),
        ("text", readable_over(&canvas_hex, d("text"), &["editor.foreground", "foreground"])),
        ("textMuted", readable_over(&canvas_hex, d("textMuted"), &["descriptionForeground", "disabledForeground"])),
        ("surface", solid_over(canvas, &["editorWidget.background"]).unwrap_or_else(|| d("surface"))),
        ("surfaceRaised", surface_raised.clone()),
        (
            "surfaceOverlay",
            solid_over(canvas, &["menu.background", "quickInput.background", "dropdown.background"])
                .unwrap_or_else(|| d("surfaceOverlay")),
        ),
        (
            "border",
            solid_over(canvas, &["panel.border", "editorGroup.border", "contrastBorder"])
                .unwrap_or_else(|| d("border")),
        ),
        ("input", input),
        ("placeholder", readable_over(&surface_raised, d("placeholder"), &["input.placeholderForeground"])),
        ("error", readable_status(d("error"), &["editorError.foreground", "errorForeground"])),
        ("warning", readable_status(d("warning"), &["editorWarning.foreground"])),
        (
            "accentSurface",
            solid_over(canvas, &["list.activeSelectionBackground", "list.hoverBackground"])
                .unwrap_or_else(|| d("accentSurface")),
        ),
        ("codeBackground", solid_over(canvas, &["textCodeBlock.background"]).unwrap_or_else(|| d("codeBackground"))),
        ("searchMatchBackground", search_match.clone()),
        (
            "searchMatchForeground",
            readable_over(
                &search_match,
                d("searchMatchForeground"),
                &["editor.findMatchHighlightForeground", "editor.foreground", "foreground"],
            ),
        ),
        ("searchMatchActiveBackground", search_active.clone()),
        (
            "searchMatchActiveForeground",
            readable_over(
                &search_active,
                d("searchMatchActiveForeground"),
                &["editor.findMatchForeground", "editor.foreground", "foreground"],
            ),
        ),
        ("sidebar", sidebar_hex.clone()),
        ("sidebarForeground", readable_over(&sidebar_hex, d("sidebarForeground"), &["sideBar.foreground"])),
        ("sidebarBorder", solid_over(sidebar, &["sideBar.border"]).unwrap_or_else(|| d("sidebarBorder"))),
        ("sidebarRowHover", solid_over(sidebar, &["list.hoverBackground"]).unwrap_or_else(|| d("sidebarRowHover"))),
        (
            "sidebarRowActive",
            solid_over(sidebar, &["list.inactiveSelectionBackground", "list.hoverBackground"])
                .unwrap_or_else(|| d("sidebarRowActive")),
        ),
        (
            "sidebarRowSelected",
            solid_over(sidebar, &["list.activeSelectionBackground"]).unwrap_or_else(|| d("sidebarRowSelected")),
        ),
        ("terminalBackground", terminal_hex.clone()),
        ("terminalForeground", readable_over(&terminal_hex, d("terminalForeground"), &["terminal.foreground"])),
        (
            "terminalCursor",
            solid_over(terminal, &["terminalCursor.foreground", "editorCursor.foreground"])
                .unwrap_or_else(|| d("terminalCursor")),
        ),
        (
            "terminalSelection",
            solid_over(terminal, &["terminal.selectionBackground", "editor.selectionBackground"])
                .unwrap_or_else(|| d("terminalSelection")),
        ),
        (
            "terminalScrollbar",
            solid_over(terminal, &["scrollbarSlider.background"]).unwrap_or_else(|| d("terminalScrollbar")),
        ),
        ("accent", accent_hex.clone()),
        ("focus", accent_hex.clone()),
        ("messageAction", action.clone()),
        ("messageActionForeground", readable_over(&action, d("messageActionForeground"), &["button.foreground"])),
        ("accentForeground", readable_over(&accent_hex, d("accentForeground"), &["button.foreground"])),
    ];
    let mut all = derived.clone();
    for (role, color) in overrides {
        all.set(role, color);
    }
    let name = resolve_name(value);
    let generated = theme_id_from_name(&name);
    let mut file = Map::new();
    file.insert("version".into(), THEME_FILE_VERSION.into());
    if themes::is_reserved(&generated) {
        file.insert("id".into(), format!("{generated}-vscode").into());
    }
    file.insert("name".into(), name.into());
    file.insert("appearance".into(), if dark { "dark" } else { "light" }.into());
    file.insert("colors".into(), all.to_json());
    parse_theme_file(&Value::Object(file))
}

/// Names the theme a light and dark pair become.
type PairedId<'a> = &'a dyn Fn(&ThemeDefinition, &ThemeDefinition) -> String;

/// `pairVsCodeThemes`: "X Light" and "X Dark" become one two-appearance theme named "X".
pub fn pair_themes(themes: Vec<ThemeDefinition>, paired_id: Option<PairedId<'_>>) -> Vec<ThemeDefinition> {
    struct Group {
        key: String,
        order: usize,
        light: Vec<ThemeDefinition>,
        dark: Vec<ThemeDefinition>,
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut out: Vec<(usize, ThemeDefinition)> = Vec::new();
    for (order, theme) in themes.into_iter().enumerate() {
        let key = strip_appearance_words(&theme.label);
        if theme.modes().len() != 1 || key == theme.label || !key.chars().any(|c| c.is_ascii_alphanumeric()) {
            out.push((order, theme));
            continue;
        }
        let index = match groups.iter().position(|group| group.key == key) {
            Some(index) => index,
            None => {
                groups.push(Group { key, order, light: Vec::new(), dark: Vec::new() });
                groups.len() - 1
            }
        };
        let group = &mut groups[index];
        if theme.dark { group.dark.push(theme) } else { group.light.push(theme) }
    }
    for group in groups {
        if let ([light], [dark]) = (&group.light[..], &group.dark[..]) {
            let mut file = json!({
                "version": THEME_FILE_VERSION,
                "name": group.key,
                "appearance": "light",
                "colors": light.roles.to_json(),
                "variants": { "dark": dark.roles.to_json() },
            });
            if let Some(paired_id) = paired_id {
                file["id"] = paired_id(light, dark).into();
            }
            if let Ok(theme) = parse_theme_file(&file) {
                out.push((group.order, theme));
                continue;
            }
        }
        out.extend(group.light.into_iter().chain(group.dark).map(|theme| (group.order, theme)));
    }
    out.sort_by_key(|(order, _)| *order);
    out.into_iter().map(|(_, theme)| theme).collect()
}

/// `label.replace(/\b(?:light|dark)\b/gi, " ")`, whitespace collapsed.
fn strip_appearance_words(label: &str) -> String {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    let lower = label.to_ascii_lowercase();
    let (mut out, mut i) = (String::new(), 0);
    while i < label.len() {
        let hit = ["light", "dark"].into_iter().find(|w| {
            lower[i..].starts_with(w)
                && !word(label[..i].chars().next_back())
                && !word(label[i + w.len()..].chars().next())
        });
        if let Some(w) = hit {
            out.push(' ');
            i += w.len();
        } else {
            let c = label[i..].chars().next().unwrap_or(' ');
            out.push(c);
            i += c.len_utf8();
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `resolveThemeLabelCollisions`: themes that would share an id take their file's name,
/// then a number.
pub fn resolve_label_collisions(entries: Vec<(ThemeDefinition, Option<String>)>) -> Vec<ThemeDefinition> {
    let count = |id: &str| entries.iter().filter(|(theme, _)| theme.id == id).count();
    let relabelled: Vec<ThemeDefinition> = entries
        .iter()
        .map(|(theme, source)| {
            if count(&theme.id) < 2 {
                return theme.clone();
            }
            let stem = source.as_deref().map(|name| name.rsplit_once('.').map_or(name, |(stem, _)| stem));
            stem.map(humanize_theme_name)
                .filter(|name| name.to_lowercase() != theme.label.to_lowercase())
                .and_then(|name| theme_file::renamed(theme, &name).ok())
                .unwrap_or_else(|| theme.clone())
        })
        .collect();
    let mut seen = std::collections::HashSet::new();
    relabelled
        .into_iter()
        .map(|theme| {
            if seen.insert(theme.id.clone()) {
                return theme;
            }
            for suffix in 2..100 {
                if let Ok(candidate) = theme_file::renamed(&theme, &format!("{} {suffix}", theme.label))
                    && seen.insert(candidate.id.clone())
                {
                    return candidate;
                }
            }
            theme
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_drops_comments_and_trailing_commas() {
        let value = parse_jsonc("{ // a\n \"a\": \"//x\", /* b */ \"c\": [1, 2,],\n}").unwrap();
        assert_eq!(value, json!({"a": "//x", "c": [1, 2]}));
        assert_eq!(parse_jsonc("{\"a\": \"q\\\"/*\"}").unwrap(), json!({"a": "q\"/*"}));
        assert!(parse_jsonc("{ /* open").is_none());
    }

    fn vscode(name: &str, kind: &str, background: &str) -> Value {
        json!({
            "name": name,
            "type": kind,
            "colors": {
                "editor.background": background,
                "editor.foreground": if kind == "dark" { "#d4d4d4" } else { "#333333" },
                "focusBorder": "#007fd4",
                "button.background": "#0e639c",
                "sideBar.background": "#25252680",
            },
            "tokenColors": [],
        })
    }

    #[test]
    fn a_vscode_theme_maps_onto_roles_and_stays_readable() {
        let theme = parse_theme_import(&vscode("dark-plus", "dark", "#1e1e1e").to_string()).unwrap();
        assert_eq!((theme.label.as_str(), theme.id.as_str(), theme.dark), ("Dark Plus", "dark-plus", true));
        let hex = |role: &str| theme_color::to_hex(theme.roles.get(role)).unwrap();
        assert_eq!(
            (hex("canvas"), hex("accent"), hex("messageAction")),
            ("#1e1e1e".into(), "#007fd4".into(), "#0e639c".into())
        );
        assert_eq!(hex("text"), "#d4d4d4");
        // The translucent sidebar is composited onto the canvas.
        assert_eq!(hex("sidebar"), rgb_hex(Rgb { r: 33.5, g: 33.5, b: 34. }));
        assert!(
            parse_theme_import("{\"colors\": {\"editor.foreground\": \"#fff\"}, \"tokenColors\": []}")
                .unwrap_err()
                .contains("editor.background")
        );
        // A name that slugs to a reserved id gets a suffix.
        assert_eq!(parse_vscode_theme(&vscode("Dark", "dark", "#000000")).unwrap().id, "dark-vscode");
        assert_eq!(parse_theme_import("not json").unwrap_err(), "That theme file is not valid JSON.");
    }

    #[test]
    fn light_and_dark_twins_pair_and_label_collisions_rename() {
        let light = parse_vscode_theme(&vscode("Solar Light", "light", "#fdf6e3")).unwrap();
        let dark = parse_vscode_theme(&vscode("Solar Dark", "dark", "#002b36")).unwrap();
        let other = parse_vscode_theme(&vscode("Other", "dark", "#000010")).unwrap();
        let paired = pair_themes(vec![other.clone(), light, dark], None);
        assert_eq!(paired.len(), 2);
        assert_eq!((paired[1].label.as_str(), paired[1].modes()), ("Solar", vec![false, true]));
        let renamed = resolve_label_collisions(vec![
            (other.clone(), Some("first-pick.json".into())),
            (other, Some("first-pick.json".into())),
        ]);
        assert_eq!(renamed.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(), ["First Pick", "First Pick 2"]);
        assert_eq!(humanize_theme_name("one-dark_pro"), "One Dark Pro");
        assert_eq!(humanize_theme_name("Night Owl"), "Night Owl");
    }
}
