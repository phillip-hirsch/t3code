//! Webhook tasks: the editor's "On webhook" fields and the list row's deliveries dialog
//! (`WebhookEndpointField`, `WebhookDeliveryMode`, `WebhookFiltersField`, the webhook branch of
//! `ScheduledTaskEditorForm` and `WebhookDeliveriesDialog` in
//! `apps/web/src/components/settings/ScheduledTasksSettings.tsx`).

use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogDescription, DialogFooter, DialogHeader, DialogTitle};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Task, WeakEntity, Window,
    div, px,
};
use serde_json::{Value, json};

use super::logic::relative_label;
use super::webhook_logic::{
    self, Encoding, FilterDraft, FilterOp, FormError, MAX_WEBHOOK_FILTERS, Tone, WebhookDraft, delivery_label,
    delivery_tone,
};
use crate::app::Workspace;
use crate::rpc::RpcClient;
use crate::theme::{Tokens, tokens};

pub(super) use webhook_logic::DEFAULT_WEBHOOK_PROMPT;

/// What webhook controls need from the window's connection.
#[derive(Clone)]
pub(super) struct WebhookContext {
    client: RpcClient,
    /// `useEnvironmentHttpBaseUrl`, and the session that reads `/api/connect/link-state`.
    endpoint: Option<crate::backend::Endpoint>,
    workspace: WeakEntity<Workspace>,
}

impl WebhookContext {
    pub(super) fn new(client: RpcClient, workspace: &Entity<Workspace>, cx: &App) -> Self {
        Self { client, endpoint: workspace.read(cx).endpoint().cloned(), workspace: workspace.downgrade() }
    }
}

/// `ScheduledTaskEditorForm`'s webhook fields.
pub(super) struct WebhookFields {
    context: WebhookContext,
    /// The task being edited as last loaded; its `webhook` is the URL, `hasSecret` the stored secret.
    task: Option<Value>,
    disabled: bool,
    url: Entity<InputState>,
    max_age: Entity<InputState>,
    replay_key: Entity<InputState>,
    coalesce_key: Entity<InputState>,
    coalesce_window: Entity<InputState>,
    signature_enabled: bool,
    signature_header: Entity<InputState>,
    signature_prefix: Entity<InputState>,
    signature_secret: Entity<InputState>,
    signature_encoding: Encoding,
    filters: Vec<FilterRow>,
    saved_filters: Vec<FilterDraft>,
    next_filter_id: u64,
    rotating: bool,
    /// Resets "Copied" after 2s, like `useCopyToClipboard`.
    copied: Option<Task<()>>,
    /// The relay's `holdWebhooksWhileOffline`, once read, for a task with a relay URL.
    hold_while_offline: Option<bool>,
    _link_state: Option<Task<()>>,
}

struct FilterRow {
    /// Form identity only, so a removed row's state never passes to the row after it.
    id: u64,
    path: Entity<InputState>,
    op: FilterOp,
    value: Entity<InputState>,
}

/// The editor's webhook block; `fields` is the editor's retained [`WebhookFields`].
pub(super) fn render_webhook_fields(fields: &Entity<WebhookFields>) -> impl IntoElement {
    fields.clone()
}

impl WebhookFields {
    pub(super) fn new(
        context: WebhookContext,
        task: Option<Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let draft = WebhookDraft::from_schedule(task.as_ref().map(|task| &task["schedule"]));
        let mut input = |value: &str, placeholder: &str| {
            let (value, placeholder) = (value.to_owned(), placeholder.to_owned());
            cx.new(|cx| InputState::new(window, cx).default_value(value).placeholder(placeholder))
        };
        let url = input("", "");
        let max_age = input(&draft.max_delivery_age_minutes, "Run every request");
        let replay_key = input(&draft.replay_key, "{{headers.delivery-id}}");
        let coalesce_key = input(&draft.coalesce_key, "{{body.id}}");
        let coalesce_window = input(&draft.coalesce_window_seconds, "120");
        let signature_header = input(&draft.signature_header, "");
        let signature_prefix = input(&draft.signature_prefix, "None");
        let signature_secret = cx.new(|cx| InputState::new(window, cx).masked(true));
        let mut fields = Self {
            context,
            task: None,
            disabled: false,
            url,
            max_age,
            replay_key,
            coalesce_key,
            coalesce_window,
            signature_enabled: draft.signature_enabled,
            signature_header,
            signature_prefix,
            signature_secret,
            signature_encoding: draft.signature_encoding,
            filters: Vec::new(),
            saved_filters: draft.saved_filters,
            next_filter_id: 0,
            rotating: false,
            copied: None,
            hold_while_offline: None,
            _link_state: None,
        };
        for filter in draft.filters {
            fields.push_filter(filter, window, cx);
        }
        fields.set_task(task, window, cx);
        fields
    }

