//! Settings → Keybindings (`KeybindingsSettings.tsx`, `KeybindingsSettings.logic.ts`).
//! Edits go through `server.upsertKeybinding` / `server.removeKeybinding`; the server's
//! `keybindingsUpdated` event (and each RPC's result) then reinstalls GPUI's keymap
//! (`app::keymap`). Keybindings are environment-wide in the web, never project-scoped.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::LazyLock;

use gpui_kit::Focusable as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _, Toggle, ToggleVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Div, ElementId, Entity, FontWeight, InteractiveElement as _,
    IntoElement, Keystroke, MouseButton, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, WeakEntity, Window, anchored, deferred, div, px,
};
use serde_json::{Value, json};

use super::widgets::{RowFrame, card, select_trigger, settings_section};

const MOD_LABEL: &str = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" };
use crate::app::Workspace;
use crate::keybindings::{
    self, Rule, STATIC_COMMANDS, Shortcut, When, defaults, parse_shortcut, parse_when, shortcut_input, when_expression,
};
use crate::theme::{self, Tokens};

const MAC: bool = cfg!(target_os = "macos");
/// The draft row of "Add keybinding".
const NEW_ROW: &str = "new";

// ---------------------------------------------------------------------------
// Rows (`KeybindingsSettings.logic.ts`)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Default,
    Custom,
    Project,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Source::Default => "Default",
            Source::Custom => "Custom",
            Source::Project => "Project",
        }
    }
}

#[derive(Clone, Debug)]
struct KeyRow {
    id: String,
    command: String,
    key: String,
    when: String,
    source: Source,
    default_key: Option<String>,
    default_when: String,
    rule: Rule,
    conflicts: Vec<String>,
}

fn same_binding(rule: &Rule, key: &str, when: &str) -> bool {
    shortcut_input(&rule.shortcut) == key && when_expression(rule.when_ast.as_ref()) == when
}

fn source_for(rule: &Rule, key: &str, when: &str) -> Source {
    if rule.command.starts_with("script.") {
        Source::Project
    } else if defaults().iter().any(|d| d.command == rule.command && same_binding(d, key, when)) {
        Source::Default
    } else {
        Source::Custom
    }
}

/// The default a row resets to: the same binding, else the same condition, else the command's first.
fn default_for(rule: &Rule, key: &str, when: &str) -> Option<&'static Rule> {
    let commands = || defaults().iter().filter(|d| d.command == rule.command);
    commands()
        .find(|d| same_binding(d, key, when))
        .or_else(|| commands().find(|d| when_expression(d.when_ast.as_ref()) == when))
        .or_else(|| commands().next())
}

/// `shortcutConflictKey`: compares resolved chords, so `mod+k` and `ctrl+k` collide where mod is Ctrl.
fn conflict_key(shortcut: &Shortcut, mac: bool) -> String {
    let meta = shortcut.meta_key || (shortcut.mod_key && mac);
    let ctrl = shortcut.ctrl_key || (shortcut.mod_key && !mac);
    let flag = |on: bool, name: &'static str| if on { name } else { "" };
    [
        shortcut.key.as_str(),
        flag(meta, "meta"),
        flag(ctrl, "ctrl"),
        flag(shortcut.shift_key, "shift"),
        flag(shortcut.alt_key, "alt"),
    ]
    .join("|")
}

/// `keybindingConflictLabels`: other rows on the same chord whose conditions can both hold.
fn conflict_labels(rows: &[KeyRow], row_id: &str, key: &str, when: &str, mac: bool) -> Vec<String> {
    let Some(shortcut) = parse_shortcut(key).filter(|_| !key.trim().is_empty()) else { return Vec::new() };
    let chord = conflict_key(&shortcut, mac);
    let mut labels: Vec<String> = rows
        .iter()
        .filter(|row| {
            row.id != row_id
                && conflict_key(&row.rule.shortcut, mac) == chord
                && (row.when.is_empty() || when.is_empty() || row.when == when)
        })
        .map(|row| command_label(&row.command))
        .collect();
    labels.sort();
    labels.dedup();
    labels
}

/// Usage commands keep the Usage page's order; everything else sorts by `key`.
const USAGE_ORDER: [&str; 8] = [
    "usage.open",
    "usage.cost",
    "usage.tokens",
    "usage.limits",
    "usage.period.day",
    "usage.period.week",
    "usage.period.month",
    "usage.period.quarter",
];

fn compare_commands(left: &str, right: &str, key: impl Fn(&str) -> String) -> Ordering {
    let rank = |command: &str| USAGE_ORDER.iter().position(|c| *c == command);
    match (rank(left), rank(right)) {
        (Some(l), Some(r)) => l.cmp(&r),
        (l, r) => {
            let first_usage = USAGE_ORDER[1];
            locale_cmp(
                &key(if l.is_none() { left } else { first_usage }),
                &key(if r.is_none() { right } else { first_usage }),
            )
        }
    }
}

/// An approximation of `String.prototype.localeCompare`: case-insensitive first, lowercase before uppercase.
fn locale_cmp(left: &str, right: &str) -> Ordering {
    left.to_lowercase().cmp(&right.to_lowercase()).then_with(|| right.cmp(left))
}

/// `buildKeybindingRows`: one row per distinct (command, key, when), sorted by command then key.
fn build_rows(rules: &[Rule], mac: bool) -> Vec<KeyRow> {
    let mut rows: Vec<KeyRow> = Vec::new();
    let mut index_by_id: HashMap<String, usize> = HashMap::new();
    for rule in rules {
        let key = shortcut_input(&rule.shortcut);
        let when = when_expression(rule.when_ast.as_ref());
        let default = default_for(rule, &key, &when);
        let row = KeyRow {
            id: format!("{}\u{0}{key}\u{0}{when}", rule.command),
            command: rule.command.clone(),
            source: source_for(rule, &key, &when),
            default_key: default.map(|d| shortcut_input(&d.shortcut)),
            default_when: when_expression(default.and_then(|d| d.when_ast.as_ref())),
            rule: rule.clone(),
            conflicts: Vec::new(),
            key,
            when,
        };
        match index_by_id.get(&row.id) {
            Some(&index) => rows[index] = row,
            None => {
                index_by_id.insert(row.id.clone(), rows.len());
                rows.push(row);
            }
        }
    }
    let conflicts: Vec<Vec<String>> =
        rows.iter().map(|row| conflict_labels(&rows, &row.id, &row.key, &row.when, mac)).collect();
    for (row, conflicts) in rows.iter_mut().zip(conflicts) {
        row.conflicts = conflicts;
    }
    rows.sort_by(|a, b| {
        compare_commands(&a.command, &b.command, str::to_owned).then_with(|| locale_cmp(&a.key, &b.key))
    });
    rows
}

fn filter_rows<'a>(rows: &'a [KeyRow], query: &str) -> Vec<&'a KeyRow> {
    let query = query.trim().to_lowercase();
    rows.iter()
        .filter(|row| {
            query.is_empty()
                || [&row.command, &command_label(&row.command), &row.key, &row.when, row.source.label()]
                    .iter()
                    .any(|field| field.to_lowercase().contains(&query))
        })
        .collect()
}

const GROUPS: &[(&str, &str, &[&str])] = &[
    ("navigation", "Navigation", &["sidebar", "rightPanel", "commandPalette", "filePicker", "projectSearch", "editor"]),
    ("threads", "Threads", &["thread", "chat", "pullRequest"]),
    ("composer", "Composer", &["composer", "modelPicker"]),
    ("terminal", "Terminal", &["terminal"]),
    ("preview", "Preview & diff", &["preview", "diff"]),
    ("appearance", "Appearance", &["theme", "appearance", "themeEditor"]),
    ("scripts", "Project scripts", &["script"]),
    ("other", "Other", &[]),
];

