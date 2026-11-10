//! The settings rows' model and effort pickers (`ProviderModelPicker` and `TraitsPicker` in a
//! settings row) and the text-generation default (`resolveAppModelSelectionState`).

use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Context, Div, ElementId, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _,
    Window, div, px, svg,
};
use serde_json::Value;

use crate::app::Workspace;
use crate::app::model_picker::{OnPick, PickHost, generator_owns};
use crate::model::{ModelSelection, OptionChoice, OptionDescriptor, Provider};
use crate::model_label;
use crate::theme::{Tokens, tokens};

/// The bordered trigger shared by selects and pickers (`SETTINGS_PICKER_TRIGGER_CLASSNAME`).
pub(in crate::app) fn trigger(id: impl Into<ElementId>, t: &Tokens) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .h(px(28.))
        .min_w(px(144.))
        .px(px(9.))
        .gap(px(6.))
        .rounded(px(8.))
        .border_1()
        .border_color(t.input)
        .bg(if t.dark { t.input.opacity(0.32) } else { t.background })
        .text_sm()
        .text_color(t.foreground)
}

/// The pickers' text: `ProviderModelPicker` and `TraitsPicker` draw no chevron.
fn trigger_label(label: SharedString) -> impl IntoElement {
    div().flex_1().min_w_0().truncate().child(label)
}

/// Text-generation providers a model row can pick from (`supportsTextGeneration !== false`).
pub(in crate::app) fn generates_text(provider: &Provider) -> bool {
    provider.supports_text_generation != Some(false)
}

