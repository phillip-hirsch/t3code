//! "Custom model prices" (`UsagePriceOverrides.tsx`): per-model rates and "Map to" aliases,
//! saved to `usagePriceOverrides` / `usageModelAliases` with `server.updateSettings` on every
//! environment "Apply to" picks. A cell the picked environments disagree on reads "Mixed" and
//! keeps each one's rate until edited; each destination saves, fails and retries on its own.
//! The model-ID suggestions (a browser datalist) are left out.

use std::collections::HashSet;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, EntityId, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    WeakEntity, Window, div, prelude::FluentBuilder as _, px, relative,
};

use crate::app::Workspace;
use crate::usage::{self as logic, PRICE_FIELDS, PriceAttempt, PriceDraft, PriceSave, PriceTarget};

const SETTINGS_WRITE: &str = "settings:write";

struct PriceRow {
    id: usize,
    /// The saved model, or the new row's model field.
    model: Result<String, Entity<InputState>>,
    alias: Entity<InputState>,
    rates: [Entity<InputState>; 4],
    removed: bool,
    _subscriptions: Vec<Subscription>,
}

pub(in crate::app) struct PriceEditor {
    workspace: WeakEntity<Workspace>,
    /// The environments "Apply to" picked, by id; `None` is all of them.
    selected: Option<Vec<String>>,
    /// Every environment as the saved rows were built from it.
    basis: Vec<PriceTarget>,
    rows: Vec<PriceRow>,
    /// Inputs the user typed in, so a cleared "Mixed" cell still counts as edited.
    edited: HashSet<EntityId>,
    next_id: usize,
    pending: bool,
    attempt: Option<PriceAttempt>,
    _observe: Subscription,
}

impl Workspace {
    /// `priceTargetsAtom`: every environment, with why it cannot take a write now.
    fn price_targets(&self) -> Vec<PriceTarget> {
        self.sessions()
            .iter()
            .filter_map(|session| {
                let id = session.id.clone()?;
                let info = &session.server_info;
                let loaded = !info.config.is_null();
                let unavailable = if !session.is_connected() {
                    Some("Offline")
                } else if !loaded {
                    Some("Prices not loaded")
                } else {
                    match self.env_grant(&id, SETTINGS_WRITE) {
                        None => Some("Checking permissions…"),
                        Some(false) => Some("Read-only access"),
                        Some(true) => None,
                    }
                };
                let aliases = info.capabilities["usageModelAliases"] == true;
                Some(PriceTarget {
                    id: id.to_string(),
                    label: info.environment_label.clone().unwrap_or_else(|| id.to_string()),
                    prices: loaded.then(|| info.settings.get("usagePriceOverrides").clone()),
                    aliases: (loaded && aliases).then(|| info.settings.get("usageModelAliases").clone()),
                    unavailable: unavailable.map(str::to_owned),
                })
            })
            .collect()
    }

    /// Opens "Custom model prices" on the Usage page's environments, with a new row for `model`
    /// when none of them prices it yet (the model dialog's "Set price").
    pub(super) fn open_model_prices(&mut self, model: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.usage.selected.as_ref().map(|ids| ids.iter().map(ToString::to_string).collect());
        let basis = self.price_targets();
        let workspace = cx.entity();
        let editor = cx.new(|cx| {
            let mut editor = PriceEditor {
                workspace: workspace.downgrade(),
                selected,
                basis,
                rows: Vec::new(),
                edited: HashSet::new(),
                next_id: 0,
                pending: false,
                attempt: None,
                // Prices, connections and grants change while it is open.
                _observe: cx.observe(&workspace, |_, _, cx| cx.notify()),
            };
            editor.rebuild_saved_rows(window, cx);
            if let Some(model) = model.filter(|m| !logic::custom_models(&editor.selected_basis()).contains(m)) {
                editor.add_row(&model, window, cx);
            }
            editor
        });
        window.open_dialog(cx, move |dialog, _, cx| {
            // A save in flight finishes before the dialog can close.
            let open = !editor.read(cx).pending;
            dialog
                .entrance(DialogEntrance::None)
                .title(SharedString::from("Custom model prices"))
                .width(px(896.))
                .overlay_closable(open)
                .keyboard(open)
                .close_button(open)
                .child(editor.clone())
        });
    }
}

