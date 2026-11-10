//! `SecretSourceInput` and `SecretSourceSelect` (`apps/web/src/components/settings/SecretSourceInput.tsx`):
//! a credential typed in, or a reference a configured secret provider reads it from. Settings that
//! hold a `SettingsSecretValue` (provider variables and runtime fields, source-control tokens, device
//! host tokens) render their value field through [`secret_source_input`].

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, Subscription, Window, div, px,
};
use serde_json::{Value, json};

use crate::app::Workspace;
use crate::app::request_cards::{
    INVALID_REFERENCE, REFERENCE_PLACEHOLDER, SecretSource, is_one_password_reference, secret_sources,
};
use crate::theme::tokens;

const UNKNOWN_INVALID_REFERENCE: &str = "Enter a valid secret reference.";

impl Workspace {
    /// `useSecretSources`: the secret providers `environment`'s credential settings can point at,
    /// empty unless the build has secrets and the server reads references (`secretReferences`).
    pub(in crate::app) fn secret_sources_on(&self, environment: &str) -> Vec<SecretSource> {
        if !super::secrets_enabled() {
            return Vec::new();
        }
        let Some(info) = self.env_info(environment) else { return Vec::new() };
        secret_sources(info.config["secretReferences"] == true, info.settings.get("secretProviders"))
    }

    /// [`Self::secret_sources_on`] the environment Settings reads.
    pub(in crate::app) fn settings_secret_sources(&self) -> Vec<SecretSource> {
        self.settings_environment().map(|environment| self.secret_sources_on(&environment)).unwrap_or_default()
    }
}

/// A saved `SecretReference`, as opposed to a literal (or its redaction marker).
pub(in crate::app) fn as_reference(value: &Value) -> Option<&Value> {
    value.get("provider").and_then(Value::as_str).map(|_| value)
}

fn saved_provider(secret: Option<&Value>) -> Option<&str> {
    secret?.get("provider")?.as_str()
}

/// What a pick or a committed reference asks the field's owner to save.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) enum SecretChange {
    Keep,
    Set(Value),
    /// Back to a typed value: the saved reference goes.
    Clear,
}

/// The field's own state: a provider picked but not yet committed, and a reference that failed
/// validation (shown as typed, with its error).
#[derive(Clone, Debug, Default, PartialEq)]
pub(in crate::app) struct SourceDraft {
    pending: Option<SharedString>,
    rejected: Option<String>,
}

impl SourceDraft {
    /// The source shown: the pending pick, else the saved reference's provider, else Value (`None`).
    pub(in crate::app) fn provider(&self, secret: Option<&Value>) -> Option<SharedString> {
        self.pending.clone().or_else(|| saved_provider(secret).map(|provider| provider.to_owned().into()))
    }

    /// The reference field's text.
    pub(in crate::app) fn reference_text(&self, secret: Option<&Value>) -> String {
        if let Some(rejected) = &self.rejected {
            return rejected.clone();
        }
        match (self.provider(secret), saved_provider(secret)) {
            (Some(shown), Some(saved)) if shown.as_ref() == saved => {
                secret.and_then(|s| s["reference"].as_str()).unwrap_or_default().to_owned()
            }
            _ => String::new(),
        }
    }

    /// `onValidityChange`: the shown source and its committed field can be saved.
    pub(in crate::app) fn valid(&self) -> bool {
        self.pending.is_none() && self.rejected.is_none()
    }

    pub(in crate::app) fn rejected(&self) -> bool {
        self.rejected.is_some()
    }

    /// Picks Value (`None`) or a provider; a provider applies once a reference for it commits.
    pub(in crate::app) fn select(&mut self, next: Option<SharedString>, secret: Option<&Value>) -> SecretChange {
        self.rejected = None;
        match next {
            None => {
                self.pending = None;
                if secret.is_some() { SecretChange::Clear } else { SecretChange::Keep }
            }
            Some(next) => {
                self.pending = (Some(next.as_ref()) != saved_provider(secret)).then_some(next);
                SecretChange::Keep
            }
        }
    }

