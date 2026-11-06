//! Settings › Browser (`BrowserSettingsPanel` in `apps/web/src/components/settings/IntegrationsSettings.tsx`).
//!
//! Left out until this client has what they configure: "Auto-show floating preview" (agents'
//! server tabs and devices, which it can't draw), the recordings' key and mouse press overlays
//! (it doesn't record its own tabs) and importing cookies from other browsers.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogEntrance, DialogFooter};
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, px,
};
use serde_json::{Value, json};

use crate::app::Workspace;
use crate::app::pages::settings::widgets::{SettingRow, SettingsView, select_trigger, settings_section};
use crate::preview::{self, PRESETS, Profile, ProfileKind, Viewport};
use crate::theme::{self, Tokens};

const LINK_TARGETS: &[(&str, &str)] = &[("system", "Your default browser"), ("app", "T3 Code")];
const SEARCH_ENGINES: &[(&str, &str)] = &[
    ("duckduckgo", "DuckDuckGo"),
    ("google", "Google"),
    ("bing", "Bing"),
    ("brave", "Brave Search"),
    ("kagi", "Kagi"),
    ("ecosia", "Ecosia"),
    ("custom", "Custom"),
];
const APPEARANCES: &[(&str, &str)] = &[("system", "System"), ("light", "Light"), ("dark", "Dark")];
const FRAME_RATES: [i64; 2] = [30, 60];
/// `BROWSER_CUSTOM_SEARCH_URL_MAX_LENGTH`.
const CUSTOM_SEARCH_URL_MAX_LENGTH: usize = 2048;

/// The remove confirmation while its data is being deleted.
#[derive(Default)]
struct Removal {
    in_flight: bool,
    error: Option<SharedString>,
}

impl Workspace {
    pub(super) fn render_browser_settings(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let agent_description = if view.scope.is_project() {
            "Allow agents in this project to use the shared browser. Applies when the agent session next starts."
        } else {
            "Allow agents to use the shared browser. Projects can override it."
        };
        let agent_access = SettingRow::new("agent-browser-access")
            .description(agent_description)
            .setting("enableAgentBrowserAccess")
            .switch();
        let agent_access = self.setting_row(view, agent_access, window, cx);

        let profiles = self.browser_profiles_row(view, window, cx);
        let link_target = SettingRow::new("browser-link-target")
            .description(
                "Where links in the chat and terminal open. Hold ⌘ or Ctrl while clicking a link to open it in your default browser either way.",
            )
            .setting("browserLinkTarget")
            .select(LINK_TARGETS, 160.);
        let link_target = self.setting_row(view, link_target, window, cx);
        let search_engine = SettingRow::new("browser-search-engine")
            .description("What the address bar searches when you type something that isn't a URL.")
            .setting("browserSearchEngine")
            .setting("browserCustomSearchUrl")
            .select(SEARCH_ENGINES, 160.);
        let mut browsing = vec![profiles, link_target, self.setting_row(view, search_engine, window, cx)];
        if view.str("browserSearchEngine") == "custom" {
            browsing.push(self.custom_search_url_row(view, window, cx));
        }
        let advanced = [
            self.browser_viewport_row(view, window, cx),
            self.browser_zoom_row(view, window, cx),
            self.setting_row(
                view,
                SettingRow::new("browser-default-appearance")
                    .description("The color scheme pages are told to prefer. System follows your OS setting.")
                    .setting("browserDefaultAppearance")
                    .select(APPEARANCES, 160.),
                window,
                cx,
            ),
        ];
        browsing.push(self.advanced_fold("browser", advanced, cx));

        let rate = view.value("browserRecordingFrameRate").clone();
        let options = FRAME_RATES.iter().map(|r| (json!(r), format!("{r} fps").into())).collect();
        let rate_label = format!("{} fps", rate.as_i64().unwrap_or(30));
        let rate_select = self.value_select(
            "browser-recording-frame-rate",
            "browserRecordingFrameRate",
            &rate,
            options,
            rate_label,
            160.,
            cx,
        );
        let frame_rate = SettingRow::new("browser-recording-frame-rate")
            .description("Maximum recording rate. 30 fps saves CPU and storage; 60 fps is smoother.")
            .setting("browserRecordingFrameRate")
            .control(rate_select);
        let recordings = [self.setting_row(view, frame_rate, window, cx)];

        let sections = [
            settings_section("browser-agent-access", "Agent access", [agent_access], &t),
            settings_section("browser", "Browsing", browsing, &t),
            settings_section("browser-recordings", "Recordings", recordings, &t),
        ];
        self.settings_page("browser", view, sections, cx)
    }

