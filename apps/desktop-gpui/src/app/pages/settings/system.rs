//! Settings sections in the system group (Maintenance, Labs, About, Archive); see `SETTINGS` in `mod.rs`.
//! Ports `MaintenanceSettingsPanel` and `AboutSettingsPanel` (`SettingsPanels.tsx`), `LabsSettingsPanel`,
//! `BackgroundActivitySettings`, `StorageCleanupSections`, `DatabaseStorageNotice`, `AboutVersionTitle`,
//! `MobileAppSettingsRow` and `ArchivedThreadsPanel`.
//!
//! Plain setting rows are declared in `MAINTENANCE_ROWS` and `LABS_ROWS` and drawn by the placeholder
//! [`Workspace::render_setting_row`] with global patches, until the shared scoped widgets replace it.
//! Scoped in the web (`serverScoped`, per environment): enableProviderUpdateChecks,
//! continueThreadsAfterServerUpdate (also per project), backgroundActivity, storageCleanup.*,
//! guestSharingEnabled, linkedEnvironmentsEnabled, simlockDeviceHostsEnabled. Client settings, kept by
//! the app and not the server: boringSidebarEnabled, boringSidebarMultilineRows, planModeEnabled,
//! contextWindowMeterEnabled; their rows wait for the client-settings store.

mod archive;
mod background;
mod cleanup;
mod layout;

use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::Dialog;
use gpui_kit::component::input::{InputEvent, InputState, NumberInput, NumberInputEvent, StepAction};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, ClipboardItem, Context, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    WeakEntity, Window, div, prelude::FluentBuilder as _, px,
};
use serde_json::{Value, json};

use self::background::Profile;
use self::layout::{Part, row, section};
use crate::app::Workspace;
use crate::theme::{self, FONT_MONO, Tokens};

/// A plain setting row, as `searchableSetting(id)` plus the row's props in the web.
struct SettingRowSpec {
    id: &'static str,
    label: &'static str,
    description: &'static [Part],
    /// The key in the settings JSON.
    path: &'static str,
    control: Control,
    /// `"client"` for client settings; otherwise the scopes the web can set it at.
    scopes: &'static [&'static str],
    /// The default a reset button restores, for rows that show one (`SettingResetButton`).
    reset_to: Option<bool>,
}

enum Control {
    Switch,
}

const SERVER: &[&str] = &["global", "environment"];
const CLIENT: &[&str] = &["client"];

const MAINTENANCE_ROWS: &[SettingRowSpec] = &[
    SettingRowSpec {
        id: "provider-update-checks",
        label: "Provider update checks",
        description: &[Part::Text(
            "Check installed provider CLIs for newer available versions and fetch model catalog updates. When off, new models may not appear.",
        )],
        path: "enableProviderUpdateChecks",
        control: Control::Switch,
        scopes: SERVER,
        reset_to: Some(true),
    },
    SettingRowSpec {
        id: "continue-threads-after-server-update",
        label: "Continue threads after restarts",
        description: &[Part::Text(
            "Automatically resume interrupted threads after an update, crash, or machine restart on the selected environments.",
        )],
        path: "continueThreadsAfterServerUpdate",
        control: Control::Switch,
        scopes: &["global", "environment", "project"],
        reset_to: Some(true),
    },
];

