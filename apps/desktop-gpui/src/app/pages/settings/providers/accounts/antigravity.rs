//! Antigravity's setup slot (`ProviderSetupSection.tsx`): the environment that runs it, its
//! downloadable runtime (install, update, cancel, remove), then the shared sign-in row.

use gpui_kit::relative;

use super::*;

/// `installationStatusMessage`.
fn install_status(installation: Option<&InstallState>, installed: bool, custom_binary: bool, enabled: bool) -> String {
    let Some(installation) = installation else { return "Checking the Antigravity runtime.".into() };
    let mb = |bytes: u64| bytes as f64 / 1_000_000.;
    match installation.phase.as_str() {
        "downloading" => format!(
            "Downloading {:.1} MB{}.",
            mb(installation.downloaded_bytes),
            installation.total_bytes.map(|total| format!(" of {:.1} MB", mb(total))).unwrap_or_default()
        ),
        "extracting" => "Extracting Antigravity.".into(),
        "verifying" => "Checking the downloaded runtime.".into(),
        _ if installed => "Installed.".into(),
        _ if custom_binary && enabled => "The configured Antigravity runtime is unavailable.".into(),
        _ if custom_binary => "The configured Antigravity runtime has not been checked.".into(),
        _ => match installation.total_bytes.filter(|total| *total > 0) {
            Some(total) => format!("{} MB download.", mb(total).ceil()),
            None => "Not installed.".into(),
        },
    }
}

/// The install button's label.
fn install_label(installation: Option<&InstallState>, installed: bool) -> &'static str {
    match installation {
        Some(i) if i.installed_version.is_some() => {
            if i.version.is_some() && i.version != i.installed_version {
                "Update Antigravity"
            } else {
                "Reinstall Antigravity"
            }
        }
        Some(i) if matches!(i.phase.as_str(), "failed" | "cancelled") => "Retry installation",
        _ if installed => "Install managed runtime",
        _ => "Install Antigravity",
    }
}

impl Workspace {
    /// `ProviderSetupSection` in an Antigravity instance's editor.
    pub(in crate::app) fn render_antigravity_setup(
        &mut self,
        instance_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let workspace = cx.weak_entity();
        let id = instance_id.to_owned();
        window
            .use_keyed_state(SharedString::from(format!("antigravity-setup-{instance_id}")), cx, move |_, _| {
                AccountSection { antigravity: true, ..AccountSection::new(workspace, id, None) }
            })
            .into_any_element()
    }
}

