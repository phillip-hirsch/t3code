//! ACP Registry pieces of the Providers section: the registry search step of the Add provider
//! dialog (`AcpRegistrySearchStep.tsx`), its agent icons (`AcpRegistryIcon.tsx`), and an ACP
//! instance's native sessions and agent providers (`AcpSessionManagementSection.tsx`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures::FutureExt as _;
use futures::future::{BoxFuture, Shared};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, EventEmitter, FontWeight, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, WeakEntity, Window, div, px, svg,
    uniform_list,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::add::{DRIVERS, is_configured_acp_agent, settings_row};
use crate::app::Workspace;
use crate::rpc::RpcError;
use crate::theme::{FONT_MONO, Tokens, tokens};

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);
/// `ACP_REGISTRY_MAX_AGENTS`.
const MAX_AGENTS: usize = 512;
const ICON_HOST: &str = "cdn.agentclientprotocol.com";
const MAX_ICON_BYTES: usize = 512 * 1024;
const RESULT_ROW_HEIGHT: f32 = 58.;

/// The text the web shows for a failed RPC: the error's `message`, else its description.
pub(super) fn rpc_message(error: &RpcError) -> String {
    match error {
        RpcError::Failure(value) => value.get("message").and_then(Value::as_str).map(str::to_owned),
        _ => None,
    }
    .unwrap_or_else(|| error.to_string())
}

/// `AcpRegistrySearchAgent` (`packages/contracts/src/acpRegistry.ts`).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RegistryAgent {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub website: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    pub distribution: String,
}

/// Registry ids of agents T3 Code runs with its own driver; Add provider offers those drivers instead.
fn is_first_class_agent(id: &str) -> bool {
    DRIVERS.iter().any(|driver| driver.acp_ids.contains(&id))
}

/// `resolveOfficialAcpRegistryIconUrl`: only credential-free HTTPS URLs on the official CDN.
pub(super) fn official_icon_url(icon: Option<&str>) -> Option<String> {
    let rest = icon?.strip_prefix("https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    let host = authority.to_ascii_lowercase();
    let host = host.strip_suffix(":443").unwrap_or(&host);
    (host == ICON_HOST).then(|| format!("https://{ICON_HOST}{}", if path.is_empty() { "/" } else { path }))
}

// MARK: Agent icons

type IconFuture = Shared<BoxFuture<'static, Option<SharedString>>>;

/// One fetch per icon URL per run; the bytes are served to `svg()` from `remote/acp/…` so they
/// take the text color, like the web's alpha recolor filter. Non-SVG icons keep the fallback glyph.
fn load_icon(url: &str) -> IconFuture {
    static ICONS: OnceLock<Mutex<HashMap<String, IconFuture>>> = OnceLock::new();
    let mut icons = ICONS.get_or_init(Default::default).lock().unwrap();
    icons
        .entry(url.to_owned())
        .or_insert_with(|| {
            let url = url.to_owned();
            let path: SharedString = format!("remote/acp/{:016x}.svg", fxhash(&url)).into();
            let fetch = crate::runtime().spawn(fetch_icon(url));
            async move {
                let bytes = fetch.await.ok().flatten()?;
                crate::assets::put_remote(&path, bytes);
                Some(path)
            }
            .boxed()
            .shared()
        })
        .clone()
}

async fn fetch_icon(url: String) -> Option<Arc<[u8]>> {
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().ok()?;
    let response = client.get(&url).send().await.ok()?;
    let svg = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(';').next().unwrap_or_default().trim() == "image/svg+xml");
    if !response.status().is_success() || !svg || response.content_length().unwrap_or(0) > MAX_ICON_BYTES as u64 {
        return None;
    }
    let bytes = response.bytes().await.ok()?;
    (bytes.len() <= MAX_ICON_BYTES).then(|| bytes.to_vec().into())
}

fn fxhash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

/// `AcpRegistryAgentIcon`: the fetched icon, else the ACP glyph.
fn agent_icon(path: Option<&SharedString>, t: &Tokens) -> impl IntoElement {
    div()
        .size(px(24.))
        .flex_none()
        .rounded(px(6.))
        .overflow_hidden()
        .bg(t.muted)
        .flex()
        .items_center()
        .justify_center()
        .text_color(t.muted_foreground)
        .child(match path {
            Some(path) => svg().path(path.clone()).size_full().text_color(t.muted_foreground),
            None => svg().path("icons/app/acp.svg").size(px(16.)).text_color(t.muted_foreground),
        })
}

// MARK: Registry search (`useAcpRegistrySearch` + `AcpRegistrySearchStep`)

pub(super) enum AcpSearchEvent {
    Prepared(RegistryAgent),
    Manual,
    Local,
}