const LABS_ROWS: &[SettingRowSpec] = &[
    // The web also shows this on while the removed legacy sidebar's opt-in
    // (`legacySidebarEnabled`) is set, and its toggle clears that opt-in.
    SettingRowSpec {
        id: "boring-sidebar",
        label: "Boring sidebar (group by project)",
        description: &[Part::Text(
            "Group sidebar threads into project folders. Pinned threads stay on top, and Snoozed and Settled stay below the folders.",
        )],
        path: "boringSidebarEnabled",
        control: Control::Switch,
        scopes: CLIENT,
        reset_to: None,
    },
    SettingRowSpec {
        id: "boring-sidebar-multiline",
        label: "Two-line folder rows",
        description: &[Part::Text(
            "In the boring sidebar, show folder threads on two lines: the title, then the branch, pull request, and agent.",
        )],
        path: "boringSidebarMultilineRows",
        control: Control::Switch,
        scopes: CLIENT,
        reset_to: None,
    },
    // The web hides the switch behind "Sign in" while signed out of T3 Connect; this client has no
    // T3 Connect sign-in, which the web treats as a build without it (`GuestLinksRow`).
    SettingRowSpec {
        id: "guest-links",
        label: "Guest links",
        description: &[
            Part::Text(
                "Share a thread with a guest who watches it live in their browser and sends messages you approve. Needs T3 Connect: sign in and link this machine in ",
            ),
            Part::Section("Sharing & access", "access"),
            Part::Text(". While it is off, every guest link stops working."),
        ],
        path: "guestSharingEnabled",
        control: Control::Switch,
        scopes: SERVER,
        reset_to: None,
    },
    SettingRowSpec {
        id: "linked-environments-beta",
        label: "Linked environments",
        description: &[
            Part::Text(
                "Let agents here work on your other machines, and theirs here. Turn it on for each machine, then link them in ",
            ),
            Part::Section("Connections", "connections"),
            Part::Text(". While it is off, nothing new starts over a link; tasks already running finish."),
        ],
        path: "linkedEnvironmentsEnabled",
        control: Control::Switch,
        scopes: SERVER,
        reset_to: None,
    },
    SettingRowSpec {
        id: "simlock-hosts",
        label: "Simlock device hosts (alpha)",
        description: &[
            Part::Text("Let the Device panel lease simulators and emulators from your own "),
            Part::Url("Simlock", "https://incubator.callstack.com/simlock/"),
            Part::Text(" pool, added in "),
            Part::Section("Devices", "devices"),
            Part::Text(". An alpha that may change or break. T3 Code never installs Simlock."),
        ],
        path: "simlockDeviceHostsEnabled",
        control: Control::Switch,
        scopes: SERVER,
        reset_to: None,
    },
];

const LEGACY_ROWS: &[SettingRowSpec] = &[
    SettingRowSpec {
        id: "legacy-plan-mode",
        label: "Plan mode (legacy)",
        description: &[Part::Text("Restore Build/Plan, /plan, /default, and Shift+Tab. Off uses build mode.")],
        path: "planModeEnabled",
        control: Control::Switch,
        scopes: CLIENT,
        reset_to: None,
    },
    SettingRowSpec {
        id: "legacy-context-window-indicator",
        label: "Context window indicator (legacy)",
        description: &[Part::Text("Shows context window usage as a circular indicator in the composer.")],
        path: "contextWindowMeterEnabled",
        control: Control::Switch,
        scopes: CLIENT,
        reset_to: None,
    },
];

const PRIVACY_POLICY_URL: &str = "https://t3.codes/privacy-policy";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

const MOBILE_STORES: [(&str, &str, &str, &str); 2] = [
    (
        "iPhone and iPad",
        "App Store",
        "https://apps.apple.com/us/app/t3-code-remote-claude-more/id6787819824",
        "icons/app/apple.svg",
    ),
    (
        "Android",
        "Google Play",
        "https://play.google.com/store/apps/details?id=com.t3tools.t3code",
        "icons/app/android.svg",
    ),
];

impl Workspace {
    pub(super) fn render_settings_maintenance(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let updates = MAINTENANCE_ROWS.iter().map(|spec| self.render_setting_row(spec, &t, cx)).collect();
        v_flex()
            .gap_8()
            .child(section("Updates", None, updates, true, &t))
            .child(section("Background activity", None, vec![self.render_background_row(cx)], true, &t))
            .children(self.render_storage_sections(window, cx))
            .into_any_element()
    }