fn is_selected(selected: Option<&Vec<String>>, id: &str) -> bool {
    selected.is_none_or(|ids| ids.iter().any(|picked| picked == id))
}

impl PriceEditor {
    fn live_targets(&self, cx: &App) -> Vec<PriceTarget> {
        self.workspace.upgrade().map(|w| w.read(cx).price_targets()).unwrap_or_default()
    }

    fn selected_basis(&self) -> Vec<PriceTarget> {
        self.basis.iter().filter(|t| is_selected(self.selected.as_ref(), &t.id)).cloned().collect()
    }

    fn input(
        &mut self,
        value: &str,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<InputState>, Subscription) {
        let input = cx
            .new(|cx| InputState::new(window, cx).placeholder(placeholder.to_owned()).default_value(value.to_owned()));
        let subscription = cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.edited.insert(input.entity_id());
                this.attempt = None;
                cx.notify();
            }
        });
        (input, subscription)
    }

    fn saved_row(
        &mut self,
        model: &str,
        targets: &[PriceTarget],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PriceRow {
        let mut subscriptions = Vec::new();
        let (value, placeholder) = logic::alias_cell(targets, model);
        let (alias, subscription) = self.input(&value, placeholder, window, cx);
        subscriptions.push(subscription);
        let rates = std::array::from_fn(|i| {
            let (value, placeholder) = logic::price_cell(targets, model, i);
            let (input, subscription) = self.input(&value, placeholder, window, cx);
            subscriptions.push(subscription);
            input
        });
        self.next_id += 1;
        PriceRow {
            id: self.next_id,
            model: Ok(model.to_owned()),
            alias,
            rates,
            removed: false,
            _subscriptions: subscriptions,
        }
    }

    /// The saved rows again from `basis`; new rows stay.
    fn rebuild_saved_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let targets = self.selected_basis();
        let fresh: Vec<PriceRow> =
            logic::custom_models(&targets).iter().map(|model| self.saved_row(model, &targets, window, cx)).collect();
        let added = std::mem::take(&mut self.rows).into_iter().filter(|row| row.model.is_err());
        self.rows = fresh.into_iter().chain(added).collect();
        let live: HashSet<EntityId> = self.rows.iter().flat_map(row_inputs).map(Entity::entity_id).collect();
        self.edited.retain(|id| live.contains(id));
    }

    fn add_row(&mut self, model: &str, window: &mut Window, cx: &mut Context<Self>) {
        let mut subscriptions = Vec::new();
        let (field, subscription) = self.input(model, "Model ID", window, cx);
        subscriptions.push(subscription);
        field.update(cx, |input, cx| input.focus(window, cx));
        let (alias, subscription) = self.input("", "Optional", window, cx);
        subscriptions.push(subscription);
        let rates = std::array::from_fn(|i| {
            let placeholder = if PRICE_FIELDS[i].2 { "Input rate" } else { "0.00" };
            let (input, subscription) = self.input("", placeholder, window, cx);
            subscriptions.push(subscription);
            input
        });
        self.next_id += 1;
        let id = self.next_id;
        self.rows.push(PriceRow { id, model: Err(field), alias, rates, removed: false, _subscriptions: subscriptions });
        self.attempt = None;
        cx.notify();
    }

    /// The rows as drafts (`UsagePriceDraft`). A saved cell is edited once it differs from the
    /// shared value; a "Mixed" or "Unavailable" one once typed in, even back to blank.
    fn drafts(&self, cx: &App) -> Vec<PriceDraft> {
        let targets = self.selected_basis();
        let text = |input: &Entity<InputState>| input.read(cx).value().to_string();
        let edit = |input: &Entity<InputState>, (saved, placeholder): (String, &str)| {
            let value = text(input);
            let shared = placeholder != logic::MIXED && placeholder != logic::UNAVAILABLE;
            let edited = if shared { value != saved } else { self.edited.contains(&input.entity_id()) };
            edited.then_some(value)
        };
        self.rows
            .iter()
            .map(|row| match &row.model {
                Ok(model) => PriceDraft {
                    model: model.clone(),
                    is_new: false,
                    values: std::array::from_fn(|i| edit(&row.rates[i], logic::price_cell(&targets, model, i))),
                    alias: edit(&row.alias, logic::alias_cell(&targets, model)),
                    removed: row.removed,
                },
                Err(field) => PriceDraft {
                    model: text(field),
                    is_new: true,
                    values: std::array::from_fn(|i| Some(text(&row.rates[i]))),
                    alias: Some(text(&row.alias)),
                    removed: false,
                },
            })
            .collect()
    }

    fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rows.clear();
        self.basis = self.live_targets(cx);
        self.rebuild_saved_rows(window, cx);
        self.attempt = None;
        cx.notify();
    }

    fn select(&mut self, selected: Option<Vec<String>>, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = selected;
        self.basis = self.live_targets(cx);
        self.rebuild_saved_rows(window, cx);
        self.attempt = None;
        cx.notify();
    }

    /// `save`: every picked environment at once, or only the failed ones on a retry.
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        let live = self.live_targets(cx);
        let failed = self.attempt.as_ref().map(PriceAttempt::failed).unwrap_or_default();
        let retry = !failed.is_empty();
        let (destinations, drafts) = if retry {
            (failed, self.attempt.as_ref().map(|a| a.drafts.clone()).unwrap_or_default())
        } else {
            let picked = live.iter().filter(|t| is_selected(self.selected.as_ref(), &t.id));
            let destinations = picked.map(|t| (t.id.clone(), t.label.clone())).collect();
            (destinations, self.drafts(cx).into_iter().filter(|d| !d.is_empty() && !d.is_unchanged()).collect())
        };
        if destinations.is_empty() || drafts.is_empty() {
            return;
        }
        let targets: Vec<PriceTarget> = destinations
            .iter()
            .map(|(id, label)| {
                live.iter().find(|t| t.id == *id).cloned().unwrap_or_else(|| PriceTarget {
                    id: id.clone(),
                    label: label.clone(),
                    prices: None,
                    aliases: None,
                    unavailable: Some("Environment removed".into()),
                })
            })
            .collect();
        self.attempt = Some(PriceAttempt::start(self.attempt.take(), retry, drafts.clone(), &destinations));
        self.pending = true;
        cx.notify();
        let workspace = self.workspace.clone();
        for target in targets {
            let plan = logic::price_write(&target, &drafts);
            let client = workspace.upgrade().and_then(|w| w.read(cx).env_client(&target.id));
            let workspace = workspace.clone();
            cx.spawn_in(window, async move |this, cx| {
                let status = match plan {
                    Err(error) => PriceSave::Failed(error),
                    Ok(None) => PriceSave::Saved,
                    Ok(Some(input)) => {
                        // Grants change on reconnect, so check the destination again at the write.
                        let granted = cx
                            .update(|_, cx| {
                                workspace.upgrade().is_some_and(|w| w.read(cx).env_grants(&target.id, SETTINGS_WRITE))
                            })
                            .unwrap_or(false);
                        let saved = match client.filter(|_| granted) {
                            Some(client) => client.request("server.updateSettings", input).await.is_ok(),
                            None => false,
                        };
                        if saved { PriceSave::Saved } else { PriceSave::Failed("Could not save. Try again.".into()) }
                    }
                };
                let _ = this.update_in(cx, |this, window, cx| this.settle(&target.id, status, window, cx));
            })
            .detach();
        }
    }

    /// One destination settled; once all have, a clean save clears the drafts.
    fn settle(&mut self, id: &str, status: PriceSave, window: &mut Window, cx: &mut Context<Self>) {
        let Some(attempt) = self.attempt.as_mut() else { return };
        attempt.record(id, status);
        if attempt.settled() {
            self.pending = false;
            if attempt.failed().is_empty() {
                self.rows.clear();
                self.basis = self.live_targets(cx);
                self.rebuild_saved_rows(window, cx);
            }
        }
        cx.notify();
    }
}

