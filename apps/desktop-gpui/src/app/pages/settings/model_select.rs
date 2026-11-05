//! The settings rows' model and effort pickers (`ProviderModelPicker` and `TraitsPicker` in a
//! settings row) and the text-generation default (`resolveAppModelSelectionState`).

use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{Icon, Sizable as _, h_flex};
use gpui_kit::{
    AnyElement, Context, Div, ElementId, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div, px,
    svg,
};
use serde_json::Value;

use crate::app::Workspace;
use crate::model::{ModelSelection, Provider};
use crate::model_label;
use crate::theme::{Tokens, tokens};

type OnPick = Rc<dyn Fn(&mut Workspace, ModelSelection, &mut Window, &mut Context<Workspace>)>;

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

fn trigger_label(label: SharedString) -> impl IntoElement {
    h_flex()
        .flex_1()
        .min_w_0()
        .justify_between()
        .gap(px(6.))
        .child(div().truncate().child(label))
        .child(Icon::new(Lucide::ChevronDown).with_size(px(14.)).flex_none().opacity(0.7))
}

/// Text-generation providers a model row can pick from (`supportsTextGeneration !== false`).
pub(in crate::app) fn generates_text(provider: &Provider) -> bool {
    provider.supports_text_generation != Some(false)
}

impl Workspace {
    /// `ProviderModelPicker` in a settings row: provider icon, short model name and a menu of
    /// every model of every usable provider that `include` keeps.
    pub(in crate::app) fn model_picker(
        &self,
        id: &'static str,
        selection: &ModelSelection,
        label: Option<&str>,
        include: fn(&Provider) -> bool,
        cx: &mut Context<Self>,
        on_pick: impl Fn(&mut Self, ModelSelection, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let t = *tokens(cx);
        let provider = self.server_info.provider(&selection.instance_id);
        let model = self.server_info.model(selection);
        let name: SharedString = match (label, provider.zip(model)) {
            (Some(label), _) => label.to_owned().into(),
            (None, Some((provider, model))) => model_label::trigger_model_name(model, &provider.driver).into(),
            (None, None) => selection.model.clone().into(),
        };
        let icon = provider.and_then(|p| crate::assets::provider_icon(&p.driver));
        let groups: Vec<(SharedString, Vec<(SharedString, ModelSelection)>)> = self
            .server_info
            .providers
            .iter()
            .filter(|p| p.is_selectable() && include(p))
            .map(|p| {
                let models = p.models.iter().map(|m| {
                    let pick =
                        ModelSelection { instance_id: p.instance_id.clone(), model: m.slug.clone(), options: None };
                    (SharedString::from(m.name.clone().unwrap_or_else(|| m.slug.clone())), pick)
                });
                (p.display_name.clone().unwrap_or_else(|| p.instance_id.clone()).into(), models.collect())
            })
            .collect();
        let current = selection.clone();
        let view = cx.entity();
        let on_pick: OnPick = Rc::new(on_pick);
        trigger(id, &t)
            .min_w(px(0.))
            .children(icon.map(|(path, tint)| {
                svg().path(path).size(px(16.)).flex_none().text_color(tint.unwrap_or(t.foreground))
            }))
            .child(trigger_label(name))
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu: PopupMenu, window, _| {
                for (index, (group, models)) in groups.iter().enumerate() {
                    if index > 0 {
                        menu = menu.separator();
                    }
                    menu = menu.label(group.clone());
                    for (name, pick) in models {
                        let checked = pick.instance_id == current.instance_id && pick.model == current.model;
                        let (pick, on_pick) = (pick.clone(), on_pick.clone());
                        menu = menu.item(PopupMenuItem::new(name.clone()).checked(checked).on_click(
                            window.listener_for(&view, move |this, _, window, cx| {
                                on_pick(this, pick.clone(), window, cx)
                            }),
                        ));
                    }
                }
                menu.max_h(px(420.)).scrollable(true)
            })
            .into_any_element()
    }

