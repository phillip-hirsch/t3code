//! What the composer's model trigger shows: the short model name
//! (`getTriggerDisplayModelName`, `packages/provider-core/src/modelShortName.ts`)
//! and the effort word (`describeComposerEffort` with `effortDisplay: "label"`).

use serde_json::{Value, json};

use crate::model::{ModelSelection, OptionDescriptor, ProviderModel};

const FAMILY_PREFIXES: [&str; 6] = ["Claude ", "GPT-", "GPT ", "Gemini ", "Muse ", "Grok "];

const VARIANT_WORDS: &[&str] = &[
    "pro",
    "flash",
    "mini",
    "max",
    "lite",
    "nano",
    "ultra",
    "plus",
    "fast",
    "turbo",
    "preview",
    "latest",
    "thinking",
    "reasoning",
    "build",
    "code",
    "image",
    "vision",
    "audio",
    "live",
    "custom",
    "tools",
    "exp",
    "experimental",
    "chat",
    "tts",
    "stt",
    "non",
    "transcribe",
];

const REASONING_IDS: [&str; 5] = ["reasoningEffort", "effort", "reasoning", "variant", "thinking"];

/// `providerClients` model families per driver.
fn native_families(driver: &str) -> &'static [&'static str] {
    match driver {
        "codex" => &["GPT-", "GPT "],
        "claudeAgent" => &["Claude "],
        "antigravity" => &["Gemini "],
        "cursor" | "grok" => &["Grok "],
        "muse" => &["Muse "],
        _ => &[],
    }
}

pub fn trigger_model_name(model: &ProviderModel, driver: &str) -> String {
    let name = model.name.as_deref().unwrap_or(&model.slug);
    let short = model.short_name.clone().or_else(|| short_name(name, native_families(driver)));
    strip_qualifier(short.as_deref().unwrap_or(name), model.sub_provider.as_deref())
}

fn strip_qualifier(name: &str, qualifier: Option<&str>) -> String {
    let Some(qualifier) = qualifier.map(str::trim).filter(|q| !q.is_empty()) else { return name.to_owned() };
    let Some(head) = name.get(..qualifier.len()).filter(|head| head.eq_ignore_ascii_case(qualifier)) else {
        return name.to_owned();
    };
    let rest = &name[head.len()..];
    let trimmed = rest.trim_start();
    let rest = match trimmed.strip_prefix(['.', ':', '/', '-']) {
        Some(after) => after.trim_start(),
        None if trimmed.len() < rest.len() => trimmed,
        None => return name.to_owned(),
    };
    if rest.trim().is_empty() { name.to_owned() } else { rest.trim().to_owned() }
}

fn short_name(name: &str, native: &[&str]) -> Option<String> {
    let unprefixed = strip_vendor(name);
    let short = without_family(unprefixed, native)
        .or_else(|| names_model(unprefixed).then(|| unprefixed.to_owned()))
        .unwrap_or_else(|| name.to_owned());
    (short != name).then_some(short)
}

/// `^[A-Z][\w.&\- ]{0,24}: (?=\S)`
fn strip_vendor(name: &str) -> &str {
    let Some(colon) = name.find(": ") else { return name };
    let vendor = &name[..colon];
    let first_upper = vendor.chars().next().is_some_and(|c| c.is_ascii_uppercase());
    let body_ok = vendor.chars().skip(1).all(|c| c.is_alphanumeric() || "_.&- ".contains(c));
    let next_ok = name[colon + 2..].chars().next().is_some_and(|c| !c.is_whitespace());
    if first_upper && body_ok && vendor.chars().count() <= 25 && next_ok { &name[colon + 2..] } else { name }
}

fn without_family(name: &str, native: &[&str]) -> Option<String> {
    let prefix = FAMILY_PREFIXES.iter().find(|prefix| name.starts_with(**prefix))?;
    let rest = name[prefix.len()..].trim();
    let keep = if native.contains(prefix) { !non_version_words(rest).is_empty() } else { names_model(rest) };
    if !keep {
        return None;
    }
    Some(if rest.contains(char::is_whitespace) { rest.to_owned() } else { rest.replace('-', " ") })
}

fn non_version_words(text: &str) -> Vec<String> {
    let mut stripped = String::with_capacity(text.len());
    let mut depth = 0;
    for c in text.chars() {
        match c {
            '(' => depth += 1,
            ')' if depth > 0 => {
                depth -= 1;
                stripped.push(' ');
            }
            _ if depth == 0 => stripped.push(c),
            _ => {}
        }
    }
    stripped
        .split(|c: char| c.is_whitespace() || c == '-')
        .filter(|word| !word.is_empty() && !is_version(word))
        .map(str::to_owned)
        .collect()
}

