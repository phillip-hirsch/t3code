//! The footer's provider updates across machines (`useProviderUpdates`): every connected
//! environment's one-click provider updates, confirmed in `UpdateEverythingDialog` with the
//! machines and providers they touch.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogEntrance, DialogFooter};
use gpui_kit::component::{WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    Context, FontWeight, ParentElement as _, SharedString, Styled as _, Window, div, prelude::FluentBuilder as _, px,
    relative, white,
};
use serde_json::json;

use super::provider_update::dedupe_by_driver;
use crate::app::Workspace;
use crate::model::Provider;
use crate::rpc::RpcClient;
use crate::usage::provider_display_name;

/// One machine's share of the run: its name, its socket, and `(driver, instanceId)` per provider.
#[derive(Clone)]
pub(super) struct MachineUpdate {
    pub(super) label: String,
    pub(super) client: RpcClient,
    pub(super) candidates: Vec<(String, String)>,
}

/// `collectProviderUpdateCandidates`: per driver, the one instance a click updates.
fn candidates(providers: &[Provider]) -> Vec<(String, String)> {
    dedupe_by_driver(providers.iter().filter(|p| crate::inbox::is_provider_update_candidate(p)))
        .into_iter()
        .map(|p| (p.driver.clone(), p.instance_id.clone()))
        .collect()
}

/// `countLabel`: "1 provider", "2 providers".
fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

/// `describeUpdateEverything` for provider updates alone: the footer's summary, the dialog's
/// title and description, and the providers each machine updates, by display name.
fn describe(machines: &[(String, Vec<String>)]) -> (String, String, String) {
    let providers: usize = machines.iter().map(|(_, providers)| providers.len()).sum();
    let summary = format!("Update {}", count(providers, "provider"));
    let title = format!("Update {}?", count(machines.len(), "machine"));
    let description = format!("{providers} provider {}", if providers == 1 { "update" } else { "updates" });
    (summary, title, description)
}

impl Workspace {
    /// Connected machines with a provider to update, the primary first.
    pub(super) fn provider_update_plan(&self) -> Vec<MachineUpdate> {
        self.sessions()
            .iter()
            .filter_map(|session| {
                let client = session.client.clone()?;
                let candidates = candidates(&session.server_info.providers);
                let label = session.server_info.environment_label.clone().unwrap_or_else(|| "Server".into());
                (!candidates.is_empty()).then_some(MachineUpdate { label, client, candidates })
            })
            .collect()
    }

    /// The footer button's label for `plan`.
    pub(super) fn provider_update_summary(plan: &[MachineUpdate]) -> SharedString {
        describe(&named(plan)).0.into()
    }

    /// Asks with the machines and providers, then updates each machine's providers on its own socket.
    pub(super) fn confirm_provider_updates(
        &mut self,
        plan: Vec<MachineUpdate>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = named(&plan);
        let (_, title, description) = describe(&rows);
        let (title, description) = (SharedString::from(title), SharedString::from(description));
        let workspace = cx.entity().downgrade();
        let plan = Rc::new(RefCell::new(Some(plan)));
        window.open_dialog(cx, move |dialog, _, cx| {
            let t = crate::theme::tokens(cx);
            let run = {
                let (plan, workspace) = (plan.clone(), workspace.clone());
                move |window: &mut Window, cx: &mut gpui_kit::App| {
                    if let Some(plan) = plan.borrow_mut().take() {
                        let _ = workspace.update(cx, |this, cx| this.run_provider_updates(plan, cx));
                    }
                    window.close_dialog(cx);
                }
            };
            let enter = run.clone();
            let cell = |width: Option<f32>| div().min_w_0().when_some(width, |el, w| el.w(px(w)).flex_none());
            let header = h_flex()
                .gap(px(16.))
                .pb(px(6.))
                .border_b_1()
                .border_color(t.border)
                .text_xs()
                .text_color(t.muted_foreground)
                .child(cell(Some(MACHINE_WIDTH)).child("Machine"))
                .child(cell(Some(APP_WIDTH)).child("T3 Code"))
                .child(cell(None).flex_1().child("Providers"));
            let table =
                v_flex().text_sm().child(header).children(rows.iter().enumerate().map(|(i, (label, providers))| {
                    h_flex()
                        .gap(px(16.))
                        .py(px(6.))
                        .when(i + 1 < rows.len(), |el| el.border_b_1().border_color(t.border.opacity(0.5)))
                        .child(cell(Some(MACHINE_WIDTH)).truncate().child(label.clone()))
                        .child(cell(Some(APP_WIDTH)))
                        .child(cell(None).flex_1().child(providers.join(", ")))
                }));
            // `AlertDialogPopup className="sm:max-w-xl"`.
            dialog
                .entrance(DialogEntrance::None)
                .centered()
                .w(px(576.))
                .p_0()
                .gap_0()
                .rounded(px(16.))
                .border_color(if t.dark { white().opacity(0.08) } else { t.foreground.opacity(0.1) })
                .close_button(false)
                .child(
                    v_flex()
                        .gap(px(8.))
                        .p(px(24.))
                        .child(
                            div()
                                .text_xl()
                                .line_height(relative(1.375))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(t.foreground)
                                .child(title.clone()),
                        )
                        .child(div().text_sm().text_color(t.muted_foreground).child(description.clone())),
                )
                .child(div().px(px(24.)).pb(px(16.)).child(table))
                .footer(
                    DialogFooter::new()
                        .px(px(24.))
                        .py(px(16.))
                        .border_t_1()
                        .border_color(t.border)
                        .bg(t.muted.opacity(0.72))
                        .rounded_b(px(15.))
                        .child(
                            Button::new("update-plan-cancel")
                                .label("Cancel")
                                .outline()
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("update-plan-ok").label("Update").primary().on_click({
                            let run = run.clone();
                            move |_, window, cx| run(window, cx)
                        })),
                )
                .on_ok(move |_, window, cx| {
                    enter(window, cx);
                    true
                })
        });
    }

    /// `updateAll`: every machine's providers at once; a failure leaves the others updating.
    fn run_provider_updates(&mut self, plan: Vec<MachineUpdate>, cx: &mut Context<Self>) {
        let requests: Vec<_> = plan
            .into_iter()
            .flat_map(|machine| {
                machine.candidates.into_iter().map(move |(driver, instance_id)| {
                    let client = machine.client.clone();
                    async move {
                        let input = json!({"provider": driver, "instanceId": instance_id});
                        client.request("server.updateProvider", input).await
                    }
                })
            })
            .collect();
        cx.spawn(async move |this, cx| {
            let failure = futures::future::join_all(requests).await.into_iter().find_map(Result::err);
            if let Some(error) = failure {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(format!("Provider update failed: {error}").into());
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

/// `max-w-32` on the machine name; the T3 Code column is empty for provider-only updates.
const MACHINE_WIDTH: f32 = 128.;
const APP_WIDTH: f32 = 56.;

/// Each machine's name and its providers' display names.
fn named(plan: &[MachineUpdate]) -> Vec<(String, Vec<String>)> {
    plan.iter()
        .map(|machine| {
            let providers = machine.candidates.iter().map(|(driver, _)| provider_display_name(driver).to_owned());
            (machine.label.clone(), providers.collect())
        })
        .collect()
}