    pub(super) fn render_settings_labs(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let beta = LABS_ROWS.iter().map(|spec| self.render_setting_row(spec, &t, cx)).collect();
        let legacy = LEGACY_ROWS.iter().map(|spec| self.render_setting_row(spec, &t, cx)).collect();
        v_flex()
            .gap_8()
            .child(section("Beta", None, beta, true, &t))
            .child(section("Legacy", None, legacy, true, &t))
            .into_any_element()
    }

    pub(super) fn render_settings_about(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let policy = &self.server_info.config["managedPolicy"];
        let organization = policy["organization"].as_str().map(str::to_owned);
        let mut app = Vec::new();
        if let Some(organization) = &organization {
            let help = policy["supportUrl"].as_str().map(|url| {
                let url = url.to_owned();
                Button::new("about-org-help")
                    .outline()
                    .small()
                    .label("Get help")
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .into_any_element()
            });
            let description =
                format!("This environment is managed by {organization}. Some settings and features follow its policy.");
            app.push(row("Organization", Some(text(description)), None, help, &t).into_any_element());
        }
        app.push(
            row(version_title(&t), Some(text("Current version of the application.")), None, None, &t)
                .into_any_element(),
        );
        app.push(
            row(
                "Mobile app",
                Some(text("Connect to your server from your phone or tablet.")),
                None,
                Some(mobile_buttons(&t)),
                &t,
            )
            .into_any_element(),
        );
        let privacy = match organization.filter(|_| policy.pointer("/limits/telemetry") == Some(&Value::Bool(false))) {
            Some(organization) => format!("How we handle your data. {organization} turned off anonymous usage data."),
            None => "How we handle your data, including the anonymous usage data T3 Code collects.".to_owned(),
        };
        let view_policy = Button::new("about-privacy")
            .outline()
            .small()
            .label("View policy")
            .on_click(|_, _, cx| cx.open_url(PRIVACY_POLICY_URL));
        let legal = vec![
            row("Privacy policy", Some(text(privacy)), None, Some(view_policy.into_any_element()), &t)
                .into_any_element(),
        ];
        v_flex()
            .gap_8()
            .child(section("App", None, app, true, &t))
            .child(section("Legal", None, legal, true, &t))
            .into_any_element()
    }

    pub(super) fn render_settings_archived(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_archive(window, cx)
    }

    fn settings_json(&self) -> &Value {
        &self.server_info.config["settings"]
    }