/// `^v?\d+(?:\.\d+)*[a-z]?$`, case-insensitive.
fn is_version(word: &str) -> bool {
    let word = word.strip_prefix(['v', 'V']).unwrap_or(word);
    let word = match word.chars().last() {
        Some(c) if c.is_ascii_alphabetic() => &word[..word.len() - 1],
        _ => word,
    };
    !word.is_empty() && word.split('.').all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

fn names_model(text: &str) -> bool {
    non_version_words(text).iter().any(|word| !VARIANT_WORDS.contains(&word.to_lowercase().as_str()))
}

/// The model's reasoning control: a known effort id, or a select marked as reasoning.
pub fn reasoning(descriptors: &[OptionDescriptor]) -> Option<&OptionDescriptor> {
    let is_reasoning = |d: &&OptionDescriptor| {
        (d.kind == "select" && d.role.as_deref() == Some("reasoning")) || REASONING_IDS.contains(&d.id.as_str())
    };
    // Selects come before toggles in the picker's column.
    descriptors
        .iter()
        .filter(|d| d.kind == "select")
        .find(is_reasoning)
        .or_else(|| descriptors.iter().find(is_reasoning))
}

/// The selection's explicit value, else the descriptor's current or default value.
pub fn current_value(descriptor: &OptionDescriptor, selection: &ModelSelection) -> Option<Value> {
    let explicit =
        selection.options.iter().flatten().find(|o| o.get("id").and_then(Value::as_str) == Some(&descriptor.id));
    if let Some(value) = explicit.and_then(|o| o.get("value")) {
        return Some(value.clone());
    }
    if descriptor.kind == "boolean" {
        return descriptor.current_value.clone();
    }
    descriptor
        .current_value
        .clone()
        .or_else(|| descriptor.options.iter().find(|o| o.is_default).map(|o| Value::String(o.id.clone())))
}

pub fn effort_label(descriptors: &[OptionDescriptor], selection: &ModelSelection) -> Option<String> {
    let descriptor = reasoning(descriptors)?;
    let value = current_value(descriptor, selection);
    if descriptor.kind == "boolean" {
        let on = value.and_then(|v| v.as_bool())?;
        return Some(format!("{} {}", descriptor.label, if on { "On" } else { "Off" }));
    }
    let value = value?;
    descriptor.options.iter().find(|o| Some(o.id.as_str()) == value.as_str()).map(|o| o.label.clone())
}

/// `selection` with option `id` set to `value`.
pub fn with_option(selection: &ModelSelection, id: &str, value: Value) -> ModelSelection {
    let mut options: Vec<Value> = selection
        .options
        .iter()
        .flatten()
        .filter(|o| o.get("id").and_then(Value::as_str) != Some(id))
        .cloned()
        .collect();
    options.push(json!({"id": id, "value": value}));
    ModelSelection { options: Some(options), ..selection.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(name: &str) -> ProviderModel {
        serde_json::from_value(json!({"slug": "s", "name": name})).unwrap()
    }

    #[test]
    fn trigger_names_drop_the_native_family() {
        assert_eq!(trigger_model_name(&model("Claude Opus 5.5"), "claudeAgent"), "Opus 5.5");
        assert_eq!(trigger_model_name(&model("GPT-6.1-Sol"), "codex"), "6.1 Sol");
        assert_eq!(trigger_model_name(&model("GPT-5"), "codex"), "GPT-5", "a bare version keeps its family");
        assert_eq!(trigger_model_name(&model("Claude Opus 5.5"), "opencode"), "Opus 5.5");
        assert_eq!(
            trigger_model_name(&model("Claude 4 Pro"), "opencode"),
            "Claude 4 Pro",
            "variant words need the family"
        );
        assert_eq!(trigger_model_name(&model("Anthropic: Claude Haiku 5.5"), "claudeAgent"), "Haiku 5.5");
        let mut sub = model("Moonshot Kimi K3");
        sub.sub_provider = Some("Moonshot".into());
        assert_eq!(trigger_model_name(&sub, "opencode"), "Kimi K3");
    }

    #[test]
    fn effort_label_reads_the_selection_then_the_default() {
        let descriptors: Vec<OptionDescriptor> = serde_json::from_value(json!([
            {"id": "fastMode", "label": "Fast", "type": "boolean", "currentValue": false},
            {"id": "effort", "label": "Effort", "type": "select", "options": [
                {"id": "low", "label": "Low"}, {"id": "high", "label": "High", "isDefault": true}, {"id": "max", "label": "Max"}
            ]}
        ]))
        .unwrap();
        let selection = ModelSelection { instance_id: "c".into(), model: "m".into(), options: None };
        assert_eq!(effort_label(&descriptors, &selection).as_deref(), Some("High"));
        let picked = with_option(&selection, "effort", json!("max"));
        assert_eq!(effort_label(&descriptors, &picked).as_deref(), Some("Max"));
        assert_eq!(effort_label(&descriptors[..1], &selection), None, "no reasoning control, no suffix");
    }
}
