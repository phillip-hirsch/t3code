//! Settings text fields that keep their state across frames and commit on blur or Enter
//! (`DraftInput`, `NumberField`, `SecretInput` and the uncontrolled `Textarea`s in the web).

use std::rc::Rc;

use gpui_kit::component::Sizable as _;
use gpui_kit::component::input::{
    Input, InputEvent, InputState, NumberInputEvent, StepAction, Textarea, TextareaState,
};
use gpui_kit::{
    AnyElement, AppContext as _, Context, Div, Entity, IntoElement, ParentElement as _, SharedString, Styled as _,
    Subscription, WeakEntity, Window, div, px,
};

use crate::app::Workspace;

type OnCommit<T> = Rc<dyn Fn(&mut Workspace, T, &mut Window, &mut Context<Workspace>)>;

impl Workspace {
    /// A text field kept across frames, synced to `saved` and committing trimmed text on blur
    /// or Enter when it was edited (`DraftInput` and the uncontrolled `Textarea`s in the web).
    /// `normalize` rewrites each change from the previous text, as `resolvePastedPath` does.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn text_field(
        &mut self,
        key: &str,
        saved: &str,
        placeholder: &str,
        multiline: bool,
        normalize: Option<fn(&str, &str) -> String>,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_commit: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) -> FieldInput {
        let kind = if multiline { FieldKind::Area } else { FieldKind::Line };
        self.field(key, saved, placeholder, kind, normalize, window, cx, on_commit)
    }

    /// A masked field for a credential. Settings only ever hold it redacted, so the field
    /// starts empty and an empty field saves nothing (`SecretInput`).
    pub(in crate::app) fn secret_field(
        &mut self,
        key: &str,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_commit: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) -> FieldInput {
        self.field(key, "", placeholder, FieldKind::Secret, None, window, cx, on_commit)
    }

    #[allow(clippy::too_many_arguments)]
    fn field(
        &mut self,
        key: &str,
        saved: &str,
        placeholder: &str,
        kind: FieldKind,
        normalize: Option<fn(&str, &str) -> String>,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_commit: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) -> FieldInput {
        let workspace = cx.weak_entity();
        let on_commit: OnCommit<String> = Rc::new(on_commit);
        let state = window.use_keyed_state(SharedString::from(format!("settings-field-{key}")), cx, |window, cx| {
            TextField::new(kind, placeholder, saved, normalize, workspace, window, cx)
        });
        state.update(cx, |field, cx| {
            field.on_commit = Some(on_commit);
            if let (FieldInput::Line(input), true) = (&field.input, field.placeholder != placeholder) {
                field.placeholder = placeholder.to_owned();
                let placeholder = field.placeholder.clone();
                input.update(cx, |input, cx| input.set_placeholder(placeholder, window, cx));
            }
            if field.saved != saved {
                field.saved = saved.to_owned();
                field.previous = saved.to_owned();
                field.input.set_value(saved, window, cx);
            }
        });
        state.read(cx).input.clone()
    }

    /// A whole-number stepper kept across frames (`NumberField`): typing stays local until
    /// blur, Enter or a step commits it, clamped to `min..=max`; anything else reverts.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn number_field(
        &mut self,
        key: &str,
        value: i64,
        min: i64,
        max: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_commit: impl Fn(&mut Self, i64, &mut Window, &mut Context<Self>) + 'static,
    ) -> Entity<InputState> {
        let workspace = cx.weak_entity();
        let on_commit: OnCommit<i64> = Rc::new(on_commit);
        let state = window.use_keyed_state(SharedString::from(format!("settings-number-{key}")), cx, |window, cx| {
            NumberField::new(value, workspace, window, cx)
        });
        state.update(cx, |field, cx| {
            field.on_commit = Some(on_commit);
            (field.min, field.max) = (min, max);
            if field.value != value {
                field.value = value;
                field.input.update(cx, |input, cx| input.set_value(value.to_string(), window, cx));
            }
        });
        state.read(cx).input.clone()
    }
}

pub(in crate::app) struct NumberField {
    input: Entity<InputState>,
    value: i64,
    min: i64,
    max: i64,
    on_commit: Option<OnCommit<i64>>,
    workspace: WeakEntity<Workspace>,
    _subscriptions: [Subscription; 2],
}