/// `groupKeybindingRows`: by command prefix, in `GROUPS` order, unknown prefixes last.
fn group_rows<'a>(rows: &[&'a KeyRow]) -> Vec<(&'static str, &'static str, Vec<&'a KeyRow>)> {
    let group_of = |command: &str| {
        let prefix = command.split('.').next().unwrap_or_default();
        GROUPS.iter().position(|(_, _, prefixes)| prefixes.contains(&prefix)).unwrap_or(GROUPS.len() - 1)
    };
    let mut buckets: Vec<Vec<&KeyRow>> = vec![Vec::new(); GROUPS.len()];
    for row in rows {
        buckets[group_of(&row.command)].push(row);
    }
    GROUPS
        .iter()
        .zip(buckets)
        .filter(|(_, rows)| !rows.is_empty())
        .map(|((id, title, _), rows)| (*id, *title, rows))
        .collect()
}

/// `buildKeybindingCommandOptions`: every static command plus the server's, by label.
fn command_options(rules: &[Rule]) -> Vec<String> {
    let mut commands: Vec<String> = STATIC_COMMANDS.iter().map(|c| c.to_string()).collect();
    for rule in rules {
        if !commands.contains(&rule.command) {
            commands.push(rule.command.clone());
        }
    }
    commands.sort_by(|a, b| compare_commands(a, b, command_label));
    commands
}

/// `commandLabel`.
pub(crate) fn command_label(command: &str) -> String {
    let special = match command {
        "workspace.focusNext" => Some("Focus Next Workspace Area"),
        "workspace.focusPrevious" => Some("Focus Previous Workspace Area"),
        "composer.sendAlternate" => Some("Composer: Opposite Queue or Steer Action"),
        "composer.sendBackground" => Some("Composer: Start in Background"),
        "composer.sendAndNewThread" => Some("Composer: Send and Start New Thread"),
        "thread.steerQueuedMessage" => Some("Queue: Send First Queued Message"),
        "thread.editQueuedMessage" => Some("Queue: Edit Last Queued Message"),
        "thread.copyReference" => Some("Pull Request: Copy Link or Thread"),
        "usage.cost" => Some("Usage: Cost"),
        "usage.tokens" => Some("Usage: Tokens"),
        "usage.limits" => Some("Usage: Limits"),
        "usage.period.day" => Some("Usage: Period: Past 24h"),
        "usage.period.week" => Some("Usage: Period: 7 days"),
        "usage.period.month" => Some("Usage: Period: 30 days"),
        "usage.period.quarter" => Some("Usage: Period: 90 days"),
        "view.reopenClosed" => Some("Reopen Closed Tab"),
        "preview.focusUrl" => Some("Preview: Focus URL"),
        _ => None,
    };
    if let Some(label) = special {
        return label.to_owned();
    }
    if let Some(script) = command.strip_prefix("script.").and_then(|rest| rest.strip_suffix(".run")) {
        return format!("Run Script: {}", title_case(script));
    }
    command.split('.').map(title_case).collect::<Vec<_>>().join(": ")
}

/// `titleCaseCommandSegment`: splits camelCase, `-`, `_` and spaces, then capitalizes each word.
fn title_case(segment: &str) -> String {
    let mut spaced = String::with_capacity(segment.len() + 4);
    let mut previous: Option<char> = None;
    for c in segment.chars() {
        if c.is_ascii_uppercase() && previous.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit()) {
            spaced.push(' ');
        }
        spaced.push(c);
        previous = Some(c);
    }
    spaced
        .split(|c: char| c == '-' || c == '_' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

const CORE_WHEN_VARIABLES: [&str; 6] = ["terminalFocus", "terminalOpen", "isWeb", "isDesktop", "true", "false"];
const DEFAULT_WHEN_VARIABLE: &str = "terminalFocus";

/// `buildWhenVariableOptions`: the core variables first, then every one the defaults use.
fn when_variables() -> &'static [String] {
    static VARIABLES: LazyLock<Vec<String>> = LazyLock::new(|| {
        let mut names: Vec<String> = CORE_WHEN_VARIABLES.iter().map(|n| n.to_string()).collect();
        for rule in defaults() {
            collect_identifiers(rule.when_ast.as_ref(), &mut names);
        }
        let core = |name: &str| CORE_WHEN_VARIABLES.iter().position(|c| *c == name);
        names.sort_by(|a, b| match (core(a), core(b)) {
            (None, None) => locale_cmp(a, b),
            (l, r) => l.unwrap_or(usize::MAX).cmp(&r.unwrap_or(usize::MAX)),
        });
        names
    });
    &VARIABLES
}

fn collect_identifiers(node: Option<&When>, names: &mut Vec<String>) {
    match node {
        None => {}
        Some(When::Identifier { name }) => {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        Some(When::Not { node }) => collect_identifiers(Some(node), names),
        Some(When::And { left, right } | When::Or { left, right }) => {
            collect_identifiers(Some(left), names);
            collect_identifiers(Some(right), names);
        }
    }
}

fn is_known_variable(name: &str) -> bool {
    when_variables().iter().any(|known| known == name)
}

fn unknown_variables(node: Option<&When>) -> Vec<String> {
    let mut names = Vec::new();
    collect_identifiers(node, &mut names);
    names.retain(|name| !is_known_variable(name));
    names.sort();
    names
}

/// `parseWhenExpressionDraft`: blank means always; otherwise it must parse.
fn parse_when_draft(expression: &str) -> Result<Option<When>, &'static str> {
    let trimmed = expression.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    parse_when(trimmed).map(Some).ok_or("Use variables with !, &&, ||, and parentheses.")
}

/// `keybindingFromKeyboardEvent` for a GPUI keystroke: Ctrl is `mod` except on macOS, where Cmd is.
fn binding_from_keystroke(keystroke: &Keystroke, mac: bool) -> Option<String> {
    let key = keystroke.key.to_lowercase();
    let token = match key.as_str() {
        "up" => "arrowup".to_owned(),
        "down" => "arrowdown".to_owned(),
        "left" => "arrowleft".to_owned(),
        "right" => "arrowright".to_owned(),
        "space" | "enter" | "tab" | "backspace" | "delete" | "home" | "end" | "pageup" | "pagedown" => key.clone(),
        "escape" => "esc".to_owned(),
        _ if key.chars().count() == 1 => key.clone(),
        _ if key.len() <= 3
            && key.starts_with('f')
            && key[1..].chars().all(|c| c.is_ascii_digit())
            && key.len() > 1 =>
        {
            key.clone()
        }
        _ => return None,
    };
    let m = &keystroke.modifiers;
    let mut parts = Vec::new();
    if mac {
        if m.platform {
            parts.push("mod");
        }
        if m.control {
            parts.push("ctrl");
        }
    } else {
        if m.control {
            parts.push("mod");
        }
        if m.platform {
            parts.push("meta");
        }
    }
    if m.alt {
        parts.push("alt");
    }
    if m.shift {
        parts.push("shift");
    }
    parts.push(&token);
    Some(parts.join("+"))
}

// ---------------------------------------------------------------------------
// The visual "When" builder (`WhenExpressionBuilder` and its node editor)
// ---------------------------------------------------------------------------

fn condition_parts(node: &When) -> Option<(&str, bool)> {
    match node {
        When::Identifier { name } => Some((name, false)),
        When::Not { node } => match node.as_ref() {
            When::Identifier { name } => Some((name, true)),
            _ => None,
        },
        _ => None,
    }
}

fn set_condition_negated(node: &When, negated: bool) -> When {
    match condition_parts(node) {
        Some((name, _)) if negated => When::not(When::identifier(name)),
        Some((name, _)) => When::identifier(name),
        None if negated => When::not(node.clone()),
        None => node.clone(),
    }
}

fn set_condition_identifier(node: &When, identifier: &str) -> When {
    match condition_parts(node) {
        Some((_, negated)) => set_condition_negated(&When::identifier(identifier), negated),
        None => node.clone(),
    }
}

fn default_condition() -> When {
    When::identifier(DEFAULT_WHEN_VARIABLE)
}

fn default_group(or: bool) -> When {
    When::binary(or, default_condition(), When::not(default_condition()))
}

