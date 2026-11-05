//! Theme files (`parseThemeFile`, `serializeThemeFile`) and the saved-theme records of
//! `t3code:themes:v1` (`parseStoredTheme`), from `themePalette.ts`.

use serde_json::{Map, Value, json};

use crate::theme_color;
use crate::themes::{self, Collection, Roles, ThemeDefinition};

pub const THEME_FILE_VERSION: u64 = 1;

fn is_theme_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (1..=48).contains(&bytes.len())
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        && bytes[0] != b'-'
}

fn theme_label(value: &Value) -> Option<&str> {
    value.as_str().map(str::trim).filter(|label| !label.is_empty() && label.chars().count() <= 48)
}

fn appearance(value: &Value) -> Option<bool> {
    match value.as_str()? {
        "light" => Some(false),
        "dark" => Some(true),
        _ => None,
    }
}

fn collection(value: &Value) -> Option<Collection> {
    let id = value.get("id")?.as_str()?;
    let valid_id = (1..=128).contains(&id.len())
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b':' | b'-'));
    let label = theme_label(value.get("label")?)?;
    valid_id.then(|| Collection { id: id.to_owned(), label: label.to_owned() })
}

/// `themeIdFromName`: a slug, or a stable hash for names without ASCII letters or digits.
pub fn theme_id_from_name(name: &str) -> String {
    let label = name.trim().to_lowercase();
    let mut slug = String::new();
    for c in label.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(48).collect();
    if !slug.is_empty() || label.is_empty() {
        return if slug.is_empty() { "custom-theme".into() } else { slug };
    }
    let hash =
        label.encode_utf16().fold(2_166_136_261u32, |hash, unit| (hash ^ u32::from(unit)).wrapping_mul(16_777_619));
    format!("theme-{}", radix36(hash))
}

fn radix36(mut value: u32) -> String {
    let mut digits = Vec::new();
    loop {
        digits.push(char::from_digit(value % 36, 36).unwrap_or('0'));
        value /= 36;
        if value == 0 {
            break;
        }
    }
    digits.iter().rev().collect()
}

/// `parseThemeColorOverrides`: every role named must exist and hold a color.
fn overrides(value: &Value) -> Result<Map<String, Value>, String> {
    let Some(map) = value.as_object() else { return Err("Theme colors must be objects.".into()) };
    let mut overrides = Map::new();
    for (role, color) in map {
        if !themes::role_names().iter().any(|name| name == role) {
            return Err(format!("\"{role}\" is not a supported theme color role."));
        }
        let Some(color) = color.as_str().and_then(theme_color::canonical) else {
            return Err(format!("The color for \"{role}\" must be a literal CSS color such as oklch(0.62 0.2 280)."));
        };
        overrides.insert(role.clone(), color.into());
    }
    if overrides.is_empty() {
        return Err("Add at least one color role to the theme file.".into());
    }
    Ok(overrides)
}