pub(super) struct AcpRegistrySearch {
    workspace: WeakEntity<Workspace>,
    query: Entity<InputState>,
    /// The query the shown (or loading) results are for.
    submitted: Option<String>,
    results: Option<Vec<RegistryAgent>>,
    pending: bool,
    error: Option<SharedString>,
    preparing: Option<String>,
    prepare_error: Option<SharedString>,
    icons: HashMap<String, SharedString>,
    _search: Option<Task<()>>,
    _debounce: Option<Task<()>>,
    _prepare: Option<Task<()>>,
    _icons: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<AcpSearchEvent> for AcpRegistrySearch {}

impl AcpRegistrySearch {
    pub(super) fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search agents…"));
        let subscription = cx.subscribe(&query, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Change => {
                this.prepare_error = None;
                this._debounce = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SEARCH_DEBOUNCE).await;
                    let _ = this.update(cx, |this, cx| this.search(false, cx));
                }));
            }
            InputEvent::PressEnter { .. } => {
                this.prepare_error = None;
                this._debounce = None;
                this.search(true, cx);
            }
            _ => {}
        });
        let mut search = Self {
            workspace,
            query,
            submitted: None,
            results: None,
            pending: false,
            error: None,
            preparing: None,
            prepare_error: None,
            icons: HashMap::new(),
            _search: None,
            _debounce: None,
            _prepare: None,
            _icons: Vec::new(),
            _subscriptions: vec![subscription],
        };
        search.search(false, cx);
        search
    }

    pub(super) fn is_preparing(&self) -> bool {
        self.preparing.is_some()
    }

    /// Searches the current text; `refresh` repeats an unchanged query (Enter).
    fn search(&mut self, refresh: bool, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().trim().to_owned();
        if !refresh && self.submitted.as_deref() == Some(query.as_str()) {
            return;
        }
        let Some(client) = self.workspace.upgrade().and_then(|w| w.read(cx).client()) else { return };
        let input = if query.is_empty() { json!({"query": "", "limit": MAX_AGENTS}) } else { json!({"query": query}) };
        self.submitted = Some(query);
        self.pending = true;
        cx.notify();
        self._search = Some(cx.spawn(async move |this, cx| {
            let result = client.request("server.searchAcpRegistry", input).await;
            let _ = this.update(cx, |this, cx| {
                this.pending = false;
                match result.map(|value| Vec::<RegistryAgent>::deserialize(&value["agents"])) {
                    Ok(Ok(agents)) => {
                        this.error = None;
                        let agents: Vec<_> = agents.into_iter().filter(|a| !is_first_class_agent(&a.id)).collect();
                        this.load_icons(&agents, cx);
                        this.results = Some(agents);
                    }
                    Ok(Err(error)) => this.error = Some(error.to_string().into()),
                    Err(error) => this.error = Some(rpc_message(&error).into()),
                }
                cx.notify();
            });
        }));
    }

    fn load_icons(&mut self, agents: &[RegistryAgent], cx: &mut Context<Self>) {
        for agent in agents {
            let Some(url) = official_icon_url(agent.icon.as_deref()) else { continue };
            if self.icons.contains_key(&agent.id) {
                continue;
            }
            let id = agent.id.clone();
            let icon = load_icon(&url);
            self._icons.push(cx.spawn(async move |this, cx| {
                if let Some(path) = icon.await {
                    let _ = this.update(cx, |this, cx| {
                        this.icons.insert(id, path);
                        cx.notify();
                    });
                }
            }));
        }
    }

    /// `prepare`: installs or checks the agent's distribution, then hands the agent to the dialog.
    fn prepare(&mut self, agent: RegistryAgent, cx: &mut Context<Self>) {
        let Some(client) = self.workspace.upgrade().and_then(|w| w.read(cx).client()) else { return };
        self.prepare_error = None;
        self.preparing = Some(agent.id.clone());
        cx.notify();
        self._prepare = Some(cx.spawn(async move |this, cx| {
            let result = client.request("server.prepareAcpRegistryAgent", json!({"agentId": agent.id})).await;
            let _ = this.update(cx, |this, cx| {
                this.preparing = None;
                cx.notify();
                match result {
                    Ok(prepared) => {
                        let text = |key: &str, fallback: &str| {
                            prepared.get(key).and_then(Value::as_str).unwrap_or(fallback).to_owned()
                        };
                        let agent = RegistryAgent {
                            id: text("agentId", &agent.id),
                            version: text("version", &agent.version),
                            distribution: text("distribution", &agent.distribution),
                            ..agent
                        };
                        cx.emit(AcpSearchEvent::Prepared(agent));
                    }
                    Err(RpcError::Disconnected(_)) => {}
                    Err(error) => {
                        let message = rpc_message(&error);
                        this.prepare_error = Some(if message.trim().is_empty() {
                            "The ACP could not be prepared.".into()
                        } else {
                            message.into()
                        });
                    }
                }
            });
        }));
    }

    fn render_row(&self, agent: &RegistryAgent, added: bool, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let preparing = self.preparing.as_deref() == Some(agent.id.as_str());
        let progress = if agent.distribution == "binary" { "Downloading" } else { "Preparing" };
        let about = agent.website.clone().filter(|w| !w.is_empty()).or_else(|| agent.repository.clone());
        let row_id = |part: &str| ElementId::Name(format!("acp-agent-{}-{part}", agent.id).into());
        let picked = agent.clone();
        h_flex()
            .w_full()
            .h(px(RESULT_ROW_HEIGHT))
            .pr(px(10.))
            .gap(px(12.))
            .justify_between()
            .child(
                h_flex().flex_1().min_w_0().gap(px(12.)).child(agent_icon(self.icons.get(&agent.id), t)).child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_size(px(14.))
                                .font_weight(FontWeight::MEDIUM)
                                .child(agent.name.clone()),
                        )
                        .when(!agent.description.is_empty(), |col| {
                            col.child(
                                div()
                                    .mt(px(2.))
                                    .truncate()
                                    .text_size(px(12.))
                                    .text_color(t.muted_foreground)
                                    .child(agent.description.clone()),
                            )
                        }),
                ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(6.))
                    .children(about.map(|url| {
                        Button::new(row_id("about"))
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(Lucide::ExternalLink))
                            .tooltip(format!("About {}", agent.name))
                            .on_click(move |_, _, cx| cx.open_url(&url))
                    }))
                    .child(
                        Button::new(row_id("add"))
                            .xsmall()
                            .with_variant(if preparing { ButtonVariant::Secondary } else { ButtonVariant::Default })
                            .when(!preparing, |b| b.outline())
                            .label(if added {
                                "Added"
                            } else if preparing {
                                progress
                            } else {
                                "Add"
                            })
                            .disabled(added || self.preparing.is_some())
                            .on_click(cx.listener(move |this, _, _, cx| this.prepare(picked.clone(), cx))),
                    ),
            )
            .into_any_element()
    }
}