    /// `TraitsPicker` with prompt-injected effort left out: the model's effort levels and its
    /// on/off options. `None` when the model has neither.
    pub(in crate::app) fn traits_picker(
        &self,
        id: &'static str,
        selection: &ModelSelection,
        cx: &mut Context<Self>,
        on_pick: impl Fn(&mut Self, ModelSelection, &mut Window, &mut Context<Self>) + 'static,
    ) -> Option<AnyElement> {
        let t = *tokens(cx);
        let descriptors = self.server_info.model(selection)?.capabilities.as_ref()?.option_descriptors.clone();
        let effort = model_label::reasoning(&descriptors).filter(|d| d.kind == "select").cloned();
        let toggles: Vec<_> = descriptors.iter().filter(|d| d.kind == "boolean").cloned().collect();
        if effort.is_none() && toggles.is_empty() {
            return None;
        }
        let label = model_label::effort_label(&descriptors, selection).unwrap_or_else(|| "Options".into());
        let current = selection.clone();
        let view = cx.entity();
        let on_pick: OnPick = Rc::new(on_pick);
        Some(
            trigger(id, &t)
                .min_w(px(0.))
                .child(trigger_label(label.into()))
                .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu: PopupMenu, window, _| {
                    if let Some(effort) = &effort {
                        let value = model_label::current_value(effort, &current);
                        menu = menu.label(effort.label.clone());
                        for option in effort.options.iter().filter(|o| !effort.prompt_injected_values.contains(&o.id)) {
                            let pick = model_label::with_option(&current, &effort.id, Value::String(option.id.clone()));
                            let on_pick = on_pick.clone();
                            menu = menu.item(
                                PopupMenuItem::new(option.label.clone())
                                    .checked(value.as_ref().and_then(Value::as_str) == Some(&option.id))
                                    .on_click(window.listener_for(&view, move |this, _, window, cx| {
                                        on_pick(this, pick.clone(), window, cx)
                                    })),
                            );
                        }
                    }
                    for toggle in &toggles {
                        let on =
                            model_label::current_value(toggle, &current).and_then(|v| v.as_bool()).unwrap_or(false);
                        let pick = model_label::with_option(&current, &toggle.id, Value::Bool(!on));
                        let on_pick = on_pick.clone();
                        menu = menu.separator().item(PopupMenuItem::new(toggle.label.clone()).checked(on).on_click(
                            window.listener_for(&view, move |this, _, window, cx| {
                                on_pick(this, pick.clone(), window, cx)
                            }),
                        ));
                    }
                    menu
                })
                .into_any_element(),
        )
    }
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
/// the stored provider is kept.
pub(in crate::app) fn resolve_text_generation(
    selection: Option<&ModelSelection>,
    providers: &[Provider],
) -> Option<ModelSelection> {
    let usable = |p: &&Provider| p.is_selectable() && generates_text(p);
    let selected = selection.and_then(|s| providers.iter().filter(usable).find(|p| p.instance_id == s.instance_id));
    let provider = selected.or_else(|| providers.iter().find(usable))?;
    let has = |slug: &&str| provider.models.iter().any(|m| m.slug == *slug);
    let kept = selected.and(selection).map(|s| s.model.as_str()).filter(has);
    let model = kept
        .or_else(|| default_text_generation_model(&provider.driver).filter(has))
        .or_else(|| provider.default_model())?;
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
        let resolved = resolve_text_generation(Some(&stored), &providers).unwrap();
        assert_eq!(
            (resolved.instance_id.as_str(), resolved.model.as_str(), resolved.options),
            ("claude", "claude-haiku-5-5", None)
        );
        let kept = ModelSelection { instance_id: "claude".into(), model: "claude-opus-5-5".into(), options: None };
        assert_eq!(resolve_text_generation(Some(&kept), &providers).unwrap(), kept);
    }
}