    /// `server.updateSettings` with a global patch; the reply is the whole new settings.
    fn update_server_settings(&mut self, patch: Value, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("server.updateSettings", json!({ "patch": patch })).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(settings) => {
                        if let Err(error) = this.server_info.set_settings(&settings) {
                            log::warn!("bad settings reply: {error}");
                        }
                    }
                    Err(error) => notify_error(window, cx, "Could not save settings", error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The placeholder renderer for a [`SettingRowSpec`].
    fn render_setting_row(&self, spec: &'static SettingRowSpec, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let client_setting = spec.scopes == CLIENT;
        let connected = self.client().is_some();
        let value = self.settings_json()[spec.path].as_bool().unwrap_or(false);
        let view = cx.entity();
        let reset = spec.reset_to.filter(|default| connected && *default != value).map(|default| {
            let view = view.clone();
            Button::new(SharedString::from(format!("reset-{}", spec.id)))
                .ghost()
                .xsmall()
                .icon(Icon::new(IconName::Undo2))
                .tooltip("Reset to default")
                .on_click(move |_, window, cx| {
                    view.update(cx, |this, cx| this.update_server_settings(json!({ spec.path: default }), window, cx))
                })
        });
        let title = h_flex().gap_1p5().child(spec.label).children(reset);
        let control = match spec.control {
            Control::Switch => Switch::new(SharedString::from(format!("switch-{}", spec.id)))
                .checked(value)
                .accessibility_label(spec.label)
                // Client settings are not stored by this app yet.
                .disabled(client_setting || !connected)
                .on_click(move |checked, window, cx| {
                    let checked = *checked;
                    view.update(cx, |this, cx| this.update_server_settings(json!({ spec.path: checked }), window, cx))
                })
                .into_any_element(),
        };
        let status = (spec.path == "continueThreadsAfterServerUpdate" && !connected)
            .then(|| "All selected connected environments must support restart continuation.".into());
        let description = self.description(spec.id, spec.description, cx);
        row(title, Some(description), status, Some(control), t).into_any_element()
    }

    /// A row description whose section links open that settings section.
    fn description(&self, id: &'static str, parts: &'static [Part], cx: &mut Context<Self>) -> AnyElement {
        let view = cx.weak_entity();
        layout::rich_text(SharedString::from(format!("description-{id}")), parts, move |section, _, cx| {
            let _ = view.update(cx, |this, cx| this.open_settings(section, cx));
        })
    }

    /// `BackgroundActivitySettingsRow`; this client is one environment, so Advanced is always offered.
    fn render_background_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let settings = self.settings_json();
        let resolved = background::resolve_server(settings);
        let option = background::profile_option(settings);
        let description = match option {
            Some(profile) => profile.description().to_owned(),
            None => format!("Uses custom intervals. Shared policy: {}.", resolved.profile.label()),
        };
        let view = cx.entity();
        let reset = background::can_reset(settings).then(|| {
            let view = view.clone();
            Button::new("reset-background-activity")
                .ghost()
                .xsmall()
                .icon(Icon::new(IconName::Undo2))
                .tooltip("Reset to default")
                .on_click(move |_, window, cx| {
                    view.update(cx, |this, cx| {
                        let patch = json!({ "backgroundActivity": background::default_background() });
                        this.update_server_settings(patch, window, cx)
                    })
                })
        });
        let info = div()
            .id("background-activity-policy")
            .child(Icon::new(IconName::Info).size(px(14.)).text_color(t.muted_foreground))
            .tooltip(|window, cx| {
                Tooltip::new(
                    "This shared policy gates background work such as Git refreshes and provider health probes after their individual intervals elapse.",
                )
                .build(window, cx)
            });
        let title = h_flex().gap_1p5().child("Background activity").child(info).children(reset);
        let label = option.map_or("Advanced", Profile::label);
        let picker_view = view.clone();
        let picker = Button::new("background-activity-profile")
            .outline()
            .small()
            .w(px(160.))
            .label(label)
            .dropdown_caret(true)
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, window, _| {
                for profile in Profile::ALL {
                    menu = menu.item(PopupMenuItem::new(profile.label()).checked(option == Some(profile)).on_click(
                        window.listener_for(&picker_view, move |this, _, window, cx| {
                            this.update_server_settings(background::profile_patch(profile), window, cx)
                        }),
                    ));
                }
                menu.item(PopupMenuItem::new("Advanced").checked(option.is_none()).on_click(
                    window.listener_for(&picker_view, |this, _, window, cx| this.open_background_dialog(window, cx)),
                ))
            });
        let configure = option.is_none().then(|| {
            Button::new("background-activity-configure")
                .outline()
                .small()
                .icon(Icon::new(IconName::Settings))
                .tooltip("Configure background activity")
                .on_click(move |_, window, cx| view.update(cx, |this, cx| this.open_background_dialog(window, cx)))
        });
        row(
            title,
            Some(text(description)),
            None,
            Some(h_flex().gap_2().child(picker).children(configure).into_any_element()),
            &t,
        )
        .into_any_element()
    }

