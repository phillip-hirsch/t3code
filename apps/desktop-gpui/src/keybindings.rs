//! Keybinding rules as the server resolves them (`packages/contracts/src/keybindings.ts`)
//! and the shared defaults and parsers (`packages/shared/src/keybindings.ts`).

use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::Value;

pub const MAX_KEYBINDINGS_COUNT: usize = 256;
const MAX_WHEN_EXPRESSION_DEPTH: usize = 64;

/// `KeybindingShortcut`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shortcut {
    pub key: String,
    pub meta_key: bool,
    pub ctrl_key: bool,
    pub shift_key: bool,
    pub alt_key: bool,
    pub mod_key: bool,
}

/// `KeybindingWhenNode`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum When {
    Identifier { name: String },
    Not { node: Box<When> },
    And { left: Box<When>, right: Box<When> },
    Or { left: Box<When>, right: Box<When> },
}

impl When {
    pub fn identifier(name: impl Into<String>) -> Self {
        When::Identifier { name: name.into() }
    }

    pub fn not(node: When) -> Self {
        When::Not { node: Box::new(node) }
    }

    pub fn binary(or: bool, left: When, right: When) -> Self {
        let (left, right) = (Box::new(left), Box::new(right));
        if or { When::Or { left, right } } else { When::And { left, right } }
    }
}

/// `ResolvedKeybindingRule`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub command: String,
    pub shortcut: Shortcut,
    #[serde(default)]
    pub when_ast: Option<When>,
}

/// `ResolvedKeybindingsConfig`: drops undecodable rules, as `ForwardCompatibleArray` does.
pub fn decode_rules(value: &Value) -> Vec<Rule> {
    let rules: Vec<Rule> = value
        .as_array()
        .map(|items| items.iter().filter_map(|item| Rule::deserialize(item).ok()).collect())
        .unwrap_or_default();
    keep_last(rules)
}

fn keep_last(mut rules: Vec<Rule>) -> Vec<Rule> {
    let excess = rules.len().saturating_sub(MAX_KEYBINDINGS_COUNT);
    rules.drain(..excess);
    rules
}

/// `STATIC_KEYBINDING_COMMANDS`, in contract order.
pub const STATIC_COMMANDS: &[&str] = &[
    "workspace.focusNext",
    "workspace.focusPrevious",
    "sidebar.toggle",
    "navigation.back",
    "navigation.forward",
    "terminal.toggle",
    "terminal.split",
    "terminal.splitVertical",
    "terminal.new",
    "terminal.close",
    "rightPanel.toggle",
    "rightPanel.new",
    "threadPanel.toggle",
    "rightPanel.toggleMaximized",
    "rightPanel.close",
    "view.reopenClosed",
    "pullRequest.copyNumber",
    "diff.toggle",
    "preview.toggle",
    "preview.refresh",
    "preview.focusUrl",
    "preview.zoomIn",
    "preview.zoomOut",
    "preview.resetZoom",
    "commandPalette.toggle",
    "filePicker.toggle",
    "projectSearch.toggle",
    "usage.open",
    "theme.select",
    "appearance.cycle",
    "themeEditor.toggle",
    "composer.stash",
    "composer.sendAlternate",
    "composer.sendBackground",
    "composer.sendAndNewThread",
    "composer.host",
    "composer.cycleHost",
    "composer.effort",
    "composer.increaseEffort",
    "composer.decreaseEffort",
    "composer.mode",
    "composer.workspace",
    "composer.previousWorktree",
    "composer.branch",
    "chat.new",
    "chat.newLocal",
    "chat.newWithoutProject",
    "chat.find",
    "editor.openFavorite",
    "usage.cost",
    "usage.tokens",
    "usage.limits",
    "usage.period.day",
    "usage.period.week",
    "usage.period.month",
    "usage.period.quarter",
    "modelPicker.toggle",
    "modelPicker.previousProvider",
    "modelPicker.nextProvider",
    "modelPicker.jump.1",
    "modelPicker.jump.2",
    "modelPicker.jump.3",
    "modelPicker.jump.4",
    "modelPicker.jump.5",
    "modelPicker.jump.6",
    "modelPicker.jump.7",
    "modelPicker.jump.8",
    "modelPicker.jump.9",
    "thread.stop",
    "thread.steerQueuedMessage",
    "thread.editQueuedMessage",
    "thread.previous",
    "thread.next",
    "thread.copyReference",
    "thread.settle",
    "thread.pin",
    "thread.undo",
    "thread.jump.1",
    "thread.jump.2",
    "thread.jump.3",
    "thread.jump.4",
    "thread.jump.5",
    "thread.jump.6",
    "thread.jump.7",
    "thread.jump.8",
    "thread.jump.9",
];

