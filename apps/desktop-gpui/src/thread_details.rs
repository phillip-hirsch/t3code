//! What the thread details card shows (`threadDetailsItems.ts`): a client setting that stores
//! only the choices that differ from each item's default.

use serde_json::{Map, Value, json};

use crate::settings::Settings;

/// `THREAD_DETAILS_ITEMS`. Ids are stored in client settings, so never rename one.
pub const ITEMS: [(&str, &str, bool); 12] = [
    ("environment", "Environment and worktree", true),
    ("openIn", "Open in editor", true),
    ("scripts", "Project scripts", true),
    ("branch", "Branch and pull request", true),
    ("gitActions", "Git actions", true),
    ("changes", "Changes", true),
    ("servers", "Servers", true),
    ("automations", "Automations", true),
    ("lineage", "Lineage", true),
    ("sideChats", "Side chats", true),
    ("usage", "Usage limits", false),
    ("context", "Context window", false),
];

/// People who turned on the retired Labs toggle keep Usage and Context until they change them.
fn default_visible(id: &str, settings: &Settings) -> bool {
    match id {
        "usage" | "context" => settings.bool("threadDetailsUsageEnabled"),
        _ => ITEMS.iter().any(|(item, _, visible)| *item == id && *visible),
    }
}

/// `resolveThreadDetailsVisibility`, one item at a time.
pub fn visible(settings: &Settings, id: &str) -> bool {
    settings.get("threadDetailsItems")[id].as_bool().unwrap_or_else(|| default_visible(id, settings))
}

/// The number of items shown, for the Settings trigger's "N of 12 on".
pub fn visible_count(settings: &Settings) -> usize {
    ITEMS.iter().filter(|(id, ..)| visible(settings, id)).count()
}

/// `isDefaultThreadDetailsVisibility`.
pub fn is_default(settings: &Settings) -> bool {
    ITEMS.iter().all(|(id, _, default)| visible(settings, id) == *default)
}

/// `setThreadDetailsItem` as a client settings patch.
pub fn set_item(settings: &Settings, id: &str, shown: bool) -> Value {
    let mut items: Map<String, Value> = settings.get("threadDetailsItems").as_object().cloned().unwrap_or_default();
    items.remove(id);
    if shown != default_visible(id, settings) {
        items.insert(id.to_owned(), Value::Bool(shown));
    }
    json!({ "threadDetailsItems": items })
}

/// `RESET_THREAD_DETAILS_ITEMS`: also clears the retired Labs flag.
pub fn reset() -> Value {
    json!({ "threadDetailsItems": {}, "threadDetailsUsageEnabled": false })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::contract;

    fn client(stored: Value) -> Settings {
        Settings::layered(&contract().client_defaults, &stored)
    }

    #[test]
    fn only_choices_that_differ_from_the_default_are_stored() {
        let settings = client(json!({}));
        assert!(is_default(&settings));
        assert_eq!(visible_count(&settings), 10);
        assert_eq!(set_item(&settings, "usage", true), json!({ "threadDetailsItems": { "usage": true } }));
        let hidden = client(json!({ "threadDetailsItems": { "servers": false, "usage": true } }));
        assert!(!visible(&hidden, "servers") && visible(&hidden, "usage") && !is_default(&hidden));
        assert_eq!(set_item(&hidden, "servers", true), json!({ "threadDetailsItems": { "usage": true } }));
    }

    #[test]
    fn the_retired_labs_toggle_is_the_default_for_usage_and_context() {
        let labs = client(json!({ "threadDetailsUsageEnabled": true }));
        assert!(visible(&labs, "usage") && visible(&labs, "context"));
        assert!(!is_default(&labs), "Usage and Context default to off for everyone else");
        assert_eq!(set_item(&labs, "context", true), json!({ "threadDetailsItems": {} }));
        let reset = client(reset());
        assert!(is_default(&reset));
    }
}
