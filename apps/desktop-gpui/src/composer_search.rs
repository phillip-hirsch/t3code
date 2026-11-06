//! What the `$` and `/` menus list and in which order: `searchProviderSkills`
//! (`apps/web/src/providerSkillSearch.ts`), `searchSlashCommandItems`
//! (`components/chat/composerSlashCommandSearch.ts`) and `scoreQueryMatch`
//! (`packages/shared/src/searchRanking.ts`).

use crate::model::{Provider, ProviderSkill, SlashCommand};

/// One row of the `@` / `$` / `/` menu.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuItem {
    pub id: String,
    pub label: String,
    pub description: String,
    pub action: MenuAction,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MenuAction {
    File {
        path: String,
        directory: bool,
    },
    Skill {
        name: String,
        scope: Option<String>,
        path: String,
    },
    /// A provider command, sent later as `/name` text.
    Command {
        name: String,
    },
    /// `/model` opens the model picker.
    Model,
    /// `/plan` or `/default` switches the interaction mode.
    Mode(&'static str),
}

/// The skills and commands a provider offers from `cwd`, falling back to its global lists.
pub fn provider_lists<'a>(provider: &'a Provider, cwd: Option<&str>) -> (&'a [ProviderSkill], &'a [SlashCommand]) {
    match provider.workspace_snapshots.iter().find(|s| Some(s.cwd.as_str()) == cwd) {
        Some(snapshot) => (&snapshot.skills, &snapshot.slash_commands),
        None => (&provider.skills, &provider.slash_commands),
    }
}

fn display_name(skill: &ProviderSkill) -> &str {
    skill.display_name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&skill.name)
}

/// Enabled, user-invocable skills, first of each name.
pub fn invocable(skills: &[ProviderSkill]) -> Vec<&ProviderSkill> {
    let mut seen = std::collections::HashSet::new();
    skills
        .iter()
        .filter(|s| s.enabled && s.user_invocable != Some(false))
        .filter(|s| seen.insert(s.name.to_lowercase()))
        .collect()
}

struct Bases {
    exact: u32,
    prefix: u32,
    boundary: u32,
    includes: u32,
    fuzzy: Option<u32>,
}

/// `scoreQueryMatch`; `value` and `query` already lowercased. Lower is better.
fn score(value: &str, query: &str, bases: Bases, markers: &[char]) -> Option<u32> {
    if value.is_empty() || query.is_empty() {
        return None;
    }
    let penalty = u32::try_from(value.len().saturating_sub(query.len()).min(64)).unwrap_or(64);
    if value == query {
        return Some(bases.exact);
    }
    if value.starts_with(query) {
        return Some(bases.prefix + penalty);
    }
    let boundary = markers.iter().filter_map(|m| value.find(&format!("{m}{query}")).map(|i| i + m.len_utf8())).min();
    if let Some(index) = boundary {
        return Some(bases.boundary + u32::try_from(index).ok()? * 2 + penalty);
    }
    if let Some(index) = value.find(query) {
        return Some(bases.includes + u32::try_from(index).ok()? * 2 + penalty);
    }
    bases.fuzzy.and_then(|base| subsequence(value, query).map(|s| base + s))
}

/// `scoreSubsequenceMatch`.
fn subsequence(value: &str, query: &str) -> Option<u32> {
    let query: Vec<char> = query.chars().collect();
    let (mut matched, mut first, mut previous, mut gaps) = (0, None, 0usize, 0usize);
    for (index, c) in value.chars().enumerate() {
        if c != query[matched] {
            continue;
        }
        if first.is_some() {
            gaps += index - previous - 1;
        }
        let start = *first.get_or_insert(index);
        previous = index;
        matched += 1;
        if matched == query.len() {
            let span = index - start + 1 - query.len();
            let length = (value.chars().count() - query.len()).min(64);
            return u32::try_from(start * 2 + gaps * 3 + span + length).ok();
        }
    }
    None
}

fn lowest(scores: impl IntoIterator<Item = Option<u32>>) -> Option<u32> {
    scores.into_iter().flatten().min()
}