/// `parseThemeFile`.
pub fn parse_theme_file(value: &Value) -> Result<ThemeDefinition, String> {
    if !value.is_object() {
        return Err("Theme files must contain a JSON object.".into());
    }
    if value["version"].as_u64() != Some(THEME_FILE_VERSION) || !value["version"].is_number() {
        return Err(format!("This theme file uses an unsupported version. Expected {THEME_FILE_VERSION}."));
    }
    let Some(name) = theme_label(&value["name"]) else {
        return Err("Theme files need a name (48 characters or fewer).".into());
    };
    let Some(dark) = appearance(&value["appearance"]) else {
        return Err("Theme files need an appearance of \"light\" or \"dark\".".into());
    };
    if !value["colors"].is_object() {
        return Err("Theme files need a colors object.".into());
    }
    let id = match &value["id"] {
        Value::Null => theme_id_from_name(name),
        Value::String(id) => id.clone(),
        _ => String::new(),
    };
    if !is_theme_id(&id) {
        return Err("Theme ids may only contain lowercase letters, numbers, and hyphens.".into());
    }
    if themes::is_reserved(&id) {
        return Err(format!("The theme id \"{id}\" is reserved."));
    }
    let roles = Roles::layered(themes::default_roles(dark), &overrides(&value["colors"])?);
    let parsed_collection = collection(&value["collection"]);
    if !value["collection"].is_null() && parsed_collection.is_none() {
        return Err("Theme collections need a valid id and label.".into());
    }
    let mut variant = None;
    if !value["variants"].is_null() {
        let Some(variants) = value["variants"].as_object() else {
            return Err("Theme variants must be an object.".into());
        };
        for (name, colors) in variants {
            let Some(variant_dark) = appearance(&Value::from(name.as_str())) else {
                return Err("Theme variants may only be named \"light\" or \"dark\".".into());
            };
            if variant_dark == dark {
                return Err(format!(
                    "Theme variants must not repeat the base appearance \"{}\".",
                    if dark { "dark" } else { "light" }
                ));
            }
            variant = Some(Roles::layered(themes::default_roles(variant_dark), &overrides(colors)?));
        }
    }
    let mut theme = ThemeDefinition::new(id, name.to_owned(), dark, roles, variant)
        .ok_or_else(|| "That theme file is invalid.".to_owned())?;
    theme.collection = parsed_collection;
    theme.managed = value["managed"] == json!(true);
    Ok(theme)
}

/// The file `serializeThemeFile` writes: every role canonical, two-space JSON.
pub fn serialize_theme_file(theme: &ThemeDefinition) -> Result<String, String> {
    let mut file = Map::new();
    file.insert("version".into(), THEME_FILE_VERSION.into());
    file.insert("id".into(), theme.id.clone().into());
    file.insert("name".into(), theme.label.clone().into());
    file.extend(body(theme)?);
    Ok(format!("{}\n", serde_json::to_string_pretty(&Value::Object(file)).unwrap_or_default()))
}

fn body(theme: &ThemeDefinition) -> Result<Map<String, Value>, String> {
    let mut body = Map::new();
    body.insert("appearance".into(), if theme.dark { "dark" } else { "light" }.into());
    body.insert("colors".into(), theme.roles.canonical()?.to_json());
    if let Some(variant) = &theme.variant_roles {
        let name = if theme.dark { "light" } else { "dark" };
        body.insert("variants".into(), json!({ name: variant.canonical()?.to_json() }));
    }
    if let Some(collection) = &theme.collection {
        body.insert("collection".into(), json!({ "id": collection.id, "label": collection.label }));
    }
    if theme.managed {
        body.insert("managed".into(), true.into());
    }
    Ok(body)
}

/// A saved theme record (`canonicalizeThemeDefinition` as stored).
pub fn stored(theme: &ThemeDefinition) -> Value {
    let mut record = Map::new();
    record.insert("id".into(), theme.id.clone().into());
    record.insert("label".into(), theme.label.clone().into());
    record.extend(body(theme).unwrap_or_default());
    Value::Object(record)
}

/// `parseStoredTheme`: lenient, since the record was valid when it was saved.
pub fn parse_stored(value: &Value) -> Option<ThemeDefinition> {
    let id = value.get("id")?.as_str().filter(|id| is_theme_id(id) && !themes::is_reserved(id))?;
    let label = theme_label(value.get("label")?)?;
    let dark = appearance(value.get("appearance")?)?;
    let roles = Roles::layered(themes::default_roles(dark), value.get("colors")?.as_object()?);
    let mut variant = None;
    if let Some(variants) = value.get("variants") {
        for (name, colors) in variants.as_object()? {
            let variant_dark = appearance(&Value::from(name.as_str()))?;
            if variant_dark != dark {
                variant = Some(Roles::layered(themes::default_roles(variant_dark), colors.as_object()?));
            }
        }
    }
    let mut theme = ThemeDefinition::new(id.to_owned(), label.to_owned(), dark, roles, variant)?;
    theme.collection = value.get("collection").and_then(collection);
    theme.managed = value.get("managed") == Some(&json!(true));
    Some(theme)
}