    /// The latest copy of the edited task (`liveTask`): the URL and the "Unchanged" secret follow it.
    pub(super) fn set_task(&mut self, task: Option<Value>, window: &mut Window, cx: &mut Context<Self>) {
        let relay_url = |task: Option<&Value>| task.map(|task| !task["webhook"]["url"].is_null());
        let link_changed = relay_url(task.as_ref()) != relay_url(self.task.as_ref());
        self.task = task;
        let address = self.address().map(|address| address.address).unwrap_or_default();
        if self.url.read(cx).value() != address.as_str() {
            self.url.update(cx, |url, cx| url.set_value(address, window, cx));
        }
        let has_secret = self.has_stored_secret();
        self.signature_secret.update(cx, |secret, cx| {
            secret.set_placeholder(if has_secret { "Unchanged" } else { "Shared secret" }, window, cx)
        });
        if link_changed {
            self.read_link_state(cx);
        }
        cx.notify();
    }

    /// The form is read-only while the editor saves (the web's disabled `fieldset`).
    pub(super) fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        if self.disabled != disabled {
            self.disabled = disabled;
            cx.notify();
        }
    }

    /// The upsert's `schedule`, or the errors to toast.
    pub(super) fn schedule(&self, cx: &App) -> Result<Value, Vec<FormError>> {
        self.draft(cx).schedule(self.has_stored_secret())
    }

    fn draft(&self, cx: &App) -> WebhookDraft {
        let text = |input: &Entity<InputState>| input.read(cx).value().to_string();
        WebhookDraft {
            max_delivery_age_minutes: text(&self.max_age),
            replay_key: text(&self.replay_key),
            coalesce_key: text(&self.coalesce_key),
            coalesce_window_seconds: text(&self.coalesce_window),
            signature_enabled: self.signature_enabled,
            signature_header: text(&self.signature_header),
            signature_encoding: self.signature_encoding,
            signature_prefix: text(&self.signature_prefix),
            signature_secret: text(&self.signature_secret),
            filters: self
                .filters
                .iter()
                .map(|row| FilterDraft { path: text(&row.path), op: row.op, value: text(&row.value) })
                .collect(),
            saved_filters: self.saved_filters.clone(),
        }
    }

    fn has_stored_secret(&self) -> bool {
        self.task.as_ref().is_some_and(|task| {
            task["schedule"]["type"] == "webhook" && task["webhook"]["hasSecret"].as_bool() == Some(true)
        })
    }

    fn address(&self) -> Option<webhook_logic::WebhookAddress> {
        let endpoint = self.task.as_ref()?.get("webhook").filter(|e| e.is_object())?;
        let http_base = self.context.endpoint.as_ref().map(|e| e.http_base.as_str());
        Some(webhook_logic::webhook_address(endpoint, http_base))
    }

    fn push_filter(&mut self, filter: FilterDraft, window: &mut Window, cx: &mut Context<Self>) {
        self.next_filter_id += 1;
        let placeholder = value_placeholder(filter.op);
        let path = cx.new(|cx| InputState::new(window, cx).default_value(filter.path).placeholder("body.action"));
        let value = cx.new(|cx| InputState::new(window, cx).default_value(filter.value).placeholder(placeholder));
        self.filters.push(FilterRow { id: self.next_filter_id, path, op: filter.op, value });
    }

    fn set_filter_op(&mut self, id: u64, op: FilterOp, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.filters.iter_mut().find(|row| row.id == id) {
            row.op = op;
            row.value.update(cx, |value, cx| value.set_placeholder(value_placeholder(op), window, cx));
            cx.notify();
        }
    }

    /// `WebhookDeliveryMode` reads the relay link of this environment; only a relay URL shows it.
    fn read_link_state(&mut self, cx: &mut Context<Self>) {
        self.hold_while_offline = None;
        let relay = self.task.as_ref().is_some_and(|task| task["webhook"]["url"].is_string());
        let Some(endpoint) = self.context.endpoint.clone().filter(|_| relay) else {
            self._link_state = None;
            return;
        };
        self._link_state = Some(cx.spawn(async move |this, cx| {
            let request = async move {
                let response = reqwest::Client::new()
                    .get(format!("{}/api/connect/link-state", endpoint.http_base))
                    .bearer_auth(&endpoint.bearer_token)
                    .send()
                    .await?
                    .error_for_status()?;
                response.json::<Value>().await
            };
            let Ok(Ok(state)) = crate::runtime().spawn(request).await else { return };
            let _ = this.update(cx, |this, cx| {
                this.hold_while_offline = Some(state["holdWebhooksWhileOffline"].as_bool().unwrap_or(false));
                cx.notify();
            });
        }));
    }

    fn copy_url(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.url.read(cx).value().to_string()));
        self.copied = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = this.update(cx, |this, cx| {
                this.copied = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn confirm_rotate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .title("Rotate this webhook URL?")
                .description("The current URL stops working.")
                .ok_text("Rotate")
                .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    let _ = this.update(cx, |this, cx| this.rotate(window, cx));
                    true
                })
        });
    }

    fn rotate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.task.as_ref().and_then(|task| task["id"].as_str()).map(str::to_owned) else { return };
        let client = self.context.client.clone();
        self.rotating = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("scheduledTasks.rotateWebhookToken", json!({"id": id})).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.rotating = false;
                match result {
                    Ok(result) => this.set_task(Some(result["task"].clone()), window, cx),
                    Err(error) => toast(window, cx, "Could not rotate webhook URL", &error.message()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_endpoint(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let Some(address) = self.address() else {
            return text_sm(div(), t.muted_foreground).child("The URL appears after you save.").into_any_element();
        };
        let relay = self.task.as_ref().is_some_and(|task| task["webhook"]["url"].is_string());
        v_flex()
            .gap(px(6.))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.url).readonly(true).disabled(self.disabled)))
                    .child(
                        outline_button("webhook-url-copy")
                            .icon(Icon::new(Lucide::Copy))
                            .label(if self.copied.is_some() { "Copied" } else { "Copy" })
                            .disabled(!address.copyable)
                            .on_click(cx.listener(|this, _, _, cx| this.copy_url(cx))),
                    )
                    .child(
                        outline_button("webhook-url-rotate")
                            .label("Rotate")
                            .disabled(self.rotating || self.disabled)
                            .on_click(cx.listener(|this, _, window, cx| this.confirm_rotate(window, cx))),
                    ),
            )
            .children(address.note.map(|note| text_xs(div(), t.muted_foreground).child(note)))
            .children(self.hold_while_offline.filter(|_| relay).map(|hold| {
                let workspace = self.context.workspace.clone();
                text_xs(h_flex().flex_wrap(), t.muted_foreground)
                    .child(if hold {
                        "Held for up to 24 hours while this environment is offline.\u{a0}"
                    } else {
                        "Forwarded live. Requests fail while this environment is offline.\u{a0}"
                    })
                    .child(
                        div()
                            .id("webhook-delivery-mode-link")
                            .cursor_pointer()
                            .underline()
                            .child("Change in Sharing & access")
                            .on_click(move |_, window, cx| {
                                window.close_all_dialogs(cx);
                                let _ = workspace.update(cx, |workspace, cx| workspace.open_settings("access", cx));
                            }),
                    )
            }))
            .into_any_element()
    }

    fn render_filters(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let header = h_flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                v_flex()
                    .min_w_0()
                    .gap_1()
                    .child(label("Filters", t))
                    .child(text_sm(div(), t.muted_foreground).child(
                        "Run only when every filter matches. Other requests are logged as filtered. Separate \u{201c}is one of\u{201d} values with commas; a value can't contain a comma.",
                    )),
            )
            .child(
                outline_button("webhook-filter-add")
                    .flex_none()
                    .icon(Icon::new(Lucide::Plus))
                    .label("Add filter")
                    .disabled(self.disabled || self.filters.len() >= MAX_WEBHOOK_FILTERS)
                    .on_click(cx.listener(|this, _, window, cx| {
                        let empty = FilterDraft { path: String::new(), op: FilterOp::Equals, value: String::new() };
                        this.push_filter(empty, window, cx);
                        cx.notify();
                    })),
            );
        let view = cx.entity();
        // Always stacked, as on the web: the dialog stays narrow, so one line would cut paths off.
        let rows = self.filters.iter().map(|row| {
            let id = row.id;
            let op_menu = {
                let (view, current) = (view.clone(), row.op);
                outline_button(("webhook-filter-op", id))
                    .w(px(176.))
                    .flex_none()
                    .justify_between()
                    .label(row.op.label())
                    .dropdown_caret(true)
                    .disabled(self.disabled)
                    .dropdown_menu(move |mut menu: PopupMenu, window, _| {
                        for op in FilterOp::ALL {
                            menu = menu.item(PopupMenuItem::new(op.label()).checked(op == current).on_click(
                                window.listener_for(&view, move |this, _, window, cx| {
                                    this.set_filter_op(id, op, window, cx)
                                }),
                            ));
                        }
                        menu
                    })
            };
            v_flex()
                .gap_2()
                .pt_1()
                .child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(Input::new(&row.path).disabled(self.disabled)))
                        .child(
                            Button::new(("webhook-filter-remove", id))
                                .ghost()
                                .small()
                                .icon(Icon::new(Lucide::Trash))
                                .disabled(self.disabled)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.filters.retain(|row| row.id != id);
                                    cx.notify();
                                })),
                        ),
                )
                .child(h_flex().items_center().gap_2().child(op_menu).when(row.op != FilterOp::Exists, |line| {
                    line.child(div().flex_1().min_w_0().child(Input::new(&row.value).disabled(self.disabled)))
                }))
        });
        v_flex().gap_2().child(header).children(rows).into_any_element()
    }

    fn render_signature(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let (view, current) = (cx.entity(), self.signature_encoding);
        let encoding = outline_button("webhook-signature-encoding")
            .w_full()
            .justify_between()
            .label(self.signature_encoding.label())
            .dropdown_caret(true)
            .disabled(self.disabled)
            .dropdown_menu(move |mut menu: PopupMenu, window, _| {
                for encoding in [Encoding::Hex, Encoding::Base64] {
                    menu = menu.item(PopupMenuItem::new(encoding.label()).checked(encoding == current).on_click(
                        window.listener_for(&view, move |this, _, _, cx| {
                            this.signature_encoding = encoding;
                            cx.notify();
                        }),
                    ));
                }
                menu
            });
        let input = |state: &Entity<InputState>| Input::new(state).disabled(self.disabled);
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        v_flex().min_w_0().gap_1().child(label("Require signature", t)).child(
                            text_sm(div(), t.muted_foreground)
                                .child("Reject requests without a valid HMAC-SHA256 signature of the body."),
                        ),
                    )
                    .child(
                        Switch::new("webhook-signature")
                            .checked(self.signature_enabled)
                            .disabled(self.disabled)
                            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                this.signature_enabled = *checked;
                                cx.notify();
                            })),
                    ),
            )
            .when(self.signature_enabled, |column| {
                column.child(
                    v_flex()
                        .gap_3()
                        .child(
                            h_flex()
                                .gap_3()
                                .child(field("Header", None, input(&self.signature_header), t))
                                .child(field("Prefix", None, input(&self.signature_prefix), t)),
                        )
                        .child(h_flex().gap_3().child(field("Encoding", None, encoding, t)).child(field(
                            "Secret",
                            None,
                            input(&self.signature_secret).mask_toggle(),
                            t,
                        ))),
                )
            })
            .into_any_element()
    }
}