impl Workspace {
    /// `ProviderModelPicker` in a settings row, opening the shared picker over the providers
    /// `include` keeps.
    pub(in crate::app) fn model_picker(
        &self,
        id: impl Into<SharedString>,
        selection: Option<&ModelSelection>,
        label: Option<&str>,
        include: fn(&Provider) -> bool,
        cx: &mut Context<Self>,
        on_pick: impl Fn(&mut Self, ModelSelection, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        self.model_picker_among(id, selection, label, include, None, cx, on_pick)
    }

    /// [`Self::model_picker`] that also offers `extra` providers, such as text generators,
    /// in place of instances with the same id.
    pub(in crate::app) fn model_picker_among(
        &self,
        id: impl Into<SharedString>,
        selection: Option<&ModelSelection>,
        label: Option<&str>,
        include: fn(&Provider) -> bool,
        extra: Option<Rc<[Provider]>>,
        cx: &mut Context<Self>,
        on_pick: impl Fn(&mut Self, ModelSelection, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let id = id.into();
        let environment = self.settings_environment();
        let trigger =
            self.picker_trigger_among(id.clone(), selection, label, environment.as_deref(), extra.as_deref(), cx);
        let on_pick: OnPick = Rc::new(on_pick);
        let host = PickHost { environment, current: selection.cloned(), include: Rc::new(include), extra, on_pick };
        self.undocked_model_picker(id, trigger, host, cx)
    }

    /// The picker's settings trigger: provider icon and short model name from `environment`'s
    /// providers, or `label` (`triggerLabel`).
    pub(in crate::app) fn model_picker_trigger(
        &self,
        id: SharedString,
        selection: Option<&ModelSelection>,
        label: Option<&str>,
        environment: Option<&str>,
        cx: &App,
    ) -> Button {
        self.picker_trigger_among(id, selection, label, environment, None, cx)
    }

    /// [`Self::model_picker_trigger`], finding the selection's provider in `extra` first.
    pub(in crate::app) fn picker_trigger_among(
        &self,
        id: SharedString,
        selection: Option<&ModelSelection>,
        label: Option<&str>,
        environment: Option<&str>,
        extra: Option<&[Provider]>,
        cx: &App,
    ) -> Button {
        let t = *tokens(cx);
        let info = environment.and_then(|environment| self.env_info(environment));
        let provider = selection.and_then(|s| {
            let listed = info?.provider(&s.instance_id);
            let Some(extra) = extra else { return listed };
            let configured = info?.settings.get("textGenerators");
            let listed = listed.filter(|p| !generator_owns(p, configured, extra));
            extra.iter().find(|p| p.instance_id == s.instance_id).or(listed)
        });
        let model = selection.zip(provider).and_then(|(s, p)| p.models.iter().find(|m| m.slug == s.model));
        let name: SharedString = match (label, provider.zip(model), selection) {
            (Some(label), ..) => label.to_owned().into(),
            (None, Some((provider, model)), _) => model_label::trigger_model_name(model, &provider.driver).into(),
            (None, None, Some(selection)) if selection.model.is_empty() => "Choose model".into(),
            (None, None, Some(selection)) => selection.model.clone().into(),
            (None, None, None) => "Select a model".into(),
        };
        // A fixed label ("None", "Mixed") replaces the provider icon too.
        let icon = provider.filter(|_| label.is_none()).and_then(|p| crate::assets::provider_icon(&p.driver));
        trigger(id, &t)
            .min_w(px(0.))
            .children(icon.map(|(path, tint)| {
                svg().path(path).size(px(16.)).flex_none().text_color(tint.unwrap_or(t.foreground))
            }))
            .child(trigger_label(name))
    }

    /// `TraitsPicker` with prompt-injected effort left out: every select's choices, then each
    /// on/off option. `None` when the model has nothing to pick (`hasAnyControls`).
    pub(in crate::app) fn traits_picker(
        &self,
        id: impl Into<ElementId>,
        selection: &ModelSelection,
        cx: &mut Context<Self>,
        on_pick: impl Fn(&mut Self, ModelSelection, &mut Window, &mut Context<Self>) + 'static,
    ) -> Option<AnyElement> {
        let t = *tokens(cx);
        let descriptors = self.settings_info().model(selection)?.capabilities.as_ref()?.option_descriptors.clone();
        let (selects, toggles) = trait_controls(&descriptors)?;
        let label = model_label::effort_label(&descriptors, selection).unwrap_or_else(|| "Options".into());
        let current = selection.clone();
        let view = cx.entity();
        let on_pick: OnPick = Rc::new(on_pick);
        Some(
            trigger(id, &t)
                .min_w(px(0.))
                .child(Icon::new(Lucide::Gauge).with_size(px(16.)).flex_none().text_color(t.muted_foreground))
                .child(trigger_label(label.into()))
                .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu: PopupMenu, window, _| {
                    let item = |menu: PopupMenu, row: PopupMenuItem, pick: ModelSelection| {
                        let on_pick = on_pick.clone();
                        menu.item(row.on_click(window.listener_for(&view, move |this, _, window, cx| {
                            on_pick(this, pick.clone(), window, cx);
                        })))
                    };
                    for (index, select) in selects.iter().enumerate() {
                        if index > 0 {
                            menu = menu.separator();
                        }
                        let value = model_label::current_value(select, &current);
                        menu = menu.label(select.label.clone());
                        for option in select.options.iter().filter(|o| !select.prompt_injected_values.contains(&o.id)) {
                            let checked = value.as_ref().and_then(Value::as_str) == Some(&option.id);
                            let pick = model_label::with_option(&current, &select.id, Value::String(option.id.clone()));
                            menu = item(menu, choice_row(option, &t).checked(checked), pick);
                        }
                    }
                    for (index, toggle) in toggles.iter().enumerate() {
                        if index > 0 || !selects.is_empty() {
                            menu = menu.separator();
                        }
                        let on = model_label::current_value(toggle, &current).and_then(|v| v.as_bool()) == Some(true);
                        menu = menu.label(toggle.label.clone());
                        for (label, value) in [("On", true), ("Off", false)] {
                            let pick = model_label::with_option(&current, &toggle.id, Value::Bool(value));
                            menu = item(menu, PopupMenuItem::new(label).checked(on == value), pick);
                        }
                    }
                    menu
                })
                .into_any_element(),
        )
    }
}

/// `getTraitsSectionVisibility` for a settings row: the model's selects and toggles, or `None`
/// when it offers no select and neither Fast mode nor Thinking.
fn trait_controls(descriptors: &[OptionDescriptor]) -> Option<(Vec<OptionDescriptor>, Vec<OptionDescriptor>)> {
    let of = |kind: &str| descriptors.iter().filter(|d| d.kind == kind).cloned().collect::<Vec<_>>();
    let (selects, toggles) = (of("select"), of("boolean"));
    let known_toggle = toggles.iter().any(|d| matches!(d.id.as_str(), "fastMode" | "thinking"));
    (!selects.is_empty() || known_toggle).then_some((selects, toggles))
}

/// A select choice: its label with an outline "Default" badge, and its description below.
fn choice_row(option: &OptionChoice, t: &Tokens) -> PopupMenuItem {
    let (label, default, description) = (option.label.clone(), option.is_default, option.description.clone());
    let t = *t;
    PopupMenuItem::element(move |_, _| {
        let badge = div()
            .flex_none()
            .h(px(16.))
            .min_w(px(16.))
            .px(px(3.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .border_1()
            .border_color(t.input)
            .bg(if t.dark { t.input.opacity(0.32) } else { t.background })
            .text_size(px(10.))
            .line_height(px(10.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.foreground)
            .child("Default");
        v_flex()
            .w_full()
            .min_w_0()
            .child(
                h_flex()
                    .min_w_0()
                    .gap_1()
                    .child(div().min_w_0().truncate().child(label.clone()))
                    .children(default.then_some(badge)),
            )
            .children(description.clone().map(|description| {
                div().max_w(px(224.)).text_xs().text_color(t.muted_foreground.opacity(0.8)).child(description)
            }))
    })
}

/// A muted line in a row's control slot, as the web's `text-sm text-muted-foreground` spans.
pub(in crate::app) fn muted(text: impl Into<SharedString>, t: &Tokens) -> Div {
    div().text_sm().text_color(t.muted_foreground).child(text.into())
}

/// `DEFAULT_TEXT_GENERATION_MODEL_BY_PROVIDER`: the cheap generation model per driver.
fn default_text_generation_model(driver: &str) -> Option<&'static str> {
    match driver {
        "codex" => Some("gpt-6-luna"),
        "antigravity" => Some("antigravity-default"),
        "claudeAgent" => Some("claude-haiku-5-5"),
        "cursor" => Some("composer-2"),
        "opencode" => Some("opencode/big-pickle"),
        _ => None,
    }
}

/// `resolveAppModelSelectionState`: the stored text-generation model while its provider can
/// generate text, else the first such provider's cheap default. Options carry over only when
/// the stored provider is kept. A provider without models resolves with an empty model, so the
/// picker still offers the others.
pub(in crate::app) fn resolve_text_generation(
    selection: Option<&ModelSelection>,
    providers: &[&Provider],
) -> Option<ModelSelection> {
    let usable = |p: &&&Provider| p.is_selectable() && generates_text(p);
    let selected =
        selection.and_then(|s| providers.iter().filter(usable).find(|p| p.instance_id == s.instance_id).copied());
    let provider = selected.or_else(|| providers.iter().find(usable).copied())?;
    let has = |slug: &&str| provider.models.iter().any(|m| m.slug == *slug);
    let kept = selected.and(selection).map(|s| s.model.as_str()).filter(has);
    let cheap = default_text_generation_model(&provider.driver);
    let model = kept.or_else(|| cheap.filter(has)).or_else(|| provider.default_model()).or(cheap).unwrap_or_default();
    Some(ModelSelection {
        instance_id: provider.instance_id.clone(),
        model: model.to_owned(),
        options: selected.and(selection).and_then(|s| s.options.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn traits_list_every_select_and_need_a_known_control() {
        let descriptors: Vec<OptionDescriptor> = serde_json::from_value(json!([
            {"id": "effort", "label": "Effort", "type": "select", "options": [{"id": "low", "label": "Low"}]},
            {"id": "agent", "label": "Agent", "type": "select", "options": [{"id": "build", "label": "Build"}]},
            {"id": "fastMode", "label": "Fast mode", "type": "boolean"}
        ]))
        .unwrap();
        let (selects, toggles) = trait_controls(&descriptors).unwrap();
        let ids = |list: &[OptionDescriptor]| list.iter().map(|d| d.id.clone()).collect::<Vec<_>>();
        assert_eq!((ids(&selects), ids(&toggles)), (vec!["effort".into(), "agent".into()], vec!["fastMode".into()]));
        let unknown: Vec<OptionDescriptor> =
            serde_json::from_value(json!([{"id": "other", "label": "Other", "type": "boolean"}])).unwrap();
        assert!(trait_controls(&unknown).is_none(), "only Fast mode or Thinking toggles count on their own");
    }

    #[test]
    fn text_generation_falls_back_to_a_cheap_default() {
        let providers: Vec<Provider> = serde_json::from_value(json!([
            {"instanceId": "codex", "driver": "codex", "enabled": false, "models": [{"slug": "gpt-6-luna"}]},
            {"instanceId": "replay", "driver": "acpRegistry", "enabled": true, "supportsTextGeneration": false,
             "models": [{"slug": "r"}]},
            {"instanceId": "claude", "driver": "claudeAgent", "enabled": true,
             "models": [{"slug": "claude-opus-5-5"}, {"slug": "claude-haiku-5-5"}]}
        ]))
        .unwrap();
        let effort = vec![json!({"id": "reasoningEffort", "value": "low"})];
        let stored = ModelSelection { instance_id: "codex".into(), model: "gpt-6-luna".into(), options: Some(effort) };
        let providers: Vec<&Provider> = providers.iter().collect();
        let resolved = resolve_text_generation(Some(&stored), &providers).unwrap();
        assert_eq!(
            (resolved.instance_id.as_str(), resolved.model.as_str(), resolved.options),
            ("claude", "claude-haiku-5-5", None)
        );
        let kept = ModelSelection { instance_id: "claude".into(), model: "claude-opus-5-5".into(), options: None };
        assert_eq!(resolve_text_generation(Some(&kept), &providers).unwrap(), kept);

        // A generator whose models failed to load still resolves, so the picker can reach the next.
        let generators: Vec<Provider> = serde_json::from_value(json!([
            {"instanceId": "a", "driver": "openaiCompatible", "enabled": true, "status": "error", "models": []},
            {"instanceId": "b", "driver": "openaiCompatible", "enabled": true, "models": [{"slug": "m"}]}
        ]))
        .unwrap();
        let generators: Vec<&Provider> = generators.iter().collect();
        let empty = ModelSelection { instance_id: "a".into(), model: String::new(), options: None };
        assert_eq!(resolve_text_generation(None, &generators), Some(empty));
    }
}