fn row_inputs(row: &PriceRow) -> impl Iterator<Item = &Entity<InputState>> {
    row.model.as_ref().err().into_iter().chain([&row.alias]).chain(row.rates.iter())
}

impl Render for PriceEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *crate::theme::tokens(cx);
        let live = self.live_targets(cx);
        let failed = self.attempt.as_ref().map(PriceAttempt::failed).unwrap_or_default();
        let staged = |drafts: &[PriceDraft]| drafts.iter().any(|d| !d.is_empty() && !d.is_unchanged());
        // Saved prices moved under rows nobody edited: show them.
        if !self.pending && failed.is_empty() && live != self.basis && !staged(&self.drafts(cx)) {
            self.basis = live.clone();
            self.rebuild_saved_rows(window, cx);
        }
        let selected: Vec<PriceTarget> =
            live.iter().filter(|t| is_selected(self.selected.as_ref(), &t.id)).cloned().collect();
        let drafts = self.drafts(cx);
        let has_changes = staged(&drafts);
        let custom = logic::custom_models(&self.selected_basis());
        let mut errors = logic::price_table_errors(&selected, &drafts);
        // A new row needs a model, and one the table does not list yet.
        for (index, draft) in drafts.iter().enumerate().filter(|(_, d)| d.is_new && !d.is_empty()) {
            let model = draft.model.trim();
            if model.is_empty() {
                errors.push((index, "Enter a model ID.".into()));
            } else if self.attempt.is_none()
                && (custom.iter().any(|m| m == model)
                    || drafts
                        .iter()
                        .enumerate()
                        .any(|(other, d)| other != index && d.is_new && d.model.trim() == model))
            {
                errors.push((index, "This model already has a row. Edit its prices there.".into()));
            }
        }
        let locked = self.pending || !failed.is_empty();
        let destination_label = match selected.as_slice() {
            [only] => only.label.clone(),
            many => format!("{} environments", many.len()),
        };
        let selection_label =
            if self.selected.is_none() { "All environments".to_owned() } else { destination_label.clone() };
        let view = cx.entity();
        let all_ids: Vec<String> = live.iter().map(|t| t.id.clone()).collect();
        let picked_ids: Vec<String> = selected.iter().map(|t| t.id.clone()).collect();
        let all_picked = self.selected.is_none();
        let menu_rows: Vec<(String, String, bool, Option<String>)> = live
            .iter()
            .map(|t| (t.id.clone(), t.label.clone(), is_selected(self.selected.as_ref(), &t.id), t.unavailable.clone()))
            .collect();
        let muted = t.muted_foreground;
        let apply_to = h_flex()
            .min_w_0()
            .gap(px(8.))
            .child(div().flex_none().text_sm().font_weight(FontWeight::MEDIUM).child("Apply to"))
            .child(
                Button::new("usage-prices-apply")
                    .outline()
                    .small()
                    .disabled(self.pending || has_changes)
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap(px(6.))
                            .child(div().min_w_0().truncate().child(selection_label))
                            .child(Icon::new(Lucide::ChevronDown).with_size(px(14.))),
                    )
                    .dropdown_menu(move |mut menu, window, _| {
                        let toggle_all = if all_picked { Some(Vec::new()) } else { None };
                        menu = menu
                            .item(PopupMenuItem::new("All environments").checked(all_picked).on_click(
                                window.listener_for(&view, move |this, _, window, cx| {
                                    this.select(toggle_all.clone(), window, cx);
                                }),
                            ))
                            .separator();
                        for (id, label, checked, unavailable) in menu_rows.iter().cloned() {
                            let (all_ids, picked_ids) = (all_ids.clone(), picked_ids.clone());
                            let item = PopupMenuItem::element(move |_, _| {
                                h_flex()
                                    .min_w(px(200.))
                                    .gap(px(12.))
                                    .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                                    .children(unavailable.clone().map(|u| div().text_xs().text_color(muted).child(u)))
                            })
                            .checked(checked)
                            .on_click(window.listener_for(
                                &view,
                                move |this, _, window, cx| {
                                    let mut next = picked_ids.clone();
                                    match next.iter().position(|picked| *picked == id) {
                                        Some(at) => {
                                            next.remove(at);
                                        }
                                        None => next.push(id.clone()),
                                    }
                                    let all = next.len() == all_ids.len() && all_ids.iter().all(|id| next.contains(id));
                                    this.select((!all).then_some(next), window, cx);
                                },
                            ));
                            menu = menu.item(item);
                        }
                        menu
                    }),
            );
        let toolbar = h_flex()
            .flex_wrap()
            .justify_between()
            .gap(px(8.))
            .child(apply_to)
            .child(div().text_xs().text_color(t.muted_foreground).child("USD / million tokens"));

        // Model ID 22%, Map to 18%, four rates share the rest, then the 40 px action column.
        let model_cell = |model: AnyElement| div().w(relative(0.22)).min_w_0().child(model);
        let row_box = || h_flex().w_full().gap(px(8.)).px(px(8.));
        let head = |label: &str| {
            div().text_xs().font_weight(FontWeight::MEDIUM).text_color(t.muted_foreground).child(label.to_owned())
        };
        let header = row_box()
            .h(px(36.))
            .border_b_1()
            .border_color(t.border)
            .child(model_cell(head("Model ID").into_any_element()))
            .child(div().w(relative(0.18)).min_w_0().child(head("Map to")))
            .children(PRICE_FIELDS.map(|(_, label, _)| div().flex_1().min_w_0().child(head(label))))
            .child(
                div().w(px(32.)).flex_none().child(
                    Button::new("usage-price-add")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(Lucide::Plus))
                        .disabled(locked)
                        .tooltip("Add model price")
                        .on_click(cx.listener(|this, _, window, cx| this.add_row("", window, cx))),
                ),
            );
        let mut body = v_flex().w_full();
        if self.rows.is_empty() {
            let empty = if selected.iter().any(|t| t.prices.is_none()) {
                "Some environment prices are unavailable."
            } else {
                "No custom prices or mappings. Add a row to set one."
            };
            body = body.child(div().py(px(24.)).text_center().text_sm().text_color(t.muted_foreground).child(empty));
        }
        for (index, row) in self.rows.iter().enumerate() {
            let cell = |input: &Entity<InputState>| Input::new(input).small().disabled(locked).into_any_element();
            let model: AnyElement = match &row.model {
                Ok(model) => div()
                    .truncate()
                    .text_sm()
                    .font_family(t.fonts.mono)
                    .when(row.removed, |el| el.line_through().text_color(t.muted_foreground))
                    .child(model.clone())
                    .into_any_element(),
                Err(field) => cell(field),
            };
            let (icon, tip) = match (&row.model, row.removed) {
                (Err(_), _) => (Lucide::X, "Remove row"),
                (Ok(_), true) => (Lucide::RotateCcw, "Undo reset"),
                (Ok(_), false) => (Lucide::RotateCcw, "Reset to automatic"),
            };
            let id = row.id;
            let action = Button::new(ElementId::Name(format!("usage-price-action-{id}").into()))
                .ghost()
                .xsmall()
                .icon(Icon::new(icon))
                .tooltip(tip)
                .disabled(locked)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(at) = this.rows.iter().position(|r| r.id == id) {
                        if this.rows[at].model.is_err() {
                            this.rows.remove(at);
                        } else {
                            this.rows[at].removed = !this.rows[at].removed;
                            this.attempt = None;
                        }
                        cx.notify();
                    }
                }));
            let alias = row.alias.read(cx).value().trim().to_owned();
            let mut line = row_box().child(model_cell(model));
            line = if row.removed {
                line.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(t.muted_foreground)
                        .child("Automatic pricing after saving"),
                )
            } else if !alias.is_empty() {
                line.child(div().w(relative(0.18)).min_w_0().child(cell(&row.alias))).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(t.muted_foreground)
                        .child(format!("Counted as {alias}")),
                )
            } else {
                line.child(div().w(relative(0.18)).min_w_0().child(cell(&row.alias)))
                    .children(row.rates.iter().map(|rate| div().flex_1().min_w_0().child(cell(rate))))
            };
            body = body.child(
                v_flex()
                    .py(px(6.))
                    .border_b_1()
                    .border_color(t.border.opacity(0.6))
                    .child(line.child(div().w(px(32.)).flex_none().child(action)))
                    .children(errors.iter().filter(|(i, _)| *i == index).map(|(_, error)| {
                        div().px(px(8.)).pt(px(4.)).text_xs().text_color(t.error_foreground).child(error.clone())
                    })),
            );
        }
        let table: AnyElement = if selected.is_empty() {
            div()
                .text_sm()
                .text_color(t.muted_foreground)
                .child(if live.is_empty() {
                    "Connect an environment to set model prices."
                } else {
                    "Select an environment to see and change its model prices."
                })
                .into_any_element()
        } else {
            let mixed = if selected.len() > 1 {
                " Mixed cells keep each environment’s rate until you edit them."
            } else {
                ""
            };
            v_flex()
                .gap(px(16.))
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
                .child(div().text_xs().text_color(t.muted_foreground).child(format!(
                    "Blank cache rates use the input price. Enter 0 for free tokens. A mapped model’s usage counts as the model it maps to.{mixed}"
                )))
                .into_any_element()
        };
        let statuses = self.attempt.as_ref().map(|attempt| {
            v_flex().gap(px(4.)).text_xs().children(attempt.destinations.iter().map(|(id, label)| {
                let (text, failed) = match attempt.status(id) {
                    Some(PriceSave::Failed(error)) => (format!("Not saved · {error}"), true),
                    Some(PriceSave::Saved) => ("Saved".to_owned(), false),
                    _ => ("Saving…".to_owned(), false),
                };
                h_flex()
                    .justify_between()
                    .gap(px(12.))
                    .child(div().min_w_0().truncate().text_color(t.muted_foreground).child(label.clone()))
                    .child(div().text_color(if failed { t.error_foreground } else { t.muted_foreground }).child(text))
            }))
        });
        let save_label = if self.pending {
            "Saving…"
        } else if !failed.is_empty() {
            "Retry failed saves"
        } else {
            "Save changes"
        };
        let save_disabled = self.pending
            || (!has_changes && failed.is_empty())
            || (failed.is_empty() && (!errors.is_empty() || selected.is_empty()));
        v_flex()
            .gap(px(16.))
            .child(
                div()
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .child("Prices and mappings apply to all past and future usage on the environments you select."),
            )
            .child(toolbar)
            .child(table)
            .children(statuses)
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(t.muted_foreground)
                            .when(has_changes, |el| el.child(format!("Changes apply to {destination_label}"))),
                    )
                    .when(has_changes, |el| {
                        el.child(
                            Button::new("usage-price-discard")
                                .ghost()
                                .label(if failed.is_empty() { "Discard changes" } else { "Discard pending changes" })
                                .disabled(self.pending)
                                .on_click(cx.listener(|this, _, window, cx| this.discard(window, cx))),
                        )
                    })
                    .child(
                        Button::new("usage-price-save")
                            .primary()
                            .label(SharedString::from(save_label))
                            .disabled(save_disabled)
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}
