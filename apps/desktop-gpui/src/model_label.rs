//! What the composer's model trigger shows: the short model name
//! (`getTriggerDisplayModelName`, `packages/provider-core/src/modelShortName.ts`)
//! and the effort word (`describeComposerEffort` with `effortDisplay: "label"`).

use serde_json::{Value, json};

use crate::model::{ModelSelection, OptionDescriptor, Provider, ProviderModel};

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

pub(crate) const REASONING_IDS: [&str; 5] = ["reasoningEffort", "effort", "reasoning", "variant", "thinking"];

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
    value
        .and_then(|value| descriptor.options.iter().find(|o| Some(o.id.as_str()) == value.as_str()))
        .map(|o| o.label.clone())
        // Pi's thinking control has no default level: it names itself until one is picked.
        .or_else(|| (descriptor.id == "thinking").then(|| descriptor.label.clone()))
}

/// `withLegacyUltracodeEffort`: Ultracode was a Claude effort level before it became its own
/// toggle, and that level ran at xhigh.
pub fn with_legacy_ultracode(descriptors: &[OptionDescriptor], selection: ModelSelection) -> ModelSelection {
    let explicit = |id: &str| {
        selection
            .options
            .iter()
            .flatten()
            .find(|o| o.get("id").and_then(Value::as_str) == Some(id))
            .map(|o| &o["value"])
    };
    let toggle = descriptors.iter().any(|d| d.id == "ultracode" && d.kind == "boolean");
    if !toggle || explicit("effort").and_then(Value::as_str) != Some("ultracode") {
        return selection;
    }
    let mut next = with_option(&selection, "effort", json!("xhigh"));
    if explicit("ultracode").is_none() {
        next = with_option(&next, "ultracode", Value::Bool(true));
    }
    next
}

/// Where a prompt says "ultrathink" (`readPromptUltrathink`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Ultrathink {
    #[default]
    None,
    /// Only in its leading `Ultrathink:`, which a level pick removes.
    Prefix,
    /// In the text itself, which locks the levels until it goes.
    Body,
}

pub fn prompt_ultrathink(prompt: &str) -> Ultrathink {
    if !says_ultrathink(prompt) {
        Ultrathink::None
    } else if says_ultrathink(strip_ultrathink_prefix(prompt)) {
        Ultrathink::Body
    } else {
        Ultrathink::Prefix
    }
}

/// `prompt` without a leading `Ultrathink:` and the whitespace after it.
pub fn strip_ultrathink_prefix(prompt: &str) -> &str {
    const PREFIX: &str = "ultrathink:";
    match prompt.get(..PREFIX.len()) {
        Some(head) if head.eq_ignore_ascii_case(PREFIX) => prompt[PREFIX.len()..].trim_start(),
        _ => prompt,
    }
}

/// `isClaudeUltrathinkPrompt`: "ultrathink" as a whole word, in any case.
fn says_ultrathink(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let word = |at: Option<&u8>| at.is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_');
    lower
        .match_indices("ultrathink")
        .any(|(at, found)| !word(at.checked_sub(1).and_then(|i| bytes.get(i))) && !word(bytes.get(at + found.len())))
}

/// The select an Ultrathink prompt owns (`ultrathinkPromptControlled`): the model's first select,
/// when it offers prompt-injected values.
pub fn ultrathink_select(descriptors: &[OptionDescriptor], prompt: Ultrathink) -> Option<&OptionDescriptor> {
    let primary = descriptors.iter().find(|d| d.kind == "select")?;
    (prompt != Ultrathink::None && !primary.prompt_injected_values.is_empty()).then_some(primary)
}

/// Any provider's service tier, which the picker's footer offers as speed segments (`splitSpeed`).
pub fn speed_select(descriptors: &[OptionDescriptor]) -> Option<&OptionDescriptor> {
    descriptors.iter().find(|d| d.id == "serviceTier" && d.kind == "select")
}

/// `buildTraitsTriggerDisplay`'s `speedLabel`: "Fast" or "Ultrafast" while a speed option is on.
pub fn speed_label(descriptors: &[OptionDescriptor], selection: &ModelSelection) -> Option<&'static str> {
    if toggle_on(descriptors, "fastMode", selection) {
        return Some("Fast");
    }
    let tier = speed_select(descriptors)?;
    let value = current_value(tier, selection)?;
    let labelled = |label: &str| tier.options.iter().find(|o| o.label == label).map(|o| o.id.as_str());
    match value.as_str() {
        Some(id) if labelled("Ultrafast") == Some(id) => Some("Ultrafast"),
        Some(id) if labelled("Fast") == Some(id) => Some("Fast"),
        _ => None,
    }
}

