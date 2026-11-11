//! `packages/client-runtime/src/handoff.ts`: where a provider handoff came from and went, the
//! handoff the composer's pick would start, and what a same-harness model switch changed.

use serde_json::Value;

use crate::model::{ModelSelection, Provider, ProviderModel, Run, ThreadProjection, TurnItem};
use crate::model_label;

/// A provider instance, and the model it ran when known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub instance_id: String,
    pub model: Option<String>,
}

impl From<&ModelSelection> for Endpoint {
    fn from(selection: &ModelSelection) -> Self {
        Self { instance_id: selection.instance_id.clone(), model: Some(selection.model.clone()) }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoints {
    pub from: Vec<Endpoint>,
    pub to: Endpoint,
}

/// `resolveHandoffEndpoints`: a `handoff` item's sources and destination, with models from the
/// item or, for older items, the runs around it.
pub fn resolve_endpoints(item: &TurnItem, runs: &[Run]) -> Endpoints {
    let to_instance = item.str("toProviderInstanceId").unwrap_or_default().to_owned();
    let handoff_run = item.run_id.as_ref().and_then(|id| runs.iter().find(|run| &run.id == id));
    let to_model = item.str("toModel").map(str::to_owned).or_else(|| {
        let run = handoff_run.filter(|run| run.provider_instance_id.as_deref() == Some(to_instance.as_str()))?;
        Some(run.model_selection.as_ref()?.model.clone())
    });
    let selections: Vec<ModelSelection> = item
        .raw
        .get("fromModelSelections")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    let from = if selections.is_empty() {
        let instances = item.raw.get("fromProviderInstanceIds").and_then(Value::as_array);
        instances
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|instance| Endpoint {
                instance_id: instance.to_owned(),
                model: latest_run_model_before(runs, instance, handoff_run.map(|run| run.ordinal)),
            })
            .collect()
    } else {
        selections.iter().map(Endpoint::from).collect()
    };
    Endpoints { from, to: Endpoint { instance_id: to_instance, model: to_model } }
}

fn latest_run_model_before(runs: &[Run], instance_id: &str, before: Option<i64>) -> Option<String> {
    runs.iter()
        .filter(|run| run.provider_instance_id.as_deref() == Some(instance_id))
        .filter(|run| before.is_none_or(|before| run.ordinal < before))
        .max_by_key(|run| run.ordinal)
        .and_then(|run| Some(run.model_selection.as_ref()?.model.clone()))
}

/// `resolvePendingHandoff`: the handoff sending with `selection` would start, from the provider
/// the thread last ran on, unless the two continue one conversation.
pub fn pending(
    projection: &ThreadProjection,
    selection: Option<&ModelSelection>,
    providers: &[Provider],
) -> Option<Endpoints> {
    // A thread with an empty timeline has nothing to hand off.
    if projection.visible_turn_items.is_empty() {
        return None;
    }
    // The handoff row is committed before the provider starts; during startup its run counts.
    let from = projection
        .runs
        .iter()
        .rev()
        .find(|run| {
            run.status != "rolled_back"
                && (run.started_at.is_some() || (run.status == "starting" && run.context_handoff_id.is_some()))
        })?
        .model_selection
        .as_ref()?;
    let selection = selection?;
    let provider = |id: &str| providers.iter().find(|provider| provider.instance_id == id);
    let (source, target) = (provider(&from.instance_id), provider(&selection.instance_id));
    let needs_handoff = from.instance_id != selection.instance_id
        && (continuation_group(source).is_none()
            || source.map(|p| &p.driver) != target.map(|p| &p.driver)
            || continuation_group(source) != continuation_group(target));
    needs_handoff.then(|| Endpoints { from: vec![from.into()], to: selection.into() })
}

fn continuation_group(provider: Option<&Provider>) -> Option<&str> {
    Some(provider?.continuation.as_ref()?.group_key.as_str())
}

fn catalog_model<'a>(
    providers: &'a [Provider],
    instance_id: &str,
    model: &str,
) -> Option<(&'a Provider, &'a ProviderModel)> {
    let provider = providers.iter().find(|provider| provider.instance_id == instance_id)?;
    Some((provider, provider.models.iter().find(|candidate| candidate.slug == model)?))
}

