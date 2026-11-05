//! Settings sections in the connections group; see `SETTINGS` in `mod.rs`.
//!
//! Environments (`ConnectionsSettings view="environments"`), Sharing & access
//! (`ConnectionsSettings view="access"`) and Secrets (`SecretsSettingsPanel`).
//! This app keeps a catalog of paired remote environments (`saved.rs`) but has no
//! network/Tailscale/WSL controls and no T3 Connect account, so it renders what the
//! web renders without them: no network exposure or T3 Connect rows. None of these
//! sections has a plain scoped setting row; secret providers are environment-wide
//! (read-only under a project scope in the web).
//!
//! Each section keeps its state in keyed element state, so it lives while the
//! section is on screen, like the web's mounted component.

mod access;
mod environments;
mod exposure;
mod load_balancing;
pub(super) mod logic;
mod saved;
mod secrets;
pub(super) mod ui;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{AnyElement, App, Context, SharedString, Task, Window};
use serde_json::{Value, json};

use crate::app::Workspace;
use crate::app::pages::settings::widgets::SettingsView;
use crate::backend::Endpoint;
use crate::rpc::RpcClient;

use self::logic::Session;

/// How long a failed session check waits before it asks again.
const SESSION_RETRY: std::time::Duration = std::time::Duration::from_secs(5);

/// This client's `/api/auth/session` answer, as a page sees it.
#[derive(Default)]
pub(super) enum SessionLoad {
    #[default]
    Pending,
    Ready(Session),
    /// The check failed; it is retried.
    Failed,
}

impl SessionLoad {
    pub(super) fn session(&self) -> Option<&Session> {
        match self {
            Self::Ready(session) => Some(session),
            _ => None,
        }
    }

    /// A failed or pending check grants nothing, like `resolveRemoteOperateAccess`.
    pub(super) fn grants(&self, scope: &str) -> bool {
        self.session().is_some_and(|session| session.grants(scope))
    }
}

/// The session check every page that gates on grants uses: `apply` gets each answer, and a
/// failure is retried every few seconds until one succeeds or the task is dropped.
pub(super) fn check_session<T: 'static>(
    endpoint: Endpoint,
    cx: &mut Context<T>,
    apply: impl Fn(&mut T, SessionLoad, &mut Context<T>) + 'static,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        loop {
            let result = auth_request(Some(endpoint.clone()), "/api/auth/session", None).await;
            let ready = result.is_ok();
            let load = result.map_or(SessionLoad::Failed, |value| SessionLoad::Ready(Session::from_json(&value)));
            let applied = this.update(cx, |this, cx| {
                apply(this, load, cx);
                cx.notify();
            });
            if ready || applied.is_err() {
                return;
            }
            cx.background_executor().timer(SESSION_RETRY).await;
        }
    })
}

impl Workspace {
    pub(super) fn render_settings_connections(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_environments(view, window, cx)
    }

    pub(super) fn render_settings_access(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_access(view, window, cx)
    }

    pub(super) fn render_settings_secrets(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_secrets(view, window, cx)
    }

    /// The environment's name, as the web's `EnvironmentPresentation.label`.
    fn environment_label(&self) -> Option<&str> {
        self.settings_info().environment_label.as_deref()
    }

    fn server_settings(&self) -> &Value {
        &self.settings_info().settings
    }
}

/// Calls one of the server's `/api/auth/*` routes with this client's bearer session.
pub(super) async fn auth_request(
    endpoint: Option<Endpoint>,
    path: &'static str,
    body: Option<Value>,
) -> Result<Value, String> {
    let endpoint = endpoint.ok_or("Not connected to the environment.")?;
    let request = async move {
        let client = crate::app::pages::settings::http();
        let url = format!("{}{path}", endpoint.http_base);
        let request = match &body {
            Some(body) => client.post(url).json(body),
            None => client.get(url),
        };
        let response = request
            .bearer_auth(&endpoint.bearer_token)
            .timeout(std::time::Duration::from_secs(8))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status();
        let value = response.json::<Value>().await;
        if status.is_success() {
            value.map_err(|error| format!("The environment sent an unreadable response to {path}: {error}"))
        } else {
            let value = value.unwrap_or(Value::Null);
            Err(value["message"].as_str().map_or_else(|| format!("Request failed ({status})."), str::to_owned))
        }
    };
    crate::runtime().spawn(request).await.map_err(|error| error.to_string())?
}

/// `server.updateSettings` with a plain environment-wide patch.
async fn update_settings(client: Option<RpcClient>, patch: Value) -> Result<(), String> {
    let client = client.ok_or("Not connected to the environment.")?;
    client.request("server.updateSettings", json!({ "patch": patch })).await.map(drop).map_err(|e| e.to_string())
}

/// The web's `stackedThreadToast({ type: "error", title, description })`.
fn error_toast(
    window: &mut Window,
    cx: &mut App,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
) {
    window.push_notification(Notification::error(description).title(title), cx);
}

fn success_toast(
    window: &mut Window,
    cx: &mut App,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
) {
    window.push_notification(Notification::success(description).title(title), cx);
}
