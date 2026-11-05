//! "Custom model prices" (`UsagePriceOverrides.tsx`): per-model rates and "Map to" aliases,
//! saved to `usagePriceOverrides` / `usageModelAliases` with `server.updateSettings`.
//! This window reads one environment, so the web's "Apply to" picker has nothing to pick
//! and the model-ID suggestions (a browser datalist) are left out.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AppContext as _, Context, ElementId, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity, Window, div,
    prelude::FluentBuilder as _, px, relative,
};
use serde_json::{Value, json};

use crate::app::Workspace;
use crate::usage::{self as logic, PRICE_FIELDS, PriceDraft};

struct PriceRow {
    id: usize,
    /// The saved model, or the new row's model field.
    model: Result<String, Entity<InputState>>,
    alias: Entity<InputState>,
    rates: [Entity<InputState>; 4],
    removed: bool,
}

pub(in crate::app) struct PriceEditor {
    workspace: WeakEntity<Workspace>,
    /// The environment whose prices the dialog opened on; it saves there.
    environment: Option<SharedString>,
    label: String,
    prices: Value,
    aliases: Value,
    rows: Vec<PriceRow>,
    next_id: usize,
    pending: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl PriceEditor {
    fn input(
        &mut self,
        value: &str,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let input = cx
            .new(|cx| InputState::new(window, cx).placeholder(placeholder.to_owned()).default_value(value.to_owned()));
        self._subscriptions.push(cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        }));
        input
    }

    fn saved_row(&mut self, model: &str, window: &mut Window, cx: &mut Context<Self>) -> PriceRow {
        let alias = self.input(&logic::alias_cell(&self.aliases, model), "None", window, cx);
        let rates = std::array::from_fn(|i| {
            let (value, placeholder) = logic::price_cell(&self.prices, model, i);
            self.input(&value, placeholder, window, cx)
        });
        self.next_id += 1;
        PriceRow { id: self.next_id, model: Ok(model.to_owned()), alias, rates, removed: false }
    }

    fn add_row(&mut self, model: &str, window: &mut Window, cx: &mut Context<Self>) {
        let field = self.input(model, "Model ID", window, cx);
        field.update(cx, |input, cx| input.focus(window, cx));
        let alias = self.input("", "Optional", window, cx);
        let rates = std::array::from_fn(|i| {
            let placeholder = if PRICE_FIELDS[i].2 { "Input rate" } else { "0.00" };
            self.input("", placeholder, window, cx)
        });
        self.next_id += 1;
        self.rows.push(PriceRow { id: self.next_id, model: Err(field), alias, rates, removed: false });
        cx.notify();
    }

    /// The rows as drafts: a cell counts as edited once it differs from the saved value.
    fn drafts(&self, cx: &gpui_kit::App) -> Vec<PriceDraft> {
        self.rows
            .iter()
            .map(|row| {
                let text = |input: &Entity<InputState>| input.read(cx).value().to_string();
                match &row.model {
                    Ok(model) => {
                        let alias = text(&row.alias);
                        PriceDraft {
                            model: model.clone(),
                            is_new: false,
                            values: std::array::from_fn(|i| {
                                let value = text(&row.rates[i]);
                                (value != logic::price_cell(&self.prices, model, i).0).then_some(value)
                            }),
                            alias: (alias != logic::alias_cell(&self.aliases, model)).then_some(alias),
                            removed: row.removed,
                        }
                    }
                    Err(field) => PriceDraft {
                        model: text(field),
                        is_new: true,
                        values: std::array::from_fn(|i| Some(text(&row.rates[i]))),
                        alias: Some(text(&row.alias)),
                        removed: false,
                    },
                }
            })
            .collect()
    }

    fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let models: Vec<String> = self.rows.iter().filter_map(|r| r.model.clone().ok()).collect();
        self.rows = models.iter().map(|m| self.saved_row(m, window, cx)).collect();
        self.error = None;
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let drafts = self.drafts(cx);
        let Ok((prices, aliases)) = logic::price_table_changes(&self.prices, &self.aliases, &self.label, &drafts)
        else {
            return;
        };
        let Some(client) = self.environment.as_ref().and_then(|e| self.workspace.upgrade()?.read(cx).env_client(e))
        else {
            return;
        };
        let mut patch = serde_json::Map::new();
        if !prices.is_empty() {
            patch.insert("usagePriceOverrides".into(), Value::Object(prices));
        }
        if !aliases.is_empty() {
            patch.insert("usageModelAliases".into(), Value::Object(aliases));
        }
        if patch.is_empty() {
            return;
        }
        self.pending = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("server.updateSettings", json!({"patch": patch})).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.pending = false;
                match result {
                    Ok(_) => window.close_dialog(cx),
                    Err(error) => {
                        this.error = Some(format!("Not saved · {error}"));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
}

impl Render for PriceEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *crate::theme::tokens(cx);
        let drafts = self.drafts(cx);
        let errors = match logic::price_table_changes(&self.prices, &self.aliases, &self.label, &drafts) {
            Ok(_) => Vec::new(),
            Err(errors) => errors,
        };
        let mut errors = errors;
        // A new row for a model that already has one (`This model already has a row`).
        for (index, draft) in drafts.iter().enumerate().filter(|(_, d)| d.is_new && !d.is_empty()) {
            let model = draft.model.trim();
            if model.is_empty() {
                errors.push((index, "Enter a model ID.".into()));
            } else if drafts.iter().enumerate().any(|(other, d)| other != index && d.model.trim() == model) {
                errors.push((index, "This model already has a row. Edit its prices there.".into()));
            }
        }
        let has_changes = drafts.iter().any(|d| !d.is_empty() && !d.is_unchanged());
        let locked = self.pending;
        // Model ID 22%, Map to 18%, four rates share the rest, then the 40 px action column.
        let columns = |model: gpui_kit::AnyElement,
                       alias: gpui_kit::AnyElement,
                       rates: [gpui_kit::AnyElement; 4],
                       action: gpui_kit::AnyElement| {
            let [a, b, c, d] = rates;
            h_flex()
                .w_full()
                .gap(px(8.))
                .px(px(8.))
                .child(div().w(relative(0.22)).min_w_0().child(model))
                .child(div().w(relative(0.18)).min_w_0().child(alias))
                .child(div().flex_1().min_w_0().child(a))
                .child(div().flex_1().min_w_0().child(b))
                .child(div().flex_1().min_w_0().child(c))
                .child(div().flex_1().min_w_0().child(d))
                .child(div().w(px(32.)).flex_none().child(action))
        };
        let head = |label: &str| {
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.muted_foreground)
                .child(label.to_owned())
                .into_any_element()
        };
        let header = columns(
            head("Model ID"),
            head("Map to"),
            PRICE_FIELDS.map(|(_, label, _)| head(label)),
            Button::new("usage-price-add")
                .ghost()
                .xsmall()
                .icon(Icon::new(Lucide::Plus))
                .disabled(locked)
                .tooltip("Add model price")
                .on_click(cx.listener(|this, _, window, cx| this.add_row("", window, cx)))
                .into_any_element(),
        )
        .h(px(36.))
        .border_b_1()
        .border_color(t.border);
        let mut body = v_flex().w_full();
        if self.rows.is_empty() {
            body = body.child(
                div()
                    .py(px(24.))
                    .text_center()
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .child("No custom prices or mappings. Add a row to set one."),
            );
        }
        for (index, row) in self.rows.iter().enumerate() {
            let cell = |input: &Entity<InputState>| {
                Input::new(input).small().disabled(locked || row.removed).into_any_element()
            };
            let model: gpui_kit::AnyElement = match &row.model {
                Ok(model) => div()
                    .truncate()
                    .text_sm()
                    .font_family(t.fonts.mono)
                    .when(row.removed, |el| el.line_through().text_color(t.muted_foreground))
                    .child(model.clone())
                    .into_any_element(),
                Err(field) => Input::new(field).small().disabled(locked).into_any_element(),
            };
            let (icon, tip) = match (&row.model, row.removed) {
                (Err(_), _) => (Lucide::X, "Remove row"),
                (Ok(_), true) => (Lucide::RotateCcw, "Undo reset"),
                (Ok(_), false) => (Lucide::RotateCcw, "Reset to automatic"),
            };
            let id = row.id;
            let action = div()
                .id(ElementId::Name(format!("usage-price-action-{id}").into()))
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|s| s.bg(t.accent))
                .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(at) = this.rows.iter().position(|r| r.id == id) {
                        if this.rows[at].model.is_err() {
                            this.rows.remove(at);
                        } else {
                            this.rows[at].removed = !this.rows[at].removed;
                        }
                        cx.notify();
                    }
                }))
                .child(Icon::new(icon).size(px(14.)).text_color(t.muted_foreground))
                .into_any_element();
            body = body.child(
                v_flex()
                    .py(px(6.))
                    .border_b_1()
                    .border_color(t.border.opacity(0.6))
                    .child(columns(model, cell(&row.alias), row.rates.each_ref().map(cell), action))
                    .children(errors.iter().filter(|(i, _)| *i == index).map(|(_, error)| {
                        div().px(px(8.)).pt(px(4.)).text_xs().text_color(t.error_foreground).child(error.clone())
                    })),
            );
        }
        v_flex()
            .gap(px(16.))
            .child(
                div()
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .child("Prices and mappings apply to all past and future usage on the environments you select."),
            )
            .child(
                v_flex()
                    .w_full()
                    .overflow_hidden()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(t.border)
                    .child(header)
                    .child(div().id("usage-price-rows").max_h(px(360.)).overflow_y_scroll().child(body)),
            )
            .child(div().text_xs().text_color(t.muted_foreground).child(
                "Blank cache rates use the input price. Enter 0 for free tokens. A mapped model’s usage counts as the model it maps to.",
            ))
            .children(self.error.clone().map(|e| div().text_xs().text_color(t.error_foreground).child(e)))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().text_xs().text_color(t.muted_foreground).when(has_changes, |el| {
                        el.child(format!("Changes apply to {}", self.label))
                    }))
                    .when(has_changes, |el| {
                        el.child(
                            Button::new("usage-price-discard")
                                .ghost()
                                .label("Discard changes")
                                .disabled(locked)
                                .on_click(cx.listener(|this, _, window, cx| this.discard(window, cx))),
                        )
                    })
                    .child(
                        Button::new("usage-price-save")
                            .primary()
                            .label(SharedString::from(if locked { "Saving…" } else { "Save changes" }))
                            .disabled(locked || !has_changes || !errors.is_empty())
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}