/// `resolveEffort`: the reasoning control's value and label, or the stated value when the
/// catalog lacks the model.
fn effort(selection: &ModelSelection, providers: &[Provider]) -> Option<(String, String)> {
    let Some((_, model)) = catalog_model(providers, &selection.instance_id, &selection.model) else {
        let stated = selection.options.iter().flatten().find_map(|option| {
            let id = option.get("id")?.as_str()?;
            model_label::REASONING_IDS.contains(&id).then(|| option.get("value")?.as_str()).flatten()
        })?;
        return Some((stated.to_owned(), stated.to_owned()));
    };
    let descriptors = &model.capabilities.as_ref()?.option_descriptors;
    let descriptor = model_label::reasoning(descriptors).filter(|descriptor| descriptor.kind == "select")?;
    let value = model_label::current_value(descriptor, selection)?.as_str()?.to_owned();
    let label =
        descriptor.options.iter().find(|option| option.id == value).map_or_else(|| value.clone(), |o| o.label.clone());
    Some((value, label))
}

/// `ModelSwitchRow`'s label, `GPT-5.5 High → GPT-5.5 Low`, naming only what changed; `None` when
/// the switch changed neither the model nor the effort.
pub fn model_switch_label(from: &ModelSelection, to: &ModelSelection, providers: &[Provider]) -> Option<String> {
    let model_changed = from.model != to.model;
    let efforts = match (effort(from, providers), effort(to, providers)) {
        (Some(from), Some(to)) if from.0 != to.0 => Some((from.1, to.1)),
        _ => None,
    };
    if !model_changed && efforts.is_none() {
        return None;
    }
    let describe = |selection: &ModelSelection, effort: Option<&String>| {
        let name = model_changed.then(|| match catalog_model(providers, &selection.instance_id, &selection.model) {
            Some((provider, model)) => model_label::trigger_model_name(model, &provider.driver),
            None => selection.model.clone(),
        });
        name.into_iter().chain(effort.cloned()).collect::<Vec<_>>().join(" ")
    };
    Some(format!(
        "{} → {}",
        describe(from, efforts.as_ref().map(|e| &e.0)),
        describe(to, efforts.as_ref().map(|e| &e.1))
    ))
}