    /// `BackgroundActivityAdvancedDialog`.
    fn open_background_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.client().is_none() {
            return;
        }
        let workspace = cx.weak_entity();
        let fields = cx.new(|cx| BackgroundFields::new(workspace.clone(), window, cx));
        let view = cx.entity();
        window.open_dialog(cx, move |dialog: Dialog, window, cx| {
            let t = *theme::tokens(cx);
            let settings = view.read(cx).settings_json().clone();
            let resolved = background::resolve_server(&settings);
            fields.update(cx, |fields, cx| fields.sync(&resolved, window, cx));
            let fields = fields.read(cx);
            let entry = |title: &'static str, description: &'static str, control: AnyElement, top: bool| {
                h_flex()
                    .px_4()
                    .py_3()
                    .gap_3()
                    .justify_between()
                    .when(top, |el| el.border_t_1().border_color(t.border))
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
                            .child(div().text_xs().text_color(t.muted_foreground).child(description)),
                    )
                    .child(control)
            };
            let seconds = |input: &Entity<InputState>| {
                h_flex()
                    .flex_none()
                    .gap_2()
                    .child(NumberInput::new(input).small().w(px(128.)))
                    .child(div().text_xs().text_color(t.muted_foreground).child("seconds"))
                    .into_any_element()
            };
            let policy_view = view.clone();
            let policy = Button::new("background-shared-policy")
                .outline()
                .small()
                .w(px(160.))
                .label(resolved.profile.label())
                .dropdown_caret(true)
                .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, window, _| {
                    for profile in Profile::ALL {
                        let settings = settings.clone();
                        menu = menu.item(
                            PopupMenuItem::new(profile.label()).checked(resolved.profile == profile).on_click(
                                window.listener_for(&policy_view, move |this, _, window, cx| {
                                    let patch = background::shared_policy_patch(&settings, profile);
                                    this.update_server_settings(patch, window, cx)
                                }),
                            ),
                        );
                    }
                    menu
                });
            let pauses = background::PAUSE_KEYS.iter().enumerate().map(|(i, key)| {
                let label = ["Pause when host is locked", "Pause on host low power", "Pause on client low power", "Pause on battery"][i];
                let view = view.clone();
                h_flex()
                    .w_1_2()
                    .px_4()
                    .py_3()
                    .gap_3()
                    .justify_between()
                    .border_t_1()
                    .border_color(t.border)
                    .when(i % 2 == 0, |el| el.border_r_1())
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
                    .child(Switch::new(SharedString::from(format!("background-{key}"))).checked(resolved.pauses[i]).on_click(
                        move |checked, window, cx| {
                            let checked = *checked;
                            view.update(cx, |this, cx| {
                                let patch = background::override_patch(this.settings_json(), key, json!(checked));
                                this.update_server_settings(patch, window, cx)
                            })
                        },
                    ))
            });
            let reset_view = view.clone();
            dialog
                .w(px(576.))
                .title("Background Activity")
                .child(
                    div()
                        .text_sm()
                        .text_color(t.muted_foreground)
                        .child("Tune the shared power policy and the background intervals that feed it."),
                )
                .child(
                    v_flex()
                        .mt_4()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(t.border)
                        .bg(t.card)
                        .overflow_hidden()
                        .child(entry(
                            "Shared policy",
                            "Controls whether background work may run after a subscribed interval fires.",
                            policy.into_any_element(),
                            false,
                        ))
                        .child(entry(
                            "Git fetch interval",
                            "Refresh remote branch status in the background.",
                            seconds(&fields.inputs[0].input),
                            true,
                        ))
                        .child(entry(
                            "Provider health interval",
                            "Refresh provider availability, versions, and auth state. Model catalog updates follow Provider update checks.",
                            seconds(&fields.inputs[1].input),
                            true,
                        ))
                        .child(entry(
                            "Host power monitor",
                            "Poll host power state while clients are active.",
                            seconds(&fields.inputs[2].input),
                            true,
                        ))
                        .child(entry(
                            "Idle host monitor",
                            "Poll host power state when no foreground client is active.",
                            seconds(&fields.inputs[3].input),
                            true,
                        ))
                        .child(h_flex().flex_wrap().children(pauses)),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .justify_end()
                        .child(Button::new("background-reset-all").outline().label("Reset all").on_click(
                            move |_, window, cx| {
                                reset_view.update(cx, |this, cx| {
                                    let patch = json!({ "backgroundActivity": background::default_background() });
                                    this.update_server_settings(patch, window, cx)
                                })
                            },
                        ))
                        .child(
                            Button::new("background-done")
                                .primary()
                                .label("Done")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        ),
                )
        });
    }
}

