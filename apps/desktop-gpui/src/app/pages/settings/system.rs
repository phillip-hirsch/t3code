//! Settings sections in the system group (Maintenance, Labs, About, Archive); see `SETTINGS` in `mod.rs`.
//! Ports `MaintenanceSettingsPanel` and `AboutSettingsPanel` (`SettingsPanels.tsx`), `LabsSettingsPanel`,
//! `BackgroundActivitySettings`, `StorageCleanupSections`, `DatabaseStorageNotice`, `AboutVersionTitle`,
//! `MobileAppSettingsRow` and `ArchivedThreadsPanel`.
//!
//! Every setting here writes through the settings scope (`widgets.rs`).

mod archive;
pub(super) mod background;
mod cleanup;

use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::Dialog;
use gpui_kit::component::input::{InputState, NumberInput};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use serde_json::{Value, json};

use self::background::Profile;
use super::widgets::{Part, RowFrame, SettingRow, rich_text, settings_section};
use crate::app::Workspace;
use crate::app::pages::settings::widgets::SettingsView;
use crate::theme::{self, Tokens};

const GUEST_LINKS: &[Part] = &[
    Part::Text(
        "Share a thread with a guest who watches it live in their browser and sends messages you approve. Needs T3 Connect: sign in and link this machine in ",
    ),
    Part::Section("Sharing & access", "access"),
    Part::Text(". While it is off, every guest link stops working."),
];
const LINKED_ENVIRONMENTS: &[Part] = &[
    Part::Text(
        "Let agents here work on your other machines, and theirs here. Turn it on for each machine, then link them in ",
    ),
    Part::Section("Connections", "connections"),
    Part::Text(". While it is off, nothing new starts over a link; tasks already running finish."),
];
const SIMLOCK: &[Part] = &[
    Part::Text("Let the Device panel lease simulators and emulators from your own "),
    Part::Url("Simlock", "https://incubator.callstack.com/simlock/"),
    Part::Text(" pool, added in "),
    Part::Section("Devices", "devices"),
    Part::Text(". An alpha that may change or break. T3 Code never installs Simlock."),
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
    pub(super) fn render_settings_maintenance(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let connected = self.settings_client().is_some();
        let background = self.render_background_row(cx);
        let storage = self.render_storage_sections(window, cx);
        let mut row = |this: &mut Self, row: SettingRow| this.setting_row(&view, row, window, cx);
        let updates = [
            row(
                self,
                SettingRow::new("provider-update-checks")
                    .description("Check installed provider CLIs for newer available versions and fetch model catalog updates. When off, new models may not appear.")
                    .setting("enableProviderUpdateChecks")
                    .switch(),
            ),
            row(
                self,
                SettingRow::new("continue-threads-after-server-update")
                    .description("Automatically resume interrupted threads after an update, crash, or machine restart on the selected environments.")
                    .status((!connected).then_some("All selected connected environments must support restart continuation."))
                    .setting("continueThreadsAfterServerUpdate")
                    .switch(),
            ),
        ];
        let mut sections = vec![
            settings_section("updates", "Updates", updates, &t),
            settings_section("background-activity", "Background activity", [background], &t),
        ];
        sections.extend(storage);
        let environment_id = view.environment().map(|e| e.id.to_owned());
        let view_diagnostics = Button::new("view-diagnostics").outline().small().label("View diagnostics").on_click(
            cx.listener(move |this, _, _, cx| {
                this.settings_ui.scope = this.settings_ui.scope.with_machine(environment_id.clone());
                this.select_settings_section("diagnostics", cx);
            }),
        );
        let description = if view.scope.kind == crate::settings::scope::ScopeKind::Environment {
            "Inspect processes, resource use, and logs on this environment."
        } else {
            "Inspect processes, resource use, and logs on one environment at a time."
        };
        let diagnostics = self.setting_row(
            &view,
            SettingRow::new("diagnostics").description(description).control(view_diagnostics),
            window,
            cx,
        );
        sections.push(settings_section("diagnostics-section", "Diagnostics", [diagnostics], &t));
        self.settings_page("maintenance", &view, sections, cx)
    }

    pub(super) fn render_settings_labs(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        // The web also shows this on while the removed legacy sidebar's opt-in
        // (`legacySidebarEnabled`) is set, and its toggle clears that opt-in.
        let boring = view.bool("boringSidebarEnabled") || view.bool("legacySidebarEnabled");
        let boring_switch = super::widgets::switch(&"boring-sidebar".into(), boring, false, &t).on_click(cx.listener(
            move |this, _, window, cx| {
                let patch = json!({ "boringSidebarEnabled": !boring, "legacySidebarEnabled": false });
                this.update_settings(patch, window, cx)
            },
        ));
        let workspace = cx.weak_entity();
        let described = |id: &'static str, parts: &'static [Part]| rich_text(id, parts, workspace.clone());
        let mut row = |this: &mut Self, row: SettingRow| this.setting_row(&view, row, window, cx);
        let beta = [
            row(
                self,
                SettingRow::new("boring-sidebar")
                    .description("Group sidebar threads into project folders. Pinned threads stay on top, and Snoozed and Settled stay below the folders.")
                    .setting("boringSidebarEnabled")
                    .control(boring_switch),
            ),
            row(
                self,
                SettingRow::new("boring-sidebar-multiline")
                    .description("In the boring sidebar, show folder threads on two lines: the title, then the branch, pull request, and agent.")
                    .setting("boringSidebarMultilineRows")
                    .switch(),
            ),
            // The web hides the switch behind "Sign in" while signed out of T3 Connect; this client has
            // no T3 Connect sign-in, which the web treats as a build without it (`GuestLinksRow`).
            row(
                self,
                SettingRow::new("guest-links")
                    .description(described("guest-links-description", GUEST_LINKS))
                    .setting("guestSharingEnabled")
                    .switch(),
            ),
            row(
                self,
                SettingRow::new("linked-environments-beta")
                    .description(described("linked-environments-description", LINKED_ENVIRONMENTS))
                    .setting("linkedEnvironmentsEnabled")
                    .switch(),
            ),
            row(
                self,
                SettingRow::new("simlock-hosts")
                    .description(described("simlock-description", SIMLOCK))
                    .setting("simlockDeviceHostsEnabled")
                    .switch(),
            ),
        ];
        let legacy = [
            row(
                self,
                SettingRow::new("legacy-plan-mode")
                    .description("Restore Build/Plan, /plan, /default, and Shift+Tab. Off uses build mode.")
                    .setting("planModeEnabled")
                    .switch(),
            ),
            row(
                self,
                SettingRow::new("legacy-context-window-indicator")
                    .description("Shows context window usage as a circular indicator in the composer.")
                    .setting("contextWindowMeterEnabled")
                    .switch(),
            ),
        ];
        let sections = [settings_section("beta", "Beta", beta, &t), settings_section("legacy", "Legacy", legacy, &t)];
        self.settings_page("labs", &view, sections, cx)
    }

    pub(super) fn render_settings_about(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let cli_row = self.about_cli_row(&t, window, cx);
        let policy = &self.settings_info().managed_policy;
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
            app.push(
                RowFrame::new("Organization")
                    .description(text(description))
                    .control(div().children(help))
                    .render(&t)
                    .into_any_element(),
            );
        }
        app.push(
            RowFrame::new(version_title(&t))
                .description(text(self.about_update_description()))
                .control(self.about_update_button())
                .render(&t)
                .into_any_element(),
        );
        app.extend(cli_row);
        app.push(
            RowFrame::new("Mobile app")
                .description(text("Connect to your server from your phone or tablet."))
                .control(mobile_buttons(&t))
                .render(&t)
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
        let view_licenses = Button::new("about-licenses")
            .outline()
            .small()
            .label("View licenses")
            .on_click(cx.listener(|this, _, _, cx| this.select_settings_section("open-source-licenses", cx)));
        let legal = [
            RowFrame::new("Privacy policy")
                .description(text(privacy))
                .control(view_policy)
                .render(&t)
                .into_any_element(),
            RowFrame::new("Open source licenses")
                .description(text("Notices for dependencies, assets, and optional tools used by T3 Code."))
                .control(view_licenses)
                .render(&t)
                .into_any_element(),
        ];
        let sections = [settings_section("app", "App", app, &t), settings_section("legal", "Legal", legal, &t)];
        self.settings_page("about", &view, sections, cx)
    }

    pub(super) fn render_settings_archived(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_archive(view, window, cx)
    }

    fn settings_json(&self) -> &Value {
        &self.settings_info().settings
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
                        this.update_settings(patch, window, cx)
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
        let title = h_flex().gap_1p5().child("Background activity").child(info);
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
                            this.update_settings(background::profile_patch(profile), window, cx)
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
        RowFrame::new(title)
            .description(text(description))
            .reset(reset)
            .control(h_flex().gap_2().child(picker).children(configure))
            .render(&t)
            .into_any_element()
    }

    /// `BackgroundActivityAdvancedDialog`.
    fn open_background_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_client().is_none() {
            return;
        }
        let (view, scope) = (cx.entity(), self.settings_environment());
        window.open_dialog(cx, move |dialog: Dialog, window, cx| {
            let t = *theme::tokens(cx);
            // Settings fell back to another environment (this one disconnected): edit nothing there.
            if view.read(cx).settings_environment() != scope {
                return dialog.w(px(576.)).title("Background Activity").child(
                    div()
                        .text_sm()
                        .text_color(t.muted_foreground)
                        .child("This environment disconnected. Reconnect it to change its background activity."),
                );
            }
            let settings = view.read(cx).settings_json().clone();
            let resolved = background::resolve_server(&settings);
            // (min, step) per `background::DURATION_KEYS`, as the web's `IntervalSecondsField`s.
            let inputs = [(0, 5), (0, 30), (5, 5), (5, 30)].into_iter().enumerate().map(|(index, (min, step))| {
                let key = background::DURATION_KEYS[index];
                let seconds = resolved.seconds(index) as i64;
                view.update(cx, |this, cx| {
                    // Saved in milliseconds, so the seconds must fit `i64` times 1000.
                    let max = i64::MAX / 1000;
                    this.number_field(key, seconds, (min, max, step), window, cx, move |this, seconds, window, cx| {
                        let patch = background::override_patch(this.settings_json(), key, json!(seconds.saturating_mul(1000)));
                        this.update_settings(patch, window, cx)
                    })
                })
            });
            let inputs: Vec<Entity<InputState>> = inputs.collect();
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
                                    this.update_settings(patch, window, cx)
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
                                this.update_settings(patch, window, cx)
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
                            seconds(&inputs[0]),
                            true,
                        ))
                        .child(entry(
                            "Provider health interval",
                            "Refresh provider availability, versions, and auth state. Model catalog updates follow Provider update checks.",
                            seconds(&inputs[1]),
                            true,
                        ))
                        .child(entry(
                            "Host power monitor",
                            "Poll host power state while clients are active.",
                            seconds(&inputs[2]),
                            true,
                        ))
                        .child(entry(
                            "Idle host monitor",
                            "Poll host power state when no foreground client is active.",
                            seconds(&inputs[3]),
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
                                    this.update_settings(patch, window, cx)
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
    crate::timestamp::parse_ms(iso).map_or_else(String::new, |at| super::ago_label(now_ms - at))
}

/// `formatBytes` (`apps/web/src/lib/formatBytes.ts`).
pub(super) fn format_bytes(value: f64) -> String {
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
                .font_family(t.fonts.mono)
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
        let now = crate::timestamp::parse_ms("2026-11-05T12:00:00Z").unwrap();
        assert_eq!(relative_label("2026-11-05T11:59:30Z", now), "just now");
        assert_eq!(relative_label("2026-11-05T11:15:00Z", now), "45m ago");
        assert_eq!(relative_label("2026-11-03T12:00:00Z", now), "2d ago");
    }
}