/// `flattenWhenChildren`: a chain of one operator as its operand list.
fn flatten(node: &When, or: bool) -> Vec<When> {
    match node {
        When::Or { left, right } if or => [flatten(left, or), flatten(right, or)].concat(),
        When::And { left, right } if !or => [flatten(left, or), flatten(right, or)].concat(),
        node => vec![node.clone()],
    }
}

fn build_group(children: Vec<When>, or: bool) -> Option<When> {
    children.into_iter().reduce(|left, right| When::binary(or, left, right))
}

fn is_or(node: &When) -> bool {
    matches!(node, When::Or { .. })
}

#[derive(Clone, Debug)]
enum WhenEdit {
    Negate(bool),
    Identifier(String),
    Operator { or: bool },
    AddCondition,
    AddGroup,
    Remove,
}

/// Applies `edit` to the node at `path` (operand indexes through groups, `0` through a
/// negated group). Removing a group's last operand leaves the default condition, and
/// removing the root clears the expression.
fn edit_at(node: &When, path: &[usize], edit: &WhenEdit) -> Option<When> {
    let Some((&index, rest)) = path.split_first() else { return apply_edit(node, edit) };
    match node {
        When::Not { node: inner } if condition_parts(node).is_none() => {
            Some(edit_at(inner, rest, edit).map(When::not).unwrap_or_else(|| node.clone()))
        }
        When::And { .. } | When::Or { .. } => {
            let or = is_or(node);
            let mut children = flatten(node, or);
            match children.get(index).and_then(|child| edit_at(child, rest, edit)) {
                Some(next) if index < children.len() => children[index] = next,
                _ if index < children.len() => {
                    children.remove(index);
                }
                _ => {}
            }
            Some(build_group(children, or).unwrap_or_else(default_condition))
        }
        _ => Some(node.clone()),
    }
}

fn apply_edit(node: &When, edit: &WhenEdit) -> Option<When> {
    let or = is_or(node);
    Some(match edit {
        WhenEdit::Negate(negated) => match node {
            When::Not { node: inner } if condition_parts(node).is_none() && !negated => (**inner).clone(),
            _ => set_condition_negated(node, *negated),
        },
        WhenEdit::Identifier(name) => set_condition_identifier(node, name),
        WhenEdit::Operator { or: next } if *next != or => build_group(flatten(node, or), *next)?,
        WhenEdit::Operator { .. } => node.clone(),
        WhenEdit::AddCondition => build_group([flatten(node, or), vec![default_condition()]].concat(), or)?,
        WhenEdit::AddGroup => build_group([flatten(node, or), vec![default_group(!or)]].concat(), or)?,
        WhenEdit::Remove => return None,
    })
}

/// `whenNodeRemoveLabel`.
fn remove_label(node: &When, depth: usize) -> &'static str {
    if depth == 0 {
        "Clear all conditions"
    } else if condition_parts(node).is_some() {
        "Remove condition"
    } else {
        "Remove group and its conditions"
    }
}

// ---------------------------------------------------------------------------
// The panel
// ---------------------------------------------------------------------------

/// A row's unsaved edits; a row without one shows its saved binding.
#[derive(Clone)]
struct Draft {
    key: String,
    when: Option<When>,
    when_valid: bool,
}

impl Draft {
    fn of(row: &KeyRow) -> Self {
        Self { key: row.key.clone(), when: row.rule.when_ast.clone(), when_valid: true }
    }
}

struct NewDraft {
    command: Option<String>,
    draft: Draft,
}

struct Catalog {
    source: Rc<Vec<Rule>>,
    rows: Vec<KeyRow>,
    commands: Vec<String>,
}

impl Catalog {
    fn new(source: Rc<Vec<Rule>>) -> Self {
        let merged = keybindings::merge_with_defaults(&source);
        Self { rows: build_rows(&merged, MAC), commands: command_options(&merged), source }
    }
}

