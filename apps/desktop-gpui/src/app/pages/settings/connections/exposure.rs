//! Network access and Tailscale HTTPS for the server this app started
//! (`renderNetworkAccessRow`, `renderTailscaleRow` and their dialogs in `ConnectionsSettings.tsx`).
//! Both settings live in `desktop-settings.json`; a change restarts the app, as Electron
//! relaunches, so the server starts with the new exposure.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogClose, DialogEntrance, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, WindowExt as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use super::logic::remote_clients_denial;
use super::ui;
use crate::app::Workspace;
use crate::app::pages::settings::widgets::RowFrame;
use crate::desktop::Desktop;
use crate::desktop::exposure::{self, ExposureView};
use crate::desktop::settings::ExposureMode;
use crate::theme::Tokens;

impl Workspace {
    /// The rows for a backend this app manages, or `None` when it connected to one it did not
    /// start (then the read-only row explains where exposure is set).
    pub(super) fn local_backend_rows(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        t: &Tokens,
    ) -> Option<Vec<AnyElement>> {
        if self.environment_remote || self.settings_endpoint().is_none() {
            return None;
        }
        if self.desktop.exposure.is_none() && !self.desktop.exposure_pending {
            self.load_exposure(window, cx);
        }
        let view = self.desktop.exposure.clone();
        let settings = &cx.global::<Desktop>().settings;
        let (mode, serve_enabled, serve_port) =
            (settings.server_exposure_mode, settings.tailscale_serve_enabled, settings.tailscale_serve_port);
        let denial = remote_clients_denial(&self.settings_info().managed_policy);
        let accessible = view.as_ref().is_some_and(|v| v.exposure.mode == ExposureMode::NetworkAccessible);

        let description: SharedString = match &view {
            None => "Loading\u{2026}".into(),
            Some(v) if accessible => match (&v.exposure.endpoint_url, &v.exposure.advertised_host) {
                (Some(url), _) => format!("Reachable at {url}").into(),
                (None, Some(host)) => format!("Exposed on all interfaces. Pairing links use {host}.").into(),
                (None, None) => "Exposed on all interfaces.".into(),
            },
            Some(_) => "Limited to this machine.".into(),
        };
        // A policy keeps the way in closed; turning access off stays possible.
        let inert = denial.clone().filter(|_| !accessible);
        let switch =
            Switch::new("network-access").checked(accessible).disabled(view.is_none() || inert.is_some()).on_click(
                cx.listener(move |this, checked: &bool, window, cx| this.confirm_network_access(*checked, window, cx)),
            );
        let control = div()
            .id("network-access-control")
            .when_some(inert, |el, message| el.opacity(0.5).tooltip(ui::tooltip(message)));
        let mut rows = vec![
            RowFrame::new("Network access")
                .description(div().child(description))
                .control(control.child(switch))
                .render(t)
                .into_any_element(),
        ];

        let tailscale = view.as_ref().and_then(|v| v.tailscale.clone());
        let tailscale_description: SharedString = match &tailscale {
            Some(endpoint) if endpoint.available => endpoint.url.clone().into(),
            Some(_) => "Use Tailscale Serve to expose this backend through a MagicDNS HTTPS URL.".into(),
            None => exposure::missing_tailscale_description(mode, serve_enabled).into(),
        };
        let mut row = RowFrame::new("Tailscale HTTPS").description(div().child(tailscale_description));
        if let Some(endpoint) = tailscale {
            let available = endpoint.available;
            let inert = denial.filter(|_| !available);
            let switch = Switch::new("tailscale-https").checked(available).disabled(inert.is_some()).on_click(
                cx.listener(move |this, checked: &bool, window, cx| {
                    if *checked {
                        this.confirm_tailscale_setup(endpoint.url.clone(), serve_port, window, cx);
                    } else {
                        this.confirm_tailscale_disable(window, cx);
                    }
                }),
            );
            let control = div()
                .id("tailscale-https-control")
                .when_some(inert, |el, message| el.opacity(0.5).tooltip(ui::tooltip(message)));
            row = row.control(control.child(switch));
        }
        rows.push(row.render(t).into_any_element());
        Some(rows)
    }

    fn load_exposure(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = cx.global::<Desktop>().settings.clone();
        let port = self
            .settings_endpoint()
            .as_ref()
            .and_then(|e| e.http_base.rsplit(':').next()?.parse::<u16>().ok())
            .unwrap_or_default();
        self.desktop.exposure_pending = true;
        let work = cx.background_executor().spawn(async move {
            let (exposure, serve_enabled, serve_port) = exposure::for_launch(&settings, port);
            // Asking Tailscale can prompt on some systems, so only when exposure could use it.
            let tailscale = (exposure.mode == ExposureMode::NetworkAccessible || serve_enabled)
                .then(|| exposure::tailscale_https(serve_enabled, serve_port))
                .flatten();
            ExposureView { exposure, tailscale }
        });
        self.desktop._exposure_task = Some(cx.spawn_in(window, async move |this, cx| {
            let view = work.await;
            let _ = this.update(cx, |this, cx| {
                this.desktop.exposure_pending = false;
                this.desktop.exposure = Some(view);
                cx.notify();
            });
        }));
    }

