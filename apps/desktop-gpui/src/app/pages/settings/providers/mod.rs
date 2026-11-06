//! Settings › Providers (`apps/web/src/components/settings/ProviderSettingsPanel.tsx`): the
//! provider instances listed beside the selected one's editor, then Advanced.
//!
//! The native client talks to one environment, which is the web's single-primary-device case, so
//! there are no device tabs. Advanced uses the shared settings rows; everything else here is
//! environment-wide.

mod accent;
mod accounts;
pub(in crate::app) use accounts::chatgpt_usage_button;
mod acp;
mod add;
mod codex_callback;
mod drivers;
mod instances;
mod models;
mod usage_sources;

use std::collections::{HashMap, HashSet};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState, NumberInput};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window,
    div, px,
};
use serde_json::{Value, json};

use super::system::background;
use super::widgets::{SettingRow, select_trigger};
use crate::app::Workspace;
use crate::settings::contract;
use crate::theme::{self, Tokens};

/// A driver's label and its settings that may name a secret reference, with their field labels.
pub(super) fn driver_secret_fields(kind: &str) -> Option<(&'static str, Vec<(&'static str, &'static str)>)> {
    let driver = drivers::driver(kind)?;
    Some((driver.label, driver.fields.iter().filter(|f| f.secret).map(|f| (f.key, f.label)).collect()))
}

/// What the page keeps between frames, like the panel's React state.
#[derive(Default)]
pub(super) struct ProvidersPage {
    selected: Option<String>,
    refreshing: bool,
    /// Instances whose `server.updateProvider` call is in flight.
    updating: HashSet<String>,
    updating_all: bool,
    inputs: HashMap<SharedString, PageInput>,
    models: models::ModelsUi,
    environment: instances::EnvironmentDraft,
    /// Managed Codex instances whose folded Runtime section is open.
    runtime_open: HashSet<String>,
}

/// A text field the page owns; `synced` is the saved value it last showed.
struct PageInput {
    state: Entity<InputState>,
    synced: SharedString,
    placeholder: SharedString,
    _subscription: Subscription,
}

type InputHandler = Box<dyn Fn(&mut Workspace, &Entity<InputState>, &InputEvent, &mut Window, &mut Context<Workspace>)>;

/// `IDLE_COMPACTION_MODES`.
const IDLE_COMPACTION_MODES: &[(&str, &str)] =
    &[("onReturn", "When you return"), ("beforeCacheExpires", "Before the cache expires (beta)"), ("off", "Never")];

/// `PROVIDER_HEALTH_INTERVAL_STEP_SECONDS`.
const HEALTH_STEP_SECONDS: i64 = 30;
const HEALTH_KEY: &str = "providerHealthRefreshInterval";
/// Its index in [`background::DURATION_KEYS`].
const HEALTH: usize = 1;

/// `globalIdleCompaction`: the mode, or the legacy switch from before modes.
fn idle_compaction<'a>(mode: &'a Value, legacy: &'a Value) -> &'a str {
    mode.as_str().unwrap_or(match legacy {
        Value::String(mode) => mode,
        Value::Bool(true) => "beforeCacheExpires",
        _ => "onReturn",
    })
}

/// The detail card's height: `h-[min(44rem,calc(100dvh-11rem))] min-h-[32rem]`.
fn card_height(window: &Window) -> f32 {
    (f32::from(window.viewport_size().height) - 176.).clamp(512., 704.)
}

impl Workspace {
    pub(super) fn render_providers_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        if self.settings_info().settings.is_null() {
            return div()
                .p_8()
                .text_sm()
                .text_color(t.muted_foreground)
                .child("Connect an environment to set up its providers.")
                .into_any_element();
        }
        let page = window.use_keyed_state("settings-providers", cx, |_, _| ProvidersPage::default());
        let view = self.settings_view();
        let override_projects =
            instances::override_projects(&view, self.settings_info().capability("projectProviderInstanceOverrides"));
        let rows = instances::instance_rows(&self.settings_info().settings, &self.settings_info().providers_json);
        let selected = {
            let chosen = page.read(cx).selected.clone();
            rows.iter().find(|row| Some(&row.id) == chosen.as_ref()).or(rows.first()).cloned()
        };
        let list = rows
            .iter()
            .map(|row| {
                self.render_instance_list_row(
                    &page,
                    row,
                    selected.as_ref() == Some(row),
                    &override_projects,
                    window,
                    cx,
                )
            })
            .collect::<Vec<_>>();
        let editor =
            selected.as_ref().map(|row| self.render_instance_editor(&page, row, &override_projects, window, cx));