pub(super) struct KeybindingsPanel {
    workspace: WeakEntity<Workspace>,
    search: Entity<InputState>,
    query: String,
    catalog: Catalog,
    connected: bool,
    drafts: HashMap<String, Draft>,
    adding: Option<NewDraft>,
    /// The row recording a shortcut; keystrokes go to it until one is captured or Escape.
    recording: Option<String>,
    /// The command an upsert or remove is in flight for.
    saving: Option<String>,
    /// The row whose When builder is open, and that builder's expression field.
    when_open: Option<String>,
    when_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl KeybindingsPanel {
    fn new(
        workspace: Entity<Workspace>,
        source: Rc<Vec<Rule>>,
        connected: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search keybindings"));
        let when_input = cx.new(|cx| InputState::new(window, cx).placeholder("Always"));
        let panel = cx.weak_entity();
        let subscriptions = vec![
            cx.subscribe(&search, |this, search, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = search.read(cx).value().to_string();
                    cx.notify();
                }
            }),
            cx.subscribe(&when_input, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value();
                    this.on_when_text(&text, cx);
                }
            }),
            cx.observe(&workspace, |this, workspace, cx| {
                let workspace = workspace.read(cx);
                let connected = workspace.client().is_some();
                if !Rc::ptr_eq(&this.catalog.source, &workspace.server_info.keybindings) || connected != this.connected
                {
                    let source = workspace.server_info.keybindings.clone();
                    this.connected = connected;
                    this.set_catalog(source);
                    cx.notify();
                }
            }),
            // Runs before GPUI's keymap, so a recorded shortcut never fires its current action,
            // and Escape closes the innermost layer instead of the whole Settings modal.
            cx.intercept_keystrokes(move |event, window, cx| {
                let consumed = panel.update(cx, |panel, cx| panel.on_keystroke(&event.keystroke, window, cx));
                if consumed.unwrap_or(false) {
                    cx.stop_propagation();
                }
            }),
        ];
        Self {
            workspace: workspace.downgrade(),
            search,
            query: String::new(),
            catalog: Catalog::new(source),
            connected,
            drafts: HashMap::new(),
            adding: None,
            recording: None,
            saving: None,
            when_open: None,
            when_input,
            _subscriptions: subscriptions,
        }
    }

    fn set_catalog(&mut self, source: Rc<Vec<Rule>>) {
        self.catalog = Catalog::new(source);
        let ids: HashSet<&str> = self.catalog.rows.iter().map(|row| row.id.as_str()).collect();
        self.drafts.retain(|id, _| ids.contains(id.as_str()));
        if self.recording.as_deref().is_some_and(|id| id != NEW_ROW && !ids.contains(id)) {
            self.recording = None;
        }
        if self.when_open.as_deref().is_some_and(|id| id != NEW_ROW && !ids.contains(id)) {
            self.when_open = None;
        }
    }

    fn row(&self, id: &str) -> Option<&KeyRow> {
        self.catalog.rows.iter().find(|row| row.id == id)
    }

    /// The key, condition and condition validity a row shows.
    fn draft_parts<'a>(&'a self, row: &'a KeyRow) -> (&'a str, Option<&'a When>, bool) {
        match self.drafts.get(&row.id) {
            Some(draft) => (&draft.key, draft.when.as_ref(), draft.when_valid),
            None => (&row.key, row.rule.when_ast.as_ref(), true),
        }
    }

    fn draft_mut(&mut self, id: &str) -> Option<&mut Draft> {
        if id == NEW_ROW {
            return self.adding.as_mut().map(|adding| &mut adding.draft);
        }
        if !self.drafts.contains_key(id) {
            let draft = Draft::of(self.row(id)?);
            self.drafts.insert(id.to_owned(), draft);
        }
        self.drafts.get_mut(id)
    }

    fn visible(&self, cx: &App) -> bool {
        self.workspace.upgrade().is_some_and(|w| w.read(cx).settings.as_deref() == Some("keybindings"))
    }

    fn on_keystroke(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.visible(cx) {
            self.recording = None;
            self.when_open = None;
            return false;
        }
        if let Some(id) = self.recording.clone() {
            let captured = if keystroke.key == "escape" {
                // `NEW_ROW` starts unassigned; a saved row goes back to its binding.
                Some(self.row(&id).map(|row| row.key.clone()).unwrap_or_default())
            } else {
                binding_from_keystroke(keystroke, MAC)
            };
            if let Some(key) = captured {
                if let Some(draft) = self.draft_mut(&id) {
                    draft.key = key;
                }
                self.recording = None;
                cx.notify();
            }
            return true;
        }
        if keystroke.key != "escape" {
            return false;
        }
        if self.when_open.is_some() {
            self.close_when(cx);
            return true;
        }
        if self.search.read(cx).focus_handle(cx).is_focused(window) {
            if self.query.is_empty() {
                window.blur(cx);
            } else {
                self.search.update(cx, |search, cx| search.set_value("", window, cx));
                self.query.clear();
                cx.notify();
            }
            return true;
        }
        false
    }

    fn start_recording(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.connected {
            self.recording = Some(id.to_owned());
            cx.notify();
        }
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        if self.recording.take().is_some() {
            cx.notify();
        }
    }

    fn toggle_when(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.when_open.as_deref() == Some(id) {
            self.close_when(cx);
            return;
        }
        self.close_when(cx);
        let expression = match id {
            NEW_ROW => when_expression(self.adding.as_ref().and_then(|a| a.draft.when.as_ref())),
            id => self.row(id).map(|row| when_expression(self.draft_parts(row).1)).unwrap_or_default(),
        };
        self.when_input.update(cx, |input, cx| input.set_value(expression, window, cx));
        self.when_open = Some(id.to_owned());
        cx.notify();
    }

    /// Closing drops an unparsed expression and keeps the last valid condition.
    fn close_when(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.when_open.take() {
            if let Some(draft) = self.draft_mut(&id) {
                draft.when_valid = true;
            }
            cx.notify();
        }
    }

    fn on_when_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(id) = self.when_open.clone() else { return };
        let parsed = parse_when_draft(text);
        if let Some(draft) = self.draft_mut(&id) {
            match parsed {
                Ok(when) => {
                    draft.when = when;
                    draft.when_valid = true;
                }
                Err(_) => draft.when_valid = false,
            }
        }
        cx.notify();
    }

    fn edit_when(&mut self, id: &str, path: &[usize], edit: WhenEdit, window: &mut Window, cx: &mut Context<Self>) {
        self.set_when(id, |root| root.and_then(|root| edit_at(root, path, &edit)), window, cx);
    }

    /// The builder's own "+ Condition" / "+ Group" join the whole expression with `&&`,
    /// where a group's buttons extend that group.
    fn add_to_root(&mut self, id: &str, group: bool, window: &mut Window, cx: &mut Context<Self>) {
        let added = if group { default_group(true) } else { default_condition() };
        self.set_when(
            id,
            |root| Some(root.map_or(added.clone(), |root| When::binary(false, root.clone(), added.clone()))),
            window,
            cx,
        );
    }

    fn set_when(
        &mut self,
        id: &str,
        next: impl FnOnce(Option<&When>) -> Option<When>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(draft) = self.draft_mut(id) else { return };
        draft.when = next(draft.when.as_ref());
        draft.when_valid = true;
        let expression = when_expression(draft.when.as_ref());
        self.when_input.update(cx, |input, cx| input.set_value(expression, window, cx));
        cx.notify();
    }

    fn client(&self, cx: &App) -> Option<crate::rpc::RpcClient> {
        self.workspace.upgrade()?.read(cx).client()
    }

    /// `server.upsertKeybinding`, or `server.removeKeybinding` when `remove`.
    fn send(&mut self, command: String, payload: Value, remove: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client(cx) else { return };
        self.saving = Some(command);
        cx.notify();
        let method = if remove { "server.removeKeybinding" } else { "server.upsertKeybinding" };
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request(method, payload).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = None;
                match result {
                    Ok(value) => {
                        if !remove {
                            this.adding = None;
                        }
                        if let Some(workspace) = this.workspace.upgrade() {
                            workspace.update(cx, |workspace, cx| {
                                workspace.server_info.keybindings =
                                    Rc::new(keybindings::decode_rules(&value["keybindings"]));
                                workspace.sync_keymap(cx);
                                cx.notify();
                            });
                        }
                    }
                    Err(error) => {
                        let (title, fallback) = if remove {
                            ("Unable to remove keybinding", "The keybinding was not removed.")
                        } else {
                            ("Unable to save keybinding", "The keybinding was not saved.")
                        };
                        let message = error.to_string();
                        let message = if message.is_empty() { fallback.to_owned() } else { message };
                        window.push_notification(Notification::error(message).title(title), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (command, key, when, replace) = if id == NEW_ROW {
            let Some(adding) = &self.adding else { return };
            let Some(command) = adding.command.clone() else { return };
            (command, adding.draft.key.clone(), when_expression(adding.draft.when.as_ref()), None)
        } else {
            let Some(row) = self.row(id) else { return };
            let (key, when, _) = self.draft_parts(row);
            (row.command.clone(), key.to_owned(), when_expression(when), Some(target(row)))
        };
        let mut payload = json!({"command": command, "key": key.trim()});
        if !when.trim().is_empty() {
            payload["when"] = when.trim().into();
        }
        if let Some(replace) = replace {
            payload["replace"] = replace;
        }
        self.send(command, payload, false, window, cx);
    }

    fn reset(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.row(id) else { return };
        let Some(default_key) = row.default_key.clone() else { return };
        let mut payload = json!({"command": row.command, "key": default_key, "replace": target(row)});
        if !row.default_when.trim().is_empty() {
            payload["when"] = row.default_when.clone().into();
        }
        self.send(row.command.clone(), payload, false, window, cx);
    }

    fn remove(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.row(id) else { return };
        let (command, payload) = (row.command.clone(), target(row));
        self.send(command, payload, true, window, cx);
    }

    /// `shell.openInEditor` with the first available editor, in `EDITORS` order.
    fn open_config_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else { return };
        let workspace = workspace.read(cx);
        let (Some(client), Some(path)) = (workspace.client(), workspace.server_info.keybindings_config_path.clone())
        else {
            return;
        };
        let available = &workspace.server_info.available_editors;
        let Some(editor) = EDITORS.iter().find(|id| available.iter().any(|a| a == *id)) else {
            let message = format!("No available editor can open {path}.");
            window.push_notification(Notification::error(message).title("Unable to open keybindings file"), cx);
            return;
        };
        let payload = json!({"cwd": path, "editor": editor});
        cx.spawn_in(window, async move |_, cx| {
            if let Err(error) = client.request("shell.openInEditor", payload).await {
                let _ = cx.update(|window, cx| {
                    window.push_notification(
                        Notification::error(error.to_string()).title("Unable to open keybindings file"),
                        cx,
                    )
                });
            }
        })
        .detach();
    }
}

/// `rowKeybindingTarget`: the binding an upsert replaces or a remove deletes.
fn target(row: &KeyRow) -> Value {
    let mut target = json!({"command": row.command, "key": row.key});
    if !row.when.trim().is_empty() {
        target["when"] = row.when.clone().into();
    }
    target
}

/// `EDITORS` ids in `packages/contracts/src/editor.ts` order.
const EDITORS: &[&str] = &[
    "cursor",
    "trae",
    "kiro",
    "vscode",
    "vscode-insiders",
    "vscodium",
    "zed",
    "antigravity",
    "idea",
    "aqua",
    "clion",
    "datagrip",
    "dataspell",
    "goland",
    "phpstorm",
    "pycharm",
    "rider",
    "rubymine",
    "rustrover",
    "webstorm",
    "file-manager",
];

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

impl Render for KeybindingsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let rows = filter_rows(&self.catalog.rows, &self.query);
        let groups = group_rows(&rows);
        let mut children = vec![settings_section("keybindings", "Keybindings", [self.render_toolbar(&t, cx)], &t)];
        if !self.connected {
            children.push(
                div()
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child("Connect an environment to change keybindings.")
                    .into_any_element(),
            );
        }
        let mut body = Vec::new();
        if self.adding.is_some() {
            body.push(card([self.render_new_row(&t, window, cx)], &t));
        }
        if groups.is_empty() {
            body.push(card(
                [div()
                    .px(px(16.))
                    .py(px(48.))
                    .text_center()
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .child("No keybindings match your search.")
                    .into_any_element()],
                &t,
            ));
        }
        for (_, title, rows) in groups {
            let rendered: Vec<AnyElement> = rows.iter().map(|row| self.render_row(row, &t, window, cx)).collect();
            body.push(settings_section(SharedString::from(title), title, rendered, &t));
        }
        children.push(v_flex().gap(px(32.)).children(body).into_any_element());
        v_flex()
            .id("settings-keybindings")
            .gap(px(32.))
            .children(children)
            // Builder and menu surfaces occlude, so this only sees clicks outside them.
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close_when(cx)))
    }
}