/// `DEFAULT_KEYBINDINGS` as (key, command, when); the numbered jumps are added by [`defaults`].
const DEFAULT_KEYBINDINGS: &[(&str, &str, Option<&str>)] = &[
    ("mod+b", "sidebar.toggle", None),
    ("f6", "workspace.focusNext", None),
    ("shift+f6", "workspace.focusPrevious", None),
    ("mod+f6", "workspace.focusNext", None),
    ("mod+shift+f6", "workspace.focusPrevious", None),
    ("mod+[", "navigation.back", Some("!terminalFocus")),
    ("mod+]", "navigation.forward", Some("!terminalFocus")),
    ("mod+j", "terminal.toggle", None),
    ("mod+alt+b", "rightPanel.toggle", None),
    ("mod+shift+b", "threadPanel.toggle", Some("!terminalFocus")),
    ("mod+t", "rightPanel.new", Some("rightPanelOpen && !terminalFocus")),
    ("mod+d", "terminal.split", Some("terminalFocus")),
    ("mod+shift+d", "terminal.splitVertical", Some("terminalFocus")),
    ("mod+n", "terminal.new", Some("terminalFocus")),
    ("mod+w", "terminal.close", Some("terminalFocus")),
    ("mod+w", "rightPanel.close", Some("!terminalFocus")),
    ("mod+shift+t", "view.reopenClosed", None),
    ("mod+d", "diff.toggle", Some("!terminalFocus")),
    ("mod+shift+j", "preview.toggle", None),
    ("mod+r", "preview.refresh", Some("previewFocus")),
    ("mod+l", "preview.focusUrl", Some("previewFocus")),
    ("mod+=", "preview.zoomIn", Some("previewFocus")),
    ("mod++", "preview.zoomIn", Some("previewFocus")),
    ("mod+-", "preview.zoomOut", Some("previewFocus")),
    ("mod+0", "preview.resetZoom", Some("previewFocus")),
    ("mod+k", "commandPalette.toggle", Some("!terminalFocus")),
    ("mod+p", "filePicker.toggle", Some("!terminalFocus")),
    ("mod+shift+f", "projectSearch.toggle", Some("!terminalFocus")),
    ("mod+u", "usage.open", Some("!terminalFocus")),
    ("mod+alt+a", "theme.select", Some("!terminalFocus")),
    ("mod+alt+shift+a", "appearance.cycle", Some("!terminalFocus")),
    ("mod+alt+shift+t", "themeEditor.toggle", None),
    ("mod+s", "composer.stash", Some("!terminalFocus")),
    ("mod+shift+enter", "thread.steerQueuedMessage", Some("!terminalFocus")),
    ("alt+arrowup", "thread.editQueuedMessage", Some("composerFocus")),
    ("mod+enter", "composer.sendAlternate", Some("composerFocus && turnRunning")),
    ("mod+enter", "composer.sendBackground", Some("composerFocus && draftThreadRoute")),
    ("mod+alt+enter", "composer.sendBackground", Some("composerFocus && draftThreadRoute")),
    ("mod+alt+enter", "composer.sendAndNewThread", Some("composerFocus && !draftThreadRoute")),
    ("mod+n", "chat.new", Some("!terminalFocus")),
    ("mod+shift+o", "chat.new", Some("!terminalFocus")),
    ("mod+shift+n", "chat.newLocal", Some("!terminalFocus")),
    ("mod+alt+n", "chat.newWithoutProject", Some("!terminalFocus")),
    ("mod+f", "chat.find", Some("!terminalFocus && !previewFocus")),
    ("mod+shift+m", "modelPicker.toggle", Some("!terminalFocus")),
    ("mod+shift+h", "composer.host", Some("!terminalFocus")),
    ("mod+shift+e", "composer.effort", Some("!terminalFocus")),
    ("mod+shift+a", "composer.mode", Some("!terminalFocus")),
    ("mod+shift+x", "composer.workspace", Some("!terminalFocus")),
    ("mod+shift+g", "composer.branch", Some("!terminalFocus")),
    ("mod+shift+l", "composer.previousWorktree", Some("!terminalFocus")),
    ("mod+shift+k", "pullRequest.copyNumber", Some("!terminalFocus")),
    ("mod+shift+arrowup", "modelPicker.previousProvider", Some("modelPickerOpen")),
    ("mod+shift+arrowdown", "modelPicker.nextProvider", Some("modelPickerOpen")),
    ("mod+o", "editor.openFavorite", None),
    ("mod+shift+[", "thread.previous", None),
    ("mod+shift+]", "thread.next", None),
    ("mod+shift+c", "thread.copyReference", Some("!terminalFocus")),
    ("mod+shift+s", "thread.settle", Some("!terminalFocus")),
    ("mod+shift+p", "thread.pin", Some("!terminalFocus")),
    ("mod+z", "thread.undo", Some("!terminalFocus && !editableFocus")),
];