impl AccountSection {
    /// `runCommand`: one setup request at a time, labelled while it runs.
    fn run_setup(&mut self, label: &'static str, tag: &'static str, payload: Value, cx: &mut Context<Self>) {
        let Some(client) = self.client(cx).filter(|_| self.setup_pending.is_none()) else { return };
        self.setup_pending = Some(label);
        self.setup_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = client.request(tag, payload).await;
            let _ = this.update(cx, |this, cx| {
                this.setup_pending = None;
                if let Err(error) = result {
                    this.setup_error = Some(match error {
                        RpcError::Disconnected(_) => "Provider setup failed. Try again.".into(),
                        error => failure_message(&error, "Provider setup failed.").into(),
                    });
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn confirm_remove_runtime(&mut self, environment: String, window: &mut Window, cx: &mut Context<Self>) {
        let section = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let section = section.clone();
            alert
                .entrance(DialogEntrance::None)
                .title(format!("Remove the downloaded Antigravity runtime from {environment}?"))
                .description("Google sign-in and thread history are kept.")
                .show_cancel(true)
                .ok_text("Confirm")
                .on_ok(move |_, _, cx| {
                    let _ = section.update(cx, |this, cx| {
                        let payload = this.input();
                        this.run_setup("Removing runtime", "provider.install.remove", payload, cx);
                    });
                    true
                })
        });
    }

    pub(super) fn render_antigravity(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let Some(workspace) = self.workspace.upgrade() else { return div().into_any_element() };
        let (instance, provider, environment, client) = {
            let workspace = workspace.read(cx);
            let info = &workspace.settings_info();
            (
                info.settings
                    .get("providerInstances")
                    .get(self.instance_id.as_str())
                    .cloned()
                    .unwrap_or_else(|| json!({ "driver": "antigravity" })),
                info.provider(&self.instance_id).cloned(),
                info.environment_label.clone().unwrap_or_else(|| "this environment".into()),
                workspace.client(),
            )
        };
        let enabled = crate::settings::scope::instance_enabled(&instance);
        let rule = || div().border_t_1().border_color(t.border.opacity(0.5));

        let enable_instance = instance.clone();
        let enable_id = self.instance_id.clone();
        let environment_control = v_flex()
            .items_end()
            .gap_2()
            .text_xs()
            .child(div().text_color(t.muted_foreground).child(environment.clone()))
            .when(!enabled, |el| {
                el.child(button("antigravity-enable", "Enable Antigravity").outline().on_click(cx.listener(
                    move |this, _, window, cx| {
                        let mut next = enable_instance.clone();
                        next["enabled"] = true.into();
                        let id = enable_id.clone();
                        let _ = this.workspace.update(cx, |workspace, cx| {
                            workspace.persist_provider_instance(&id, next, window, cx);
                        });
                    },
                )))
            });
        let environment_row = settings_row(
            &t,
            "Environment",
            Some(text("Device that runs this provider.")),
            Some(environment_control.into_any_element()),
        );
        let Some(provider) = provider.filter(|provider| provider.setup.is_some()) else {
            return v_flex()
                .child(environment_row)
                .child(rule().child(settings_row(
                    &t,
                    "Update required",
                    Some(text("Update this environment to manage Antigravity.")),
                    None,
                )))
                .into_any_element();
        };

        let instance_id = self.instance_id.clone();
        subscribe(
            self,
            |s| &mut s.auth,
            client.clone(),
            "provider.auth.subscribe",
            &instance_id,
            Self::auth_updated,
            cx,
        );
        subscribe(self, |s| &mut s.install, client.clone(), "provider.install.subscribe", &instance_id, |_, _| {}, cx);

        let installation = self.install.data.clone();
        let auth_active = self.auth.data.as_ref().is_some_and(|auth| auth_running(&auth.phase));
        let install_active = installation.as_ref().is_some_and(|i| install_running(&i.phase));
        let custom_binary =
            instance.pointer("/config/binaryPath").and_then(Value::as_str).is_some_and(|path| !path.trim().is_empty());
        let installed = provider.installed
            || (!custom_binary && installation.as_ref().is_some_and(|i| i.installed_version.is_some()));
        let can_install = provider.setup.as_ref().is_some_and(|setup| setup.can_install);
        let query_error = self.auth.error.clone().or_else(|| self.install.error.clone());
        let actions_disabled = self.setup_pending.is_some() || query_error.is_some();
        let status = install_status(installation.as_ref(), installed, custom_binary, enabled);

        let progress = installation
            .as_ref()
            .filter(|i| i.phase == "downloading")
            .and_then(|i| Some(i.downloaded_bytes as f32 / i.total_bytes.filter(|total| *total > 0)? as f32));
        let message = installation
            .as_ref()
            .and_then(|i| i.message.clone())
            .filter(|message| !install_active && !message.is_empty() && *message != status);
        let action = match installation.as_ref().and_then(|i| i.operation_id.clone()) {
            Some(operation_id) if install_active => Some(
                button("antigravity-cancel-install", "Cancel installation")
                    .outline()
                    .w_full()
                    .disabled(actions_disabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let payload = json!({ "instanceId": this.instance_id, "operationId": operation_id });
                        this.run_setup("Cancelling installation", "provider.install.cancel", payload, cx);
                    })),
            ),
            _ if !install_active && can_install => Some(
                button("antigravity-install", install_label(installation.as_ref(), installed))
                    .outline()
                    .w_full()
                    .disabled(actions_disabled || auth_active)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let payload = this.input();
                        this.run_setup("Starting installation", "provider.install.start", payload, cx);
                    })),
            ),
            _ => None,
        };
        let remove =
            installation.as_ref().is_some_and(|i| i.can_remove && !install_active).then(|| {
                let environment = environment.clone();
                Button::new("antigravity-remove-runtime")
                    .ghost()
                    .size(px(28.))
                    .p_0()
                    .disabled(actions_disabled || auth_active)
                    .child(Icon::empty().path("icons/trash-2.svg").size(px(14.)))
                    .tooltip("Remove downloaded runtime")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.confirm_remove_runtime(environment.clone(), window, cx)
                    }))
            });
        let runtime_control = v_flex()
            .w(px(224.))
            .gap_2()
            .text_xs()
            .text_color(t.muted_foreground)
            .child(div().min_h(px(16.)).text_right().child(status))
            .child(div().h(px(4.)).when_some(progress, |el, progress| {
                el.child(
                    div()
                        .size_full()
                        .rounded_full()
                        .overflow_hidden()
                        .bg(t.muted)
                        .child(div().h_full().w(relative(progress.min(1.))).rounded_full().bg(t.primary)),
                )
            }))
            .when_some(message, |el, message| el.child(div().text_right().child(message)))
            .child(
                h_flex()
                    .min_h(px(28.))
                    .gap_2()
                    .child(div().w(px(28.)).flex_none().children(remove))
                    .child(div().flex_1().min_w_0().children(action)),
            );
        let notes = v_flex()
            .gap_2()
            .when(custom_binary, |el| el.child("Uses the custom binary path below. Installation keeps that path."))
            .when(!installed && !can_install, |el| {
                el.child("Automatic installation unavailable. Set a binary path or use another environment.")
            });
        let runtime_row = settings_row(
            &t,
            "Runtime",
            Some(v_flex().gap_2().child("Install and manage Antigravity.").child(notes).into_any_element()),
            Some(runtime_control.into_any_element()),
        );

        let snapshot = Snapshot { provider, instance, environment, client };
        let account = self.render_generic(&snapshot, window, cx);
        let error = self.setup_error.clone().or_else(|| query_error.clone().map(Into::into));
        v_flex()
            .child(environment_row)
            .child(rule().child(runtime_row))
            .child(rule().child(account))
            .when_some(error, |el, error| {
                el.child(
                    rule().child(
                        v_flex()
                            .px_4()
                            .py_3()
                            .gap_2()
                            .text_xs()
                            .child(div().text_color(t.error_foreground).child(error))
                            .when(query_error.is_some(), |el| {
                                el.child(div().child(
                                    button("antigravity-retry-status", "Retry setup status").outline().on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.auth = Live::default();
                                            this.install = Live::default();
                                            cx.notify();
                                        }),
                                    ),
                                ))
                            }),
                    ),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(phase: &str, fields: Value) -> InstallState {
        let mut value = json!({ "phase": phase });
        value.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        InstallState::deserialize(value).unwrap()
    }

    #[test]
    fn runtime_status_follows_the_install_phase() {
        assert_eq!(install_status(None, false, false, false), "Checking the Antigravity runtime.");
        let downloading = state("downloading", json!({"downloadedBytes": 1_260_000, "totalBytes": 40_000_000}));
        assert_eq!(install_status(Some(&downloading), false, false, true), "Downloading 1.3 MB of 40.0 MB.");
        let unknown_size = state("downloading", json!({"downloadedBytes": 500_000}));
        assert_eq!(install_status(Some(&unknown_size), false, false, true), "Downloading 0.5 MB.");
        let idle = state("idle", json!({"totalBytes": 40_100_000}));
        assert_eq!(install_status(Some(&idle), false, false, true), "41 MB download.");
        assert_eq!(install_status(Some(&state("idle", json!({}))), false, false, true), "Not installed.");
        assert_eq!(install_status(Some(&idle), true, false, true), "Installed.");
        assert_eq!(
            install_status(Some(&idle), false, true, true),
            "The configured Antigravity runtime is unavailable."
        );
        assert_eq!(
            install_status(Some(&idle), false, true, false),
            "The configured Antigravity runtime has not been checked."
        );
    }

    #[test]
    fn install_button_names_the_next_step() {
        assert_eq!(install_label(None, false), "Install Antigravity");
        assert_eq!(install_label(None, true), "Install managed runtime");
        assert_eq!(install_label(Some(&state("failed", json!({}))), false), "Retry installation");
        let current = state("succeeded", json!({"version": "1.2", "installedVersion": "1.2"}));
        assert_eq!(install_label(Some(&current), true), "Reinstall Antigravity");
        let behind = state("succeeded", json!({"version": "1.3", "installedVersion": "1.2"}));
        assert_eq!(install_label(Some(&behind), true), "Update Antigravity");
    }
}