impl KeybindingsPanel {
    fn render_toolbar(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let can_open = self.connected
            && self.workspace.upgrade().is_some_and(|w| w.read(cx).server_info.keybindings_config_path.is_some());
        h_flex()
            .flex_wrap()
            .gap(px(8.))
            .px(px(16.))
            .py(px(12.))
            .child(
                div().flex_1().min_w(px(160.)).child(
                    Input::new(&self.search)
                        .h(px(32.))
                        .prefix(Icon::new(IconName::Search).size(px(14.)).text_color(t.muted_foreground)),
                ),
            )
            .child(
                Button::new("keybindings-add")
                    .outline()
                    .h(px(32.))
                    .px(px(12.))
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .icon(Icon::new(IconName::Plus).size(px(16.)))
                    .label("Add keybinding")
                    .disabled(self.adding.is_some() || !self.connected)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.adding = Some(NewDraft {
                            command: None,
                            draft: Draft { key: String::new(), when: None, when_valid: true },
                        });
                        cx.notify();
                    })),
            )
            .child(
                Button::new("keybindings-open-file")
                    .outline()
                    .size(px(32.))
                    .icon(Icon::new(IconName::FileBraces).size(px(16.)))
                    .tooltip("Open keybindings.json")
                    .disabled(!can_open)
                    .on_click(cx.listener(|this, _, window, cx| this.open_config_file(window, cx))),
            )
            .into_any_element()
    }

    fn render_row(&self, row: &KeyRow, t: &Tokens, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (key, when, when_valid) = self.draft_parts(row);
        let expression = when_expression(when);
        let dirty = key != row.key || expression != row.when;
        let conflicts = if dirty {
            conflict_labels(&self.catalog.rows, &row.id, key, &expression, MAC)
        } else {
            row.conflicts.clone()
        };
        let label = command_label(&row.command);
        let command: SharedString = row.command.clone().into();
        let title = h_flex()
            .id(SharedString::from(format!("kb-title-{}", row.id)))
            .gap(px(8.))
            .child(label)
            .when(row.source != Source::Default, |el| el.child(badge(row.source.label(), t)))
            .tooltip(move |window, cx| Tooltip::new(command.clone()).build(window, cx));
        let description = self.render_when_line(&row.id, &expression, when, t, window, cx);
        let saving = self.saving.as_deref() == Some(row.command.as_str());
        let control = h_flex()
            .flex_wrap()
            .justify_end()
            .gap(px(6.))
            .children(conflict_warning(&row.id, &conflicts, t))
            .children(self.render_row_menu(row, saving, t, cx))
            .children(dirty.then(|| {
                let id = row.id.clone();
                Button::new(SharedString::from(format!("kb-save-{}", row.id)))
                    .primary()
                    .small()
                    .h(px(28.))
                    .label(if saving { "Saving" } else { "Save" })
                    .disabled(saving || key.trim().is_empty() || !when_valid)
                    .on_click(cx.listener(move |this, _, window, cx| this.save(&id, window, cx)))
            }))
            .child(self.render_key_control(&row.id, key, &row.key, dirty, t, cx));
        let rendered = RowFrame::new(title).description(description).control(control);
        div().group("kb-row").child(rendered.render(t)).into_any_element()
    }

    fn render_row_menu(&self, row: &KeyRow, saving: bool, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let can_reset = row.source == Source::Custom && row.default_key.is_some();
        let can_remove = row.source != Source::Default;
        if !can_reset && !can_remove {
            return None;
        }
        let panel = cx.entity();
        let id = row.id.clone();
        Some(
            div()
                .opacity(0.)
                .group_hover("kb-row", |style| style.opacity(1.))
                .child(
                    Button::new(SharedString::from(format!("kb-menu-{}", row.id)))
                        .ghost()
                        .size(px(28.))
                        .text_color(t.muted_foreground)
                        .icon(Icon::new(IconName::Ellipsis).size(px(14.)))
                        .disabled(saving)
                        .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, window, _| {
                            if can_reset {
                                let id = id.clone();
                                menu = menu.item(PopupMenuItem::new("Reset to default").on_click(
                                    window.listener_for(&panel, move |this, _, window, cx| this.reset(&id, window, cx)),
                                ));
                            }
                            if can_remove {
                                let id = id.clone();
                                menu =
                                    menu.item(PopupMenuItem::new("Remove").on_click(
                                        window.listener_for(&panel, move |this, _, window, cx| {
                                            this.remove(&id, window, cx)
                                        }),
                                    ));
                            }
                            menu
                        }),
                )
                .into_any_element(),
        )
    }

    /// `KeybindingKeyControl`: the pill while pristine, else the capture field.
    fn render_key_control(
        &self,
        id: &str,
        key: &str,
        saved_key: &str,
        dirty: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let recording = self.recording.as_deref() == Some(id);
        let owned = id.to_owned();
        if !recording && key == saved_key && !saved_key.is_empty() && !dirty {
            return div()
                .id(SharedString::from(format!("kb-pill-{id}")))
                .h(px(28.))
                .mr(px(-6.))
                .px(px(6.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .border_1()
                .border_color(gpui_kit::transparent_black())
                .cursor_pointer()
                .hover(|style| style.border_color(t.border.opacity(0.7)).bg(t.accent))
                .child(pill(key, t))
                .on_click(cx.listener(move |this, _, _, cx| this.start_recording(&owned, cx)))
                .into_any_element();
        }
        capture_field(id, if recording { "" } else { key }, recording, t)
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.start_recording(&owned, cx)))
            .when(recording, |el| el.on_mouse_down_out(cx.listener(|this, _, _, cx| this.stop_recording(cx))))
            .into_any_element()
    }

    /// "When" and the trigger of the condition builder.
    fn render_when_line(
        &self,
        id: &str,
        expression: &str,
        when: Option<&When>,
        t: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.when_open.as_deref() == Some(id);
        let owned = id.to_owned();
        let trigger = h_flex()
            .id(SharedString::from(format!("kb-when-{id}")))
            .h(px(24.))
            .min_w_0()
            .px(px(6.))
            .gap(px(4.))
            .rounded(px(6.))
            .cursor_pointer()
            .text_xs()
            .text_color(if expression.is_empty() { t.muted_foreground } else { t.foreground })
            .hover(|style| style.bg(t.accent))
            .when(open, |el| el.bg(t.accent))
            .child(div().truncate().font_family(t.fonts.mono).child(if expression.is_empty() {
                SharedString::new_static("Always")
            } else {
                SharedString::from(expression.to_owned())
            }))
            .child(Icon::new(IconName::ChevronDown).size(px(14.)).text_color(t.foreground.opacity(0.6)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.toggle_when(&owned, window, cx);
                    cx.stop_propagation();
                }),
            );
        h_flex()
            .h(px(24.))
            .gap(px(6.))
            .child(div().text_xs().text_color(t.muted_foreground.opacity(0.7)).child("When"))
            .child(div().relative().min_w_0().child(trigger).when(open, |el| {
                el.child(
                    div().absolute().top(px(30.)).left_0().child(
                        deferred(
                            anchored()
                                .snap_to_window_with_margin(px(8.))
                                .child(self.render_when_builder(id, when, t, window, cx)),
                        )
                        .with_priority(1),
                    ),
                )
            }))
            .into_any_element()
    }

    /// `WhenExpressionBuilder`.
    fn render_when_builder(
        &self,
        id: &str,
        when: Option<&When>,
        t: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = self.when_input.read(cx).value();
        let parsed = parse_when_draft(&text);
        let unknown = parsed.as_ref().map(|when| unknown_variables(when.as_ref())).unwrap_or_default();
        let add_button = |label: &'static str, primary: bool, group: bool, cx: &mut Context<Self>| {
            let owned = id.to_owned();
            Button::new(SharedString::from(format!("kb-when-root-{label}-{primary}")))
                .when(primary, |b| b.primary())
                .when(!primary, |b| b.outline())
                .compact()
                .small()
                .h(px(28.))
                .gap(px(4.))
                .icon(Icon::new(IconName::Plus).size(px(14.)))
                .label(label)
                .on_click(cx.listener(move |this, _, window, cx| this.add_to_root(&owned, group, window, cx)))
        };
        let tree = match when {
            Some(node) => self.render_when_node(id, node, Vec::new(), 0, true, t, window, cx),
            None => h_flex()
                .flex_wrap()
                .gap(px(8.))
                .p(px(12.))
                .rounded(px(6.))
                .border_1()
                .border_dashed()
                .border_color(t.border.opacity(0.8))
                .bg(t.muted.opacity(0.15))
                .child(add_button("Condition", true, false, cx))
                .child(add_button("Group", false, true, cx))
                .into_any_element(),
        };
        v_flex()
            .id(SharedString::from(format!("kb-when-builder-{id}")))
            .occlude()
            .w(px(544.))
            .p(px(16.))
            .gap(px(12.))
            .rounded(px(8.))
            .border_1()
            .border_color(t.border)
            .bg(t.popover)
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .justify_between()
                    .gap(px(12.))
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(t.foreground).child("When"))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(add_button("Condition", false, false, cx))
                            .child(add_button("Group", false, true, cx)),
                    ),
            )
            .child(
                v_flex()
                    .gap(px(6.))
                    .child(
                        Input::new(&self.when_input)
                            .small()
                            .font_family(t.fonts.mono)
                            .when(!unknown.is_empty(), |input| input.suffix(unknown_warning(id, t))),
                    )
                    .children(parsed.as_ref().err().map(|message| {
                        h_flex()
                            .gap(px(6.))
                            .text_size(px(10.))
                            .text_color(t.error_foreground)
                            .child(Icon::new(IconName::CircleX).size(px(14.)))
                            .child(*message)
                    })),
            )
            .child(div().relative().child(tree).when(parsed.is_err(), |el| {
                el.child(
                    div()
                        .absolute()
                        .inset_0()
                        .occlude()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p(px(16.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(t.error.opacity(0.3))
                        .bg(t.background.opacity(0.75))
                        .text_center()
                        .text_xs()
                        .text_color(t.error_foreground)
                        .child("Fix the expression above to continue editing visually."),
                )
            }))
            .into_any_element()
    }

    /// `WhenExpressionNodeEditor`, one node and its operands.
    #[allow(clippy::too_many_arguments)]
    fn render_when_node(
        &self,
        id: &str,
        node: &When,
        path: Vec<usize>,
        depth: usize,
        removable: bool,
        t: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = format!("kb-node-{id}-{path:?}");
        let panel = cx.entity();
        let edit = |edit: WhenEdit| {
            let (owned, path, panel) = (id.to_owned(), path.clone(), panel.clone());
            move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                panel.update(cx, |this, cx| this.edit_when(&owned, &path, edit.clone(), window, cx))
            }
        };
        let remove = removable.then(|| {
            Button::new(SharedString::from(format!("{key}-remove")))
                .ghost()
                .size(px(28.))
                .icon(Icon::new(IconName::Minus).size(px(14.)))
                .tooltip(remove_label(node, depth))
                .on_click(edit(WhenEdit::Remove))
        });
        let not_toggle = |negated: bool, cx: &mut Context<Self>| {
            let (owned, path) = (id.to_owned(), path.clone());
            let panel = cx.entity();
            Toggle::new(SharedString::from(format!("{key}-not")))
                .outline()
                .small()
                .min_w(px(40.))
                .h(px(28.))
                .checked(negated)
                .child("Not")
                .on_click(move |checked: &bool, window, cx| {
                    let (owned, path, checked) = (owned.clone(), path.clone(), *checked);
                    panel.update(cx, |this, cx| this.edit_when(&owned, &path, WhenEdit::Negate(checked), window, cx));
                })
        };
        if let Some((name, negated)) = condition_parts(node) {
            return h_flex()
                .gap(px(8.))
                .px(px(8.))
                .py(px(8.))
                .rounded(px(6.))
                .border_1()
                .border_color(t.border.opacity(0.7))
                .bg(t.background.opacity(0.6))
                .child(not_toggle(negated, cx))
                .child(div().flex_1().min_w_0().child(self.render_variable_select(&key, id, &path, name, t, cx)))
                .children(remove)
                .into_any_element();
        }
        let branch = |child: AnyElement| connector(child, depth, t);
        if let When::Not { node: inner } = node {
            let mut inner_path = path.clone();
            inner_path.push(0);
            return v_flex()
                .gap(px(8.))
                .p(px(8.))
                .rounded(px(8.))
                .border_1()
                .border_color(t.border.opacity(if depth > 0 { 0.5 } else { 0.7 }))
                .bg(if depth > 0 { t.background.opacity(0.5) } else { t.muted.opacity(0.2) })
                .child(h_flex().gap(px(8.)).child(not_toggle(true, cx)).child(div().flex_1()).children(remove))
                .child(branch(self.render_when_node(id, inner, inner_path, depth + 1, false, t, window, cx)))
                .into_any_element();
        }
        let or = is_or(node);
        let panel = cx.entity();
        let operator = {
            let (owned, path) = (id.to_owned(), path.clone());
            select_trigger(&SharedString::from(format!("{key}-op")), if or { "or" } else { "and" }, 96., t)
                .dropdown_menu(move |mut menu, window, _| {
                    for (label, next) in [("and", false), ("or", true)] {
                        let (owned, path) = (owned.clone(), path.clone());
                        menu = menu.item(PopupMenuItem::new(label).checked(next == or).on_click(window.listener_for(
                            &panel,
                            move |this, _, window, cx| {
                                this.edit_when(&owned, &path, WhenEdit::Operator { or: next }, window, cx)
                            },
                        )));
                    }
                    menu
                })
        };
        let small_add = |label: &'static str, edit_kind: WhenEdit| {
            Button::new(SharedString::from(format!("{key}-add-{label}")))
                .outline()
                .compact()
                .small()
                .h(px(28.))
                .gap(px(4.))
                .icon(Icon::new(IconName::Plus).size(px(14.)))
                .label(label)
                .on_click(edit(edit_kind))
        };
        let children: Vec<AnyElement> = flatten(node, or)
            .iter()
            .enumerate()
            .map(|(index, child)| {
                let mut child_path = path.clone();
                child_path.push(index);
                branch(self.render_when_node(id, child, child_path, depth + 1, true, t, window, cx))
            })
            .collect();
        v_flex()
            .gap(px(8.))
            .p(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(t.border.opacity(if depth > 0 { 0.7 } else { 0.6 }))
            .bg(if depth > 0 { t.background.opacity(0.55) } else { t.muted.opacity(0.1) })
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .child(operator)
                    .child(small_add("Condition", WhenEdit::AddCondition))
                    .child(small_add("Group", WhenEdit::AddGroup))
                    .child(div().flex_1())
                    .children(remove),
            )
            .child(v_flex().gap(px(8.)).children(children))
            .into_any_element()
    }

    /// `WhenVariableSelect`: the known variables, plus this condition's own if it is unknown.
    fn render_variable_select(
        &self,
        key: &str,
        id: &str,
        path: &[usize],
        name: &str,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let unknown = !is_known_variable(name);
        let mut options: Vec<String> = Vec::with_capacity(when_variables().len() + 1);
        if unknown {
            options.push(name.to_owned());
        }
        options.extend(when_variables().iter().cloned());
        let (owned, path, current) = (id.to_owned(), path.to_vec(), name.to_owned());
        let panel = cx.entity();
        let label = h_flex()
            .min_w_0()
            .gap(px(4.))
            .child(div().truncate().child(name.to_owned()))
            .when(unknown, |el| el.child(unknown_warning(key, t)));
        select_trigger(&SharedString::from(format!("{key}-var")), label, 0., t)
            .w_full()
            .dropdown_menu(move |mut menu, window, _| {
                for option in &options {
                    let (owned, path, picked) = (owned.clone(), path.clone(), option.clone());
                    menu = menu.item(PopupMenuItem::new(option.clone()).checked(*option == current).on_click(
                        window.listener_for(&panel, move |this, _, window, cx| {
                            this.edit_when(&owned, &path, WhenEdit::Identifier(picked.clone()), window, cx)
                        }),
                    ));
                }
                menu.max_h(px(288.)).scrollable(true)
            })
            .into_any_element()
    }

    /// `NewKeybindingSettingsRow`.
    fn render_new_row(&self, t: &Tokens, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(adding) = &self.adding else { return div().into_any_element() };
        let draft = &adding.draft;
        let expression = when_expression(draft.when.as_ref());
        let conflicts = conflict_labels(&self.catalog.rows, NEW_ROW, &draft.key, &expression, MAC);
        let can_save = adding.command.is_some() && !draft.key.trim().is_empty() && draft.when_valid;
        let saving = self.saving.is_some();
        let panel = cx.entity();
        let commands = self.catalog.commands.clone();
        let current = adding.command.clone();
        let command_label_el = div()
            .truncate()
            .text_color(if current.is_some() { t.foreground } else { t.muted_foreground })
            .child(current.as_deref().map(command_label).unwrap_or_else(|| "Command".to_owned()));
        let command_select = select_trigger(&"kb-new-command".into(), command_label_el, 224., t).dropdown_menu(
            move |mut menu, window, _| {
                for command in &commands {
                    let picked = command.clone();
                    menu = menu.item(
                        PopupMenuItem::new(command_label(command)).checked(current.as_ref() == Some(command)).on_click(
                            window.listener_for(&panel, move |this, _, _, cx| {
                                if let Some(adding) = &mut this.adding {
                                    adding.command = Some(picked.clone());
                                }
                                cx.notify();
                            }),
                        ),
                    );
                }
                menu.max_h(px(288.)).scrollable(true)
            },
        );
        let description = self.render_when_line(NEW_ROW, &expression, draft.when.as_ref(), t, window, cx);
        let row = RowFrame::new("New keybinding").description(description).control(
            h_flex()
                .flex_wrap()
                .gap(px(8.))
                .child(command_select)
                .children(conflict_warning(NEW_ROW, &conflicts, t))
                .child(self.render_key_control(NEW_ROW, &draft.key, "", true, t, cx))
                .child(
                    Button::new("kb-new-save")
                        .primary()
                        .small()
                        .h(px(28.))
                        .label(if saving { "Saving" } else { "Save" })
                        .disabled(saving || !can_save)
                        .on_click(cx.listener(|this, _, window, cx| this.save(NEW_ROW, window, cx))),
                )
                .child(
                    Button::new("kb-new-cancel")
                        .ghost()
                        .size(px(28.))
                        .text_color(t.muted_foreground)
                        .icon(Icon::new(IconName::X).size(px(14.)))
                        .tooltip("Cancel")
                        .disabled(saving)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.adding = None;
                            if this.recording.as_deref() == Some(NEW_ROW) {
                                this.recording = None;
                            }
                            if this.when_open.as_deref() == Some(NEW_ROW) {
                                this.when_open = None;
                            }
                            cx.notify();
                        })),
                ),
        );
        div().bg(t.muted.opacity(0.15)).child(row.render(t)).into_any_element()
    }
}