    /// The "Enable network access?" / "Disable network access?" confirmation.
    fn confirm_network_access(&mut self, enable: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (title, body, confirm) = if enable {
            (
                "Enable network access?",
                "Let your other devices connect to T3 Code over the network. Pair devices to give them access. T3 Code will restart.",
                "Restart and enable",
            )
        } else {
            (
                "Disable network access?",
                "Devices connected over your local network will disconnect. Existing tunnels, such as T3 Connect or Tailscale HTTPS, keep working. T3 Code will restart.",
                "Restart and disable",
            )
        };
        window.open_dialog(cx, move |dialog, _, _| {
            let button =
                Button::new("network-access-confirm").label(confirm).primary().on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    let mode = if enable { ExposureMode::NetworkAccessible } else { ExposureMode::LocalOnly };
                    let desktop = cx.global_mut::<Desktop>();
                    desktop.settings.server_exposure_mode = mode;
                    desktop.settings.save();
                    cx.restart();
                });
            dialog.entrance(DialogEntrance::None).title(title).child(v_flex().child(body)).footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("network-access-cancel").label("Cancel").outline()))
                    .child(button),
            )
        });
    }

    /// "Set up Tailscale HTTPS?", with the HTTPS port.
    fn confirm_tailscale_setup(&mut self, url: String, port: u16, window: &mut Window, cx: &mut Context<Self>) {
        let input: Entity<InputState> = cx.new(|cx| InputState::new(window, cx).default_value(port.to_string()));
        window.open_dialog(cx, move |dialog, _, cx| {
            let t = *crate::theme::tokens(cx);
            let text = input.read(cx).value();
            let parsed = exposure::parse_serve_port(&text);
            // `resolveTailscaleServeBaseUrl`: the URL with the typed port, or as detected.
            let endpoint = match (parsed, url.split_once("://").map(|(_, rest)| rest)) {
                (Some(port), Some(rest)) => {
                    let host = rest.split(['/', ':']).next().unwrap_or(rest);
                    exposure::tailscale_https_url(host, port)
                }
                _ => url.clone(),
            };
            let enable = Button::new("tailscale-enable").label("Enable").primary().disabled(parsed.is_none()).on_click(
                move |_, window, cx| {
                    let Some(port) = parsed else { return };
                    window.close_dialog(cx);
                    let desktop = cx.global_mut::<Desktop>();
                    desktop.settings.tailscale_serve_enabled = true;
                    desktop.settings.tailscale_serve_port = port;
                    desktop.settings.save();
                    cx.restart();
                },
            );
            dialog
                .entrance(DialogEntrance::None)
                .title("Set up Tailscale HTTPS?")
                .w(px(448.))
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            "T3 Code will restart the local backend with Tailscale Serve enabled and ask Tailscale to proxy HTTPS traffic to this backend.",
                        )
                        .child(
                            v_flex()
                                .gap_2()
                                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("HTTPS port"))
                                .child(Input::new(&input)),
                        )
                        .when(parsed.is_none(), |el| {
                            el.child(div().text_xs().text_color(t.error_foreground).child("Enter a port from 1 to 65535."))
                        })
                        .child(
                            v_flex()
                                .rounded(px(6.))
                                .border_1()
                                .border_color(t.border)
                                .px_3()
                                .py_2()
                                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(t.muted_foreground).child("HTTPS endpoint"))
                                .child(div().mt_1().text_sm().truncate().child(endpoint)),
                        ),
                )
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().child(Button::new("tailscale-cancel").label("Cancel").outline()))
                        .child(enable),
                )
        });
    }

    /// "Disable Tailscale HTTPS?".
    fn confirm_tailscale_disable(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.open_dialog(cx, move |dialog, _, _| {
            let button =
                Button::new("tailscale-disable").label("Restart and disable").danger().on_click(|_, window, cx| {
                    window.close_dialog(cx);
                    let desktop = cx.global_mut::<Desktop>();
                    desktop.settings.tailscale_serve_enabled = false;
                    desktop.settings.save();
                    cx.restart();
                });
            dialog
                .entrance(DialogEntrance::None)
                .title("Disable Tailscale HTTPS?")
                .child(v_flex().child("T3 Code will restart the local backend without Tailscale Serve."))
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new().child(Button::new("tailscale-disable-cancel").label("Cancel").outline()),
                        )
                        .child(button),
                )
        });
    }
}