fn score_skill(skill: &ProviderSkill, query: &str) -> Option<u32> {
    let lower = |s: Option<&str>| s.unwrap_or_default().to_lowercase();
    let text = |value: String, base: u32| {
        score(
            &value,
            query,
            Bases { exact: base, prefix: base + 2, boundary: base + 4, includes: base + 6, fuzzy: None },
            &[' ', '-', '_', '/'],
        )
    };
    lowest([
        score(
            &skill.name.to_lowercase(),
            query,
            Bases { exact: 0, prefix: 2, boundary: 4, includes: 6, fuzzy: Some(100) },
            &['-', '_', '/'],
        ),
        score(
            &display_name(skill).to_lowercase(),
            query,
            Bases { exact: 1, prefix: 3, boundary: 5, includes: 7, fuzzy: Some(110) },
            &[' ', '-', '_', '/'],
        ),
        text(lower(skill.short_description.as_deref()), 20),
        text(lower(skill.description.as_deref()), 30),
        score(
            &lower(skill.scope.as_deref()),
            query,
            Bases { exact: 40, prefix: 42, boundary: u32::MAX / 2, includes: 44, fuzzy: None },
            &[],
        ),
    ])
}

/// Sorts by score, then tie-breaker, dropping misses; an empty query keeps the order.
fn rank<T>(items: Vec<T>, query: &str, score: impl Fn(&T) -> Option<u32>, tie: impl Fn(&T) -> String) -> Vec<T> {
    if query.is_empty() {
        return items;
    }
    let mut scored: Vec<_> = items.into_iter().filter_map(|item| Some((score(&item)?, tie(&item), item))).collect();
    scored.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    scored.into_iter().map(|(_, _, item)| item).collect()
}

fn skill_item(skill: &ProviderSkill, label: String, fallback: &str) -> MenuItem {
    let description = skill
        .short_description
        .clone()
        .or_else(|| skill.description.clone())
        .or_else(|| skill.scope.as_ref().map(|scope| format!("{scope} skill")))
        .unwrap_or_else(|| fallback.to_owned());
    MenuItem {
        id: format!("skill:{}", skill.name),
        label,
        description,
        action: MenuAction::Skill { name: skill.name.clone(), scope: skill.scope.clone(), path: skill.path.clone() },
    }
}

/// The `$` menu.
pub fn skill_items(skills: &[ProviderSkill], query: &str) -> Vec<MenuItem> {
    let query = query
        .trim()
        .trim_start_matches(|c: char| !c.is_alphanumeric() && c != ':' && c != '_' && c != '-')
        .to_lowercase();
    let skills = rank(
        invocable(skills),
        &query,
        |s| score_skill(s, &query),
        |s| format!("{}\0{}", display_name(s).to_lowercase(), s.name),
    );
    skills.into_iter().map(|s| skill_item(s, display_name(s).to_owned(), "Run provider skill")).collect()
}

/// The `/` menu: `/model`, `/plan` and `/default` while plan mode is on, then the
/// provider's commands, then skills as `/skill:name`. Provider commands work only at the
/// very start of the prompt.
pub fn slash_items(
    skills: &[ProviderSkill],
    commands: &[SlashCommand],
    query: &str,
    at_start: bool,
    plan_mode: bool,
) -> Vec<MenuItem> {
    let skills = invocable(skills);
    let mut items = vec![MenuItem {
        id: "slash:model".into(),
        label: "/model".into(),
        description: "Switch response model for this thread".into(),
        action: MenuAction::Model,
    }];
    if plan_mode {
        items.extend(
            [
                ("plan", "Switch this thread into plan mode"),
                ("default", "Switch this thread back to normal build mode"),
            ]
            .map(|(mode, description)| MenuItem {
                id: format!("slash:{mode}"),
                label: format!("/{mode}"),
                description: description.into(),
                action: MenuAction::Mode(mode),
            }),
        );
    }
    if at_start {
        let skill_names: Vec<String> = skills.iter().map(|s| s.name.to_lowercase()).collect();
        items.extend(commands.iter().filter(|c| !skill_names.contains(&c.name.to_lowercase())).map(|c| {
            let hint = c.input.as_ref().and_then(|i| i.get("hint")).and_then(|h| h.as_str()).map(str::to_owned);
            MenuItem {
                id: format!("provider-slash-command:{}", c.name),
                label: format!("/{}", c.name),
                description: c.description.clone().or(hint).unwrap_or_else(|| "Run provider command".into()),
                action: MenuAction::Command { name: c.name.clone() },
            }
        }));
    }
    items.extend(skills.iter().map(|s| skill_item(s, format!("/skill:{}", s.name), "")));
    let query = query.trim().trim_start_matches('/').to_lowercase();
    let by_name: std::collections::HashMap<&str, &ProviderSkill> =
        skills.iter().map(|s| (s.name.as_str(), *s)).collect();
    rank(
        items,
        &query,
        |item| match &item.action {
            MenuAction::Skill { name, .. } => {
                if query == "skill" {
                    return Some(0);
                }
                let skill_query = query.strip_prefix("skill:").unwrap_or(&query);
                let skill = by_name.get(name.as_str())?;
                let hit = if skill_query.is_empty() { Some(0) } else { score_skill(skill, skill_query) };
                hit.or_else(|| "skill".starts_with(query.as_str()).then_some(u32::MAX))
            }
            _ => {
                let name = item.label.trim_start_matches('/').to_lowercase();
                lowest([
                    score(
                        &name,
                        &query,
                        Bases { exact: 0, prefix: 2, boundary: 4, includes: 6, fuzzy: Some(100) },
                        &['-', '_', '/'],
                    ),
                    score(
                        &item.description.to_lowercase(),
                        &query,
                        Bases { exact: 20, prefix: 22, boundary: 24, includes: 26, fuzzy: None },
                        &[' ', '-', '_', '/'],
                    ),
                ])
            }
        },
        |item| match &item.action {
            MenuAction::Model => "0\0model".into(),
            MenuAction::Mode(mode) => format!("0\0{mode}"),
            MenuAction::Command { name } => format!("1\0{name}"),
            MenuAction::Skill { name, .. } => format!("2\0{name}"),
            MenuAction::File { path, .. } => path.clone(),
        },
    )
}