    /// Commits the reference field's `text` for the shown provider.
    pub(in crate::app) fn commit(
        &mut self,
        text: &str,
        secret: Option<&Value>,
        sources: &[SecretSource],
    ) -> SecretChange {
        let Some(provider) = self.provider(secret) else { return SecretChange::Keep };
        if text.trim().is_empty() {
            self.rejected = None;
            return if self.pending.is_none() && secret.is_some() { SecretChange::Clear } else { SecretChange::Keep };
        }
        match reference_for(&provider, text, sources) {
            Some(reference) => {
                self.rejected = None;
                self.pending = None;
                SecretChange::Set(reference)
            }
            None => {
                self.rejected = Some(text.to_owned());
                SecretChange::Keep
            }
        }
    }
}

/// `decodeSecretReference` (`SecretProviderId` and `SecretReferencePath`), then the provider
/// definition's `isReference` for a provider this build reads.
fn reference_for(provider: &str, text: &str, sources: &[SecretSource]) -> Option<Value> {
    let reference = text.trim();
    let mut slug = provider.chars();
    let slug_ok = provider.chars().count() <= 64
        && slug.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && slug.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'));
    let path_ok = !reference.is_empty() && reference.chars().count() <= 1024 && !reference.contains(['\r', '\n']);
    let known = sources.iter().any(|source| source.id.as_ref() == provider);
    (slug_ok && path_ok && (!known || is_one_password_reference(reference)))
        .then(|| json!({"provider": provider, "reference": reference}))
}

type OnSecret = Rc<dyn Fn(Option<Value>, &mut Window, &mut App)>;

/// The keyed state behind one [`secret_source_input`].
pub(in crate::app) struct SecretSourceField {
    draft: SourceDraft,
    input: Entity<InputState>,
    placeholder: SharedString,
    secret: Option<Value>,
    sources: Vec<SecretSource>,
    on_secret: Option<OnSecret>,
    _subscription: Subscription,
}

impl SecretSourceField {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                this.commit(window, cx);
            }
        });
        Self {
            draft: SourceDraft::default(),
            input,
            placeholder: SharedString::default(),
            secret: None,
            sources: Vec::new(),
            on_secret: None,
            _subscription: subscription,
        }
    }

    /// `DraftInput`'s commit: only an edited reference.
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        if text == self.draft.reference_text(self.secret.as_ref()) {
            return;
        }
        let change = self.draft.commit(&text, self.secret.as_ref(), &self.sources);
        self.apply(change, window, cx);
    }

    fn select(&mut self, next: Option<SharedString>, window: &mut Window, cx: &mut Context<Self>) {
        let change = self.draft.select(next, self.secret.as_ref());
        let text = self.draft.reference_text(self.secret.as_ref());
        self.input.update(cx, |input, cx| input.set_value(text, window, cx));
        self.apply(change, window, cx);
    }

    fn apply(&mut self, change: SecretChange, window: &mut Window, cx: &mut Context<Self>) {
        cx.notify();
        let Some(on_secret) = self.on_secret.clone() else { return };
        match change {
            SecretChange::Keep => {}
            SecretChange::Set(reference) => on_secret(Some(reference), window, cx),
            SecretChange::Clear => on_secret(None, window, cx),
        }
    }
}

/// The parts of a [`secret_source_input`] for the caller to lay out.
pub(in crate::app) struct SecretParts {
    /// The source picker; `None` when there is nothing to pick.
    pub(in crate::app) source: Option<AnyElement>,
    /// The caller's value field, or the reference field and its error.
    pub(in crate::app) value: AnyElement,
    /// Whether the picked source and committed field can be saved (`onValidityChange`).
    pub(in crate::app) valid: bool,
}