/// Whether the boolean option `id` is on for `selection`.
pub fn toggle_on(descriptors: &[OptionDescriptor], id: &str, selection: &ModelSelection) -> bool {
    descriptors.iter().find(|d| d.id == id && d.kind == "boolean").and_then(|d| current_value(d, selection))
        == Some(Value::Bool(true))
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

/// `getDisplayModelName`: the model's full name without its sub-provider prefix.
pub fn display_model_name(model: &ProviderModel) -> String {
    strip_qualifier(model.name.as_deref().unwrap_or(&model.slug), model.sub_provider.as_deref())
}

/// `scoreModelPickerSearch` (`modelPickerSearch.ts`): every whitespace token must
/// match a field (name, short name, sub-provider, driver, provider, all joined);
/// lower is better, favorites rank 24 higher.
pub fn search_score(
    model: &ProviderModel,
    driver: &str,
    provider_name: &str,
    favorite: bool,
    query: &str,
) -> Option<i64> {
    let tokens: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if tokens.is_empty() {
        return Some(0);
    }
    let name = model.name.as_deref().unwrap_or(&model.slug).to_lowercase();
    let short = model.short_name.as_deref().map(str::to_lowercase);
    let joined = tokens.join(" ");
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined == squash(&name) || short.as_deref().is_some_and(|s| joined == squash(s)) {
        return Some(-1000);
    }
    let mut fields: Vec<String> = vec![name];
    fields.extend(short);
    fields.extend(model.sub_provider.as_deref().map(str::to_lowercase));
    fields.push(driver.to_lowercase());
    fields.push(provider_name.to_lowercase());
    fields.push(fields.iter().filter(|f| !f.is_empty()).cloned().collect::<Vec<_>>().join(" "));
    let mut total = 0;
    for token in &tokens {
        total +=
            fields.iter().enumerate().filter_map(|(i, f)| token_score(f, token, i64::try_from(i).ok()? * 10)).min()?;
    }
    Some(if favorite { total - 24 } else { total })
}

/// `COMPOSER_MODEL_MENTION_LIMIT`.
const MENTION_LIMIT: usize = 8;

/// The provider name a picker or mention shows: its display name, else its instance id.
pub fn provider_name(provider: &Provider) -> &str {
    provider.display_name.as_deref().unwrap_or(&provider.instance_id)
}

/// `rankComposerModelMentions`: the models an `@` query names, best first. Every word must start
/// a word of the model's names, so fuzzy hits can't push files out of the menu; `:`, `/` and `-`
/// join words, since a mention has no spaces. A bare or one-letter query names none.
pub fn rank_mentions<'a>(
    candidates: &[(&'a Provider, &'a ProviderModel)],
    query: &str,
) -> Vec<(&'a Provider, &'a ProviderModel)> {
    for words in mention_queries(query) {
        let tokens: Vec<String> = words.to_lowercase().split_whitespace().map(str::to_owned).collect();
        let mut ranked: Vec<_> = (candidates.iter().enumerate())
            .filter_map(|(order, &(provider, model))| {
                let haystack = [
                    model.name.as_deref().unwrap_or(&model.slug),
                    model.short_name.as_deref().unwrap_or_default(),
                    model.sub_provider.as_deref().unwrap_or_default(),
                    provider.driver.as_str(),
                    provider_name(provider),
                ]
                .join(" ")
                .to_lowercase();
                if !tokens.iter().all(|token| starts_a_word(&haystack, token)) {
                    return None;
                }
                let score = search_score(model, &provider.driver, provider_name(provider), false, &words)?;
                Some((score, order, (provider, model)))
            })
            .collect();
        if !ranked.is_empty() {
            ranked.sort_by_key(|(score, order, _)| (*score, *order));
            return ranked.into_iter().take(MENTION_LIMIT).map(|(.., candidate)| candidate).collect();
        }
    }
    Vec::new()
}

/// `mentionSearchQueries`: the query with `:` and `/` as spaces, then with `-` too.
fn mention_queries(query: &str) -> Vec<String> {
    let spaced = collapse_to_spaces(query, &[':', '/']);
    let unhyphenated = collapse_to_spaces(&spaced, &['-']);
    if unhyphenated.chars().filter(|c| !c.is_whitespace()).count() < 2 {
        return Vec::new();
    }
    if unhyphenated == spaced { vec![spaced] } else { vec![spaced, unhyphenated] }
}

/// Each run of `separators` as one space, trimmed.
fn collapse_to_spaces(text: &str, separators: &[char]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_run = false;
    for c in text.chars() {
        let separator = separators.contains(&c);
        if !separator {
            out.push(c);
        } else if !in_run {
            out.push(' ');
        }
        in_run = separator;
    }
    out.trim().to_owned()
}

