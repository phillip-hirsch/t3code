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
mod logic;
mod saved;
mod secrets;
mod ui;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{AnyElement, App, Context, SharedString, Window};
use serde_json::{Value, json};

use crate::app::Workspace;
use crate::backend::Endpoint;
use crate::rpc::RpcClient;

impl Workspace {
    pub(super) fn render_settings_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_environments(window, cx)
    }

    pub(super) fn render_settings_access(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_access(window, cx)
    }

    pub(super) fn render_settings_secrets(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_secrets(window, cx)
    }

    /// The environment's name, as the web's `EnvironmentPresentation.label`.
    fn environment_label(&self) -> Option<&str> {
        self.server_info.environment_label.as_deref()
    }

    fn server_settings(&self) -> &Value {
        &self.server_info.settings
    }
}

/// Calls one of the server's `/api/auth/*` routes with this client's bearer session.
async fn auth_request(endpoint: Option<Endpoint>, path: &'static str, body: Option<Value>) -> Result<Value, String> {
    let endpoint = endpoint.ok_or("Not connected to the environment.")?;
    let request = async move {
        let client = reqwest::Client::new();
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
        let value: Value = response.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            Ok(value)
        } else {
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