impl SecretParts {
    /// The default layout: the picker, then the field.
    pub(in crate::app) fn row(self) -> AnyElement {
        match self.source {
            None => self.value,
            Some(source) => {
                h_flex().flex_1().min_w_0().items_start().gap(px(6.)).child(source).child(self.value).into_any_element()
            }
        }
    }
}

/// `SecretSourceInput`: `value_field` (the caller's typed-value input) while the source is Value,
/// else a reference field that commits on blur or Enter. `secret` is the saved reference, if any;
/// `on_secret` saves a committed reference, or `None` to go back to a typed value.
#[expect(clippy::too_many_arguments, reason = "one field's parts, mirroring the web component's props")]
pub(in crate::app) fn secret_source_input(
    key: impl Into<SharedString>,
    label: &str,
    secret: Option<&Value>,
    sources: &[SecretSource],
    disabled: bool,
    value_field: impl FnOnce() -> AnyElement,
    on_secret: impl Fn(Option<Value>, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> SecretParts {
    let key = key.into();
    let secret = secret.and_then(as_reference);
    let state = window.use_keyed_state(SharedString::from(format!("secret-source-{key}")), cx, SecretSourceField::new);
    let on_secret: OnSecret = Rc::new(on_secret);
    let (draft, input) = state.update(cx, |field, cx| {
        field.secret = secret.cloned();
        field.sources = sources.to_vec();
        field.on_secret = Some(on_secret);
        let provider = field.draft.provider(secret);
        let known = provider.as_ref().is_some_and(|p| sources.iter().any(|s| &s.id == p));
        let placeholder: SharedString = if known { REFERENCE_PLACEHOLDER.into() } else { SharedString::default() };
        if field.placeholder != placeholder {
            field.placeholder = placeholder.clone();
            field.input.update(cx, |input, cx| input.set_placeholder(placeholder, window, cx));
        }
        let text = field.draft.reference_text(secret);
        let focused = field.input.read(cx).focus_handle(cx).is_focused(window);
        if !focused && field.input.read(cx).value().as_ref() != text {
            field.input.update(cx, |input, cx| input.set_value(text, window, cx));
        }
        (field.draft.clone(), field.input.clone())
    });
    let provider = draft.provider(secret);
    let t = *tokens(cx);
    let source = (!sources.is_empty() || provider.is_some()).then(|| {
        let state = state.downgrade();
        source_select(
            SharedString::from(format!("secret-source-select-{key}")),
            label,
            provider.as_ref(),
            sources,
            disabled,
            move |next, window, cx| {
                let _ = state.update(cx, |field, cx| field.select(next, window, cx));
            },
        )
        .into_any_element()
    });
    let value = match &provider {
        None => value_field(),
        Some(provider) => {
            let known = sources.iter().any(|source| &source.id == provider);
            let message = if known { INVALID_REFERENCE } else { UNKNOWN_INVALID_REFERENCE };
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(4.))
                .child(Input::new(&input).small().font_family(t.fonts.mono).disabled(disabled))
                .children(draft.rejected().then(|| div().text_size(px(11.)).text_color(t.error).child(message)))
                .into_any_element()
        }
    };
    SecretParts { source, value, valid: draft.valid() }
}

/// `SecretSourceSelect`: Value or one of `sources`; a `current` provider no longer among them
/// stays listed under its id.
pub(in crate::app) fn source_select(
    id: SharedString,
    label: &str,
    current: Option<&SharedString>,
    sources: &[SecretSource],
    disabled: bool,
    on_pick: impl Fn(Option<SharedString>, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let one_password = || Icon::empty().path("icons/app/onepassword.svg");
    let known = current.is_none_or(|current| sources.iter().any(|source| &source.id == current));
    let icon = match current {
        None => Icon::new(IconName::Type),
        Some(_) if known => one_password(),
        Some(_) => Icon::new(IconName::KeyRound),
    };
    let title: SharedString = match current {
        None => "Value".into(),
        Some(current) => sources.iter().find(|s| &s.id == current).map_or_else(|| current.clone(), |s| s.label.clone()),
    };
    let unknown = current.filter(|_| !known).cloned();
    let sources = sources.to_vec();
    let on_pick = Rc::new(on_pick);
    Button::new(id)
        .outline()
        .small()
        .flex_none()
        .icon(icon.size(px(14.)))
        .disabled(disabled)
        .tooltip(format!("{label} source: {title}"))
        .dropdown_menu(move |menu, _, _| {
            let pick = |source: Option<SharedString>| {
                let on_pick = on_pick.clone();
                move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| on_pick(source.clone(), window, cx)
            };
            let entry = |icon: Icon, text: SharedString| {
                PopupMenuItem::element(move |_, _| {
                    h_flex().gap(px(8.)).child(icon.clone().size(px(14.))).child(text.clone())
                })
            };
            let mut menu = menu.item(entry(Icon::new(IconName::Type), "Value".into()).on_click(pick(None)));
            for source in &sources {
                menu = menu.item(entry(one_password(), source.label.clone()).on_click(pick(Some(source.id.clone()))));
            }
            if let Some(unknown) = &unknown {
                menu = menu
                    .item(entry(Icon::new(IconName::KeyRound), unknown.clone()).on_click(pick(Some(unknown.clone()))));
            }
            menu
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources() -> Vec<SecretSource> {
        vec![SecretSource { id: "op".into(), label: "1Password".into() }]
    }

    #[test]
    fn a_picked_provider_applies_once_its_reference_commits() {
        let mut draft = SourceDraft::default();
        assert_eq!(draft.select(Some("op".into()), None), SecretChange::Keep);
        assert_eq!((draft.provider(None).as_deref(), draft.valid()), (Some("op"), false));
        assert_eq!(draft.commit("  ", None, &sources()), SecretChange::Keep, "nothing to clear yet");
        assert_eq!(draft.commit("not a reference", None, &sources()), SecretChange::Keep);
        assert!(draft.rejected() && !draft.valid());
        assert_eq!(draft.reference_text(None), "not a reference", "the rejected text stays shown");
        let reference = json!({"provider": "op", "reference": "op://vault/item/field"});
        assert_eq!(draft.commit(" op://vault/item/field ", None, &sources()), SecretChange::Set(reference.clone()));
        assert!(draft.valid());
        assert_eq!(draft.reference_text(Some(&reference)), "op://vault/item/field");
    }

    #[test]
    fn going_back_to_value_or_emptying_the_reference_clears_a_saved_one() {
        let saved = json!({"provider": "op", "reference": "op://vault/item/field"});
        let mut draft = SourceDraft::default();
        assert_eq!(draft.provider(Some(&saved)).as_deref(), Some("op"));
        assert_eq!(draft.select(None, Some(&saved)), SecretChange::Clear);
        assert_eq!(draft.select(None, None), SecretChange::Keep);
        assert_eq!(draft.commit("", Some(&saved), &sources()), SecretChange::Clear);
        // A different provider picked over a saved reference keeps it until a reference commits.
        assert_eq!(draft.select(Some("vault".into()), Some(&saved)), SecretChange::Keep);
        assert_eq!(draft.commit("", Some(&saved), &sources()), SecretChange::Keep);
        assert_eq!(draft.reference_text(Some(&saved)), "");
        assert_eq!(draft.select(Some("op".into()), Some(&saved)), SecretChange::Keep);
        assert!(draft.valid(), "picking the saved provider again is the saved state");
    }

    #[test]
    fn references_to_providers_this_build_cannot_read_only_need_the_schema() {
        assert_eq!(
            reference_for("homelab", " vault#token ", &sources()),
            Some(json!({"provider": "homelab", "reference": "vault#token"}))
        );
        assert_eq!(reference_for("op", "vault#token", &sources()), None, "1Password's own syntax");
        assert_eq!(reference_for("-bad", "x", &sources()), None);
        assert_eq!(reference_for("homelab", "a\nb", &sources()), None);
    }
}