    fn custom_search_url_row(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let saved = view.str("browserCustomSearchUrl").to_owned();
        let field = self.text_field(
            "browser-custom-search-url",
            &saved,
            "https://example.com/search?q=%s",
            false,
            None,
            window,
            cx,
            |this, text, window, cx| {
                let text: String = text.trim().chars().take(CUSTOM_SEARCH_URL_MAX_LENGTH).collect();
                this.update_settings(json!({"browserCustomSearchUrl": text}), window, cx);
            },
        );
        let invalid = !preview::valid_search_template(&saved);
        let row = SettingRow::titled("browser-custom-search-url", "Custom search URL")
            .description("Use %s where the search terms go.")
            .status(invalid.then_some("Enter an http or https URL containing %s. Searches use DuckDuckGo until then."))
            .control(field.sized(px(256.)));
        self.setting_row(view, row, window, cx)
    }

    /// `BrowserViewportSetting`: Fill, Responsive or a device preset, with the size and a rotate
    /// button for anything but Fill.
    fn browser_viewport_row(&mut self, view: &SettingsView, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let viewport: Viewport =
            serde::Deserialize::deserialize(view.value("browserDefaultViewport")).unwrap_or_default();
        let choice = viewport.choice().to_owned();
        let label = match choice.as_str() {
            "fill" => "Fill panel",
            "responsive" => "Responsive",
            id => PRESETS.iter().find(|p| p.id == id).map_or("Responsive", |p| p.label),
        };
        let workspace = cx.entity();
        let (picked, checked) = (viewport.clone(), choice.clone());
        let select = select_trigger(&"browser-default-viewport".into(), label, 176., &t).dropdown_menu_with_anchor(
            gpui_kit::Anchor::TopRight,
            move |mut menu, window, _| {
                let pick = |choice: &'static str| {
                    let current = picked.clone();
                    window.listener_for(&workspace, move |this: &mut Workspace, _, window, cx| {
                        if let Some(next) = current.chosen(choice) {
                            this.update_settings(json!({"browserDefaultViewport": next.to_json()}), window, cx);
                        }
                    })
                };
                menu = menu
                    .item(PopupMenuItem::new("Fill panel").checked(checked == "fill").on_click(pick("fill")))
                    .item(
                        PopupMenuItem::new("Responsive").checked(checked == "responsive").on_click(pick("responsive")),
                    )
                    .label("Standard");
                for preset in &PRESETS {
                    let item = PopupMenuItem::element(move |_, cx| {
                        let muted = theme::tokens(cx).muted_foreground;
                        h_flex().w_full().justify_between().gap(px(20.)).child(preset.label).child(
                            div().text_xs().text_color(muted).child(format!("{} × {}", preset.width, preset.height)),
                        )
                    });
                    menu = menu.item(item.checked(checked == preset.id).on_click(pick(preset.id)));
                }
                menu.max_h(px(360.)).scrollable(true)
            },
        );
        let mut control = h_flex().flex_wrap().justify_end().gap(px(8.)).child(select);
        if let Some((width, height)) = viewport.size() {
            let field = |this: &mut Self, width_axis: bool, value: f32, window: &mut Window, cx: &mut Context<Self>| {
                let key = if width_axis { "browser-viewport-width" } else { "browser-viewport-height" };
                let range = (preview::VIEWPORT_MIN_DIMENSION, preview::VIEWPORT_MAX_DIMENSION, 1);
                #[expect(clippy::cast_possible_truncation, reason = "the field shows a whole number; `as` saturates")]
                let input = this.number_field(key, value as i64, range, window, cx, move |this, value, window, cx| {
                    let current: Viewport =
                        serde::Deserialize::deserialize(this.client_settings.values.get("browserDefaultViewport"))
                            .unwrap_or_default();
                    if let Some(next) = current.with_dimension(width_axis, value) {
                        this.update_settings(json!({"browserDefaultViewport": next.to_json()}), window, cx);
                    }
                });
                div().w(px(80.)).child(Input::new(&input).small())
            };
            let rotated = viewport.rotated().to_json();
            let orientation = if height >= width { "landscape" } else { "portrait" };
            let rotate = Button::new("browser-viewport-rotate")
                .ghost()
                .size(px(28.))
                .p_0()
                .rounded(px(6.))
                .child(Icon::empty().path("icons/app/screen-rotation.svg").size(px(16.)).text_color(t.muted_foreground))
                .tooltip(format!("Rotate to {orientation}"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.update_settings(json!({"browserDefaultViewport": rotated.clone()}), window, cx);
                }));
            control = control.child(
                h_flex()
                    .gap(px(4.))
                    .child(field(self, true, width, window, cx))
                    .child(div().text_xs().text_color(t.muted_foreground).child("×"))
                    .child(field(self, false, height, window, cx))
                    .child(rotate),
            );
        }
        let row = SettingRow::new("browser-default-viewport")
            .description("Tab size for you and agents. Fill fits the panel; other sizes show the device toolbar.")
            .reset((choice != "fill").then(|| json!({"browserDefaultViewport": {"_tag": "fill"}})))
            .control(control);
        self.setting_row(view, row, window, cx)
    }

    /// `BrowserZoomSetting`; zoom levels compare as numbers, since a stored `1` and `1.0` are the same zoom.
    fn browser_zoom_row(&mut self, view: &SettingsView, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let zoom = view.value("browserDefaultZoomFactor").as_f64().unwrap_or(1.);
        let label = |level: f64| format!("{}%", (level * 100.).round());
        let options = preview::ZOOM_LEVELS
            .iter()
            .map(|&level| (level, label(level).into(), (level - zoom).abs() < 1e-9))
            .collect();
        let select =
            self.menu_select("browser-default-zoom", options, label(zoom), 160., cx, |this, level, window, cx| {
                this.update_settings(json!({"browserDefaultZoomFactor": zoom_value(level)}), window, cx);
            });
        let row = SettingRow::new("browser-default-zoom")
            .description("Page zoom applied to new browser tabs.")
            .reset(((zoom - 1.).abs() > 1e-9).then(|| json!({"browserDefaultZoomFactor": 1})))
            .control(select);
        self.setting_row(view, row, window, cx)
    }

    /// `BrowserProfilesSetting`: the profile list with its menus, and "Add profile".
    fn browser_profiles_row(&mut self, view: &SettingsView, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let settings = &self.client_settings.values;
        let at_limit = preview::profiles_full(settings);
        let default_id = preview::default_profile_id(settings);
        let listed: Vec<Profile> = preview::resolve_profiles(&preview::user_profiles(settings))
            .into_iter()
            .filter(|p| p.kind != ProfileKind::Incognito)
            .collect();
        // Clearing reaches the server's browser too, so it waits for a connection, as the web does.
        let connected = self.settings_client().is_some();
        let workspace = cx.entity();
        let add = Button::new("browser-add-profile")
            .outline()
            .small()
            .h(px(28.))
            .px(px(9.))
            .gap(px(6.))
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .child(Icon::new(IconName::Plus).size(px(14.)))
            .child("Add profile")
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, {
                let workspace = workspace.clone();
                move |menu, window, _| {
                    menu.item(PopupMenuItem::new("Blank profile").disabled(at_limit).on_click(
                        window.listener_for(&workspace, |this, _, window, cx| this.create_browser_profile(window, cx)),
                    ))
                    .when(at_limit, |menu| {
                        menu.item(PopupMenuItem::new("You\u{2019}ve reached the profile limit").disabled(true))
                    })
                }
            });
        let rows: Vec<AnyElement> = listed
            .iter()
            .enumerate()
            .map(|(index, profile)| {
                let is_default = profile.id == default_id;
                let name: AnyElement = if profile.built_in() {
                    div().truncate().text_sm().text_color(t.foreground).child(profile.name.clone()).into_any_element()
                } else {
                    let id = profile.id.clone();
                    self.text_field(
                        &format!("browser-profile-{}", profile.id),
                        &profile.name,
                        "",
                        false,
                        None,
                        window,
                        cx,
                        move |this, name, window, cx| this.rename_browser_profile(&id, &name, window, cx),
                    )
                    .sized(px(224.))
                    .into_any_element()
                };
                h_flex()
                    .px(px(12.))
                    .py(px(8.))
                    .gap(px(12.))
                    .when(index > 0, |el| el.border_t_1().border_color(t.border.opacity(0.6)))
                    .child(
                        h_flex().flex_1().min_w_0().gap(px(8.)).child(name).when(is_default, |el| el.child(badge(&t))),
                    )
                    .child(profile_menu(&workspace, profile.clone(), is_default, connected, &t))
                    .into_any_element()
            })
            .collect();
        let list = v_flex()
            .mt(px(8.))
            .mb(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(t.border.opacity(0.6))
            .overflow_hidden()
            .children(rows);
        let row = SettingRow::new("browser-profiles")
            .description("Profiles separate cookies and logins. Incognito data is cleared when the app closes.")
            .control(add)
            .body(list);
        self.setting_row(view, row, window, cx)
    }

    fn create_browser_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = &self.client_settings.values;
        let name =
            preview::unique_profile_name("New profile", &preview::resolve_profiles(&preview::user_profiles(settings)));
        let id = format!("profile-{}", uuid::Uuid::new_v4());
        let profile = Profile { id, name, kind: ProfileKind::Persistent };
        if let Some(profiles) = preview::profiles_with_added(settings, &profile) {
            self.update_settings(json!({"browserProfiles": profiles}), window, cx);
        }
    }

    fn rename_browser_profile(&mut self, id: &str, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let name: String = name.trim().chars().take(preview::PROFILE_NAME_MAX_LENGTH).collect();
        if name.is_empty() {
            return;
        }
        let profiles = preview::profiles_with_renamed(&self.client_settings.values, id, &name);
        self.update_settings(json!({"browserProfiles": profiles}), window, cx);
    }

    /// Clears now, or asks first when the server's browser has tabs to close.
    fn confirm_clear_browser_profile(&mut self, profile: Profile, window: &mut Window, cx: &mut Context<Self>) {
        if !self.settings_info().capability("serverBrowser") {
            return self.clear_browser_profile_data(profile, window, cx);
        }
        let workspace = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (workspace, profile) = (workspace.clone(), profile.clone());
            dialog
                .entrance(DialogEntrance::None)
                .w(px(448.))
                .title(format!("Clear \u{201c}{}\u{201d}\u{2019}s cookies and cache?", profile.name))
                .child(dialog_text(
                    "You are signed out of its sites. Server browser tabs open in this profile close now.",
                    theme::tokens(cx),
                ))
                .footer(DialogFooter::new().child(cancel_button("clear-profile-cancel", false)).child(
                    Button::new("clear-profile-confirm").label("Clear data").danger().on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        workspace.update(cx, |this, cx| this.clear_browser_profile_data(profile.clone(), window, cx));
                    }),
                ))
        });
    }

    fn clear_browser_profile_data(&mut self, profile: Profile, window: &mut Window, cx: &mut Context<Self>) {
        let task = self.clear_browser_profile(profile.id, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |_, window, cx| {
                let notification = match result {
                    Ok(()) => Notification::success("").title(format!("Cleared {}'s cookies and cache", profile.name)),
                    Err(error) => {
                        log::warn!("clearing browser profile data failed: {error}");
                        Notification::error("").title(format!("Could not clear {}'s data", profile.name))
                    }
                };
                window.push_notification(notification, cx);
            });
        })
        .detach();
    }

    fn confirm_remove_browser_profile(&mut self, profile: Profile, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = cx.entity();
        let removal: Entity<Removal> = cx.new(|_| Removal::default());
        window.open_dialog(cx, move |dialog, _, cx| {
            let t = theme::tokens(cx);
            let Removal { in_flight, error } = removal.read(cx);
            let in_flight = *in_flight;
            let remove = {
                let (workspace, removal, profile) = (workspace.clone(), removal.clone(), profile.clone());
                move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut gpui_kit::App| {
                    removal.update(cx, |r, cx| {
                        *r = Removal { in_flight: true, error: None };
                        cx.notify();
                    });
                    workspace.update(cx, |this, cx| this.remove_browser_profile(&profile, &removal, window, cx));
                }
            };
            dialog
                .entrance(DialogEntrance::None)
                .overlay_closable(!in_flight)
                .keyboard(!in_flight)
                .close_button(!in_flight)
                .w(px(448.))
                .title(format!("Remove \u{201c}{}\u{201d}?", profile.name))
                .child(
                    v_flex()
                        .gap(px(8.))
                        .child(dialog_text(
                            "Its cookies and logins are deleted. Desktop tabs already open in this profile stay \
                             open until you close them; server browser tabs close now.",
                            t,
                        ))
                        .children(error.clone().map(|error| div().text_sm().text_color(t.error).child(error))),
                )
                .footer(
                    DialogFooter::new().child(cancel_button("remove-profile-cancel", in_flight)).child(
                        Button::new("remove-profile-confirm")
                            .label(if in_flight { "Removing\u{2026}" } else { "Remove profile" })
                            .danger()
                            .disabled(in_flight)
                            .on_click(remove),
                    ),
                )
        });
    }

    /// Deletes the profile's data first, so a failure leaves the profile listed to retry.
    fn remove_browser_profile(
        &mut self,
        profile: &Profile,
        removal: &Entity<Removal>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self.clear_browser_profile(profile.id.clone(), window, cx);
        let (id, removal) = (profile.id.clone(), removal.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(error) = result {
                    log::warn!("removing browser profile data failed: {error}");
                    removal.update(cx, |r, cx| {
                        *r = Removal {
                            in_flight: false,
                            error: Some("Profile data could not be deleted. Try again.".into()),
                        };
                        cx.notify();
                    });
                    return window.refresh();
                }
                let settings = &this.client_settings.values;
                let profiles = preview::profiles_without(settings, &id);
                let mut patch = json!({"browserProfiles": profiles});
                if settings.get("browserDefaultProfileId").as_str() == Some(id.as_str()) {
                    patch["browserDefaultProfileId"] = preview::DEFAULT_PROFILE_ID.into();
                }
                this.update_settings(patch, window, cx);
                window.close_dialog(cx);
            });
        })
        .detach();
    }
}