/// `(?:^|[^\p{L}\p{N}])token`: `token` starts the text or follows a non-alphanumeric.
fn starts_a_word(haystack: &str, token: &str) -> bool {
    haystack.match_indices(token).any(|(at, _)| haystack[..at].chars().next_back().is_none_or(|c| !c.is_alphanumeric()))
}

/// `scoreQueryMatch` with `ignorePosition`.
fn token_score(value: &str, token: &str, base: i64) -> Option<i64> {
    if value.is_empty() {
        return None;
    }
    if value == token {
        return Some(base);
    }
    if value.starts_with(token) {
        return Some(base + 2);
    }
    if [' ', '-', '_', '/'].iter().any(|m| value.contains(&format!("{m}{token}"))) {
        return Some(base + 4);
    }
    if value.contains(token) {
        return Some(base + 6);
    }
    if token.chars().count() < 3 {
        return None;
    }
    let token: Vec<char> = token.chars().collect();
    let (mut matched, mut first, mut previous, mut gaps) = (0, None, 0usize, 0usize);
    for (index, c) in value.chars().enumerate() {
        if c != token[matched] {
            continue;
        }
        if first.is_some() {
            gaps += index - previous - 1;
        }
        let start = *first.get_or_insert(index);
        previous = index;
        matched += 1;
        if matched == token.len() {
            return Some(base + 100 + i64::try_from(gaps * 3 + index - start + 1 - token.len()).ok()?);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_legacy_ultracode_effort_becomes_xhigh_with_the_toggle_on() {
        let descriptors: Vec<OptionDescriptor> = serde_json::from_value(json!([
            {"id": "effort", "label": "Effort", "type": "select", "options": [{"id": "xhigh", "label": "Extra high"}]},
            {"id": "ultracode", "label": "Ultracode", "type": "boolean", "currentValue": false},
        ]))
        .unwrap();
        let selection = |options: Value| ModelSelection {
            instance_id: "claudeAgent".into(),
            model: "opus".into(),
            options: serde_json::from_value(options).unwrap(),
        };
        let legacy = with_legacy_ultracode(&descriptors, selection(json!([{"id": "effort", "value": "ultracode"}])));
        assert_eq!(effort_label(&descriptors, &legacy).as_deref(), Some("Extra high"));
        assert!(toggle_on(&descriptors, "ultracode", &legacy));
        let off = selection(json!([{"id": "effort", "value": "ultracode"}, {"id": "ultracode", "value": false}]));
        assert!(!toggle_on(&descriptors, "ultracode", &with_legacy_ultracode(&descriptors, off)));
        // Older servers still offer Ultracode as a level.
        let old = with_legacy_ultracode(&descriptors[..1], selection(json!([{"id": "effort", "value": "ultracode"}])));
        assert_eq!(current_value(&descriptors[0], &old), Some(json!("ultracode")));
    }

    #[test]
    fn picker_search_needs_every_token_and_prefers_names() {
        let opus = model("Claude Opus 5.5");
        assert_eq!(search_score(&opus, "claudeAgent", "Claude", false, "claude opus 5.5"), Some(-1000));
        assert_eq!(search_score(&opus, "claudeAgent", "Claude", false, "opus"), Some(4));
        assert_eq!(search_score(&opus, "claudeAgent", "Claude", true, "opus"), Some(-20));
        assert_eq!(search_score(&opus, "claudeAgent", "Claude", false, "opus gpt"), None);
        assert!(search_score(&opus, "claudeAgent", "Claude", false, "cop").is_some(), "fuzzy from 3 letters");
        assert_eq!(search_score(&opus, "claudeAgent", "Claude", false, "zz"), None);
    }

    fn model(name: &str) -> ProviderModel {
        serde_json::from_value(json!({"slug": "s", "name": name})).unwrap()
    }

    #[test]
    fn mentions_need_every_word_to_start_a_name_word() {
        let provider = |id: &str, driver: &str, name: &str| -> Provider {
            serde_json::from_value(json!({"instanceId": id, "driver": driver, "displayName": name})).unwrap()
        };
        let named = |slug: &str, name: &str, sub: Option<&str>| -> ProviderModel {
            serde_json::from_value(json!({"slug": slug, "name": name, "subProvider": sub})).unwrap()
        };
        let (traex, opencode, claude) = (
            provider("traex", "codex", "Traex"),
            provider("opencode", "opencode", "OpenCode"),
            provider("claude", "claudeAgent", "Claude"),
        );
        let models = [
            named("gpt", "GPT-6.1", None),
            named("traex-gpt", "GPT-6.1", Some("Traex")),
            named("opus", "Claude Opus 5.5", Some("Anthropic")),
            named("opus", "Claude Opus 5.5", None),
        ];
        let candidates =
            [(&traex, &models[0]), (&opencode, &models[1]), (&opencode, &models[2]), (&claude, &models[3])];
        let rank = |query: &str| {
            let mut ids: Vec<String> = rank_mentions(&candidates, query)
                .into_iter()
                .map(|(p, m)| format!("{}:{}", p.instance_id, m.slug))
                .collect();
            ids.sort();
            ids
        };
        for query in ["", "-", "---", ":/-/:", "g"] {
            assert_eq!(rank(query), Vec::<String>::new(), "a bare or one-letter @{query}");
        }
        assert_eq!(rank("gpt"), ["opencode:traex-gpt", "traex:gpt"], "every provider that serves it");
        for query in ["opencode:traex", "opencode/gpt", "opencode-gpt"] {
            assert_eq!(rank(query), ["opencode:traex-gpt"], "{query} joins words");
        }
        assert_eq!(rank("gpt-6.1"), ["opencode:traex-gpt", "traex:gpt"], "a hyphenated name stays whole");
        for query in ["src", "agent", "pus"] {
            assert_eq!(rank(query), Vec::<String>::new(), "words that only contain @{query}");
        }
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
    fn an_ultrathink_prompt_owns_the_first_select_that_injects_it() {
        assert_eq!(prompt_ultrathink("fix it"), Ultrathink::None);
        assert_eq!(prompt_ultrathink("Ultrathink: fix it"), Ultrathink::Prefix);
        assert_eq!(prompt_ultrathink("ULTRATHINK:   fix it"), Ultrathink::Prefix);
        assert_eq!(prompt_ultrathink("please ultrathink about it"), Ultrathink::Body);
        assert_eq!(prompt_ultrathink("Ultrathink: and ultrathink"), Ultrathink::Body);
        assert_eq!(prompt_ultrathink("ultrathinking"), Ultrathink::None, "a whole word only");
        assert_eq!(strip_ultrathink_prefix("Ultrathink:  fix it"), "fix it");
        assert_eq!(strip_ultrathink_prefix("fix it"), "fix it");
        let descriptors: Vec<OptionDescriptor> = serde_json::from_value(json!([
            {"id": "effort", "label": "Effort", "type": "select", "promptInjectedValues": ["ultrathink"],
             "options": [{"id": "high", "label": "High"}, {"id": "ultrathink", "label": "Ultrathink"}]},
        ]))
        .unwrap();
        assert_eq!(ultrathink_select(&descriptors, Ultrathink::Prefix).map(|d| d.id.as_str()), Some("effort"));
        assert!(ultrathink_select(&descriptors, Ultrathink::None).is_none());
        let plain: Vec<OptionDescriptor> = serde_json::from_value(json!([
            {"id": "effort", "label": "Effort", "type": "select", "options": [{"id": "high", "label": "High"}]},
        ]))
        .unwrap();
        assert!(ultrathink_select(&plain, Ultrathink::Body).is_none(), "the model must inject it");
    }

    #[test]
    fn service_tiers_are_speeds_and_name_the_fast_ones() {
        let descriptors: Vec<OptionDescriptor> = serde_json::from_value(json!([
            {"id": "serviceTier", "label": "Service Tier", "type": "select", "currentValue": "default", "options": [
                {"id": "default", "label": "Normal", "isDefault": true},
                {"id": "fast", "label": "Fast"}, {"id": "priority", "label": "Ultrafast"}
            ]}
        ]))
        .unwrap();
        // Pi offers the same tiers as Codex, so the tier alone makes the speed control.
        let selection = ModelSelection { instance_id: "pi".into(), model: "m".into(), options: None };
        assert!(speed_select(&descriptors).is_some());
        assert_eq!(speed_label(&descriptors, &selection), None);
        let fast = with_option(&selection, "serviceTier", json!("fast"));
        assert_eq!(speed_label(&descriptors, &fast), Some("Fast"));
        let ultra = with_option(&selection, "serviceTier", json!("priority"));
        assert_eq!(speed_label(&descriptors, &ultra), Some("Ultrafast"));
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
        let mut unset = descriptors;
        unset[1].options.iter_mut().for_each(|o| o.is_default = false);
        assert_eq!(effort_label(&unset, &selection), None, "an unset effort has no label");
        unset[1].id = "thinking".into();
        unset[1].label = "Thinking".into();
        assert_eq!(effort_label(&unset, &selection).as_deref(), Some("Thinking"), "Pi's thinking names itself");
    }
}