/// The four interval fields of the background activity dialog.
struct BackgroundFields {
    inputs: Vec<NumberField>,
}

impl BackgroundFields {
    fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // (min, step) per `background::DURATION_KEYS`, as the web's `IntervalSecondsField`s.
        let fields = [(0., 5.), (0., 30.), (5., 5.), (5., 30.)];
        let inputs = background::DURATION_KEYS
            .iter()
            .zip(fields)
            .map(|(key, (min, step))| {
                let workspace = workspace.clone();
                NumberField::new(step, min, f64::MAX, window, cx, move |_, seconds, window, cx| {
                    let _ = workspace.update(cx, |this, cx| {
                        let patch = background::override_patch(this.settings_json(), key, json!(seconds as u64 * 1000));
                        this.update_server_settings(patch, window, cx)
                    });
                })
            })
            .collect();
        Self { inputs }
    }

    fn sync(&mut self, resolved: &background::Resolved, window: &mut Window, cx: &mut App) {
        for (i, field) in self.inputs.iter_mut().enumerate() {
            field.sync(resolved.seconds(i) as f64, window, cx);
        }
    }
}

/// A number field that commits on Enter, on blur and on its step buttons, like base-ui's
/// `NumberField` `onValueCommitted`. It follows the saved value whenever that changes.
pub(super) struct NumberField {
    pub(super) input: Entity<InputState>,
    saved: Option<f64>,
    _subscriptions: [Subscription; 2],
}

impl NumberField {
    pub(super) fn new<S: 'static>(
        step: f64,
        min: f64,
        max: f64,
        window: &mut Window,
        cx: &mut Context<S>,
        commit: impl Fn(&mut S, f64, &mut Window, &mut Context<S>) + 'static,
    ) -> Self {
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            // Steps come back as events, so a step commits like Enter does.
            state.set_step(None, window, cx);
            state
        });
        let commit = Rc::new(commit);
        let entered = commit.clone();
        let subscriptions = [
            cx.subscribe_in(&input, window, move |this, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    let parsed = input.read(cx).value().trim().parse::<f64>().ok().filter(|v| v.is_finite());
                    match parsed {
                        Some(value) => {
                            let value = value.round().clamp(min, max);
                            input.update(cx, |input, cx| input.set_value(format_number(value), window, cx));
                            entered(this, value, window, cx);
                        }
                        // An empty or invalid entry goes back to the saved value on the next sync.
                        None => input.update(cx, |input, cx| input.set_value("", window, cx)),
                    }
                }
            }),
            cx.subscribe_in(&input, window, move |this, input, event: &NumberInputEvent, window, cx| {
                let NumberInputEvent::Step(action) = event;
                let current = input.read(cx).value().trim().parse::<f64>().unwrap_or(min);
                let value = match action {
                    StepAction::Increment => current + step,
                    StepAction::Decrement => current - step,
                }
                .round()
                .clamp(min, max);
                input.update(cx, |input, cx| input.set_value(format_number(value), window, cx));
                commit(this, value, window, cx);
            }),
        ];
        Self { input, saved: None, _subscriptions: subscriptions }
    }

    /// Shows `value` when it differs from the value last shown as saved.
    pub(super) fn sync(&mut self, value: f64, window: &mut Window, cx: &mut App) {
        let text = format_number(value);
        let blank = self.input.read(cx).value().is_empty();
        if self.saved != Some(value) || blank {
            self.saved = Some(value);
            self.input.update(cx, |input, cx| input.set_value(text, window, cx));
        }
    }
}

fn format_number(value: f64) -> String {
    format!("{value}")
}

fn text(content: impl Into<SharedString>) -> AnyElement {
    div().child(content.into()).into_any_element()
}

