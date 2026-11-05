//! The model picker (`ProviderModelPicker.tsx`, `ModelPickerContent.tsx`, `ModelPickerSidebar.tsx`,
//! `ModelListRow.tsx`): a provider rail, search and the model list. The composer's is docked
//! (`ComposerModelEffort.tsx`): it adds the effort column and a footer with the speed and
//! permissions controls, and stays open while picking. Settings rows and the scheduled task
//! editor open the plain one with their own selection and setter.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    Anchor, AnyElement, AppContext as _, Context, ElementId, Entity, Focusable as _, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, Keystroke, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, prelude::FluentBuilder as _, px, svg,
};
use serde_json::{Value, json};

use super::Workspace;
use super::composer::{control_button, runtime_modes};
use crate::model::{ModelSelection, OptionDescriptor, Provider, ProviderModel};
use crate::model_label;
use crate::theme::{self, Tokens};

const RAIL: f32 = 44.;
const LIST: f32 = 316.;
const EFFORT: f32 = 192.;
const BODY_HEIGHT: f32 = 346.;
const COMPOSER: &str = "model-picker";

/// `onInstanceModelChange` for a picker outside the composer.
pub(in crate::app) type OnPick = Rc<dyn Fn(&mut Workspace, ModelSelection, &mut Window, &mut Context<Workspace>)>;

/// What an undocked picker picks for: its row's selection, the providers it offers, and the setter.
#[derive(Clone)]
pub(in crate::app) struct PickHost {
    pub current: Option<ModelSelection>,
    pub include: fn(&Provider) -> bool,
    pub on_pick: OnPick,
}

#[derive(Clone, PartialEq)]
enum Tab {
    Favorites,
    Provider(String),
}

#[derive(Default)]
pub(super) struct PickerState {
    /// The open picker's trigger id; only one is open at a time.
    open: Option<SharedString>,
    /// `None` while the composer's picker is open: it reads the composer's live selection.
    host: Option<PickHost>,
    tab: Option<Tab>,
    search: Option<Entity<InputState>>,
    query: String,
    highlighted: usize,
    show_legacy: bool,
    /// Options last picked per `(instance, model)`, restored when switching back.
    remembered: HashMap<(String, String), Vec<Value>>,
    _search_changes: Option<Subscription>,
}

/// One row of the list, resolved once per render while the picker is open.
struct Row {
    selection: ModelSelection,
    name: String,
    sub_provider: Option<String>,
    icon: Option<(&'static str, Option<Hsla>)>,
    selected: bool,
    favorite: bool,
    new: bool,
    legacy: bool,
}

impl PickerState {
    /// Closes the picker without moving focus, as a target change does.
    pub(super) fn dismiss(&mut self) {
        self.open = None;
        self.host = None;
    }
}

impl Workspace {
    pub(super) fn model_picker_open(&self) -> bool {
        self.picker.open.is_some()
    }

    pub(super) fn composer_picker_open(&self) -> bool {
        self.picker.open.as_deref() == Some(COMPOSER)
    }

    pub(super) fn open_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.target.is_none() || self.server_info.model_choices().is_empty() {
            return;
        }
        self.open_picker(COMPOSER.into(), None, window, cx);
    }

    fn open_picker(&mut self, id: SharedString, host: Option<PickHost>, window: &mut Window, cx: &mut Context<Self>) {
        let search = match self.picker.search.clone() {
            Some(search) => search,
            None => {
                let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search models..."));
                self.picker._search_changes = Some(cx.subscribe_in(&search, window, |this, search, event, _, cx| {
                    if let InputEvent::Change = event {
                        this.picker.query = search.read(cx).value().trim().to_owned();
                        this.picker.highlighted = 0;
                        cx.notify();
                    }
                }));
                self.picker.search = Some(search.clone());
                search
            }
        };
        search.update(cx, |search, cx| {
            search.set_value("", window, cx);
            search.focus(window, cx);
        });
        self.picker.query.clear();
        self.picker.highlighted = 0;
        self.picker.tab = None;
        self.picker.open = Some(id);
        self.picker.host = host;
        cx.notify();
    }