impl Render for WebhookFields {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *tokens(cx);
        let input = |state: &Entity<InputState>| Input::new(state).disabled(self.disabled);
        let note = |text: &'static str| text_xs(div(), t.muted_foreground).child(text);
        v_flex()
            .gap_4()
            .child(self.render_endpoint(&t, cx))
            .child(note(
                "Each request runs the prompt. Use {{body.path}}, {{headers.name}}, {{query.name}}, {{body}} or {{request}} in the prompt; only what it names reaches the agent.",
            ))
            .child(field("Skip requests older than", Some("minutes, optional"), input(&self.max_age), &t))
            .child(self.render_filters(&t, cx))
            .child(field("Duplicate request key", Some("template, optional"), input(&self.replay_key), &t))
            .child(note("Requests with the same key run only once within 7 days."))
            .child(field("Coalesce key", Some("template, optional"), input(&self.coalesce_key), &t))
            .child(field("Coalesce window", Some("seconds, 10\u{2013}3600"), input(&self.coalesce_window), &t))
            .child(note(
                "Coalescing runs the latest payload once when the window from the first delivery closes. Empty keys run normally.",
            ))
            .child(self.render_signature(&t, cx))
    }
}

fn value_placeholder(op: FilterOp) -> &'static str {
    if op == FilterOp::In { "opened, reopened" } else { "opened" }
}