const USAGE_PAGE_KEYBINDINGS: &[(&str, &str, Option<&str>)] = &[
    ("c", "usage.cost", Some("usagePageOpen")),
    ("t", "usage.tokens", Some("usagePageOpen")),
    ("l", "usage.limits", Some("usagePageOpen")),
    ("mod+shift+1", "usage.period.day", Some("usagePageOpen")),
    ("mod+shift+2", "usage.period.week", Some("usagePageOpen")),
    ("mod+shift+3", "usage.period.month", Some("usagePageOpen")),
    ("mod+shift+4", "usage.period.quarter", Some("usagePageOpen")),
];

/// `DEFAULT_RESOLVED_KEYBINDINGS`.
pub fn defaults() -> &'static [Rule] {
    static DEFAULTS: LazyLock<Vec<Rule>> = LazyLock::new(|| {
        fn jumps(prefix: &'static str, when: &'static str) -> impl Iterator<Item = (String, String, Option<String>)> {
            (1..=9).map(move |n| (format!("mod+{n}"), format!("{prefix}.jump.{n}"), Some(when.to_owned())))
        }
        let owned = |list: &[(&str, &str, Option<&str>)]| {
            list.iter()
                .map(|(key, command, when)| (key.to_string(), command.to_string(), when.map(str::to_owned)))
                .collect::<Vec<_>>()
        };
        let all = owned(DEFAULT_KEYBINDINGS)
            .into_iter()
            .chain(jumps("thread", "!modelPickerOpen && isDesktop"))
            .chain(jumps("modelPicker", "modelPickerOpen && isDesktop"))
            .chain(owned(USAGE_PAGE_KEYBINDINGS));
        keep_last(all.filter_map(|(key, command, when)| compile(&key, command, when.as_deref())).collect())
    });
    &DEFAULTS
}

/// `compileResolvedKeybindingRule`.
pub fn compile(key: &str, command: impl Into<String>, when: Option<&str>) -> Option<Rule> {
    let shortcut = parse_shortcut(key)?;
    let when_ast = match when {
        Some(when) => Some(parse_when(when)?),
        None => None,
    };
    Some(Rule { command: command.into(), shortcut, when_ast })
}

/// `mergeWithDefaultKeybindings`: a command the server lists replaces all of its defaults.
pub fn merge_with_defaults(custom: &[Rule]) -> Vec<Rule> {
    let overridden: std::collections::HashSet<&str> = custom.iter().map(|rule| rule.command.as_str()).collect();
    let merged =
        defaults().iter().filter(|rule| !overridden.contains(rule.command.as_str())).chain(custom).cloned().collect();
    keep_last(merged)
}

/// `parseKeybindingShortcut`.
pub fn parse_shortcut(value: &str) -> Option<Shortcut> {
    let lower = value.to_lowercase();
    let mut tokens: Vec<&str> = lower.split('+').map(str::trim).collect();
    let mut trailing_empty = 0;
    while tokens.last() == Some(&"") {
        trailing_empty += 1;
        tokens.pop();
    }
    if trailing_empty > 0 {
        tokens.push("+");
    }
    if tokens.is_empty() || tokens.iter().any(|token| token.is_empty()) {
        return None;
    }
    let mut shortcut = Shortcut::default();
    let mut key = None;
    for token in tokens {
        match token {
            "cmd" | "meta" => shortcut.meta_key = true,
            "ctrl" | "control" => shortcut.ctrl_key = true,
            "shift" => shortcut.shift_key = true,
            "alt" | "option" => shortcut.alt_key = true,
            "mod" => shortcut.mod_key = true,
            other => {
                if key.is_some() {
                    return None;
                }
                key = Some(match other {
                    "space" => " ",
                    "esc" => "escape",
                    other => other,
                });
            }
        }
    }
    shortcut.key = key?.to_owned();
    Some(shortcut)
}