fn notify_error(window: &mut Window, cx: &mut App, title: impl Into<SharedString>, message: impl Into<SharedString>) {
    window.push_notification(Notification::error(message).title(title), cx);
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// `formatRelativeTimeLabel` (`apps/web/src/timestampFormat.ts`).
fn relative_label(iso: &str, now_ms: i64) -> String {
    let Some(at) = crate::inbox::parse_ms(iso) else { return String::new() };
    let minutes = (now_ms - at).max(0) / 60_000;
    match minutes {
        0 => "just now".into(),
        1..60 => format!("{minutes}m ago"),
        60..1440 => format!("{}h ago", minutes / 60),
        _ => format!("{}d ago", minutes / 1440),
    }
}

/// `formatBytes` (`apps/web/src/lib/formatBytes.ts`).
fn format_bytes(value: f64) -> String {
    if value.round() < 1024. {
        return format!("{} B", value.round());
    }
    let scaled = |v: f64| {
        format!(
            "{v:.*}",
            if v >= 100. {
                0
            } else if v >= 10. {
                1
            } else {
                2
            }
        )
    };
    let units = ["KB", "MB", "GB", "TB"];
    let mut next = value / 1024.;
    let mut unit = 0;
    while scaled(next).parse::<f64>().unwrap_or(0.) >= 1024. && unit < units.len() - 1 {
        next /= 1024.;
        unit += 1;
    }
    format!("{} {}", scaled(next), units[unit])
}

/// `AboutVersionTitle`: "Version" and the version, which copies itself.
fn version_title(t: &Tokens) -> AnyElement {
    h_flex()
        .items_baseline()
        .gap_2()
        .child("Version")
        .child(
            div()
                .id("about-version")
                .font_family(FONT_MONO)
                .text_size(px(11.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.muted_foreground)
                .cursor_pointer()
                .hover(|style| style.text_color(t.foreground))
                .tooltip(|window, cx| Tooltip::new("Copy version").build(window, cx))
                .on_click(|_: &ClickEvent, window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(APP_VERSION.to_owned()));
                    window.push_notification(Notification::success("Copied version"), cx);
                })
                .child(APP_VERSION),
        )
        .into_any_element()
}

/// `MobileAppSettingsRow`: a popover per store with its link. The web's popover also
/// shows the link as a QR code; this client has no QR encoder yet.
fn mobile_buttons(t: &Tokens) -> AnyElement {
    let foreground = t.foreground;
    h_flex()
        .gap_2()
        .children(MOBILE_STORES.map(|(label, store, url, icon)| {
            Popover::new(SharedString::from(format!("mobile-{store}")))
                .anchor(gpui_kit::Anchor::TopRight)
                .trigger(
                    Button::new(SharedString::from(format!("mobile-trigger-{store}")))
                        .outline()
                        .small()
                        .icon(Icon::empty().path(icon))
                        .label(label),
                )
                .content(move |_, _, _| {
                    v_flex()
                        .items_center()
                        .gap_3()
                        .p_2()
                        .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(foreground).child(store))
                        .child(
                            Button::new(SharedString::from(format!("mobile-open-{store}")))
                                .outline()
                                .small()
                                .label(format!("Open {store}"))
                                .on_click(move |_, _, cx| cx.open_url(url)),
                        )
                })
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes_and_relative_times_like_the_web() {
        assert_eq!(format_bytes(512.), "512 B");
        assert_eq!(format_bytes(1536.), "1.50 KB");
        assert_eq!(format_bytes(1024. * 1023.6), "1.00 MB", "the unit is picked after rounding");
        assert_eq!(format_bytes(150. * 1024. * 1024.), "150 MB");
        let now = crate::inbox::parse_ms("2026-11-05T12:00:00Z").unwrap();
        assert_eq!(relative_label("2026-11-05T11:59:30Z", now), "just now");
        assert_eq!(relative_label("2026-11-05T11:15:00Z", now), "45m ago");
        assert_eq!(relative_label("2026-11-03T12:00:00Z", now), "2d ago");
    }
}