/// `HandoffEndpoint`'s label: the model's short name, else the written model, else the provider's
/// name; and the provider's display name for its tooltip.
pub fn endpoint_label(endpoint: &Endpoint, providers: &[Provider]) -> (String, String) {
    let provider = providers.iter().find(|provider| provider.instance_id == endpoint.instance_id);
    let display = provider.and_then(|p| p.display_name.clone()).unwrap_or_else(|| endpoint.instance_id.clone());
    let model = endpoint.model.as_deref().map(str::trim).filter(|model| !model.is_empty());
    let label = match model {
        Some(model) => match catalog_model(providers, &endpoint.instance_id, model) {
            Some((provider, found)) => model_label::trigger_model_name(found, &provider.driver),
            None => model.to_owned(),
        },
        None => display.clone(),
    };
    (label, display)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn providers() -> Vec<Provider> {
        let effort = json!({"id": "effort", "label": "Effort", "type": "select", "options": [
            {"id": "high", "label": "High", "isDefault": true}, {"id": "low", "label": "Low"}]});
        serde_json::from_value(json!([
            {"instanceId": "codex", "driver": "codex", "displayName": "Codex", "models": [
                {"slug": "gpt-5.5", "name": "GPT-5.5", "capabilities": {"optionDescriptors": [effort]}},
                {"slug": "gpt-5.4", "name": "GPT-5.4", "capabilities": {"optionDescriptors": [effort]}}]},
            {"instanceId": "claude", "driver": "claudeAgent", "displayName": "Claude", "models": []}
        ]))
        .unwrap()
    }

    fn selection(instance: &str, model: &str, effort: Option<&str>) -> ModelSelection {
        let options = effort.map(|effort| vec![json!({"id": "effort", "value": effort})]);
        ModelSelection { instance_id: instance.into(), model: model.into(), options }
    }

    #[test]
    fn a_model_switch_names_only_what_changed() {
        let providers = providers();
        let label = |from, to| model_switch_label(&from, &to, &providers);
        assert_eq!(
            label(selection("codex", "gpt-5.5", None), selection("codex", "gpt-5.5", Some("low"))).as_deref(),
            Some("High → Low")
        );
        assert_eq!(
            label(selection("codex", "gpt-5.5", Some("low")), selection("codex", "gpt-5.4", Some("low"))).as_deref(),
            Some("GPT-5.5 → GPT-5.4")
        );
        assert_eq!(label(selection("codex", "gpt-5.5", Some("high")), selection("codex", "gpt-5.5", None)), None);
        // A model the catalog lacks keeps its slug and its stated effort.
        assert_eq!(
            label(selection("claude", "opus", Some("max")), selection("claude", "opus", Some("low"))).as_deref(),
            Some("max → low")
        );
    }

    #[test]
    fn handoff_endpoints_fill_models_from_the_runs() {
        let item: TurnItem = serde_json::from_value(json!({"id": "h", "type": "handoff", "runId": "r2",
            "status": "completed", "fromProviderInstanceIds": ["claude"], "toProviderInstanceId": "codex"}))
        .unwrap();
        let runs: Vec<Run> = serde_json::from_value(json!([
            {"id": "r0", "status": "completed", "ordinal": 0, "providerInstanceId": "claude",
                "modelSelection": {"instanceId": "claude", "model": "sonnet"}},
            {"id": "r1", "status": "completed", "ordinal": 1, "providerInstanceId": "claude",
                "modelSelection": {"instanceId": "claude", "model": "opus"}},
            {"id": "r2", "status": "running", "ordinal": 2, "providerInstanceId": "codex",
                "modelSelection": {"instanceId": "codex", "model": "gpt-5.5"}}
        ]))
        .unwrap();
        let endpoint =
            |instance: &str, model: &str| Endpoint { instance_id: instance.into(), model: Some(model.into()) };
        assert_eq!(
            resolve_endpoints(&item, &runs),
            Endpoints { from: vec![endpoint("claude", "opus")], to: endpoint("codex", "gpt-5.5") }
        );
        assert_eq!(endpoint_label(&endpoint("codex", "gpt-5.5"), &providers()), ("GPT-5.5".into(), "Codex".into()));
    }

    #[test]
    fn a_pick_on_another_harness_is_a_pending_handoff() {
        let projection: ThreadProjection = serde_json::from_value(json!({
            "thread": {"id": "t", "title": "T"},
            "runs": [{"id": "r", "status": "completed", "ordinal": 0, "startedAt": "2026-11-04T10:00:00.000Z",
                "providerInstanceId": "codex", "modelSelection": {"instanceId": "codex", "model": "gpt-5.5"}}],
            "visibleTurnItems": [{"visibility": "visible", "sourceThreadId": "t", "sourceItemId": "u",
                "item": {"id": "u", "type": "user_message", "status": "completed", "text": "hi"}}]
        }))
        .unwrap();
        let providers = providers();
        let same = selection("codex", "gpt-5.4", None);
        assert_eq!(pending(&projection, Some(&same), &providers), None);
        let other = selection("claude", "opus", None);
        assert_eq!(pending(&projection, Some(&other), &providers).map(|e| e.to.instance_id), Some("claude".to_owned()));
    }
}