impl Workspace {
    /// Opens "Custom model prices", with a new row for `model` when it has no price yet
    /// (the model dialog's "Set price").
    pub(super) fn open_model_prices(&mut self, model: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let environment = self.active_environment().cloned();
        let settings = &self.server_info().settings;
        let prices = settings.get("usagePriceOverrides").clone();
        let aliases = settings.get("usageModelAliases").clone();
        let label = self.server_info().environment_label.clone().unwrap_or_else(|| "this environment".into());
        let workspace = cx.weak_entity();
        let editor = cx.new(|cx| {
            let mut editor = PriceEditor {
                workspace,
                environment,
                label,
                prices,
                aliases,
                rows: Vec::new(),
                next_id: 0,
                pending: false,
                error: None,
                _subscriptions: Vec::new(),
            };
            let mut models: Vec<String> = [&editor.prices, &editor.aliases]
                .iter()
                .flat_map(|map| map.as_object().into_iter().flat_map(|m| m.keys().cloned()))
                .collect();
            models.sort();
            models.dedup();
            editor.rows = models.iter().map(|m| editor.saved_row(m, window, cx)).collect();
            if let Some(model) = model.filter(|m| !models.contains(m)) {
                editor.add_row(&model, window, cx);
            }
            editor
        });
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .entrance(DialogEntrance::None)
                .title(SharedString::from("Custom model prices"))
                .width(px(896.))
                .child(editor.clone())
        });
    }
}