/// `shortcutToKeybindingInput`: the inverse of [`parse_shortcut`], in canonical modifier order.
pub fn shortcut_input(shortcut: &Shortcut) -> String {
    let mut parts = Vec::new();
    for (on, name) in [
        (shortcut.mod_key, "mod"),
        (shortcut.meta_key, "meta"),
        (shortcut.ctrl_key, "ctrl"),
        (shortcut.alt_key, "alt"),
        (shortcut.shift_key, "shift"),
    ] {
        if on {
            parts.push(name);
        }
    }
    parts.push(match shortcut.key.as_str() {
        " " => "space",
        "escape" => "esc",
        key => key,
    });
    parts.join("+")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Identifier(&'a str),
    Not,
    And,
    Or,
    LParen,
    RParen,
}

fn tokenize(expression: &str) -> Option<Vec<Token<'_>>> {
    let mut tokens = Vec::new();
    let mut rest = expression;
    while let Some(current) = rest.chars().next() {
        if current.is_whitespace() {
            rest = &rest[current.len_utf8()..];
            continue;
        }
        let (token, len) = if rest.starts_with("&&") {
            (Token::And, 2)
        } else if rest.starts_with("||") {
            (Token::Or, 2)
        } else if current == '!' {
            (Token::Not, 1)
        } else if current == '(' {
            (Token::LParen, 1)
        } else if current == ')' {
            (Token::RParen, 1)
        } else if current.is_ascii_alphabetic() || current == '_' {
            let len =
                rest.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))).unwrap_or(rest.len());
            (Token::Identifier(&rest[..len]), len)
        } else {
            return None;
        };
        tokens.push(token);
        rest = &rest[len..];
    }
    Some(tokens)
}

/// `parseKeybindingWhenExpression`: `!` binds tightest, then `&&`, then `||`.
pub fn parse_when(expression: &str) -> Option<When> {
    struct Parser<'a> {
        tokens: Vec<Token<'a>>,
        index: usize,
    }
    impl Parser<'_> {
        fn peek(&self) -> Option<Token<'_>> {
            self.tokens.get(self.index).copied()
        }
        fn primary(&mut self, depth: usize) -> Option<When> {
            if depth > MAX_WHEN_EXPRESSION_DEPTH {
                return None;
            }
            match self.peek()? {
                Token::Identifier(name) => {
                    let node = When::identifier(name);
                    self.index += 1;
                    Some(node)
                }
                Token::LParen => {
                    self.index += 1;
                    let node = self.or(depth + 1)?;
                    if self.peek() != Some(Token::RParen) {
                        return None;
                    }
                    self.index += 1;
                    Some(node)
                }
                _ => None,
            }
        }
        fn unary(&mut self, depth: usize) -> Option<When> {
            let mut nots = 0;
            while self.peek() == Some(Token::Not) {
                self.index += 1;
                nots += 1;
                if nots > MAX_WHEN_EXPRESSION_DEPTH {
                    return None;
                }
            }
            let mut node = self.primary(depth)?;
            for _ in 0..nots {
                node = When::not(node);
            }
            Some(node)
        }
        fn and(&mut self, depth: usize) -> Option<When> {
            let mut left = self.unary(depth)?;
            while self.peek() == Some(Token::And) {
                self.index += 1;
                left = When::binary(false, left, self.unary(depth)?);
            }
            Some(left)
        }
        fn or(&mut self, depth: usize) -> Option<When> {
            let mut left = self.and(depth)?;
            while self.peek() == Some(Token::Or) {
                self.index += 1;
                left = When::binary(true, left, self.and(depth)?);
            }
            Some(left)
        }
    }
    let tokens = tokenize(expression)?;
    if tokens.is_empty() {
        return None;
    }
    let mut parser = Parser { tokens, index: 0 };
    let ast = parser.or(0)?;
    (parser.index == parser.tokens.len()).then_some(ast)
}