/// `1` for 100%, so the stored value matches the default the reset compares against.
#[expect(clippy::cast_possible_truncation, reason = "only whole levels reach the cast")]
fn zoom_value(level: f64) -> Value {
    if level.fract() == 0. { json!(level as i64) } else { json!(level) }
}

/// `AlertDialogDescription`.
fn dialog_text(text: &'static str, t: &Tokens) -> impl IntoElement {
    div().text_sm().text_color(t.muted_foreground).child(text)
}

/// `AlertDialogClose` as an outline "Cancel"; unlike `DialogClose` it keeps its own width.
fn cancel_button(id: &'static str, disabled: bool) -> Button {
    Button::new(id).label("Cancel").outline().disabled(disabled).on_click(|_, window, cx| window.close_dialog(cx))
}

/// The web's default `Badge`.
fn badge(t: &Tokens) -> impl IntoElement {
    div()
        .flex_none()
        .h(px(18.))
        .px(px(3.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .bg(t.primary)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.primary_foreground)
        .child("Default")
}

/// A profile's options: set as default, clear its data, and remove it unless it is built in.
fn profile_menu(
    workspace: &Entity<Workspace>,
    profile: Profile,
    is_default: bool,
    connected: bool,
    t: &Tokens,
) -> impl IntoElement {
    let (workspace, error) = (workspace.clone(), t.error);
    Button::new(SharedString::from(format!("browser-profile-menu-{}", profile.id)))
        .ghost()
        .xsmall()
        .size(px(24.))
        .text_color(t.muted_foreground)
        .icon(Icon::new(IconName::EllipsisVertical).size(px(14.)))
        .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, window, _| {
            let on = |action: fn(&mut Workspace, Profile, &mut Window, &mut Context<Workspace>)| {
                let profile = profile.clone();
                window.listener_for(&workspace, move |this, _, window, cx| action(this, profile.clone(), window, cx))
            };
            menu = menu
                .item(PopupMenuItem::new("Set as default").disabled(is_default).on_click(on(
                    |this, profile, window, cx| {
                        this.update_settings(json!({"browserDefaultProfileId": profile.id}), window, cx);
                    },
                )))
                .item(
                    PopupMenuItem::new("Clear cookies and cache")
                        .disabled(!connected)
                        .on_click(on(Workspace::confirm_clear_browser_profile)),
                );
            if !profile.built_in() {
                let remove =
                    PopupMenuItem::element(move |_, _| div().text_color(error).child("Remove profile and data"));
                menu = menu.item(remove.disabled(!connected).on_click(on(Workspace::confirm_remove_browser_profile)));
            }
            if !connected {
                menu = menu
                    .separator()
                    .item(PopupMenuItem::new("Connect to an environment to clear profile data").disabled(true));
            }
            menu
        })
}