impl Render for AcpRegistrySearch {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *tokens(cx);
        let preparing = self.preparing.is_some();
        let instances =
            self.workspace.upgrade().map(|w| w.read(cx).server_info.provider_instances.clone()).unwrap_or_default();
        let search_bar = h_flex()
            .flex_wrap()
            .gap(px(8.))
            .child(
                div().min_w(px(160.)).flex_1().child(
                    Input::new(&self.query)
                        .small()
                        .disabled(preparing)
                        .prefix(Icon::new(Lucide::Search).size(px(14.)).text_color(t.muted_foreground)),
                ),
            )
            .child(
                Button::new("acp-enter-manually")
                    .ghost()
                    .small()
                    .label("Enter manually")
                    .disabled(preparing)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AcpSearchEvent::Manual))),
            )
            .child(
                Button::new("acp-local-command")
                    .outline()
                    .small()
                    .label("Local ACP command")
                    .disabled(preparing)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AcpSearchEvent::Local))),
            );
        let error = self.prepare_error.clone().or_else(|| self.error.clone());
        let body: Option<AnyElement> = match &self.results {
            None if self.pending => Some(
                div()
                    .min_h(px(80.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(14.))
                    .text_color(t.muted_foreground)
                    .child("Searching the registry...")
                    .into_any_element(),
            ),
            None => None,
            Some(results) if results.is_empty() => Some(
                v_flex()
                    .min_h(px(112.))
                    .items_center()
                    .justify_center()
                    .px(px(16.))
                    .rounded(px(16.))
                    .border_1()
                    .border_dashed()
                    .border_color(t.border)
                    .child(div().text_size(px(14.)).font_weight(FontWeight::MEDIUM).child("No compatible agents found"))
                    .child(
                        div()
                            .mt(px(4.))
                            .text_size(px(12.))
                            .text_color(t.muted_foreground)
                            .child("Try a broader search."),
                    )
                    .into_any_element(),
            ),
            Some(results) => {
                let count = results.len();
                let added: Vec<bool> = results.iter().map(|a| is_configured_acp_agent(&instances, &a.id)).collect();
                Some(
                    uniform_list(
                        "acp-registry-results",
                        count,
                        cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                            let t = *tokens(cx);
                            let results = this.results.clone().unwrap_or_default();
                            range
                                .filter_map(|ix| {
                                    let agent = results.get(ix)?;
                                    Some(this.render_row(agent, added.get(ix).copied().unwrap_or(false), &t, cx))
                                })
                                .collect()
                        }),
                    )
                    .h(px((count as f32 * RESULT_ROW_HEIGHT).min(256.)))
                    .into_any_element(),
                )
            }
        };
        v_flex()
            .gap(px(12.))
            .child(search_bar)
            .children(error.map(|error| {
                div()
                    .px(px(14.))
                    .py(px(10.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(t.error.opacity(0.32))
                    .bg(t.error.opacity(0.04))
                    .text_size(px(14.))
                    .text_color(t.error_foreground)
                    .child(error)
            }))
            .children(body)
    }
}

// MARK: Native sessions and agent providers (`AcpSessionManagementSection`)

/// `AcpRegistrySession`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeSession {
    session_id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    imported_thread_id: Option<String>,
}

/// `AcpRegistryConfigurableProvider`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentProvider {
    provider_id: String,
    #[serde(default)]
    supported: Vec<String>,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    current: Option<AgentProviderRoute>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentProviderRoute {
    api_type: String,
    base_url: String,
}

/// An agent provider's unsaved edits; the inputs hold the base URL and the write-only headers.
struct ProviderDraft {
    api_type: String,
    base_url: Entity<InputState>,
    headers: Entity<InputState>,
}

/// `parseProviderHeaders`: empty is no headers, else a JSON object of strings.
fn parse_provider_headers(text: &str) -> Result<Option<Map<String, Value>>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    match serde_json::from_str::<Value>(text).map_err(|e| e.to_string())? {
        Value::Object(map) if map.values().all(Value::is_string) => Ok(Some(map)),
        _ => Err("Headers must be a JSON object with string values.".into()),
    }
}