impl NumberField {
    fn new(value: i64, workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        input.update(cx, |input, cx| input.set_value(value.to_string(), window, cx));
        let subscriptions = [
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    let typed = this.input.read(cx).value().trim().parse::<f64>().ok();
                    this.commit(typed.map(|typed| typed.round() as i64), window, cx);
                }
            }),
            cx.subscribe_in(&input, window, |this, _, NumberInputEvent::Step(action), window, cx| {
                let typed = this.input.read(cx).value().trim().parse::<f64>();
                let current = typed.map_or(this.value, |typed| typed.round() as i64);
                let step = if matches!(action, StepAction::Increment) { 1 } else { -1 };
                this.commit(Some(current + step), window, cx);
            }),
        ];
        Self { input, value, min: i64::MIN, max: i64::MAX, on_commit: None, workspace, _subscriptions: subscriptions }
    }

    fn commit(&mut self, next: Option<i64>, window: &mut Window, cx: &mut Context<Self>) {
        let next = next.map_or(self.value, |next| next.clamp(self.min, self.max));
        self.input.update(cx, |input, cx| input.set_value(next.to_string(), window, cx));
        if next == self.value {
            return;
        }
        let (Some(on_commit), Some(workspace)) = (self.on_commit.clone(), self.workspace.upgrade()) else { return };
        workspace.update(cx, |this, cx| on_commit(this, next, window, cx));
    }
}

#[derive(Clone)]
pub(in crate::app) enum FieldInput {
    Line(Entity<InputState>),
    Area(Entity<TextareaState>),
}

impl FieldInput {
    pub(in crate::app) fn value(&self, cx: &gpui_kit::App) -> String {
        match self {
            Self::Line(input) => input.read(cx).value().to_string(),
            Self::Area(input) => input.read(cx).value().to_string(),
        }
    }

    fn set_value(&self, value: &str, window: &mut Window, cx: &mut gpui_kit::App) {
        let value = value.to_owned();
        match self {
            Self::Line(input) => input.update(cx, |input, cx| input.set_value(value, window, cx)),
            Self::Area(input) => input.update(cx, |input, cx| input.set_value(value, window, cx)),
        }
    }

    pub(in crate::app) fn element(&self) -> AnyElement {
        match self {
            Self::Line(input) => Input::new(input).small().into_any_element(),
            Self::Area(input) => Textarea::new(input).into_any_element(),
        }
    }

    /// The field at a fixed width, as the web's `w-*` classes set it.
    pub(in crate::app) fn sized(&self, width: gpui_kit::Pixels) -> Div {
        div().w(width).child(self.element())
    }
}

pub(in crate::app) struct TextField {
    input: FieldInput,
    saved: String,
    /// The text before the latest change, for `normalize`.
    previous: String,
    placeholder: String,
    /// Credentials clear once saved; settings only ever hold them redacted.
    secret: bool,
    normalize: Option<fn(&str, &str) -> String>,
    edited: bool,
    on_commit: Option<OnCommit<String>>,
    workspace: WeakEntity<Workspace>,
    _subscription: Subscription,
}

#[derive(Clone, Copy)]
enum FieldKind {
    Line,
    Area,
    Secret,
}

impl TextField {
    fn new(
        kind: FieldKind,
        placeholder: &str,
        saved: &str,
        normalize: Option<fn(&str, &str) -> String>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let shown_placeholder = placeholder.to_owned();
        let placeholder = placeholder.to_owned();
        let (input, subscription) = if matches!(kind, FieldKind::Area) {
            let input = cx.new(|cx| TextareaState::new(window, cx).rows(4).placeholder(placeholder));
            let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                this.on_event(event, window, cx)
            });
            (FieldInput::Area(input), subscription)
        } else {
            let masked = matches!(kind, FieldKind::Secret);
            let input = cx.new(|cx| InputState::new(window, cx).masked(masked).placeholder(placeholder));
            let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                this.on_event(event, window, cx)
            });
            (FieldInput::Line(input), subscription)
        };
        input.set_value(saved, window, cx);
        Self {
            input,
            saved: saved.to_owned(),
            previous: saved.to_owned(),
            placeholder: shown_placeholder,
            secret: matches!(kind, FieldKind::Secret),
            normalize,
            edited: false,
            on_commit: None,
            workspace,
            _subscription: subscription,
        }
    }

    fn on_event(&mut self, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        let enter = matches!(event, InputEvent::PressEnter { .. }) && matches!(self.input, FieldInput::Line(_));
        match event {
            InputEvent::Change => {
                self.edited = true;
                let current = self.input.value(cx);
                let next =
                    self.normalize.map_or_else(|| current.clone(), |normalize| normalize(&self.previous, &current));
                if next != current {
                    self.input.set_value(&next, window, cx);
                }
                self.previous = next;
                return;
            }
            InputEvent::Blur => {}
            _ if enter => {}
            _ => return,
        }
        if !std::mem::take(&mut self.edited) {
            return;
        }
        let value = self.input.value(cx).trim().to_owned();
        if value == self.saved {
            return;
        }
        let (Some(on_commit), Some(workspace)) = (self.on_commit.clone(), self.workspace.upgrade()) else { return };
        workspace.update(cx, |this, cx| on_commit(this, value, window, cx));
        if self.secret {
            self.input.set_value("", window, cx);
        }
    }
}