/// `parseStoredThemes`: valid records, first of each id.
pub fn parse_stored_list(value: &Value) -> Vec<ThemeDefinition> {
    let mut themes: Vec<ThemeDefinition> = Vec::new();
    for theme in value.as_array().into_iter().flatten().filter_map(parse_stored) {
        if !themes.iter().any(|existing| existing.id == theme.id) {
            themes.push(theme);
        }
    }
    themes
}

/// A theme file for `theme` under another name (the import dialog's `versionedCopy` and the
/// VS Code import's renames): a new id from the name, no collection.
pub fn renamed(theme: &ThemeDefinition, name: &str) -> Result<ThemeDefinition, String> {
    let mut file = Map::new();
    file.insert("version".into(), THEME_FILE_VERSION.into());
    file.insert("name".into(), name.chars().take(48).collect::<String>().into());
    let mut body = body(theme)?;
    body.remove("collection");
    file.extend(body);
    parse_theme_file(&Value::Object(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_follow_names() {
        assert_eq!(theme_id_from_name("  Aurora Borealis! "), "aurora-borealis");
        assert_eq!(theme_id_from_name(""), "custom-theme");
        assert_eq!(theme_id_from_name("夜"), theme_id_from_name(" 夜 "));
        assert!(theme_id_from_name("夜").starts_with("theme-"));
        assert_ne!(theme_id_from_name("夜"), theme_id_from_name("朝"));
        assert_eq!(theme_id_from_name(&"a".repeat(60)).len(), 48);
    }

    #[test]
    fn theme_files_round_trip_and_reject_bad_input() {
        let theme = parse_theme_file(&json!({
            "version": 1,
            "name": "Aurora",
            "appearance": "dark",
            "colors": { "canvas": "#101014", "accent": "rgb(139 92 246)" },
            "variants": { "light": { "canvas": "#fafafa" } },
            "managed": true,
        }))
        .unwrap();
        assert_eq!((theme.id.as_str(), theme.dark, theme.managed), ("aurora", true, true));
        assert_eq!(theme.roles.get("canvas"), theme_color::canonical("#101014").unwrap());
        assert_eq!(theme.roles.get("text"), themes::default_roles(true).get("text"), "unnamed roles fall back");
        assert!(theme.variant_roles.is_some());
        let file: Value = serde_json::from_str(&serialize_theme_file(&theme).unwrap()).unwrap();
        assert_eq!(parse_theme_file(&file).unwrap(), theme);
        assert_eq!(parse_stored(&stored(&theme)).unwrap(), theme);

        let error = |value: Value| parse_theme_file(&value).unwrap_err();
        assert!(error(json!({"version": 2})).contains("unsupported version"));
        assert!(error(json!({"version": 1, "name": "A", "appearance": "dim", "colors": {}})).contains("appearance"));
        assert!(
            error(json!({"version": 1, "name": "A", "appearance": "light", "colors": {}})).contains("at least one")
        );
        assert!(
            error(json!({"version": 1, "name": "A", "appearance": "light", "colors": {"nope": "#fff"}}))
                .contains("not a supported")
        );
        assert!(
            error(json!({"version": 1, "name": "Dark", "appearance": "light", "colors": {"canvas": "#fff"}}))
                .contains("reserved")
        );
        assert!(
            error(json!({"version": 1, "name": "A", "appearance": "light", "colors": {"canvas": "#fff"},
            "variants": {"light": {"canvas": "#fff"}}}))
            .contains("must not repeat")
        );
    }

    #[test]
    fn stored_lists_keep_the_first_valid_record_of_each_id() {
        let theme = |id: &str, label: &str| json!({"id": id, "label": label, "appearance": "light", "colors": {}});
        let list =
            parse_stored_list(&json!([theme("a", "One"), theme("a", "Two"), theme("Bad!", "x"), 5, theme("b", "B")]));
        let labels: Vec<_> = list.iter().map(|theme| theme.label.as_str()).collect();
        assert_eq!(labels, ["One", "B"]);
    }
}