pub(super) struct AcpSessions {
    workspace: WeakEntity<Workspace>,
    instance_id: String,
    read_only: bool,
    project_id: Option<String>,
    sessions: Vec<NativeSession>,
    next_cursor: Option<String>,
    loading: bool,
    importing: Option<String>,
    deleting: Option<String>,
    logging_out: bool,
    providers: Vec<AgentProvider>,
    loading_providers: bool,
    saving_provider: Option<String>,
    drafts: HashMap<String, ProviderDraft>,
    _tasks: Vec<Task<()>>,
}

impl AcpSessions {
    fn new(workspace: WeakEntity<Workspace>, instance_id: String, read_only: bool) -> Self {
        Self {
            workspace,
            instance_id,
            read_only,
            project_id: None,
            sessions: Vec::new(),
            next_cursor: None,
            loading: false,
            importing: None,
            deleting: None,
            logging_out: false,
            providers: Vec::new(),
            loading_providers: false,
            saving_provider: None,
            drafts: HashMap::new(),
            _tasks: Vec::new(),
        }
    }

    fn pending(&self) -> bool {
        self.loading
            || self.importing.is_some()
            || self.deleting.is_some()
            || self.loading_providers
            || self.saving_provider.is_some()
    }

    fn select_project(&mut self, project_id: String, cx: &mut Context<Self>) {
        self.project_id = Some(project_id);
        self.sessions.clear();
        self.next_cursor = None;
        self.providers.clear();
        self.drafts.clear();
        cx.notify();
    }