// ---------------------------------------------------------------------------
// Deliveries

/// Adds the row menu's "Deliveries" entry for a webhook task; other tasks get the menu unchanged.
pub(super) fn webhook_menu_item(menu: PopupMenu, context: &WebhookContext, task: &Value) -> PopupMenu {
    if task["schedule"]["type"] != "webhook" {
        return menu;
    }
    let (client, task) = (context.client.clone(), task.clone());
    menu.item(
        PopupMenuItem::new("Deliveries")
            .icon(Icon::new(Lucide::Inbox))
            .on_click(move |_, window, cx| open_deliveries(client.clone(), &task, window, cx)),
    )
}

/// `WebhookDeliveriesDialog`: recent requests to a task's URL, and one request in full.
pub(super) fn open_deliveries(client: RpcClient, task: &Value, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| {
        let mut deliveries = Deliveries {
            client,
            task_id: task["id"].as_str().unwrap_or_default().to_owned(),
            title: task["title"].as_str().unwrap_or_default().to_owned().into(),
            list: None,
            list_error: None,
            listing: None,
            selected: None,
        };
        deliveries.refresh(cx);
        deliveries
    });
    window.open_dialog(cx, move |dialog, _, _| dialog.w(px(672.)).p_0().close_button(false).child(view.clone()));
}