/// The tree lines from a group to one operand.
fn connector(child: AnyElement, depth: usize, t: &Tokens) -> AnyElement {
    let line = if depth == 0 { t.border } else { t.border.opacity(0.7) };
    div()
        .relative()
        .pl(px(16.))
        .child(div().absolute().top_0().bottom_0().left(px(6.)).w(px(1.)).bg(line))
        .child(div().absolute().top(px(16.)).left(px(6.)).h(px(1.)).w(px(10.)).bg(line))
        .child(child)
        .into_any_element()
}

/// `KeybindingPill`: one key cap per `+`-separated part.
fn pill(value: &str, t: &Tokens) -> Div {
    h_flex().gap(px(4.)).children(value.split('+').map(|part| {
        let label: SharedString = match part {
            "mod" => MOD_LABEL.into(),
            "shift" => "⇧".into(),
            "alt" => (if MAC { "⌥" } else { "Alt" }).into(),
            "ctrl" => "⌃".into(),
            part if part.chars().count() == 1 => part.to_uppercase().into(),
            part => part.to_owned().into(),
        };
        div()
            .h(px(20.))
            .min_w(px(20.))
            .px(px(4.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .bg(t.foreground.opacity(0.07))
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.muted_foreground)
            .child(label)
    }))
}

/// The `Input` that records a shortcut; it shows "Press shortcut" while recording.
fn capture_field(id: &str, value: &str, recording: bool, t: &Tokens) -> gpui_kit::Stateful<Div> {
    div()
        .id(SharedString::from(format!("kb-capture-{id}")))
        .w(px(176.))
        .h(px(28.))
        .px(px(10.))
        .flex()
        .items_center()
        .rounded(px(8.))
        .border_1()
        .border_color(if recording { t.ring } else { t.input })
        .bg(if t.dark { t.input.opacity(0.32) } else { t.background })
        .cursor_text()
        .font_family(t.fonts.mono)
        .text_sm()
        .map(|el| {
            if value.is_empty() {
                el.text_color(t.muted_foreground.opacity(0.72)).child(if recording {
                    "Press shortcut"
                } else {
                    "Unassigned"
                })
            } else {
                el.text_color(t.foreground).child(value.to_owned())
            }
        })
}

fn badge(label: &'static str, t: &Tokens) -> Div {
    div()
        .h(px(18.))
        .px(px(4.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .border_1()
        .border_color(t.border)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.foreground)
        .child(label)
}

fn warning_icon(id: ElementId, tooltip: String, t: &Tokens) -> gpui_kit::Stateful<Div> {
    let tooltip: SharedString = tooltip.into();
    div()
        .id(id)
        .size(px(20.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .text_color(t.warning)
        .hover(|style| style.bg(t.warning.opacity(0.1)))
        .child(Icon::new(IconName::TriangleAlert).size(px(14.)))
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
}

/// `KeybindingConflictWarning`.
fn conflict_warning(id: &str, labels: &[String], t: &Tokens) -> Option<AnyElement> {
    let description = match labels {
        [] => return None,
        [one] => format!("Conflicts with {one}."),
        _ => format!(
            "Conflicts with {}{}.",
            labels.iter().take(3).cloned().collect::<Vec<_>>().join(", "),
            if labels.len() > 3 { ", and more" } else { "" }
        ),
    };
    let tooltip = format!("{description} The most recent matching binding wins when both conditions can apply.");
    Some(warning_icon(SharedString::from(format!("kb-conflict-{id}")).into(), tooltip, t).into_any_element())
}

/// `UnknownWhenVariableWarning`.
fn unknown_warning(id: &str, t: &Tokens) -> AnyElement {
    let tooltip = "T3 Code does not recognize this condition yet. It can still be saved, but it may not match unless the runtime provides it.";
    warning_icon(SharedString::from(format!("kb-unknown-{id}")).into(), tooltip.to_owned(), t).into_any_element()
}

impl Workspace {
    pub(super) fn render_keybindings_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (workspace, source, connected) =
            (cx.entity(), self.server_info.keybindings.clone(), self.client().is_some());
        let view = self
            .keybindings_panel
            .get_or_insert_with(|| cx.new(|cx| KeybindingsPanel::new(workspace, source, connected, window, cx)).into())
            .clone();
        // Not `cached`: its dropdowns and popovers keep state in their own entities,
        // whose updates would not repaint a cached subtree.
        let settings = self.settings_view();
        self.settings_page("keybindings", &settings, [div().child(view).into_any_element()], cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::compile;
    use gpui_kit::Modifiers;

    fn rule(key: &str, command: &str, when: Option<&str>) -> Rule {
        compile(key, command, when).unwrap()
    }

    #[test]
    fn rows_know_their_source_default_and_conflicts() {
        let custom = [rule("mod+k", "sidebar.toggle", None), rule("mod+j", "script.build-app.run", None)];
        let rows = build_rows(&keybindings::merge_with_defaults(&custom), false);
        let sidebar = rows.iter().find(|row| row.command == "sidebar.toggle").unwrap();
        assert_eq!(sidebar.source, Source::Custom);
        assert_eq!((sidebar.default_key.as_deref(), sidebar.default_when.as_str()), (Some("mod+b"), ""));
        assert_eq!(
            sidebar.conflicts,
            ["Command Palette: Toggle"],
            "mod+k when !terminalFocus overlaps an unconditional mod+k"
        );
        let script = rows.iter().find(|row| row.command == "script.build-app.run").unwrap();
        assert_eq!(script.source, Source::Project);
        assert!(script.conflicts.contains(&"Terminal: Toggle".to_owned()));
        let default = rows.iter().find(|row| row.command == "chat.find").unwrap();
        assert_eq!(default.source, Source::Default);
        // terminal.close (terminalFocus) and rightPanel.close (!terminalFocus) share mod+w but never both hold.
        let close = rows.iter().find(|row| row.command == "terminal.close").unwrap();
        assert!(close.conflicts.is_empty());
    }

    #[test]
    fn conflicts_compare_resolved_chords() {
        let rows = build_rows(&[rule("mod+k", "commandPalette.toggle", None)], false);
        assert_eq!(conflict_labels(&rows, "x", "ctrl+k", "", false), ["Command Palette: Toggle"]);
        assert!(conflict_labels(&rows, "x", "ctrl+k", "", true).is_empty(), "mod is Cmd on macOS");
        assert!(conflict_labels(&rows, "x", "", "", false).is_empty());
        assert!(
            conflict_labels(&rows, &rows[0].id, "mod+k", "", false).is_empty(),
            "a row never conflicts with itself"
        );
    }

    #[test]
    fn rows_group_sort_and_filter_like_the_web() {
        let rows = build_rows(defaults(), false);
        let all: Vec<&KeyRow> = rows.iter().collect();
        let groups: Vec<&str> = group_rows(&all).iter().map(|(id, ..)| *id).collect();
        assert_eq!(groups, ["navigation", "threads", "composer", "terminal", "preview", "appearance", "other"]);
        let usage: Vec<&str> =
            rows.iter().filter(|r| r.command.starts_with("usage.")).map(|r| r.command.as_str()).collect();
        assert_eq!(usage, &USAGE_ORDER[..], "usage commands keep the Usage page's order");
        let found = filter_rows(&rows, "  COPY link ");
        assert_eq!(found.iter().map(|r| r.command.as_str()).collect::<Vec<_>>(), ["thread.copyReference"]);
        assert!(filter_rows(&rows, "custom").is_empty(), "the source is searchable");
    }

    #[test]
    fn command_labels() {
        assert_eq!(command_label("rightPanel.toggleMaximized"), "Right Panel: Toggle Maximized");
        assert_eq!(command_label("thread.jump.3"), "Thread: Jump: 3");
        assert_eq!(command_label("script.build-app.run"), "Run Script: Build App");
        assert_eq!(command_label("usage.period.week"), "Usage: Period: 7 days");
        assert_eq!(command_label("preview.focusUrl"), "Preview: Focus URL");
        let options = command_options(&[rule("mod+j", "script.lint.run", None)]);
        assert!(options.contains(&"script.lint.run".to_owned()) && options.len() == STATIC_COMMANDS.len() + 1);
    }

    #[test]
    fn when_variables_put_the_core_first() {
        let variables = when_variables();
        assert_eq!(&variables[..6], CORE_WHEN_VARIABLES.map(String::from));
        assert!(variables.iter().any(|v| v == "composerFocus"));
        assert_eq!(unknown_variables(parse_when("isDesktop && fooBar").as_ref()), ["fooBar"]);
        assert_eq!(parse_when_draft("  "), Ok(None));
        assert!(parse_when_draft("a &&").is_err());
    }

    #[test]
    fn keystrokes_record_as_bindings() {
        let keystroke = |key: &str, control, shift, platform| Keystroke {
            modifiers: Modifiers { control, shift, platform, ..Default::default() },
            key: key.into(),
            key_char: None,
        };
        assert_eq!(binding_from_keystroke(&keystroke("k", true, true, false), false).as_deref(), Some("mod+shift+k"));
        assert_eq!(binding_from_keystroke(&keystroke("k", true, false, true), true).as_deref(), Some("mod+ctrl+k"));
        assert_eq!(binding_from_keystroke(&keystroke("up", false, false, false), false).as_deref(), Some("arrowup"));
        assert_eq!(binding_from_keystroke(&keystroke("f12", false, false, false), false).as_deref(), Some("f12"));
        assert_eq!(binding_from_keystroke(&keystroke("shift", false, true, false), false), None, "a lone modifier");
        let recorded = binding_from_keystroke(&keystroke("[", true, false, false), false).unwrap();
        assert_eq!(shortcut_input(&parse_shortcut(&recorded).unwrap()), "mod+[");
    }

    #[test]
    fn the_builder_edits_operands_in_place() {
        let root = parse_when("a && b && !c").unwrap();
        let negate = edit_at(&root, &[1], &WhenEdit::Negate(true)).unwrap();
        assert_eq!(when_expression(Some(&negate)), "(a && !b) && !c");
        let renamed = edit_at(&root, &[2], &WhenEdit::Identifier("d".into())).unwrap();
        assert_eq!(when_expression(Some(&renamed)), "(a && b) && !d", "renaming keeps the negation");
        let or = edit_at(&root, &[], &WhenEdit::Operator { or: true }).unwrap();
        assert_eq!(when_expression(Some(&or)), "(a || b) || !c", "chains print grouped, as `whenAstToExpression` does");
        let removed = edit_at(&root, &[0], &WhenEdit::Remove).unwrap();
        assert_eq!(when_expression(Some(&removed)), "b && !c");
        let grouped = edit_at(&root, &[], &WhenEdit::AddGroup).unwrap();
        assert_eq!(when_expression(Some(&grouped)), "((a && b) && !c) && (terminalFocus || !terminalFocus)");
        let nested = edit_at(&grouped, &[3, 1], &WhenEdit::Remove).unwrap();
        assert_eq!(
            when_expression(Some(&nested)),
            "((a && b) && !c) && terminalFocus",
            "a group of one is its operand"
        );
        assert_eq!(edit_at(&root, &[], &WhenEdit::Remove), None, "removing the root clears the condition");
        let lone = parse_when("(a || b)").unwrap();
        let emptied = edit_at(&edit_at(&lone, &[0], &WhenEdit::Remove).unwrap(), &[], &WhenEdit::Negate(false));
        assert_eq!(when_expression(emptied.as_ref()), "b");
        let negated_group = parse_when("!(a || b)").unwrap();
        let unwrapped = edit_at(&negated_group, &[], &WhenEdit::Negate(false)).unwrap();
        assert_eq!(when_expression(Some(&unwrapped)), "a || b");
        let inner = edit_at(&negated_group, &[0, 1], &WhenEdit::Identifier("c".into())).unwrap();
        assert_eq!(when_expression(Some(&inner)), "!(a || c)");
    }
}