    /// Sends `method`, then hands the result to `done` on this entity.
    fn call(
        &mut self,
        method: &'static str,
        input: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<Value, RpcError>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some(client) = self.workspace.upgrade().and_then(|w| w.read(cx).client()) else { return };
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let result = client.request(method, input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                done(this, result, window, cx);
                cx.notify();
            });
        }));
    }

    fn load_sessions(&mut self, cursor: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project_id) = self.project_id.clone().filter(|_| !self.loading) else { return };
        self.loading = true;
        let mut input = json!({"instanceId": self.instance_id, "projectId": project_id});
        if let Some(cursor) = &cursor {
            input["cursor"] = cursor.clone().into();
        }
        let append = cursor.is_some();
        self.call("server.listAcpRegistrySessions", input, window, cx, move |this, result, window, cx| {
            this.loading = false;
            let page = result.and_then(|value| {
                let sessions = Vec::<NativeSession>::deserialize(&value["sessions"])
                    .map_err(|e| RpcError::Defect(e.to_string()))?;
                Ok((sessions, value["nextCursor"].as_str().map(str::to_owned)))
            });
            match page {
                Ok((sessions, next)) => {
                    if !append {
                        this.sessions.clear();
                    }
                    this.sessions.extend(sessions);
                    this.next_cursor = next;
                }
                Err(error) => report_failure(window, cx, "Could not list ACP sessions", &error),
            }
        });
    }

    fn import_session(&mut self, session: NativeSession, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project_id) = self.project_id.clone().filter(|_| self.importing.is_none()) else { return };
        self.importing = Some(session.session_id.clone());
        let input = json!({
            "instanceId": self.instance_id,
            "projectId": project_id,
            "sessionId": session.session_id,
            "title": session.title,
            "updatedAt": session.updated_at,
        });
        self.call("server.importAcpRegistrySession", input, window, cx, move |this, result, window, cx| {
            this.importing = None;
            match result {
                Ok(value) => {
                    let thread_id = value["threadId"].as_str().map(str::to_owned);
                    for candidate in &mut this.sessions {
                        if candidate.session_id == session.session_id {
                            candidate.imported_thread_id = thread_id.clone();
                        }
                    }
                    let title = if value["imported"].as_bool() == Some(true) {
                        "ACP session imported"
                    } else {
                        "ACP session already imported"
                    };
                    window.push_notification(Notification::success(title), cx);
                }
                Err(error) => report_failure(window, cx, "Could not import ACP session", &error),
            }
        });
    }

    fn confirm_delete_session(&mut self, session: NativeSession, window: &mut Window, cx: &mut Context<Self>) {
        if self.project_id.is_none() || self.deleting.is_some() || session.imported_thread_id.is_some() {
            return;
        }
        let message = format!(
            "Permanently delete native ACP session \"{}\"?",
            session.title.as_deref().unwrap_or(&session.session_id)
        );
        let view = cx.entity();
        confirm_destructive(window, cx, message, "Delete", move |window, cx| {
            view.update(cx, |this, cx| this.delete_session(session.clone(), window, cx));
        });
    }

    fn delete_session(&mut self, session: NativeSession, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project_id) = self.project_id.clone().filter(|_| self.deleting.is_none()) else { return };
        self.deleting = Some(session.session_id.clone());
        let input = json!({"instanceId": self.instance_id, "projectId": project_id, "sessionId": session.session_id});
        self.call("server.deleteAcpRegistrySession", input, window, cx, move |this, result, window, cx| {
            this.deleting = None;
            match result {
                Ok(_) => {
                    this.sessions.retain(|candidate| candidate.session_id != session.session_id);
                    window.push_notification(Notification::success("ACP session deleted"), cx);
                }
                Err(error) => report_failure(window, cx, "Could not delete ACP session", &error),
            }
        });
    }

    /// Lists the agent's providers. Drafts reset for `reset` (just saved) and new providers; others keep their edits.
    fn load_providers(&mut self, reset: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project_id) = self.project_id.clone().filter(|_| !self.loading_providers) else { return };
        self.loading_providers = true;
        let input = json!({"instanceId": self.instance_id, "projectId": project_id});
        self.call("server.listAcpRegistryProviders", input, window, cx, move |this, result, window, cx| {
            this.loading_providers = false;
            let providers = result.and_then(|value| {
                Vec::<AgentProvider>::deserialize(&value["providers"]).map_err(|e| RpcError::Defect(e.to_string()))
            });
            match providers {
                Ok(providers) => {
                    let mut drafts = std::mem::take(&mut this.drafts);
                    for provider in &providers {
                        let keep = reset.as_ref().is_some_and(|id| *id != provider.provider_id);
                        let draft = drafts.remove(&provider.provider_id).filter(|_| keep);
                        let draft = draft.unwrap_or_else(|| new_draft(provider, window, cx));
                        this.drafts.insert(provider.provider_id.clone(), draft);
                    }
                    this.providers = providers;
                }
                Err(error) => report_failure(window, cx, "Could not list ACP providers", &error),
            }
        });
    }

    fn save_provider(&mut self, provider_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project_id) = self.project_id.clone() else { return };
        if self.saving_provider.is_some() || self.loading_providers {
            return;
        }
        let Some(draft) = self.drafts.get(&provider_id) else { return };
        let base_url = draft.base_url.read(cx).value().to_string();
        if draft.api_type.is_empty() || base_url.is_empty() {
            return;
        }
        let headers = match parse_provider_headers(&draft.headers.read(cx).value()) {
            Ok(headers) => headers,
            Err(error) => {
                window.push_notification(Notification::error(error).title("Invalid provider headers"), cx);
                return;
            }
        };
        let mut input = json!({
            "instanceId": self.instance_id,
            "projectId": project_id,
            "providerId": provider_id,
            "apiType": draft.api_type,
            "baseUrl": base_url,
        });
        if let Some(headers) = headers {
            input["headers"] = Value::Object(headers);
        }
        self.saving_provider = Some(provider_id.clone());
        self.call("server.setAcpRegistryProvider", input, window, cx, move |this, result, window, cx| {
            this.saving_provider = None;
            match result {
                Ok(_) => {
                    window.push_notification(Notification::success("ACP provider configured"), cx);
                    this.load_providers(Some(provider_id), window, cx);
                }
                Err(error) => report_failure(window, cx, "Could not configure ACP provider", &error),
            }
        });
    }

    fn confirm_disable_provider(&mut self, provider_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.project_id.is_none() || self.saving_provider.is_some() || self.loading_providers {
            return;
        }
        let view = cx.entity();
        let message = format!("Disable ACP provider \"{provider_id}\"?");
        confirm_destructive(window, cx, message, "Disable", move |window, cx| {
            view.update(cx, |this, cx| this.disable_provider(provider_id.clone(), window, cx));
        });
    }

    fn disable_provider(&mut self, provider_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project_id) = self.project_id.clone() else { return };
        if self.saving_provider.is_some() {
            return;
        }
        self.saving_provider = Some(provider_id.clone());
        let input = json!({"instanceId": self.instance_id, "projectId": project_id, "providerId": provider_id});
        self.call("server.disableAcpRegistryProvider", input, window, cx, move |this, result, window, cx| {
            this.saving_provider = None;
            match result {
                Ok(_) => {
                    window.push_notification(Notification::success("ACP provider disabled"), cx);
                    this.load_providers(Some(provider_id), window, cx);
                }
                Err(error) => report_failure(window, cx, "Could not disable ACP provider", &error),
            }
        });
    }

    fn logout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.logging_out {
            return;
        }
        self.logging_out = true;
        let input = json!({"instanceId": self.instance_id});
        self.call("server.logoutAcpRegistry", input, window, cx, |this, result, window, cx| {
            this.logging_out = false;
            match result {
                Ok(_) => {
                    this.sessions.clear();
                    this.next_cursor = None;
                    window.push_notification(Notification::success("Logged out of ACP agent"), cx);
                }
                Err(error) => report_failure(window, cx, "Could not log out of ACP agent", &error),
            }
        });
    }
}

fn new_draft(provider: &AgentProvider, window: &mut Window, cx: &mut App) -> ProviderDraft {
    let base_url = provider.current.as_ref().map(|c| c.base_url.clone()).unwrap_or_default();
    ProviderDraft {
        api_type: provider
            .current
            .as_ref()
            .map(|c| c.api_type.clone())
            .or_else(|| provider.supported.first().cloned())
            .unwrap_or_default(),
        base_url: cx
            .new(|cx| InputState::new(window, cx).placeholder("https://api.example.com").default_value(base_url)),
        headers: cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Write-only headers JSON, e.g. {\"Authorization\":\"Bearer …\"}")
        }),
    }
}

