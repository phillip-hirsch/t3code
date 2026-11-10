//! Pending questions and the answers built from their drafts, as
//! `packages/client-runtime/src/state/pendingUserInput.ts`.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::composer_context::ContextRecords;
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

/// What a question's answer carries beside its text (`attachmentCount`, `attachmentsBlocked`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Attached {
    pub count: usize,
    /// Still preparing or uploading, or failed: the answer waits.
    pub blocked: bool,
}

/// The open question card: which request, which question, and every draft. `A` is an
/// attachment, kept per question (`questionAttachmentDraftId`).
pub struct QuestionDraft<A> {
    pub request_id: String,
    /// Never empty: [`QuestionDraft::new`] refuses an empty list.
    pub questions: Vec<Question>,
    pub index: usize,
    pub drafts: HashMap<String, Draft>,
    pub collapsed: bool,
    /// What the editor held when the card took it: the thread's prompt, or a queued message's edit.
    pub prompt: String,
    /// The prompt's attachments, set aside with it.
    pub prompt_attachments: Vec<A>,
    /// The context records the prompt's chips name, set aside with it.
    pub prompt_records: ContextRecords,
    /// The attachments of questions other than the one on screen, whose are the composer's.
    pub attachments: HashMap<String, Vec<A>>,
}

impl<A> QuestionDraft<A> {
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
        Some(Self {
            request_id,
            questions,
            index: 0,
            drafts,
            collapsed: false,
            prompt: String::new(),
            prompt_attachments: Vec::new(),
            prompt_records: ContextRecords::default(),
            attachments: HashMap::new(),
        })
    }

    /// The question on screen.
    pub fn active(&self) -> &Question {
        &self.questions[self.index]
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

/// The answer to submit for one question, or `None` while it is unanswered. Attachments
/// alone answer a question that takes custom answers, with empty text.
pub fn answer(question: &Question, draft: Option<&Draft>, attached: Attached) -> Option<Value> {
    if attached.blocked {
        return None;
    }
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
    let custom = custom.or_else(|| (question.allows_custom() && attached.count > 0).then(String::new));
    if question.multi_select {
        if !selected.is_empty() {
            return Some(Value::Array(selected.into_iter().map(Value::String).collect()));
        }
        return custom.map(Value::String);
    }
    selected.into_iter().next().or(custom).map(Value::String)
}

/// Every answer keyed by question id, or `None` while any is unanswered.
pub fn answers(
    questions: &[Question],
    drafts: &HashMap<String, Draft>,
    attached: impl Fn(&str) -> Attached,
) -> Option<Value> {
    let mut map = Map::new();
    for question in questions {
        map.insert(question.id.clone(), answer(question, drafts.get(&question.id), attached(&question.id))?);
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

    const NONE: Attached = Attached { count: 0, blocked: false };

    fn question(value: Value) -> Question {
        serde_json::from_value(value).unwrap()
    }

    fn options() -> Value {
        json!([{"label": "Red", "description": "r"}, {"label": "Blue", "description": "b", "value": "blue "}])
    }

    #[test]
    fn single_select_answers_with_the_option_and_custom_text_wins() {
        let q = question(json!({"id": "q", "header": "H", "question": "?", "options": options()}));
        assert_eq!(answer(&q, None, NONE), None);
        let picked = toggle(&q, None, "Red");
        assert_eq!(answer(&q, Some(&picked), NONE), Some(json!("Red")));
        assert!(is_selected(&q, Some(&picked), "Red"));
        let typed = set_custom(&q, Some(&picked), "  green ".into());
        assert_eq!(answer(&q, Some(&typed), NONE), Some(json!("green")));
        assert!(!is_selected(&q, Some(&typed), "Red"));
        let cleared = set_custom(&q, Some(&picked), String::new());
        assert_eq!(answer(&q, Some(&cleared), NONE), Some(json!("Red")), "empty text keeps the pick");
    }

    #[test]
    fn multi_select_toggles_exact_values_in_order() {
        let q = question(json!({"id": "q", "header": "H", "question": "?", "options": options(), "multiSelect": true}));
        let draft = toggle(&q, None, "blue ");
        let draft = toggle(&q, Some(&draft), "Red");
        assert_eq!(answer(&q, Some(&draft), NONE), Some(json!(["blue ", "Red"])));
        let draft = toggle(&q, Some(&draft), "blue ");
        assert_eq!(answer(&q, Some(&draft), NONE), Some(json!(["Red"])));
        assert_eq!(toggle(&q, Some(&draft), "nope"), draft, "unknown option changes nothing");
    }

    #[test]
    fn answers_need_every_question_and_honor_initial_and_choice_only() {
        let initial =
            question(json!({"id": "a", "header": "H", "question": "?", "options": [], "initialAnswer": "draft"}));
        let choice = question(
            json!({"id": "b", "header": "H", "question": "?", "options": options(), "allowCustomAnswer": false}),
        );
        let questions = vec![initial, choice.clone()];
        let mut state = QuestionDraft::<()>::new("r".into(), questions.clone()).unwrap();
        assert_eq!(answers(&questions, &state.drafts, |_| NONE), None);
        let typed = set_custom(&choice, None, "free text".into());
        assert_eq!(answer(&choice, Some(&typed), NONE), None, "choice-only ignores typing");
        state.drafts.insert("b".into(), toggle(&choice, None, "Red"));
        assert_eq!(answers(&questions, &state.drafts, |_| NONE), Some(json!({"a": "draft", "b": "Red"})));
    }

    #[test]
    fn a_pick_replaces_the_seeded_answer_until_the_user_types() {
        let q =
            question(json!({"id": "q", "header": "H", "question": "?", "options": options(), "initialAnswer": "seed"}));
        let state = QuestionDraft::<()>::new("r".into(), vec![q.clone()]).unwrap();
        let seeded = state.drafts.get("q");
        assert_eq!(answer(&q, seeded, NONE), Some(json!("seed")));
        let picked = toggle(&q, seeded, "Red");
        assert_eq!(answer(&q, Some(&picked), NONE), Some(json!("Red")));
        assert!(is_selected(&q, Some(&picked), "Red"));
        let typed = set_custom(&q, Some(&picked), "mine".into());
        assert_eq!(answer(&q, Some(&typed), NONE), Some(json!("mine")));
        assert!(!is_selected(&q, Some(&typed), "Red"));
        // Emptying a seeded answer submits it empty, as the web does.
        assert_eq!(answer(&q, Some(&set_custom(&q, Some(&typed), String::new())), NONE), Some(json!("")));
    }

    #[test]
    fn attachments_answer_a_question_once_they_have_uploaded() {
        let open = question(json!({"id": "q", "header": "H", "question": "?", "options": options()}));
        let multi =
            question(json!({"id": "m", "header": "H", "question": "?", "options": options(), "multiSelect": true}));
        let choice = question(
            json!({"id": "c", "header": "H", "question": "?", "options": options(), "allowCustomAnswer": false}),
        );
        let ready = Attached { count: 2, blocked: false };
        let uploading = Attached { count: 1, blocked: true };
        assert_eq!(answer(&open, None, ready), Some(json!("")), "attachments alone answer with empty text");
        assert_eq!(answer(&multi, None, ready), Some(json!("")));
        assert_eq!(answer(&choice, None, ready), None, "a choice-only question still needs its pick");
        assert_eq!(answer(&open, None, uploading), None, "an upload in flight holds the answer");
        let typed = set_custom(&open, None, "see the screenshot".into());
        assert_eq!(answer(&open, Some(&typed), ready), Some(json!("see the screenshot")));
        assert_eq!(answer(&open, Some(&typed), uploading), None, "even a typed answer waits for its uploads");
        let picked = toggle(&open, None, "Red");
        assert_eq!(answer(&open, Some(&picked), ready), Some(json!("Red")));
        let drafts = HashMap::new();
        let attached = |id: &str| if id == "q" { ready } else { NONE };
        assert_eq!(answers(std::slice::from_ref(&open), &drafts, attached), Some(json!({"q": ""})));
        assert_eq!(answers(&[open, multi], &drafts, attached), None, "the other question is unanswered");
    }
}