struct Deliveries {
    client: RpcClient,
    task_id: String,
    title: SharedString,
    /// Kept while a refresh runs, as the web query keeps its data.
    list: Option<Vec<Value>>,
    list_error: Option<SharedString>,
    /// The list request in flight; "Refresh" waits for it.
    listing: Option<Task<()>>,
    selected: Option<Selected>,
}

struct Selected {
    delivery: Option<Value>,
    error: Option<SharedString>,
    _load: Task<()>,
}

impl Deliveries {
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (client, id) = (self.client.clone(), self.task_id.clone());
        self.listing = Some(cx.spawn(async move |this, cx| {
            let input = json!({"id": id, "acceptAnyOutcome": true});
            let result = client.request("scheduledTasks.listWebhookDeliveries", input).await;
            let _ = this.update(cx, |this, cx| {
                this.listing = None;
                match result {
                    Ok(result) => {
                        this.list = Some(result["deliveries"].as_array().cloned().unwrap_or_default());
                        this.list_error = None;
                    }
                    Err(error) => this.list_error = Some(error.message().into()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn select(&mut self, delivery_id: String, cx: &mut Context<Self>) {
        let (client, id) = (self.client.clone(), self.task_id.clone());
        let load = cx.spawn(async move |this, cx| {
            let input = json!({"id": id, "deliveryId": delivery_id, "acceptAnyOutcome": true});
            let result = client.request("scheduledTasks.getWebhookDelivery", input).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(selected) = &mut this.selected {
                    match result {
                        Ok(result) => selected.delivery = Some(result["delivery"].clone()),
                        Err(error) => selected.error = Some(error.message().into()),
                    }
                }
                cx.notify();
            });
        });
        self.selected = Some(Selected { delivery: None, error: None, _load: load });
        cx.notify();
    }

    fn render_list(&self, t: &Tokens, now: i64, cx: &mut Context<Self>) -> AnyElement {
        let Some(list) = &self.list else {
            return text_sm(div(), t.muted_foreground).child("Loading deliveries\u{2026}").into_any_element();
        };
        if list.is_empty() {
            return text_sm(div(), t.muted_foreground).child("No requests yet.").into_any_element();
        }
        v_flex()
            .children(list.iter().enumerate().map(|(index, delivery)| {
                let id = delivery["id"].as_str().unwrap_or_default().to_owned();
                let pending = delivery["pendingWindow"].as_bool() == Some(true);
                let outcome = delivery["outcome"].as_str().unwrap_or_default();
                let missing = delivery["missingFields"].as_array().map_or(0, Vec::len);
                let filtered_reason = delivery["error"].as_str().filter(|_| outcome == "filtered").map(str::to_owned);
                let muted = |text: String| div().text_color(t.muted_foreground).child(text);
                text_sm(h_flex(), t.foreground)
                    .id(SharedString::from(format!("delivery-{id}")))
                    .w_full()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .py_2()
                    .when(index > 0, |row| row.border_t_1().border_color(t.border))
                    .cursor_pointer()
                    .hover(|row| row.bg(t.accent.opacity(0.4)))
                    .child(badge(delivery_tone(outcome, pending), delivery_label(outcome, pending), t))
                    .child(div().child(delivery["method"].as_str().unwrap_or_default().to_owned()))
                    .child(muted(relative_label(delivery["receivedAt"].as_str(), now)))
                    .children(filtered_reason.map(muted))
                    .when(missing > 0, |row| {
                        row.child(muted(format!("{missing} empty placeholder{}", if missing == 1 { "" } else { "s" })))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.select(id.clone(), cx)))
            }))
            .into_any_element()
    }

    fn render_delivery(delivery: &Value, t: &Tokens, now: i64) -> AnyElement {
        let pending = delivery["pendingWindow"].as_bool() == Some(true);
        let outcome = delivery["outcome"].as_str().unwrap_or_default();
        let missing: Vec<&str> = delivery["missingFields"]
            .as_array()
            .map(|f| f.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let headers = delivery["headers"]
            .as_object()
            .map(|headers| {
                headers
                    .iter()
                    .map(|(name, value)| format!("{name}: {}", value.as_str().unwrap_or_default()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let query = delivery["query"].as_str().filter(|q| !q.is_empty());
        let body = delivery["body"].as_str().filter(|b| !b.is_empty()).unwrap_or("(empty)");
        let prompt = delivery["renderedPrompt"].as_str().unwrap_or(if pending {
            "Run scheduled when the window closes; no run started yet."
        } else {
            "No run was started for this request."
        });
        v_flex()
            .gap_4()
            .child(
                text_sm(h_flex(), t.foreground)
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(badge(delivery_tone(outcome, pending), delivery_label(outcome, pending), t))
                    .child(format!(
                        "{} \u{b7} {}",
                        delivery["method"].as_str().unwrap_or_default(),
                        relative_label(delivery["receivedAt"].as_str(), now)
                    ))
                    .when(delivery["signatureVerified"].as_bool() == Some(true), |line| {
                        line.child(div().text_color(t.muted_foreground).child("Signature verified"))
                    }),
            )
            .children(delivery["error"].as_str().map(|error| {
                let color = if outcome == "filtered" { t.muted_foreground } else { t.error_foreground };
                text_sm(div(), color).child(error.to_owned())
            }))
            .child(block(if pending { "Scheduled run" } else { "Prompt sent to the agent" }, prompt, "prompt", t))
            .when(!missing.is_empty(), |column| {
                column.child(
                    text_sm(div(), t.muted_foreground).child(format!("Empty placeholders: {}", missing.join(", "))),
                )
            })
            .child(block("Headers", &headers, "headers", t))
            .children(query.map(|query| block("Query", query, "query", t)))
            .child(block(
                if delivery["bodyTruncated"].as_bool() == Some(true) { "Body (truncated)" } else { "Body" },
                body,
                "body",
                t,
            ))
            .into_any_element()
    }
}

impl Render for Deliveries {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *tokens(cx);
        let now = crate::app::sidebar::now_ms();
        let error = match &self.selected {
            Some(selected) => selected.error.clone(),
            None => self.list_error.clone(),
        };
        let content = match &self.selected {
            Some(Selected { delivery: Some(delivery), .. }) => Self::render_delivery(delivery, &t, now),
            Some(Selected { error: None, .. }) => {
                text_sm(div(), t.muted_foreground).child("Loading delivery\u{2026}").into_any_element()
            }
            Some(_) => div().into_any_element(),
            None => self.render_list(&t, now, cx),
        };
        let back_or_refresh = if self.selected.is_some() {
            outline_button("deliveries-back").label("Back").on_click(cx.listener(|this, _, _, cx| {
                this.selected = None;
                cx.notify();
            }))
        } else {
            outline_button("deliveries-refresh")
                .label("Refresh")
                .disabled(self.listing.is_some())
                .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
        };
        v_flex()
            .child(
                DialogHeader::new()
                    .p_6()
                    .pb_3()
                    .child(DialogTitle::new().child(format!("Deliveries \u{b7} {}", self.title)))
                    .child(DialogDescription::new().child("Recent requests to this task's webhook URL.")),
            )
            .child(
                div()
                    .id("deliveries-panel")
                    .px_6()
                    .pb_1()
                    .max_h(px(520.))
                    .overflow_y_scroll()
                    .children(error.map(|error| text_sm(div(), t.error_foreground).child(error)))
                    .child(content),
            )
            .child(
                DialogFooter::new().px_6().py_4().gap_2().justify_end().child(back_or_refresh).child(
                    Button::new("deliveries-done")
                        .primary()
                        .small()
                        .label("Done")
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            )
    }
}

/// `DeliveryBlock`: a label over a scrolling monospace box.
fn block(title: &'static str, text: &str, id: &'static str, t: &Tokens) -> impl IntoElement {
    v_flex().gap_1().child(label(title, t)).child(
        div()
            .id(id)
            .max_h(px(256.))
            .overflow_y_scroll()
            .rounded(px(6.))
            .bg(t.muted)
            .p_2()
            .font_family(t.fonts.mono)
            .text_xs()
            .line_height(px(16.))
            .text_color(t.foreground)
            .child(text.to_owned()),
    )
}

// ---------------------------------------------------------------------------
// Shared bits

/// The web `Badge` (`variant` from [`Tone`]), default size.
pub(super) fn badge(tone: Tone, text: impl Into<SharedString>, t: &Tokens) -> impl IntoElement {
    let tint = |color: gpui_kit::Hsla| (color.opacity(if t.dark { 0.16 } else { 0.08 }), color);
    let (bg, fg) = match tone {
        Tone::Success => tint(t.success_foreground),
        Tone::Warning => (t.warning.opacity(if t.dark { 0.16 } else { 0.08 }), t.warning_foreground),
        Tone::Error => (t.error.opacity(if t.dark { 0.16 } else { 0.08 }), t.error_foreground),
        Tone::Outline => (if t.dark { t.input.opacity(0.32) } else { t.background }, t.foreground),
    };
    div()
        .flex_none()
        .h(px(18.))
        .min_w(px(18.))
        .px(px(3.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .border_1()
        .border_color(if tone == Tone::Outline { t.input } else { gpui_kit::transparent_black() })
        .bg(bg)
        .text_color(fg)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .child(text.into())
}

/// The editor's `Field`: a label (with an optional muted hint on the right) over its control.
fn field(title: &'static str, hint: Option<&'static str>, control: impl IntoElement, t: &Tokens) -> impl IntoElement {
    v_flex()
        .flex_1()
        .min_w_0()
        .gap(px(6.))
        .child(h_flex().items_baseline().justify_between().child(label(title, t)).children(
            hint.map(|hint| div().text_size(px(11.)).text_color(t.muted_foreground.opacity(0.8)).child(hint)),
        ))
        .child(control)
}

/// `Label`: text-sm/4, medium.
fn label(text: &'static str, t: &Tokens) -> impl IntoElement {
    div().text_sm().line_height(px(16.)).font_weight(FontWeight::MEDIUM).text_color(t.foreground).child(text)
}

fn text_sm<E: gpui_kit::Styled>(el: E, color: gpui_kit::Hsla) -> E {
    el.text_sm().line_height(px(20.)).text_color(color)
}

fn text_xs<E: gpui_kit::Styled>(el: E, color: gpui_kit::Hsla) -> E {
    el.text_xs().line_height(px(16.)).text_color(color)
}

/// `Button variant="outline" size="sm"`: 28px tall.
fn outline_button(id: impl Into<gpui_kit::ElementId>) -> Button {
    Button::new(id).outline().small().h(px(28.))
}

fn toast(window: &mut Window, cx: &mut App, title: &'static str, description: &str) {
    window.push_notification(Notification::error(description.to_owned()).title(title), cx);
}