fn report_failure(window: &mut Window, cx: &mut App, title: &str, error: &RpcError) {
    if matches!(error, RpcError::Disconnected(_)) {
        return;
    }
    window.push_notification(Notification::error(rpc_message(error)).title(title.to_owned()), cx);
}

/// `ensureLocalApi().dialogs.confirm(message, { variant: "destructive" })`.
fn confirm_destructive(
    window: &mut Window,
    cx: &mut App,
    message: String,
    action: &'static str,
    on_confirm: impl Fn(&mut Window, &mut App) + 'static,
) {
    let on_confirm = std::rc::Rc::new(on_confirm);
    window.open_alert_dialog(cx, move |alert, _, _| {
        let on_confirm = on_confirm.clone();
        alert.title(message.clone()).show_cancel(true).ok_text(action).ok_variant(ButtonVariant::Danger).on_ok(
            move |_, window, cx| {
                on_confirm(window, cx);
                true
            },
        )
    });
}

impl AcpSessions {
    fn project_picker(
        &self,
        id: &'static str,
        projects: &[(String, String)],
        disabled: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = self.project_id.clone();
        let label = projects.iter().find(|(id, _)| Some(id) == current.as_ref()).map(|(_, title)| title.clone());
        let view = cx.entity();
        let projects = projects.to_vec();
        Button::new(id)
            .outline()
            .small()
            .w(px(160.))
            .disabled(disabled)
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .justify_between()
                    .gap(px(4.))
                    .child(div().truncate().child(label.unwrap_or_default()))
                    .child(Icon::new(Lucide::ChevronDown).size(px(14.)).text_color(t.muted_foreground)),
            )
            .dropdown_menu(move |mut menu, window, _| {
                for (project_id, title) in &projects {
                    let picked = project_id.clone();
                    menu = menu.item(
                        PopupMenuItem::new(title.clone()).checked(Some(project_id) == current.as_ref()).on_click(
                            window.listener_for(&view, move |this, _, _, cx| this.select_project(picked.clone(), cx)),
                        ),
                    );
                }
                menu
            })
            .into_any_element()
    }

    fn render_sessions(&self, can_import: bool, can_delete: bool, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .px(px(16.))
            .pb(px(4.))
            .gap(px(8.))
            .when(!self.sessions.is_empty(), |col| {
                col.child(v_flex().border_y_1().border_color(t.border.opacity(0.6)).children(
                    self.sessions.iter().enumerate().map(|(index, session)| {
                        let imported = session.imported_thread_id.is_some();
                        let id =
                            |part: &str| ElementId::Name(format!("acp-session-{}-{part}", session.session_id).into());
                        let import_target = session.clone();
                        let delete_target = session.clone();
                        h_flex()
                            .min_w_0()
                            .justify_between()
                            .gap(px(12.))
                            .py(px(8.))
                            .when(index > 0, |row| row.border_t_1().border_color(t.border.opacity(0.6)))
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(12.))
                                            .child(session.title.clone().unwrap_or_else(|| session.session_id.clone())),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .font_family(FONT_MONO)
                                            .text_size(px(10.))
                                            .text_color(t.muted_foreground)
                                            .child(session.session_id.clone()),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .flex_none()
                                    .gap(px(4.))
                                    .child(
                                        Button::new(id("import"))
                                            .ghost()
                                            .xsmall()
                                            .label(if imported {
                                                "Imported"
                                            } else if self.importing.as_deref() == Some(&session.session_id) {
                                                "Importing"
                                            } else {
                                                "Import"
                                            })
                                            .disabled(!can_import || imported || self.importing.is_some())
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.import_session(import_target.clone(), window, cx)
                                            })),
                                    )
                                    .when(can_delete, |row| {
                                        row.child(
                                            Button::new(id("delete"))
                                                .ghost()
                                                .xsmall()
                                                .label(if self.deleting.as_deref() == Some(&session.session_id) {
                                                    "Deleting"
                                                } else {
                                                    "Delete"
                                                })
                                                .disabled(imported || self.deleting.is_some())
                                                .on_click(cx.listener(move |this, _, window, cx| {
                                                    this.confirm_delete_session(delete_target.clone(), window, cx)
                                                })),
                                        )
                                    }),
                            )
                    }),
                ))
            })
            .when_some(self.next_cursor.clone(), |col, cursor| {
                col.child(
                    h_flex().child(
                        Button::new("acp-sessions-load-more")
                            .ghost()
                            .xsmall()
                            .label(if self.loading { "Loading" } else { "Load more" })
                            .disabled(self.loading)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.load_sessions(Some(cursor.clone()), window, cx)
                            })),
                    ),
                )
            })
            .into_any_element()
    }

    fn render_provider(
        &self,
        provider: &AgentProvider,
        read_only: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(draft) = self.drafts.get(&provider.provider_id) else { return div().into_any_element() };
        let saving = self.saving_provider.is_some();
        let id = |part: &str| ElementId::Name(format!("acp-provider-{}-{part}", provider.provider_id).into());
        let provider_id = provider.provider_id.clone();
        let save_id = provider_id.clone();
        let disable_id = provider_id.clone();
        let base_url_empty = draft.base_url.read(cx).value().is_empty();
        let status = format!(
            "{}{}",
            if provider.current.is_none() { "Disabled" } else { "Configured" },
            if provider.required { " · Required" } else { "" }
        );
        let view = cx.entity();
        let supported = provider.supported.clone();
        let current = draft.api_type.clone();
        let protocol = Button::new(id("protocol"))
            .outline()
            .small()
            .w(px(144.))
            .disabled(read_only || saving)
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(draft.api_type.clone())
                    .child(Icon::new(Lucide::ChevronDown).size(px(14.)).text_color(t.muted_foreground)),
            )
            .dropdown_menu(move |mut menu, window, _| {
                for protocol in &supported {
                    let picked = protocol.clone();
                    let target = provider_id.clone();
                    menu = menu.item(PopupMenuItem::new(protocol.clone()).checked(*protocol == current).on_click(
                        window.listener_for(&view, move |this, _, _, cx| {
                            if let Some(draft) = this.drafts.get_mut(&target) {
                                draft.api_type = picked.clone();
                                cx.notify();
                            }
                        }),
                    ));
                }
                menu
            });
        v_flex()
            .gap(px(8.))
            .py(px(12.))
            .child(
                h_flex()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(provider.provider_id.clone()),
                            )
                            .child(div().text_size(px(10.)).text_color(t.muted_foreground).child(status)),
                    )
                    .child(
                        h_flex()
                            .gap(px(4.))
                            .child(
                                Button::new(id("save"))
                                    .outline()
                                    .xsmall()
                                    .label(if self.saving_provider.as_deref() == Some(&provider.provider_id) {
                                        "Saving"
                                    } else {
                                        "Save"
                                    })
                                    .disabled(
                                        read_only
                                            || saving
                                            || self.loading_providers
                                            || draft.api_type.is_empty()
                                            || base_url_empty,
                                    )
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.save_provider(save_id.clone(), window, cx)
                                    })),
                            )
                            .when(!provider.required && provider.current.is_some(), |row| {
                                row.child(
                                    Button::new(id("disable"))
                                        .ghost()
                                        .xsmall()
                                        .label("Disable")
                                        .disabled(read_only || saving || self.loading_providers)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.confirm_disable_provider(disable_id.clone(), window, cx)
                                        })),
                                )
                            }),
                    ),
            )
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(protocol)
                    .child(Input::new(&draft.base_url).small().flex_1().disabled(read_only || saving)),
            )
            .child(Input::new(&draft.headers).small().disabled(read_only || saving))
            .into_any_element()
    }

    fn render_section(
        &mut self,
        provider: &crate::model::Provider,
        projects: &[(String, String)],
        read_only: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let t = *tokens(cx);
        let sessions = provider.native_sessions.as_ref();
        let can_list = sessions.is_some_and(|s| s.can_list);
        let can_import = sessions.is_some_and(|s| s.can_load || s.can_resume);
        let can_delete = sessions.is_some_and(|s| s.can_delete);
        let can_logout = provider.auth.can_logout == Some(true) && !provider.setup.as_ref().is_some_and(|s| s.can_authenticate);
        let can_configure = provider.configurable_providers;
        if !can_list && !can_logout && !can_configure {
            return None;
        }
        let pending = self.pending();
        let controls = h_flex()
            .flex_wrap()
            .gap(px(8.))
            .when(can_logout, |row| {
                row.child(
                    Button::new("acp-logout")
                        .outline()
                        .small()
                        .label(if self.logging_out { "Logging out" } else { "Log out" })
                        .disabled(read_only || self.logging_out)
                        .on_click(cx.listener(|this, _, window, cx| this.logout(window, cx))),
                )
            })
            .when(can_list && !projects.is_empty(), |row| {
                row.child(self.project_picker("acp-sessions-project", projects, pending, &t, cx)).child(
                    Button::new("acp-list-sessions")
                        .outline()
                        .small()
                        .label(if self.loading {
                            "Loading"
                        } else if self.sessions.is_empty() {
                            "List sessions"
                        } else {
                            "Refresh"
                        })
                        .disabled(self.loading || self.project_id.is_none())
                        .on_click(cx.listener(|this, _, window, cx| this.load_sessions(None, window, cx))),
                )
            });
        let status =
            (can_list && projects.is_empty()).then(|| "Add a project before importing sessions.".into_any_element());
        let show_sessions = can_list && (!self.sessions.is_empty() || self.next_cursor.is_some());
        let mut section = v_flex()
            .gap(px(12.))
            .child(settings_row(
                "acp-native-sessions",
                "Native sessions",
                Some("Resume agent-owned conversations as T3 threads.".into()),
                status,
                Some(controls.into_any_element()),
                &t,
            ))
            .when(show_sessions, |col| col.child(self.render_sessions(can_import, can_delete, &t, cx)));
        if can_configure {
            let header = h_flex()
                .flex_wrap()
                .justify_between()
                .gap(px(8.))
                .child(
                    v_flex()
                        .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).child("Agent providers"))
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(t.muted_foreground)
                                .child("Configure non-secret routing. Headers are write-only."),
                        ),
                )
                .child(
                    Button::new("acp-list-providers")
                        .outline()
                        .xsmall()
                        .label(if self.loading_providers {
                            "Loading"
                        } else if self.providers.is_empty() {
                            "List providers"
                        } else {
                            "Refresh"
                        })
                        .disabled(self.loading_providers || self.saving_provider.is_some() || self.project_id.is_none())
                        .on_click(cx.listener(|this, _, window, cx| this.load_providers(None, window, cx))),
                );
            let providers = self.providers.clone();
            section = section.child(
                v_flex()
                    .gap(px(12.))
                    .px(px(16.))
                    .py(px(12.))
                    .border_t_1()
                    .border_color(t.border.opacity(0.6))
                    .child(header)
                    .when(!can_list && projects.len() > 1, |col| {
                        col.child(h_flex().child(self.project_picker(
                            "acp-providers-project",
                            projects,
                            pending,
                            &t,
                            cx,
                        )))
                    })
                    .when(!providers.is_empty(), |col| {
                        col.child(v_flex().border_y_1().border_color(t.border.opacity(0.6)).children(
                            providers.iter().enumerate().map(|(index, provider)| {
                                div()
                                    .when(index > 0, |row| row.border_t_1().border_color(t.border.opacity(0.6)))
                                    .child(self.render_provider(provider, read_only, &t, cx))
                            }),
                        ))
                    }),
            );
        }
        Some(section.into_any_element())
    }
}