        let add_row = div()
            .id("providers-add-row")
            .flex()
            .w_full()
            .items_center()
            .gap_3()
            .px_4()
            .py_3()
            .text_sm()
            .text_color(t.muted_foreground)
            .cursor_pointer()
            .hover(|s| s.bg(t.muted.opacity(0.25)).text_color(t.foreground))
            .child(Icon::new(gpui_kit::assets::IconName::Plus).size(px(16.)))
            .child("Add provider")
            .on_click(cx.listener({
                let page = page.clone();
                move |this, _, window, cx| this.open_add_provider_dialog(page.clone(), window, cx)
            }));

        let card = div()
            .flex()
            .h(px(card_height(window)))
            .w_full()
            .overflow_hidden()
            .rounded_xl()
            .border_1()
            .border_color(t.border.opacity(0.6))
            .bg(t.card.opacity(0.4))
            .child(
                div()
                    .id("providers-list")
                    .w(px(272.))
                    .flex_none()
                    .h_full()
                    .overflow_y_scroll()
                    .border_r_1()
                    .border_color(t.border.opacity(0.6))
                    .bg(t.muted.opacity(0.1))
                    .child(divided(list.into_iter().chain([add_row.into_any_element()]), &t)),
            )
            .child(
                div().id("providers-detail").flex_1().min_w_0().h_full().overflow_y_scroll().child(match editor {
                    Some(editor) => v_flex().w_full().gap_6().py_4().children(editor).into_any_element(),
                    None => div()
                        .p_6()
                        .text_sm()
                        .text_color(t.muted_foreground)
                        .child("No providers configured.")
                        .into_any_element(),
                }),
            );

