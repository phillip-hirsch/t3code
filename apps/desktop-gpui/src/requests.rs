//! Pending questions and the answers built from their drafts, as
//! `packages/client-runtime/src/state/pendingUserInput.ts`.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::model::TurnItem;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: String,
    #[serde(default)]
    pub header: String,
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multi_select: bool,
    #[serde(default)]
    pub allow_custom_answer: Option<bool>,
    #[serde(default)]
    pub initial_answer: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
}

impl QuestionOption {
    pub fn value(&self) -> &str {
        self.value.as_deref().unwrap_or(&self.label)
    }
}

impl Question {
    pub fn allows_custom(&self) -> bool {
        self.allow_custom_answer != Some(false)
    }

    /// Option values match exactly; options without a value match by trimmed label.
    fn resolve_option(&self, value: &str) -> Option<String> {
        if self.options.iter().any(|o| o.value.as_deref() == Some(value)) {
            return Some(value.to_owned());
        }
        let label = value.trim();
        let found = !label.is_empty() && self.options.iter().any(|o| o.value.is_none() && o.label.trim() == label);
        found.then(|| label.to_owned())
    }

    fn selected(&self, draft: &Draft) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for value in draft.selected.iter().filter_map(|v| self.resolve_option(v)) {
            if !out.contains(&value) {
                out.push(value);
            }
        }
        out
    }
}

/// One question's draft. `custom` is `None` until the user types, which
/// matters for questions with an initial answer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Draft {
    pub selected: Vec<String>,
    pub custom: Option<String>,
}

/// The open question card: which request, which question, and every draft.
pub struct QuestionDraft {
    pub request_id: String,
    /// Never empty: [`QuestionDraft::new`] refuses an empty list.
    pub questions: Vec<Question>,
    pub index: usize,
    pub drafts: HashMap<String, Draft>,
    pub collapsed: bool,
    /// The thread's prompt, set aside while the card holds the editor.
    pub prompt: String,
}

impl QuestionDraft {
    /// A fresh draft, seeded with each question's initial answer; `None` without questions.
    pub fn new(request_id: String, questions: Vec<Question>) -> Option<Self> {
        if questions.is_empty() {
            return None;
        }
        let drafts = questions
            .iter()
            .filter(|q| q.allows_custom())
            .filter_map(|q| {
                Some((q.id.clone(), Draft { selected: Vec::new(), custom: Some(q.initial_answer.clone()?) }))
            })
            .collect();
        Some(Self { request_id, questions, index: 0, drafts, collapsed: false, prompt: String::new() })
    }
}

/// The item's questions; an unreadable payload is logged and reads as none.
pub fn questions(item: &TurnItem) -> Vec<Question> {
    let Some(raw) = item.raw.get("questions") else { return Vec::new() };
    Vec::deserialize(raw).unwrap_or_else(|error| {
        log::warn!("unreadable questions on {}: {error}", item.id);
        Vec::new()
    })
}

/// The answer to submit for one question, or `None` while it is unanswered.
pub fn answer(question: &Question, draft: Option<&Draft>) -> Option<Value> {
    let custom = draft.and_then(|d| d.custom.as_deref());
    let custom = if !question.allows_custom() {
        None
    } else if question.initial_answer.is_some() {
        custom.map(str::to_owned)
    } else {
        custom.map(str::trim).filter(|c| !c.is_empty()).map(str::to_owned)
    };
    if let Some(custom) = custom.as_ref().filter(|c| !c.is_empty()) {
        return Some(Value::String(custom.clone()));
    }
    let selected = draft.map(|d| question.selected(d)).unwrap_or_default();
    if question.multi_select {
        if !selected.is_empty() {
            return Some(Value::Array(selected.into_iter().map(Value::String).collect()));
        }
        return custom.map(Value::String);
    }
    selected.into_iter().next().or(custom).map(Value::String)
}

/// Every answer keyed by question id, or `None` while any is unanswered.
pub fn answers(questions: &[Question], drafts: &HashMap<String, Draft>) -> Option<Value> {
    let mut map = Map::new();
    for question in questions {
        map.insert(question.id.clone(), answer(question, drafts.get(&question.id))?);
    }
    Some(Value::Object(map))
}

/// Typing replaces the selection unless the text is empty.
pub fn set_custom(question: &Question, draft: Option<&Draft>, custom: String) -> Draft {
    if !question.allows_custom() {
        return draft.cloned().unwrap_or_default();
    }
    let keep = question.initial_answer.is_none() && custom.trim().is_empty();
    let selected = match draft {
        Some(draft) if keep => question.selected(draft),
        _ => Vec::new(),
    };
    Draft { selected, custom: Some(custom) }
}