/// A `projects.searchEntries` result as menu rows: basename, then parent folder.
pub fn file_items(result: &serde_json::Value) -> Vec<MenuItem> {
    let entries = result.get("entries").and_then(|e| e.as_array()).map(Vec::as_slice).unwrap_or_default();
    entries
        .iter()
        .filter_map(|entry| {
            let path = entry.get("path")?.as_str()?;
            let directory = entry.get("kind").and_then(|k| k.as_str()) == Some("directory");
            let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
            Some(MenuItem {
                id: format!("path:{}:{path}", if directory { "directory" } else { "file" }),
                label: name.to_owned(),
                description: parent.to_owned(),
                action: MenuAction::File { path: path.to_owned(), directory },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn skills() -> Vec<ProviderSkill> {
        serde_json::from_value(json!([
            {"name": "review", "path": "/s/review", "enabled": true, "description": "Review a pull request"},
            {"name": "deploy-app", "path": "/s/deploy", "enabled": true, "displayName": "Deploy", "scope": "project"},
            {"name": "hidden", "path": "/s/h", "enabled": true, "userInvocable": false},
            {"name": "off", "path": "/s/o", "enabled": false},
            {"name": "Review", "path": "/s/dup", "enabled": true},
        ]))
        .unwrap()
    }

    fn labels(items: &[MenuItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn skills_hide_disabled_and_agent_only_ones_and_rank_by_name_first() {
        let skills = skills();
        assert_eq!(labels(&skill_items(&skills, "")), ["review", "Deploy"]);
        assert_eq!(labels(&skill_items(&skills, "$dep")), ["Deploy"]);
        assert_eq!(labels(&skill_items(&skills, "pull")), ["review"], "descriptions match too");
        assert_eq!(skill_items(&skills, "dep")[0].description, "project skill");
    }

    #[test]
    fn slash_menu_offers_provider_commands_only_at_the_start() {
        let skills = skills();
        let commands: Vec<SlashCommand> =
            serde_json::from_value(json!([{"name": "compact", "description": "Summarize"}, {"name": "review"}]))
                .unwrap();
        let all = slash_items(&skills, &commands, "", true, false);
        assert_eq!(
            labels(&all),
            ["/model", "/compact", "/skill:review", "/skill:deploy-app"],
            "skills shadow commands"
        );
        assert_eq!(
            labels(&slash_items(&skills, &commands, "", false, false)),
            ["/model", "/skill:review", "/skill:deploy-app"]
        );
        assert_eq!(labels(&slash_items(&skills, &commands, "/com", true, false)), ["/compact"]);
        assert_eq!(labels(&slash_items(&skills, &commands, "skill:dep", true, false)), ["/skill:deploy-app"]);
        assert_eq!(
            labels(&slash_items(&[], &commands, "", false, true)),
            ["/model", "/plan", "/default"],
            "plan mode adds its commands anywhere in the prompt"
        );
        assert_eq!(labels(&slash_items(&[], &commands, "pla", true, true)), ["/plan"]);
    }

    #[test]
    fn file_results_split_name_and_folder() {
        let items = file_items(
            &json!({"entries": [{"path": "src/app/mod.rs", "kind": "file"}, {"path": "src", "kind": "directory"}]}),
        );
        assert_eq!((items[0].label.as_str(), items[0].description.as_str()), ("mod.rs", "src/app"));
        assert_eq!(items[1].action, MenuAction::File { path: "src".into(), directory: true });
    }
}