        let providers = v_flex()
            .gap(px(10.))
            .child(
                h_flex()
                    .min_h(px(28.))
                    .px_4()
                    .justify_between()
                    .gap_4()
                    .child(
                        h_flex().gap_1().child(section_title("Providers", &t)).child(
                            ghost_icon_button("providers-add", gpui_kit::assets::IconName::Plus, &t)
                                .tooltip("Add provider")
                                .on_click(cx.listener({
                                    let page = page.clone();
                                    move |this, _, window, cx| this.open_add_provider_dialog(page.clone(), window, cx)
                                })),
                        ),
                    )
                    .child(self.render_providers_header_actions(&page, &t, cx)),
            )
            .child(card);
        let content = [
            providers.into_any_element(),
            self.render_usage_limit_sources(window, cx),
            self.render_providers_advanced(window, cx).into_any_element(),
        ];
        self.settings_page("providers", &view, content, cx)
    }

    /// "Update all" and the refresh button with when providers were last checked.
    fn render_providers_header_actions(
        &self,
        page: &Entity<ProvidersPage>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let state = page.read(cx);
        let refreshing = state.refreshing;
        let updating_all = state.updating_all;
        let candidates = instances::one_click_update_candidates(&self.settings_info().providers_json);
        let label: Option<SharedString> = if refreshing {
            Some("Refreshing providers".into())
        } else {
            instances::last_checked_label(&self.settings_info().providers_json, now_ms()).map(Into::into)
        };
        let refresh_page = page.clone();
        let update_page = page.clone();
        h_flex()
            .gap_2()
            .mr(px(-8.))
            .when(!candidates.is_empty() || updating_all, |el| {
                el.child(
                    ghost_text_button("providers-update-all", if updating_all { "Updating…" } else { "Update all" }, t)
                        .disabled(updating_all)
                        .tooltip("Update every provider with an update available")
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.update_all_providers(&update_page, window, cx)),
                        ),
                )
            })
            .child(
                ghost_text_button("providers-refresh", "", t)
                    .icon(Icon::new(gpui_kit::assets::IconName::RefreshCw).size(px(14.)))
                    .when_some(label, gpui_kit::component::button::Button::label)
                    .disabled(refreshing)
                    .tooltip("Refresh provider status")
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.refresh_providers(&refresh_page, window, cx)),
                    ),
            )
    }

    /// Providers › Advanced: the health check interval, a Background activity override
    /// (`backgroundActivityOverrideSettings`), then `CompactIdleThreadsRow`.
    fn render_providers_advanced(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let view = self.settings_view();

        let resolved = background::resolve_server(&self.settings_info().settings);
        let seconds = resolved.seconds(HEALTH);
        let reset_health = (seconds != background::preset(resolved.profile).seconds(HEALTH)).then(|| {
            let mut patch = background::override_patch(&self.settings_info().settings, HEALTH_KEY, Value::Null);
            if let Some(overrides) = patch["backgroundActivity"]["overrides"].as_object_mut() {
                overrides.remove(HEALTH_KEY);
            }
            patch
        });
        let input = self.number_field(
            "provider-health-check-interval",
            i64::try_from(seconds).unwrap_or(i64::MAX),
            (0, i64::MAX, HEALTH_STEP_SECONDS),
            window,
            cx,
            |this, seconds, window, cx| {
                let patch =
                    background::override_patch(&this.settings_info().settings, HEALTH_KEY, json!(seconds * 1000));
                this.update_settings(patch, window, cx);
            },
        );
        let health = SettingRow::new("provider-health-check-interval")
            .tip("This interval is configured here, then the shared Background activity policy decides whether provider probes may run when the timer fires. Custom intervals appear as Advanced in Maintenance settings.")
            .description("Refresh provider status, versions, and models in the background. Set to 0 to disable.")
            .reset(reset_health)
            .control(
                h_flex()
                    .gap_2()
                    .child(NumberInput::new(&input).small().w(px(128.)))
                    .child(div().text_xs().text_color(t.muted_foreground).child("seconds")),
            );

        let mode_of = |settings: &crate::settings::Settings| {
            idle_compaction(settings.get("idleCompaction"), settings.get("compactIdleThreads")).to_owned()
        };
        let modes: Vec<String> = view.targets.iter().map(|target| mode_of(&target.settings)).collect();
        let mixed = modes.iter().any(|mode| Some(mode) != modes.first());
        let current = idle_compaction(view.value("idleCompaction"), view.value("compactIdleThreads")).to_owned();
        let never_available = view.targets.iter().all(|target| {
            !target.settings.get("idleCompaction").is_null() || target.settings.get("compactIdleThreads").is_string()
        });
        let default_mode = idle_compaction(&Value::Null, contract().default_value("compactIdleThreads"));
        let label = if mixed {
            "Mixed"
        } else {
            IDLE_COMPACTION_MODES.iter().find(|(value, _)| *value == current).map_or(current.as_str(), |(_, l)| l)
        };
        let workspace = cx.entity();
        let select = select_trigger(&"compact-idle-threads".into(), label.to_owned(), 224., &t)
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, window, _| {
                for (value, label) in IDLE_COMPACTION_MODES {
                    menu = menu.item(
                        PopupMenuItem::new(*label)
                            .checked(!mixed && current == *value)
                            .disabled(*value == "off" && !never_available)
                            .on_click(window.listener_for(&workspace, move |this, _, window, cx| {
                                this.update_settings(json!({ "idleCompaction": value }), window, cx);
                            })),
                    );
                }
                menu
            });
        let compact = SettingRow::new("compact-idle-threads")
            .description("When to compact an idle Claude thread over 200,000 tokens: after you return, just before its prompt cache expires, or never. Each Claude provider can override this.")
            .setting("idleCompaction")
            .setting("compactIdleThreads")
            .reset((mode_of(&self.settings_info().settings) != default_mode).then(|| json!({ "idleCompaction": default_mode })))
            .control(select);

        let rows = [self.setting_row(&view, health, window, cx), self.setting_row(&view, compact, window, cx)];
        v_flex().gap(px(10.)).child(h_flex().min_h(px(28.)).px_4().child(section_title("Advanced", &t))).child(group(
            divided(rows, &t),
            true,
            &t,
        ))
    }

    /// `server.updateSettings`; the reply is the saved settings, applied at once.
    pub(super) fn save_settings(
        &mut self,
        payload: Value,
        failure: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.providers_request(
            "server.updateSettings",
            payload,
            failure,
            window,
            cx,
            |this, environment, reply, _, _| {
                this.apply_environment_event(
                    &environment,
                    &json!({"type": "settingsUpdated", "payload": {"settings": reply}}),
                );
            },
        );
    }

    /// Upserts one provider instance (`persistInstance`). Turning off the instance that writes
    /// commit messages and titles also resets that model, like the web.
    pub(super) fn persist_provider_instance(
        &mut self,
        instance_id: &str,
        next: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let settings = &self.settings_info().settings;
        let previous = instances::instance_config(settings, instance_id);
        let disabling = !instances::resolve_enabled(&next) && previous.as_ref().is_none_or(instances::resolve_enabled);
        let mut patch = json!({});
        if disabling
            && lookup(settings, "textGenerationModelSelection.instanceId").and_then(Value::as_str) == Some(instance_id)
        {
            patch["textGenerationModelSelection"] = json!({
                "instanceId": "codex",
                "model": "gpt-6-luna",
                "options": [{"id": "reasoningEffort", "value": "low"}],
            });
        }
        let payload = json!({
            "patch": patch,
            "providerInstanceMutation": {"operation": "upsert", "instanceId": instance_id, "instance": next},
        });
        self.save_settings(payload, "Could not update provider instance", window, cx);
    }

    /// `server.refreshProviders` with models and secrets reloaded, as the refresh button does.
    fn refresh_providers(&mut self, page: &Entity<ProvidersPage>, window: &mut Window, cx: &mut Context<Self>) {
        if page.read(cx).refreshing {
            return;
        }
        page.update(cx, |page, cx| {
            page.refreshing = true;
            cx.notify();
        });
        let page = page.clone();
        let payload = json!({"refreshModels": true, "reloadSecrets": true});
        self.providers_request_then(
            "server.refreshProviders",
            payload,
            None,
            window,
            cx,
            move |this, environment, result, _, cx| {
                if let Ok(reply) = result {
                    this.apply_environment_event(&environment, &json!({"type": "providerStatuses", "payload": reply}));
                }
                page.update(cx, |page, cx| {
                    page.refreshing = false;
                    cx.notify();
                });
            },
        );
    }

    /// `server.updateProvider` for one instance (`runProviderUpdate`).
    pub(super) fn run_provider_update(
        &mut self,
        page: &Entity<ProvidersPage>,
        driver: &str,
        instance_id: &str,
        target_version: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !page.update(cx, |page, _| page.updating.insert(instance_id.to_owned())) {
            return;
        }
        let mut payload = json!({"provider": driver, "instanceId": instance_id});
        if let Some(version) = target_version {
            payload["targetVersion"] = json!(version);
        }
        let label = drivers::driver(driver).map_or(driver.to_owned(), |d| d.label.to_owned());
        let (page, instance_id) = (page.clone(), instance_id.to_owned());
        self.providers_request_then(
            "server.updateProvider",
            payload,
            None,
            window,
            cx,
            move |this, environment, result, window, cx| {
                match result {
                    Ok(reply) => {
                        this.apply_environment_event(
                            &environment,
                            &json!({"type": "providerStatuses", "payload": reply}),
                        );
                    }
                    Err(error) => window.push_notification(
                        Notification::error(error).title(SharedString::from(format!("Could not update {label}"))),
                        cx,
                    ),
                }
                page.update(cx, |page, cx| {
                    page.updating.remove(&instance_id);
                    cx.notify();
                });
            },
        );
    }

    /// "Update all": every one-click update candidate, then one summary notification.
    fn update_all_providers(&mut self, page: &Entity<ProvidersPage>, window: &mut Window, cx: &mut Context<Self>) {
        let candidates = instances::one_click_update_candidates(&self.settings_info().providers_json);
        let Some(client) = self.settings_client() else { return };
        if candidates.is_empty() || page.read(cx).updating_all {
            return;
        }
        page.update(cx, |page, cx| {
            page.updating_all = true;
            cx.notify();
        });
        let page = page.clone();
        cx.spawn_in(window, async move |this, cx| {
            let mut failures = Vec::new();
            for (driver, instance_id) in candidates {
                let payload = json!({"provider": driver, "instanceId": instance_id});
                if let Err(error) = client.request("server.updateProvider", payload).await {
                    failures
                        .push(format!("{}: {error}", drivers::driver(&driver).map_or(driver.as_str(), |d| d.label)));
                }
            }
            let _ = this.update_in(cx, |_, window, cx| {
                if !failures.is_empty() {
                    window.push_notification(
                        Notification::error(failures.join("\n")).title("Some providers could not update"),
                        cx,
                    );
                }
                page.update(cx, |page, cx| {
                    page.updating_all = false;
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// A request whose failure becomes an error notification titled `failure`.
    fn providers_request(
        &mut self,
        method: &'static str,
        payload: Value,
        failure: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_success: impl FnOnce(&mut Self, SharedString, Value, &mut Window, &mut Context<Self>) + 'static,
    ) {
        self.providers_request_then(
            method,
            payload,
            Some(failure),
            window,
            cx,
            move |this, environment, result, window, cx| {
                if let Ok(reply) = result {
                    on_success(this, environment, reply, window, cx);
                }
            },
        );
    }

    /// A request to the settings environment; `done` gets that environment, so a reply lands
    /// there even when Settings has moved on.
    fn providers_request_then(
        &mut self,
        method: &'static str,
        payload: Value,
        failure: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, SharedString, Result<Value, String>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some(environment) = self.settings_environment() else { return };
        self.providers_request_on(environment, method, payload, failure, window, cx, done);
    }

    fn providers_request_on(
        &mut self,
        environment: SharedString,
        method: &'static str,
        payload: Value,
        failure: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, SharedString, Result<Value, String>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some(client) = self.env_client(&environment) else { return };
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request(method, payload).await.map_err(|error| error.to_string());
            let _ = this.update_in(cx, |this, window, cx| {
                if let (Err(error), Some(title)) = (&result, failure) {
                    window.push_notification(Notification::error(error.clone()).title(title), cx);
                }
                done(this, environment, result, window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Applies a server event to the environment a request went to, if it is still here.
    fn apply_environment_event(&mut self, environment: &str, event: &Value) {
        let Some(info) = self.env_info_mut(environment) else { return };
        if let Err(error) = info.apply(event) {
            log::warn!("settings reply did not decode: {error}");
        }
    }

    /// A text field that commits like the web's `DraftInput`: on blur or Enter, when it differs from
    /// the saved `value`, which it follows while unfocused.
    pub(super) fn draft_input(
        &mut self,
        page: &Entity<ProvidersPage>,
        key: impl Into<SharedString>,
        value: &str,
        placeholder: &str,
        masked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
        commit: impl Fn(&mut Workspace, String, &mut Window, &mut Context<Workspace>) + 'static,
    ) -> Entity<InputState> {
        let key = key.into();
        let input = self.page_input(
            page,
            key.clone(),
            value,
            placeholder,
            masked,
            window,
            cx,
            Box::new({
                let (page, key) = (page.clone(), key.clone());
                move |this, input, event, window, cx| {
                    if !matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        return;
                    }
                    let text = input.read(cx).value();
                    let synced = page.read(cx).inputs.get(&key).map(|i| i.synced.clone()).unwrap_or_default();
                    if text != synced {
                        commit(this, text.to_string(), window, cx);
                    }
                }
            }),
        );
        let focused = input.read(cx).focus_handle(cx).is_focused(window);
        let synced = page.read(cx).inputs.get(&key).map(|i| i.synced.clone()).unwrap_or_default();
        if !focused && synced.as_ref() != value {
            let value = SharedString::from(value.to_owned());
            input.update(cx, |input, cx| input.set_value(value.clone(), window, cx));
            page.update(cx, |page, _| {
                if let Some(entry) = page.inputs.get_mut(&key) {
                    entry.synced = value;
                }
            });
        }
        input
    }

    /// Gets or creates the page's input under `key`; `on_event` sees all of its events.
    pub(super) fn page_input(
        &mut self,
        page: &Entity<ProvidersPage>,
        key: SharedString,
        value: &str,
        placeholder: &str,
        masked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_event: InputHandler,
    ) -> Entity<InputState> {
        if let Some(entry) = page.read(cx).inputs.get(&key) {
            let state = entry.state.clone();
            if entry.placeholder.as_ref() != placeholder {
                let placeholder = SharedString::from(placeholder.to_owned());
                state.update(cx, |input, cx| input.set_placeholder(placeholder.clone(), window, cx));
                page.update(cx, |page, _| {
                    if let Some(entry) = page.inputs.get_mut(&key) {
                        entry.placeholder = placeholder;
                    }
                });
            }
            return state;
        }
        let state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder.to_owned())
                .masked(masked)
                .default_value(value.to_owned())
        });
        let subscription = cx.subscribe_in(&state, window, move |this, input, event, window, cx| {
            on_event(this, input, event, window, cx);
        });
        page.update(cx, |page, _| {
            page.inputs.insert(
                key,
                PageInput {
                    state: state.clone(),
                    synced: value.to_owned().into(),
                    placeholder: placeholder.to_owned().into(),
                    _subscription: subscription,
                },
            )
        });
        state
    }
}

impl ProvidersPage {
    /// Drops inputs whose keys start with `prefix`, e.g. a closed editor's fields.
    fn forget_inputs(&mut self, prefix: &str) {
        self.inputs.retain(|key, _| !key.starts_with(prefix));
    }

    fn input_value(&self, key: &str, cx: &App) -> Option<String> {
        self.inputs.get(key).map(|input| input.state.read(cx).value().to_string())
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// `a.b.c` in a JSON value.
fn lookup<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(value, |value, key| value.get(key))
}

// Stand-ins for the settings layout (`settingsLayout.tsx`) until the shared settings widgets land.

/// `SettingsSection`'s title: `text-base font-semibold`.
fn section_title(title: impl Into<SharedString>, t: &Tokens) -> impl IntoElement {
    div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(t.foreground).child(title.into())
}

/// A titled section: header row, then its rows in a group. `boxed` draws the card; the provider
/// editor's sections are flat (`[data-provider-detail] [data-slot=settings-group]`).
fn section(
    title: impl Into<SharedString>,
    icon: Option<AnyElement>,
    action: Option<AnyElement>,
    rows: impl IntoIterator<Item = AnyElement>,
    boxed: bool,
    t: &Tokens,
) -> impl IntoElement {
    v_flex()
        .w_full()
        .gap(px(10.))
        .child(
            h_flex()
                .min_h(px(28.))
                .px_4()
                .justify_between()
                .gap_4()
                .child(h_flex().gap_2().children(icon).child(section_title(title, t)))
                .children(action),
        )
        .child(group(divided(rows, t), boxed, t))
}

fn group(content: impl IntoElement, boxed: bool, t: &Tokens) -> impl IntoElement {
    div()
        .when(boxed, |el| el.rounded_xl().border_1().border_color(t.border.opacity(0.6)).bg(t.card.opacity(0.4)))
        .child(content)
}

/// Children separated by `border-border/50` rules.
fn divided(children: impl IntoIterator<Item = AnyElement>, t: &Tokens) -> impl IntoElement {
    v_flex().w_full().children(children.into_iter().enumerate().map(|(index, child)| {
        div().w_full().when(index > 0, |el| el.border_t_1().border_color(t.border.opacity(0.5))).child(child)
    }))
}

/// `SettingsRow`: title and description on the left, the control on the right.
fn settings_row(
    title: impl Into<SharedString>,
    description: Option<&str>,
    control: Option<AnyElement>,
    t: &Tokens,
) -> gpui_kit::Div {
    h_flex()
        .px_4()
        .py_3()
        .gap_8()
        .justify_between()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .min_h(px(20.))
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.foreground)
                        .child(title.into()),
                )
                .when_some(description, |el, description| {
                    el.child(div().text_xs().text_color(t.muted_foreground).child(description.to_owned()))
                }),
        )
        .when_some(control, |el, control| el.child(h_flex().flex_none().gap_2().justify_end().child(control)))
}

/// `size="xs" variant="ghost-muted"` text button.
fn ghost_text_button(id: impl Into<gpui_kit::ElementId>, label: impl Into<SharedString>, t: &Tokens) -> Button {
    let label = label.into();
    Button::new(id)
        .ghost()
        .xsmall()
        .h(px(24.))
        .px(px(7.))
        .gap_1()
        .text_xs()
        .text_color(t.muted_foreground)
        .when(!label.is_empty(), |button| button.label(label))
}

/// `size="icon-micro"`/`icon-xs` `variant="ghost-muted"` icon button.
fn ghost_icon_button(id: impl Into<gpui_kit::ElementId>, icon: impl Into<Icon>, t: &Tokens) -> Button {
    Button::new(id).ghost().xsmall().size(px(24.)).text_color(t.muted_foreground).icon(Icon::new(icon).size(px(14.)))
}

/// `variant="outline" size="sm"` button.
fn outline_button(id: impl Into<gpui_kit::ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id).outline().small().h(px(28.)).label(label.into())
}

/// The web's `Select` with `size="sm"`: a trigger showing the chosen label and a menu of options.
fn select_button(
    id: SharedString,
    options: Vec<(SharedString, SharedString)>,
    current: &str,
    width: f32,
    t: &Tokens,
    on_select: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = options
        .iter()
        .find(|(value, _)| value.as_ref() == current)
        .map_or(current.to_owned().into(), |(_, l)| l.clone());
    let current = SharedString::from(current.to_owned());
    let on_select = std::rc::Rc::new(on_select);
    Button::new(id)
        .outline()
        .small()
        .h(px(28.))
        .w(px(width))
        .justify_between()
        .child(div().flex_1().truncate().text_left().child(label))
        .child(Icon::new(gpui_kit::assets::IconName::ChevronDown).size(px(14.)).text_color(t.muted_foreground))
        .dropdown_menu(move |mut menu, _, _| {
            for (value, label) in &options {
                let (on_select, value) = (on_select.clone(), value.clone());
                menu = menu.item(
                    PopupMenuItem::new(label.clone())
                        .checked(value == current)
                        .on_click(move |_, window, cx| on_select(&value, window, cx)),
                );
            }
            menu
        })
}

/// Static `(value, label)` pairs as select options.
fn options(pairs: &[(&'static str, &'static str)]) -> Vec<(SharedString, SharedString)> {
    pairs.iter().map(|(value, label)| ((*value).into(), (*label).into())).collect()
}

/// `Switch` at the web's default size (16px thumb on desktop).
fn switch(id: impl Into<gpui_kit::ElementId>, checked: bool) -> Switch {
    Switch::new(id).checked(checked)
}

fn text_input(state: &Entity<InputState>, width: Option<f32>) -> Input {
    let input = Input::new(state).small();
    match width {
        Some(width) => input.w(px(width)),
        None => input,
    }
}

fn dot(color: Hsla) -> impl IntoElement {
    div().size(px(6.)).flex_none().rounded_full().bg(color)
}