pub fn is_selected(question: &Question, draft: Option<&Draft>, value: &str) -> bool {
    let Some(draft) = draft else { return false };
    if question.allows_custom() && draft.custom.as_deref().is_some_and(|c| !c.trim().is_empty()) {
        return false;
    }
    question.resolve_option(value).is_some_and(|v| question.selected(draft).contains(&v))
}

/// Picking an option clears the custom answer; multi-select toggles.
pub fn toggle(question: &Question, draft: Option<&Draft>, value: &str) -> Draft {
    let Some(value) = question.resolve_option(value) else { return draft.cloned().unwrap_or_default() };
    let mut selected =
        if question.multi_select { draft.map(|d| question.selected(d)).unwrap_or_default() } else { Vec::new() };
    match selected.iter().position(|v| *v == value) {
        Some(index) if question.multi_select => {
            selected.remove(index);
        }
        _ => selected.push(value),
    }
    Draft { selected, custom: Some(String::new()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn question(value: Value) -> Question {
        serde_json::from_value(value).unwrap()
    }

    fn options() -> Value {
        json!([{"label": "Red", "description": "r"}, {"label": "Blue", "description": "b", "value": "blue "}])
    }

    #[test]
    fn single_select_answers_with_the_option_and_custom_text_wins() {
        let q = question(json!({"id": "q", "header": "H", "question": "?", "options": options()}));
        assert_eq!(answer(&q, None), None);
        let picked = toggle(&q, None, "Red");
        assert_eq!(answer(&q, Some(&picked)), Some(json!("Red")));
        assert!(is_selected(&q, Some(&picked), "Red"));
        let typed = set_custom(&q, Some(&picked), "  green ".into());
        assert_eq!(answer(&q, Some(&typed)), Some(json!("green")));
        assert!(!is_selected(&q, Some(&typed), "Red"));
        let cleared = set_custom(&q, Some(&picked), "".into());
        assert_eq!(answer(&q, Some(&cleared)), Some(json!("Red")), "empty text keeps the pick");
    }

    #[test]
    fn multi_select_toggles_exact_values_in_order() {
        let q = question(json!({"id": "q", "header": "H", "question": "?", "options": options(), "multiSelect": true}));
        let draft = toggle(&q, None, "blue ");
        let draft = toggle(&q, Some(&draft), "Red");
        assert_eq!(answer(&q, Some(&draft)), Some(json!(["blue ", "Red"])));
        let draft = toggle(&q, Some(&draft), "blue ");
        assert_eq!(answer(&q, Some(&draft)), Some(json!(["Red"])));
        assert_eq!(toggle(&q, Some(&draft), "nope"), draft, "unknown option changes nothing");
    }

    #[test]
    fn answers_need_every_question_and_honor_initial_and_choice_only() {
        let initial =
            question(json!({"id": "a", "header": "H", "question": "?", "options": [], "initialAnswer": "draft"}));
        let choice = question(
            json!({"id": "b", "header": "H", "question": "?", "options": options(), "allowCustomAnswer": false}),
        );
        let questions = vec![initial.clone(), choice.clone()];
        let mut state = QuestionDraft::new("r".into(), questions.clone()).unwrap();
        assert_eq!(answers(&questions, &state.drafts), None);
        let typed = set_custom(&choice, None, "free text".into());
        assert_eq!(answer(&choice, Some(&typed)), None, "choice-only ignores typing");
        state.drafts.insert("b".into(), toggle(&choice, None, "Red"));
        assert_eq!(answers(&questions, &state.drafts), Some(json!({"a": "draft", "b": "Red"})));
    }

    #[test]
    fn a_pick_replaces_the_seeded_answer_until_the_user_types() {
        let q =
            question(json!({"id": "q", "header": "H", "question": "?", "options": options(), "initialAnswer": "seed"}));
        let state = QuestionDraft::new("r".into(), vec![q.clone()]).unwrap();
        let seeded = state.drafts.get("q");
        assert_eq!(answer(&q, seeded), Some(json!("seed")));
        let picked = toggle(&q, seeded, "Red");
        assert_eq!(answer(&q, Some(&picked)), Some(json!("Red")));
        assert!(is_selected(&q, Some(&picked), "Red"));
        let typed = set_custom(&q, Some(&picked), "mine".into());
        assert_eq!(answer(&q, Some(&typed)), Some(json!("mine")));
        assert!(!is_selected(&q, Some(&typed), "Red"));
        // Emptying a seeded answer submits it empty, as the web does.
        assert_eq!(answer(&q, Some(&set_custom(&q, Some(&typed), String::new()))), Some(json!("")));
    }
}