/// `whenAstToExpression`: groups are parenthesized, conditions and negations are not.
pub fn when_expression(node: Option<&When>) -> String {
    fn wrap(node: &When) -> String {
        match node {
            When::Identifier { .. } | When::Not { .. } => render(node),
            _ => format!("({})", render(node)),
        }
    }
    fn render(node: &When) -> String {
        match node {
            When::Identifier { name } => name.clone(),
            When::Not { node } => format!("!{}", wrap(node)),
            When::And { left, right } => format!("{} && {}", wrap(left), wrap(right)),
            When::Or { left, right } => format!("{} || {}", wrap(left), wrap(right)),
        }
    }
    node.map(render).unwrap_or_default()
}

/// `evaluateWhenNode`: `true`/`false` are literals; any other name reads `context`.
pub fn evaluate_when(node: &When, context: &impl Fn(&str) -> bool) -> bool {
    match node {
        When::Identifier { name } => match name.as_str() {
            "true" => true,
            "false" => false,
            name => context(name),
        },
        When::Not { node } => !evaluate_when(node, context),
        When::And { left, right } => evaluate_when(left, context) && evaluate_when(right, context),
        When::Or { left, right } => evaluate_when(left, context) || evaluate_when(right, context),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shortcuts_parse_and_print_canonically() {
        let shortcut = parse_shortcut("Shift+Mod+K").unwrap();
        assert!(shortcut.mod_key && shortcut.shift_key && !shortcut.ctrl_key);
        assert_eq!(shortcut_input(&shortcut), "mod+shift+k");
        assert_eq!(shortcut_input(&parse_shortcut("mod++").unwrap()), "mod++", "a trailing + is the key");
        assert_eq!(parse_shortcut("space").unwrap().key, " ");
        assert_eq!(shortcut_input(&parse_shortcut("esc").unwrap()), "esc");
        assert_eq!(parse_shortcut("mod+a+b"), None, "two keys");
        assert_eq!(parse_shortcut("mod+shift"), None, "no key");
        assert_eq!(parse_shortcut("mod++k"), None, "an empty token mid-chord");
    }

    #[test]
    fn when_expressions_respect_precedence_and_round_trip() {
        let ast = parse_when("a || !b && (c || d)").unwrap();
        assert_eq!(
            ast,
            When::binary(
                true,
                When::identifier("a"),
                When::binary(
                    false,
                    When::not(When::identifier("b")),
                    When::binary(true, When::identifier("c"), When::identifier("d"))
                )
            )
        );
        assert_eq!(when_expression(Some(&ast)), "a || (!b && (c || d))");
        assert_eq!(parse_when(&when_expression(Some(&ast))), Some(ast));
        assert_eq!(when_expression(Some(&parse_when("!(a && b)").unwrap())), "!(a && b)");
        for bad in ["", "a &&", "(a", "a b", "a & b", "1a"] {
            assert_eq!(parse_when(bad), None, "{bad:?}");
        }
        assert_eq!(parse_when(&format!("{}a{}", "(".repeat(70), ")".repeat(70))), None, "too deep");
    }

    #[test]
    fn when_evaluation_reads_the_context() {
        let ast = parse_when("!terminalFocus && isDesktop || false").unwrap();
        assert!(evaluate_when(&ast, &|name| name == "isDesktop"));
        assert!(!evaluate_when(&ast, &|_| false));
        assert!(evaluate_when(&parse_when("true").unwrap(), &|_| false));
    }

    #[test]
    fn server_rules_replace_a_commands_defaults() {
        let custom = decode_rules(&json!([
            {"command": "sidebar.toggle", "shortcut": {"key": "y", "metaKey": false, "ctrlKey": false, "shiftKey": false, "altKey": true, "modKey": true}},
            {"command": "future.thing", "shortcut": {"key": 1}},
        ]));
        assert_eq!(custom.len(), 1, "an undecodable rule is dropped");
        let merged = merge_with_defaults(&custom);
        let sidebar: Vec<String> = merged
            .iter()
            .filter(|rule| rule.command == "sidebar.toggle")
            .map(|rule| shortcut_input(&rule.shortcut))
            .collect();
        assert_eq!(sidebar, ["mod+alt+y"]);
        assert_eq!(merged.len(), defaults().len(), "one default replaced by one custom rule");
        assert_eq!(merge_with_defaults(&[]).len(), defaults().len());
        assert!(defaults().iter().any(|rule| rule.command == "thread.jump.9"));
        assert_eq!(defaults().len(), DEFAULT_KEYBINDINGS.len() + 18 + USAGE_PAGE_KEYBINDINGS.len());
    }
}