impl Render for AcpSessions {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(workspace) = self.workspace.upgrade() else { return div().into_any_element() };
        let (provider, projects) = {
            let workspace = workspace.read(cx);
            let provider = workspace.server_info.provider(&self.instance_id).cloned();
            let projects: Vec<_> = workspace.shell.projects.iter().map(|p| (p.id.clone(), p.title.clone())).collect();
            (provider, projects)
        };
        if self.project_id.is_none() {
            self.project_id = projects.first().map(|(id, _)| id.clone());
        }
        let read_only = self.read_only;
        provider
            .filter(|p| p.driver == super::add::ACP_REGISTRY)
            .and_then(|provider| self.render_section(&provider, &projects, read_only, cx))
            .unwrap_or_else(|| div().into_any_element())
    }
}

impl Workspace {
    /// `AcpSessionManagementSection` for an ACP Registry instance; `None` for other drivers or
    /// when the agent offers neither sessions, logout nor configurable providers.
    pub(in crate::app) fn render_acp_session_management(
        &mut self,
        instance_id: &str,
        read_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let provider = self.server_info.provider(instance_id)?;
        if provider.driver != super::add::ACP_REGISTRY {
            return None;
        }
        let shows = provider.native_sessions.as_ref().is_some_and(|s| s.can_list)
            || (provider.auth.can_logout == Some(true) && !provider.setup.as_ref().is_some_and(|s| s.can_authenticate))
            || provider.configurable_providers;
        if !shows {
            return None;
        }
        let workspace = cx.weak_entity();
        let owned_id = instance_id.to_owned();
        let view = window.use_keyed_state(SharedString::from(format!("acp-sessions-{instance_id}")), cx, |_, _| {
            AcpSessions::new(workspace, owned_id, read_only)
        });
        view.update(cx, |view, cx| {
            if view.read_only != read_only {
                view.read_only = read_only;
                cx.notify();
            }
        });
        Some(view.into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_credential_free_https_icons_on_the_official_cdn() {
        let ok = "https://cdn.agentclientprotocol.com/registry/icons/gemini.png";
        assert_eq!(official_icon_url(Some(ok)).as_deref(), Some(ok));
        assert_eq!(official_icon_url(Some("http://cdn.agentclientprotocol.com/icon.png")), None);
        assert_eq!(official_icon_url(Some("https://cdn.agentclientprotocol.com.evil/icon.png")), None);
        assert_eq!(official_icon_url(Some("https://user@cdn.agentclientprotocol.com/icon.png")), None);
        assert_eq!(official_icon_url(Some("https://cdn.agentclientprotocol.com:8443/icon.png")), None);
        assert_eq!(
            official_icon_url(Some("https://CDN.agentclientprotocol.com:443/a.svg")).as_deref(),
            Some("https://cdn.agentclientprotocol.com/a.svg")
        );
        assert_eq!(official_icon_url(None), None);
    }

    #[test]
    fn hides_agents_that_have_their_own_driver() {
        assert!(is_first_class_agent("codex-acp"));
        assert!(is_first_class_agent("opencode-v2"));
        assert!(!is_first_class_agent("gemini"));
    }

    #[test]
    fn provider_headers_are_a_json_object_of_strings() {
        assert_eq!(parse_provider_headers("  "), Ok(None));
        assert_eq!(
            parse_provider_headers(r#"{"Authorization":"Bearer x"}"#).unwrap().unwrap()["Authorization"],
            "Bearer x"
        );
        assert_eq!(
            parse_provider_headers(r#"{"retries":3}"#),
            Err("Headers must be a JSON object with string values.".to_owned())
        );
        assert_eq!(parse_provider_headers("[]"), Err("Headers must be a JSON object with string values.".to_owned()));
        assert!(parse_provider_headers("{oops").is_err());
    }
}