    pub(super) fn close_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.picker.open.take() else { return };
        self.picker.host = None;
        if id.as_ref() == COMPOSER {
            self.focus_composer(window, cx);
        }
        cx.notify();
    }

    /// The selection the open picker marks: its host's, else the composer's.
    fn picker_current(&self) -> Option<ModelSelection> {
        match &self.picker.host {
            Some(host) => host.current.clone(),
            None => self.composer_model(),
        }
    }

    /// The providers the open picker offers.
    fn picker_includes(&self, provider: &Provider) -> bool {
        self.picker.host.as_ref().is_none_or(|host| (host.include)(provider))
    }

    /// The docked picker keeps its popup open; any other closes and hands the model to its host.
    fn pick_row(&mut self, selection: ModelSelection, window: &mut Window, cx: &mut Context<Self>) {
        match self.picker.host.as_ref().map(|host| host.on_pick.clone()) {
            None => self.pick_model(selection, cx),
            Some(on_pick) => {
                self.close_model_picker(window, cx);
                on_pick(self, selection, window, cx);
            }
        }
    }

    fn pick_model(&mut self, selection: ModelSelection, cx: &mut Context<Self>) {
        let key = (selection.instance_id.clone(), selection.model.clone());
        let options = self.picker.remembered.get(&key).cloned();
        self.model_override = Some(ModelSelection { options, ..selection });
        self.refresh_menu_items(cx);
        cx.notify();
    }

    fn set_model_option(&mut self, id: &str, value: Value, cx: &mut Context<Self>) {
        let Some(current) = self.composer_model() else { return };
        let next = model_label::with_option(&current, id, value);
        let key = (next.instance_id.clone(), next.model.clone());
        self.picker.remembered.insert(key, next.options.clone().unwrap_or_default());
        self.model_override = Some(next);
        cx.notify();
    }

    fn toggle_favorite(&mut self, selection: &ModelSelection, cx: &mut Context<Self>) {
        let key = (selection.instance_id.clone(), selection.model.clone());
        let favorite = !self.server_info.favorites.contains(&key);
        if favorite {
            self.server_info.favorites.push(key);
        } else {
            self.server_info.favorites.retain(|f| f != &key);
        }
        cx.notify();
        let Some(client) = self.client() else { return };
        let patch = json!({"patch": {"setModelFavorites": [
            {"provider": selection.instance_id, "model": selection.model, "favorite": favorite}
        ]}});
        cx.spawn(async move |this, cx| {
            if let Err(error) = client.request("server.updateSettings", patch).await {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(error.to_string().into());
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// `providerModelPreferences[id].hiddenModels`, never hiding custom models.
    fn model_hidden(&self, provider: &Provider, model: &ProviderModel) -> bool {
        !model.is_custom
            && self.server_info.settings.get("providerModelPreferences")[provider.instance_id.as_str()]["hiddenModels"]
                .as_array()
                .is_some_and(|hidden| hidden.iter().any(|h| h.as_str() == Some(&model.slug)))
    }

    fn picker_rows<'a>(&'a self, current: Option<&ModelSelection>) -> (Vec<Row>, bool) {
        let ready =
            |p: &&Provider| p.enabled && p.availability.as_deref() != Some("unavailable") && p.status == "ready";
        let favorites = &self.server_info.favorites;
        let is_favorite =
            |p: &Provider, m: &ProviderModel| favorites.contains(&(p.instance_id.clone(), m.slug.clone()));
        let row = |p: &Provider, m: &ProviderModel| Row {
            selection: ModelSelection { instance_id: p.instance_id.clone(), model: m.slug.clone(), options: None },
            name: model_label::display_model_name(m),
            sub_provider: m.sub_provider.clone(),
            icon: crate::assets::provider_icon(&p.driver),
            selected: current.is_some_and(|c| c.instance_id == p.instance_id && c.model == m.slug),
            favorite: is_favorite(p, m),
            new: m.badge.as_deref() == Some("new"),
            legacy: m.is_legacy,
        };
        let models = |p: &'a Provider| p.models.iter().filter(move |m| !self.model_hidden(p, m)).map(move |m| (p, m));
        let providers = self.server_info.providers.iter().filter(ready).filter(|p| self.picker_includes(p));
        if !self.picker.query.is_empty() {
            let mut scored: Vec<(i64, bool, usize, Row)> = providers
                .flat_map(models)
                .enumerate()
                .filter_map(|(order, (p, m))| {
                    let name = p.display_name.as_deref().unwrap_or(&p.instance_id);
                    let favorite = is_favorite(p, m);
                    let score = model_label::search_score(m, &p.driver, name, favorite, &self.picker.query)?;
                    Some((score, !favorite, order, row(p, m)))
                })
                .collect();
            scored.sort_by_key(|(score, not_favorite, order, _)| (*score, *not_favorite, *order));
            return (scored.into_iter().map(|(.., row)| row).collect(), false);
        }
        let tab = self.picker_tab(current);
        let mut rows: Vec<Row> = match &tab {
            Tab::Favorites => {
                providers.flat_map(models).filter(|(p, m)| is_favorite(p, m)).map(|(p, m)| row(p, m)).collect()
            }
            Tab::Provider(id) => {
                providers.filter(|p| &p.instance_id == id).flat_map(models).map(|(p, m)| row(p, m)).collect()
            }
        };
        rows.sort_by_key(|r| !r.favorite);
        let legacy = rows.iter().any(|r| r.legacy && !r.favorite);
        let show_legacy = self.picker.show_legacy || rows.iter().any(|r| r.legacy && r.selected && !r.favorite);
        if !show_legacy {
            rows.retain(|r| !r.legacy || r.favorite);
        }
        (rows, legacy)
    }

    /// The open tab: picked, else favorites when there are any, else the current model's provider.
    fn picker_tab(&self, current: Option<&ModelSelection>) -> Tab {
        if let Some(tab) = &self.picker.tab {
            return tab.clone();
        }
        if !self.server_info.favorites.is_empty() {
            return Tab::Favorites;
        }
        match current {
            Some(selection) => Tab::Provider(selection.instance_id.clone()),
            None => Tab::Favorites,
        }
    }

    /// Up/Down/Enter/Escape and Ctrl+1-9 while the picker is open.
    pub(super) fn picker_key(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let current = self.picker_current();
        let (rows, _) = self.picker_rows(current.as_ref());
        let modifiers = keystroke.modifiers;
        let digit = keystroke.key.parse::<usize>().ok().filter(|d| (1..=9).contains(d));
        match keystroke.key.as_str() {
            "escape" => self.close_model_picker(window, cx),
            "down" | "up" if !rows.is_empty() => {
                let step = if keystroke.key == "up" { rows.len() - 1 } else { 1 };
                self.picker.highlighted = (self.picker.highlighted + step) % rows.len();
                cx.notify();
            }
            "enter" => {
                if let Some(row) = rows.into_iter().nth(self.picker.highlighted) {
                    self.pick_row(row.selection, window, cx);
                }
            }
            _ if digit.is_some() && (modifiers.control || modifiers.platform) && self.picker.host.is_none() => {
                if let Some(row) = rows.into_iter().nth(digit.unwrap_or(1) - 1) {
                    self.pick_row(row.selection, window, cx);
                }
            }
            _ => return false,
        }
        true
    }

    /// The trigger and, while open, the popover.
    pub(super) fn render_model_picker(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let current = self.composer_model();
        let provider = current.as_ref().and_then(|s| self.server_info.provider(&s.instance_id));
        let model = current.as_ref().and_then(|s| self.server_info.model(s));
        let driver = provider.map(|p| p.driver.as_str()).unwrap_or_default();
        let name: SharedString = match (&current, model) {
            (_, Some(model)) => model_label::trigger_model_name(model, driver).into(),
            (Some(selection), None) => selection.model.clone().into(),
            (None, None) => "Choose model".into(),
        };
        let descriptors =
            model.and_then(|m| m.capabilities.as_ref()).map(|c| c.option_descriptors.as_slice()).unwrap_or(&[]);
        let effort = current.as_ref().and_then(|s| model_label::effort_label(descriptors, s));
        let fast = current.as_ref().is_some_and(|s| {
            descriptors.iter().find(|d| d.id == "fastMode").and_then(|d| model_label::current_value(d, s))
                == Some(Value::Bool(true))
        });
        let icon = provider.and_then(|p| crate::assets::provider_icon(&p.driver));
        let open = self.composer_picker_open();
        let disabled = self.target.is_none() || self.server_info.model_choices().is_empty();
        let suffix_color = if open { t.foreground } else { t.muted_foreground };
        let trigger = control_button("model-picker", t)
            .min_w(px(52.))
            .disabled(disabled)
            .selected(open)
            .children(icon.map(|(path, tint)| {
                svg().path(path).size(px(16.)).flex_none().text_color(tint.unwrap_or(t.foreground))
            }))
            .child(
                div()
                    .max_w(px(224.))
                    .truncate()
                    .text_color(if open { t.foreground } else { t.foreground.opacity(0.8) })
                    .child(name.clone()),
            )
            .when(effort.is_some() || fast, |el| {
                el.child(
                    h_flex()
                        .ml(px(-2.))
                        .gap(px(6.))
                        .text_color(suffix_color)
                        .children(effort.map(|e| div().max_w(px(96.)).truncate().child(e)))
                        .when(fast, |el| el.child(Icon::new(Lucide::Zap).with_size(px(12.)))),
                )
            });
        let body = open.then(|| self.render_picker_body(current.as_ref(), &name, Some(descriptors), t, cx));
        self.picker_popover(COMPOSER.into(), trigger, Anchor::BottomRight, None, body, cx)
    }

    /// A picker without the effort column (`ProviderModelPicker` with no `effortColumn`) on `trigger`.
    pub(in crate::app) fn undocked_model_picker(
        &self,
        id: SharedString,
        trigger: Button,
        host: PickHost,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.picker.open.as_ref() == Some(&id);
        let body = open.then(|| {
            let t = *theme::tokens(cx);
            let current = self.picker_current();
            self.render_picker_body(current.as_ref(), &SharedString::default(), None, &t, cx)
        });
        self.picker_popover(id, trigger.selected(open), Anchor::TopLeft, Some(host), body, cx)
    }

    fn picker_popover(
        &self,
        id: SharedString,
        trigger: Button,
        anchor: Anchor,
        host: Option<PickHost>,
        body: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity();
        let open = self.picker.open.as_ref() == Some(&id);
        let mut popover = Popover::new(ElementId::Name(format!("{id}-popover").into()))
            .anchor(anchor)
            .open(open)
            .on_open_change(move |open, window, cx| {
                let (id, host) = (id.clone(), host.clone());
                view.update(cx, |this, cx| {
                    if *open { this.open_picker(id, host, window, cx) } else { this.close_model_picker(window, cx) }
                })
            })
            .trigger(trigger)
            .p_0()
            .rounded(px(8.))
            .overflow_hidden();
        // The popover focuses this itself when it opens, so a shortcut lands in the search too.
        if let Some(search) = &self.picker.search {
            popover = popover.track_focus(&search.focus_handle(cx));
        }
        popover.children(body).into_any_element()
    }

    /// The rail, search and list; `docked` carries the model's option descriptors for the effort column.
    fn render_picker_body(
        &self,
        current: Option<&ModelSelection>,
        model_name: &SharedString,
        docked: Option<&[OptionDescriptor]>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rule = t.foreground.opacity(0.056);
        let searching = !self.picker.query.is_empty();
        let (rows, has_legacy) = self.picker_rows(current);
        let tab = self.picker_tab(current);
        let search = self.picker.search.clone();
        let list = v_flex()
            .id("picker-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pl(px(8.))
            .pr(px(5.))
            .py(px(6.))
            .gap(px(2.))
            .when(rows.is_empty(), |el| {
                let text = if tab == Tab::Favorites && !searching { "No favorites yet" } else { "No models found" };
                el.child(div().p(px(8.)).flex().justify_center().text_sm().text_color(t.muted_foreground).child(text))
            })
            .children(rows.iter().enumerate().map(|(index, row)| {
                self.render_picker_model_row(row, index, tab == Tab::Favorites, docked.is_some(), t, cx)
            }))
            .when(has_legacy && !searching, |el| {
                let label = if self.picker.show_legacy { "Show less" } else { "Show more" };
                el.child(
                    div()
                        .id("picker-legacy")
                        .h(px(32.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .rounded(px(4.))
                        .text_sm()
                        .text_color(t.muted_foreground)
                        .cursor_pointer()
                        .hover(|s| s.text_color(t.foreground))
                        .child(label)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.picker.show_legacy = !this.picker.show_legacy;
                            cx.notify();
                        })),
                )
            });
        let search_row = div().px(px(12.)).pt(px(10.)).child(
            h_flex()
                .relative()
                .pb(px(6.))
                .mt(px(-1.))
                .border_b_1()
                .border_color(rule)
                .child(
                    Icon::new(Lucide::Search)
                        .with_size(px(16.))
                        .text_color(t.muted_foreground.opacity(0.55))
                        .absolute()
                        .top(px(5.))
                        .left_0(),
                )
                .children(search.map(|search| {
                    div().w_full().pl(px(20.)).child(Input::new(&search).appearance(false).small().h(px(26.)).text_sm())
                })),
        );
        let column = v_flex()
            .w(px(if searching { LIST + RAIL } else { LIST }))
            .h_full()
            .when(!searching, |el| el.border_l_1().border_color(rule))
            .child(search_row)
            .child(list);
        v_flex()
            .id("model-picker-body")
            .w(px(RAIL + LIST + if docked.is_some() { EFFORT } else { 0. }))
            .bg(t.popover)
            .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .h(px(BODY_HEIGHT))
                    .items_start()
                    .when(!searching, |el| el.child(self.render_rail(&tab, t, cx)))
                    .child(column)
                    .children(
                        docked.map(|descriptors| self.render_effort_column(current, model_name, descriptors, t, cx)),
                    ),
            )
            .children(docked.map(|descriptors| self.render_picker_footer(current, descriptors, t, cx)))
            .into_any_element()
    }

    fn render_rail(&self, tab: &Tab, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let button = |id: ElementId, selected: bool, enabled: bool| {
            div()
                .id(id)
                .relative()
                .size(px(36.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .when(enabled, |el| el.cursor_pointer().hover(|s| s.bg(t.foreground.opacity(0.1))))
                .when(!enabled, |el| el.opacity(0.5))
                .when(selected, |el| {
                    el.child(
                        div().absolute().right(px(-4.)).top(px(8.)).w(px(3.)).h(px(20.)).rounded_full().bg(t.primary),
                    )
                })
        };
        let favorites = button("rail-favorites".into(), *tab == Tab::Favorites, true)
            .child(Icon::new(Lucide::Star).with_size(px(20.)).text_color(t.foreground))
            .tooltip(|window, cx| Tooltip::new("Favorites").build(window, cx))
            .on_click(cx.listener(|this, _, _, cx| {
                this.picker.tab = Some(Tab::Favorites);
                this.picker.highlighted = 0;
                cx.notify();
            }));
        let providers = self.server_info.providers.iter().filter(|p| p.enabled && self.picker_includes(p)).map(|p| {
            let ready = p.availability.as_deref() != Some("unavailable") && p.status == "ready";
            let name = p.display_name.clone().unwrap_or_else(|| p.instance_id.clone());
            let tooltip: SharedString = match (ready, p.availability.as_deref(), &p.message) {
                (true, ..) => name.clone().into(),
                (false, Some("unavailable"), None) => format!("{name}: Unavailable.").into(),
                (false, Some("unavailable"), Some(message)) => format!("{name}: Unavailable. {message}").into(),
                (false, _, None) => format!("{name}: Not ready.").into(),
                (false, _, Some(message)) => format!("{name}: Not ready. {message}").into(),
            };
            let id = p.instance_id.clone();
            let icon: AnyElement = match crate::assets::provider_icon(&p.driver) {
                Some((path, tint)) => {
                    svg().path(path).size(px(20.)).text_color(tint.unwrap_or(t.foreground)).into_any_element()
                }
                None => {
                    div().text_size(px(10.)).font_weight(FontWeight::SEMIBOLD).child(initials(&name)).into_any_element()
                }
            };
            button(ElementId::Name(format!("rail-{id}").into()), *tab == Tab::Provider(id.clone()), ready)
                .child(div().size(px(24.)).flex().items_center().justify_center().child(icon))
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .when(ready, |el| {
                    el.on_click(cx.listener(move |this, _, _, cx| {
                        this.picker.tab = Some(Tab::Provider(id.clone()));
                        this.picker.highlighted = 0;
                        cx.notify();
                    }))
                })
        });
        v_flex()
            .id("picker-rail")
            .w(px(RAIL))
            .h_full()
            .flex_none()
            .p(px(4.))
            .gap(px(4.))
            .bg(t.foreground.opacity(0.009))
            .overflow_y_scroll()
            .child(favorites)
            .child(div().h(px(1.)).mx(px(4.)).bg(t.foreground.opacity(0.056)))
            .children(providers)
            .into_any_element()
    }

    fn render_picker_model_row(
        &self,
        row: &Row,
        index: usize,
        favorites_tab: bool,
        docked: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selection = row.selection.clone();
        let star_selection = row.selection.clone();
        let group: SharedString = format!("model-row-{}-{}", row.selection.instance_id, row.selection.model).into();
        let highlighted = index == self.picker.highlighted;
        let star_visible = row.favorite && !favorites_tab;
        h_flex()
            .id(ElementId::Name(group.clone()))
            .group(group.clone())
            .h(px(32.))
            .flex_none()
            .px(px(8.))
            .gap(px(8.))
            .rounded(px(4.))
            .text_sm()
            .cursor_pointer()
            .when(highlighted, |el| el.bg(t.foreground.opacity(0.09)))
            .hover(|s| s.bg(t.foreground.opacity(0.09)))
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                if this.picker.highlighted != index {
                    this.picker.highlighted = index;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| this.pick_row(selection.clone(), window, cx)))
            .child(
                div().size(px(16.)).flex_none().flex().items_center().justify_center().children(
                    row.icon.map(|(path, tint)| {
                        svg().path(path).size(px(16.)).text_color(tint.unwrap_or(t.muted_foreground))
                    }),
                ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.))
                    .child(div().min_w_0().truncate().text_color(t.foreground).child(row.name.clone()))
                    .when(row.selected, |el| {
                        el.child(Icon::new(Lucide::Check).with_size(px(14.)).text_color(t.muted_foreground))
                    })
                    .children(
                        row.sub_provider
                            .clone()
                            .map(|s| div().text_xs().text_color(t.muted_foreground.opacity(0.7)).child(s)),
                    )
                    .when(row.new, |el| {
                        el.child(
                            div()
                                .px(px(2.))
                                .py(px(1.))
                                .rounded(px(4.))
                                .border_1()
                                .border_color(t.primary.opacity(0.35))
                                .bg(t.primary.opacity(0.15))
                                .text_size(px(10.))
                                .line_height(px(10.))
                                .font_weight(FontWeight::BOLD)
                                .text_color(t.info_foreground)
                                .child("NEW"),
                        )
                    }),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(6.))
                    .child(
                        div()
                            .id(ElementId::Name(format!("{group}-star").into()))
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(8.))
                            .hover(|s| s.bg(t.accent))
                            .when(!star_visible && !highlighted, |el| {
                                el.invisible().group_hover(group.clone(), |s| s.visible())
                            })
                            .child(Icon::new(Lucide::Star).with_size(px(12.)).text_color(if row.favorite {
                                t.warning
                            } else {
                                t.muted_foreground
                            }))
                            .tooltip({
                                let label = if row.favorite { "Remove from favorites" } else { "Add to favorites" };
                                move |window, cx| Tooltip::new(label).build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_favorite(&star_selection, cx);
                            })),
                    )
                    .when(docked && index < 9, |el| el.child(kbd(&format!("Ctrl+{}", index + 1), t))),
            )
            .into_any_element()
    }

    fn render_effort_column(
        &self,
        current: Option<&ModelSelection>,
        model_name: &SharedString,
        descriptors: &[OptionDescriptor],
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rule = t.foreground.opacity(0.056);
        let effort = model_label::reasoning(descriptors).filter(|d| d.id != "fastMode");
        let others: Vec<&OptionDescriptor> = descriptors
            .iter()
            .filter(|d| {
                Some(d.id.as_str()) != effort.map(|e| e.id.as_str()) && d.id != "fastMode" && d.id != "serviceTier"
            })
            .collect();
        let heading = match (effort, others.is_empty()) {
            (Some(effort), _) => effort.label.clone(),
            (None, false) => "Options".into(),
            (None, true) => "Effort".into(),
        };
        let rows: Vec<AnyElement> = match (effort, current) {
            (Some(descriptor), Some(selection)) => {
                let value = model_label::current_value(descriptor, selection);
                let choices: Vec<(Value, String, bool)> = if descriptor.kind == "boolean" {
                    vec![(Value::Bool(true), "On".into(), false), (Value::Bool(false), "Off".into(), false)]
                } else {
                    descriptor
                        .options
                        .iter()
                        .filter(|o| !descriptor.prompt_injected_values.contains(&o.id))
                        .map(|o| (Value::String(o.id.clone()), o.label.clone(), o.is_default))
                        .collect()
                };
                choices
                    .into_iter()
                    .map(|(choice, label, default)| {
                        let selected = value.as_ref() == Some(&choice);
                        let id = descriptor.id.clone();
                        option_row(ElementId::Name(format!("effort-{id}-{label}").into()), t)
                            .child(div().flex_1().min_w_0().truncate().child(label))
                            .when(selected, |el| el.child(Icon::new(Lucide::Check).with_size(px(14.))))
                            .when(!selected && default, |el| {
                                el.child(div().text_xs().text_color(t.muted_foreground.opacity(0.7)).child("Default"))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| this.set_model_option(&id, choice.clone(), cx)))
                            .into_any_element()
                    })
                    .collect()
            }
            _ if others.is_empty() => {
                vec![
                    div()
                        .h(px(32.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .text_sm()
                        .text_color(t.muted_foreground)
                        .child("No effort levels")
                        .into_any_element(),
                ]
            }
            _ => Vec::new(),
        };
        let other_rows = current.map(|selection| {
            others
                .iter()
                .map(|descriptor| {
                    let value = model_label::current_value(descriptor, selection);
                    let id = descriptor.id.clone();
                    let caption = div().text_xs().text_color(t.muted_foreground).child(descriptor.label.clone());
                    if descriptor.kind == "boolean" {
                        let on = value == Some(Value::Bool(true));
                        v_flex()
                            .id(ElementId::Name(format!("option-{id}").into()))
                            .py(px(4.))
                            .px(px(8.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|s| s.bg(t.foreground.opacity(0.09)))
                            .child(caption)
                            .child(
                                h_flex()
                                    .text_sm()
                                    .child(div().flex_1().child(if on { "On" } else { "Off" }))
                                    .child(switch(on, t)),
                            )
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.set_model_option(&id, Value::Bool(!on), cx)),
                            )
                            .into_any_element()
                    } else {
                        let label = descriptor
                            .options
                            .iter()
                            .find(|o| value.as_ref().and_then(Value::as_str) == Some(o.id.as_str()))
                            .map(|o| o.label.clone())
                            .unwrap_or_default();
                        let options: Vec<(String, String, bool)> =
                            descriptor.options.iter().map(|o| (o.id.clone(), o.label.clone(), o.is_default)).collect();
                        let current = value.and_then(|v| v.as_str().map(str::to_owned));
                        let view = cx.entity();
                        Button::new(ElementId::Name(format!("option-{id}").into()))
                            .ghost()
                            .w_full()
                            .h_auto()
                            .py(px(4.))
                            .px(px(8.))
                            .rounded(px(4.))
                            .text_color(t.foreground)
                            .child(
                                v_flex().w_full().items_start().child(caption).child(
                                    h_flex()
                                        .w_full()
                                        .text_sm()
                                        .child(div().flex_1().min_w_0().truncate().child(label))
                                        .child(Icon::new(Lucide::ChevronRight).with_size(px(14.))),
                                ),
                            )
                            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopLeft, move |mut menu, window, _| {
                                for (option, label, default) in &options {
                                    let option = option.clone();
                                    let id = id.clone();
                                    let text = if *default { format!("{label}  Default") } else { label.clone() };
                                    menu = menu.item(
                                        PopupMenuItem::new(text)
                                            .checked(current.as_deref() == Some(option.as_str()))
                                            .on_click(window.listener_for(&view, move |this, _, _, cx| {
                                                this.set_model_option(&id, Value::String(option.clone()), cx)
                                            })),
                                    );
                                }
                                menu
                            })
                            .into_any_element()
                    }
                })
                .collect::<Vec<_>>()
        });
        v_flex()
            .w(px(EFFORT))
            .h_full()
            .flex_none()
            .border_l_1()
            .border_color(rule)
            .bg(t.foreground.opacity(0.012))
            .child(
                div().px(px(12.)).pt(px(10.)).child(
                    h_flex()
                        .h(px(33.))
                        .mt(px(-1.))
                        .pb(px(6.))
                        .gap(px(8.))
                        .border_b_1()
                        .border_color(rule)
                        .child(div().flex_1().text_sm().text_color(t.muted_foreground).child(heading))
                        .child(
                            div()
                                .max_w(px(96.))
                                .truncate()
                                .text_xs()
                                .text_color(t.muted_foreground)
                                .child(model_name.clone()),
                        ),
                ),
            )
            .child(
                v_flex()
                    .id("effort-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(8.))
                    .pt(px(10.))
                    .pb(px(6.))
                    .gap(px(2.))
                    .children(rows)
                    .when_some(other_rows.filter(|r| !r.is_empty()), |el, other_rows| {
                        el.child(
                            v_flex()
                                .mt(px(4.))
                                .pt(px(4.))
                                .border_t_1()
                                .border_color(rule)
                                .gap(px(2.))
                                .children(other_rows),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_picker_footer(
        &self,
        current: Option<&ModelSelection>,
        descriptors: &[OptionDescriptor],
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rule = t.foreground.opacity(0.056);
        let fast = descriptors.iter().find(|d| d.id == "fastMode" && d.kind == "boolean");
        let speed = fast.zip(current).map(|(descriptor, selection)| {
            let on = model_label::current_value(descriptor, selection) == Some(Value::Bool(true));
            let segment = |label: &'static str, checked: bool, value: bool| {
                h_flex()
                    .id(label)
                    .h(px(24.))
                    .px(px(8.))
                    .gap(px(4.))
                    .rounded(px(6.))
                    .text_xs()
                    .cursor_pointer()
                    .text_color(if checked { t.foreground } else { t.muted_foreground })
                    .when(checked, |el| el.bg(t.foreground.opacity(0.079)))
                    .when(label == "Fast", |el| el.child(Icon::new(Lucide::Zap).with_size(px(12.))))
                    .child(label)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.set_model_option("fastMode", Value::Bool(value), cx)),
                    )
            };
            h_flex()
                .p(px(2.))
                .rounded(px(8.))
                .bg(t.foreground.opacity(0.044))
                .child(segment("Standard", !on, false))
                .child(segment("Fast", on, true))
        });
        let mode = self.runtime_mode();
        let (_, label, icon) = runtime_modes().into_iter().find(|(id, ..)| *id == mode).unwrap_or(runtime_modes()[3]);
        h_flex()
            .h(px(45.))
            .px(px(12.))
            .py(px(8.))
            .gap(px(8.))
            .border_t_1()
            .border_color(rule)
            .bg(t.foreground.opacity(0.012))
            .children(speed)
            .child(div().flex_1())
            .child(self.render_permissions_control("picker-permissions", icon, label, true, t, cx))
            .into_any_element()
    }
}

fn option_row(id: ElementId, t: &Tokens) -> gpui_kit::Stateful<gpui_kit::Div> {
    h_flex()
        .id(id)
        .h(px(32.))
        .flex_none()
        .px(px(8.))
        .gap(px(8.))
        .rounded(px(4.))
        .text_sm()
        .text_color(t.foreground)
        .cursor_pointer()
        .hover(|s| s.bg(t.foreground.opacity(0.09)))
}

fn switch(on: bool, t: &Tokens) -> impl IntoElement {
    div()
        .w(px(26.))
        .h(px(16.))
        .p(px(2.))
        .rounded_full()
        .flex()
        .when(on, |el| el.justify_end().bg(t.primary))
        .when(!on, |el| el.bg(t.input))
        .child(div().size(px(12.)).rounded_full().bg(if on { t.primary_foreground } else { t.muted_foreground }))
}

pub(super) fn kbd(text: &str, t: &Tokens) -> impl IntoElement {
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
        .child(text.to_owned())
}

fn initials(name: &str) -> String {
    name.split_whitespace().filter_map(|w| w.chars().next()).take(2).collect::<String>().to_uppercase()
}