/// A textarea set below the row's text, `mt-3 max-w-2xl pb-3.5`.
pub(in crate::app) fn textarea_body(field: &FieldInput) -> AnyElement {
    div().mt(px(12.)).pb(px(14.)).max_w(px(672.)).child(field.element()).into_any_element()
}

fn is_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with(r"\\")
        || (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/'))
}

fn is_host_path(path: &str) -> bool {
    path.starts_with('/') || path.starts_with("~/") || is_windows_absolute(path)
}

/// A shell-quoted path ending `text`, `'…'` (with `'\''` escapes) or `"…"`, and where it starts.
fn quoted_path_at_end(text: &str) -> Option<(usize, String)> {
    let trimmed = text.trim_end();
    let quote = trimmed.chars().last().filter(|c| *c == '\'' || *c == '"')?;
    let end = trimmed.len() - 1;
    trimmed[..end].match_indices(quote).find_map(|(start, _)| {
        let inner = &trimmed[start + 1..end];
        let valid = !inner.is_empty() && !inner.replace(r"'\''", "").contains(quote);
        let unescaped = if quote == '\'' { inner.replace(r"'\''", "'") } else { inner.to_owned() };
        valid.then_some((start, unescaped))
    })
}

const SHELL_ESCAPED: &str = "'\"\\()[]{}<>|&;$`!*?#~^=";

fn unescape_shell(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match chars.peek() {
            Some(&next) if c == '\\' && (next.is_whitespace() || SHELL_ESCAPED.contains(next)) => {
                out.push(next);
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

/// `resolvePastedPath`: a full path pasted into a folder box replaces it, even after the text
/// already there, and shell quoting or escapes from Finder or Explorer come off.
pub(in crate::app) fn resolve_pasted_path(previous: &str, next: &str) -> String {
    let typing = if next.starts_with(previous) {
        next.chars().count() == previous.chars().count() + 1
    } else {
        previous.starts_with(next)
    };
    if typing {
        return next.to_owned();
    }
    if let Some((start, quoted)) = quoted_path_at_end(next) {
        // Text before the quote that wasn't in the box is a search, not a folder to paste after.
        if previous.starts_with(next[..start].trim_end()) && is_host_path(&quoted) {
            return quoted;
        }
    }
    let after_folder = (previous.ends_with('/') || previous.ends_with('\\')) && next.starts_with(previous);
    let path = if after_folder { &next[previous.len()..] } else { next };
    if !is_host_path(path) {
        return next.to_owned();
    }
    if is_windows_absolute(path) { path.to_owned() } else { unescape_shell(path) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_paths_replace_the_folder_box() {
        assert_eq!(resolve_pasted_path("~/cod", "~/code"), "~/code", "typing is kept");
        assert_eq!(resolve_pasted_path("~/", r"~//Users/me/My\ Repos"), "/Users/me/My Repos");
        assert_eq!(resolve_pasted_path("~/code", "~/code'/a b'"), "/a b");
        assert_eq!(resolve_pasted_path("", r#""C:\Users\me""#), r"C:\Users\me");
        assert_eq!(resolve_pasted_path("", r"'/it'\''s'"), "/it's");
        assert_eq!(resolve_pasted_path("", r"D:\worktrees"), r"D:\worktrees");
        assert_eq!(resolve_pasted_path("ab", "abc def"), "abc def", "a paste that isn't a path is left alone");
    }
}
