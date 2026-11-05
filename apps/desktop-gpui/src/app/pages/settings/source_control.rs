//! Settings › Source control (`SourceControlSettingsPanel` in
//! `apps/web/src/components/settings/SourceControlSettings.tsx`, with `ProjectDefaultsSettings
//! category="source-control"`, `SourceControlAppSignInSettings`, `SourceControlTokenSettings`,
//! `SourceControlAccountSettings`, `SourceControlHostSettings` and `SourceControlWritingSettings`).
//!
//! The Repositories and Writing & review rows write through the settings scope. Discovery,
//! sign-in, tokens, accounts and host fields are per environment (`server.updateSettings` on the
//! environment discovery ran against). The web's secret-provider picker beside token fields
//! belongs to Settings › Secrets; here a token is always a typed value.

use std::collections::HashSet;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, FontWeight, HighlightStyle, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, StyledText, Subscription, Task, Window, div, img, px, svg,
};
use serde_json::{Map, Value, json};

use super::super::fields::textarea_body;
use super::super::model_select::{generates_text, muted, resolve_text_generation};
use super::super::widgets::{SettingRow, SettingsView, section_with_action, settings_section, switch};
use crate::app::Workspace;
use crate::model::ModelSelection;
use crate::rpc::{RpcClient, RpcError};
use crate::theme::{Tokens, tokens};

const MERGE_METHODS: &[(&str, &str)] =
    &[("last", "Last selected"), ("merge", "Merge"), ("squash", "Squash and merge"), ("rebase", "Rebase and merge")];

/// `MODE_OPTIONS`: label and description per writing style.
const WRITING_STYLES: &[(&str, &str, &str)] = &[
    (
        "repo_conventions",
        "Repository conventions",
        "In each project, matches recent change descriptions and change request titles.",
    ),
    (
        "conventional_commits",
        "Conventional Commits",
        "Use Conventional Commit prefixes and keep change request text concise.",
    ),
    (
        "custom",
        "Custom instructions",
        "Use your instructions for change descriptions and change requests in every project.",
    ),
];

/// What the client knows about a host beyond discovery (`source-control-*/src/shared/definition.ts`).
struct HostDefinition {
    kind: &'static str,
    icon: &'static str,
    /// Per-hostname tokens and CLI logins (`credentials`), GitHub's only.
    credentials: Option<Credentials>,
    /// The generic settings form, for hosts without `credentials`.
    fields: &'static [HostField],
}

struct Credentials {
    cli_name: &'static str,
    cli_label: &'static str,
    login_command: &'static str,
    token_env_var: &'static str,
    token_access: &'static str,
    new_token_url: fn(&str) -> String,
    public_host: &'static str,
    app_sign_in: Option<AppSignIn>,
}

struct AppSignIn {
    app_name: &'static str,
    host: &'static str,
    install_url: &'static str,
    revoke_url: &'static str,
}

struct HostField {
    key: &'static str,
    title: &'static str,
    description: &'static str,
    secret: bool,
    placeholder: &'static str,
}

const HOSTS: &[HostDefinition] = &[
    HostDefinition {
        kind: "github",
        icon: "icons/app/source-control/github.svg",
        credentials: Some(Credentials {
            cli_name: "gh",
            cli_label: "GitHub CLI",
            login_command: "gh auth login",
            token_env_var: "GH_TOKEN",
            token_access: "read and write access to pull requests and contents",
            new_token_url: |host| format!("https://{host}/settings/personal-access-tokens/new"),
            public_host: "github.com",
            app_sign_in: Some(AppSignIn {
                app_name: "T3 Code for GitHub",
                host: "github.com",
                install_url: "https://github.com/apps/t3-code-for-github/installations/new",
                revoke_url: "https://github.com/settings/apps/authorizations",
            }),
        }),
        fields: &[],
    },
    HostDefinition { kind: "gitlab", icon: "icons/app/source-control/gitlab.svg", credentials: None, fields: &[] },
    HostDefinition {
        kind: "gitcafe",
        icon: "icons/app/source-control/gitcafe.svg",
        credentials: None,
        fields: &[HostField {
            key: "token",
            title: "Token",
            description: "A git.cafe token, used before CAFE_TOKEN and the cafe login, so GitCafe works without the CLI signed in.",
            secret: true,
            placeholder: "",
        }],
    },
    HostDefinition { kind: "forgejo", icon: "icons/app/source-control/forgejo.svg", credentials: None, fields: &[] },
    HostDefinition {
        kind: "bitbucket",
        icon: "icons/app/source-control/bitbucket.svg",
        credentials: None,
        fields: &[
            HostField {
                key: "accessToken",
                title: "Access token",
                description: "Scoped to one repository, project, or workspace. Create it in that item's Bitbucket settings.",
                secret: true,
                placeholder: "",
            },
            HostField {
                key: "email",
                title: "Atlassian account email",
                description: "With an API token instead of an access token.",
                secret: false,
                placeholder: "you@example.com",
            },
            HostField {
                key: "apiToken",
                title: "API token",
                description: "Reaches every repository your Atlassian account can. Give it read and write access to repositories and pull requests, and read:user:bitbucket.",
                secret: true,
                placeholder: "",
            },
        ],
    },
    HostDefinition {
        kind: "azure-devops",
        icon: "icons/app/source-control/azure-devops.svg",
        credentials: None,
        fields: &[],
    },
];

fn host(kind: &str) -> Option<&'static HostDefinition> {
    HOSTS.iter().find(|host| host.kind == kind)
}

/// `Option<string>` on the wire: `{"_tag": "Some", "value": …}`.
fn some(value: &Value) -> Option<&str> {
    (value["_tag"] == "Some").then(|| value["value"].as_str()).flatten()
}

/// A saved credential: a non-empty (redacted) literal, or a secret reference.
fn has_secret(value: &Value) -> bool {
    value.as_str().map_or(!value.is_null(), |text| !text.is_empty())
}

fn failure_message(error: &RpcError) -> String {
    match error {
        RpcError::Failure(failure) => failure["message"].as_str().map_or_else(|| error.to_string(), str::to_owned),
        _ => error.to_string(),
    }
}

/// `redactedPlaceholder`: the same length and punctuation, other characters scrambled stably.
fn redacted(value: &str) -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut state: u32 = 0x811c_9dc5;
    for unit in value.encode_utf16() {
        state ^= u32::from(unit);
        state = state.wrapping_mul(0x0100_0193);
    }
    value
        .chars()
        .map(|c| {
            if matches!(c, '@' | '.' | '-' | '_') {
                return c;
            }
            state = (state ^ (state >> 13)).wrapping_mul(0x85eb_ca6b);
            state = (state ^ (state >> 16)).wrapping_mul(0xc2b2_ae35);
            ALPHABET[((state as i32).unsigned_abs() % ALPHABET.len() as u32) as usize] as char
        })
        .collect()
}

/// One host's logins (`groupHostAccounts`).
#[derive(Debug, PartialEq)]
struct HostAccounts {
    host: String,
    active: Option<String>,
    selectable: Vec<String>,
    broken: Vec<(String, Option<String>)>,
    environment_variable: Option<String>,
}

fn group_host_accounts(accounts: &[Value]) -> Vec<HostAccounts> {
    let mut hosts: Vec<&str> = Vec::new();
    for account in accounts {
        let host = account["host"].as_str().unwrap_or_default();
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    hosts
        .into_iter()
        .map(|host| {
            let entries: Vec<&Value> = accounts.iter().filter(|a| a["host"] == host).collect();
            let stored: Vec<&Value> =
                entries.iter().copied().filter(|a| a.get("environmentVariable").is_none()).collect();
            let selectable: Vec<&Value> = stored.iter().copied().filter(|a| a["authenticated"] == true).collect();
            let name = |a: &Value| a["account"].as_str().unwrap_or_default().to_owned();
            HostAccounts {
                host: host.to_owned(),
                active: selectable.iter().find(|a| a["active"] == true).or(selectable.first()).map(|a| name(a)),
                selectable: selectable.iter().map(|a| name(a)).collect(),
                broken: stored
                    .iter()
                    .filter(|a| a["authenticated"] != true)
                    .map(|a| (name(a), a["error"].as_str().map(str::to_owned)))
                    .collect(),
                environment_variable: entries.iter().find_map(|a| a["environmentVariable"].as_str()).map(str::to_owned),
            }
        })
        .collect()
}

/// `nextHostAccountChoices`: the `hosts` map after one host's change; a host that is on and
/// follows the CLI's active login drops out.
fn next_host_choices(current: &Value, host: &str, enabled: Option<bool>, account: Option<Option<&str>>) -> Value {
    let previous = &current[host];
    let enabled = enabled.or(previous["enabled"].as_bool()).unwrap_or(true);
    let account = match account {
        None => previous["account"].as_str(),
        Some(account) => account,
    };
    let mut hosts: Map<String, Value> = current.as_object().cloned().unwrap_or_default();
    hosts.remove(host);
    if !enabled || account.is_some() {
        let mut choice = Map::from_iter([("enabled".to_owned(), Value::Bool(enabled))]);
        if let Some(account) = account {
            choice.insert("account".into(), account.into());
        }
        hosts.insert(host.to_owned(), Value::Object(choice));
    }
    Value::Object(hosts)
}

#[derive(Clone)]
enum SignIn {
    Idle,
    Starting,
    Waiting { user_code: String, verification_uri: String, expires_at: i64 },
    Failed(String),
}

/// What Source control keeps across frames: the discovery scan, folds and the in-progress
/// GitHub sign-in and token form.
struct SourceControlState {
    discovery: Option<Value>,
    error: Option<String>,
    pending: bool,
    _scan: Option<Task<()>>,
    expanded: HashSet<String>,
    sign_in: SignIn,
    copied: bool,
    _copied: Option<Task<()>>,
    signed_in_revealed: bool,
    accounts_revealed: bool,
    /// Discovery rows whose account is revealed.
    revealed: HashSet<String>,
    saving: bool,
    /// The token field's placeholder says a token is stored.
    token_saved: bool,
    token_host: Entity<InputState>,
    token: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl SourceControlState {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let token_host = cx.new(|cx| InputState::new(window, cx).placeholder("github.com"));
        token_host.update(cx, |input, cx| input.set_value("github.com", window, cx));
        let token = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Not set"));
        // The Save button's state follows what is typed.
        let notify = |_: &mut Self, _: Entity<InputState>, event: &InputEvent, cx: &mut Context<Self>| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        };
        let subscriptions = vec![cx.subscribe(&token_host, notify), cx.subscribe(&token, notify)];
        Self {
            discovery: None,
            error: None,
            pending: false,
            _scan: None,
            expanded: HashSet::new(),
            sign_in: SignIn::Idle,
            copied: false,
            _copied: None,
            signed_in_revealed: false,
            accounts_revealed: false,
            revealed: HashSet::new(),
            saving: false,
            token_saved: false,
            token_host,
            token,
            _subscriptions: subscriptions,
        }
    }

    /// `server.discoverSourceControl`; the first scan starts with the section.
    fn scan(&mut self, client: RpcClient, cx: &mut Context<Self>) {
        self.pending = true;
        self._scan = Some(cx.spawn(async move |this, cx| {
            let result = client.request("server.discoverSourceControl", json!({})).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(result) => {
                        this.discovery = Some(result);
                        this.error = None;
                    }
                    Err(error) => this.error = Some(failure_message(&error)),
                }
                this.pending = false;
                cx.notify();
            });
        }));
        cx.notify();
    }
}

impl Workspace {
    pub(super) fn render_source_control_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        let client = self.settings_client();
        let first_scan = client.clone();
        let state = window.use_keyed_state("settings-source-control", cx, move |window, cx| {
            let mut state = SourceControlState::new(window, cx);
            if let Some(client) = first_scan {
                state.scan(client, cx);
            }
            state
        });
        let view = self.settings_view();
        let stored = view.value("pullRequestMergeMethod").clone();
        let method = stored.as_str().unwrap_or("last");
        let merge_method = self.value_select(
            "merge-method",
            "pullRequestMergeMethod",
            &stored,
            MERGE_METHODS
                .iter()
                .map(|(id, label)| (if *id == "last" { Value::Null } else { json!(id) }, (*label).into()))
                .collect(),
            MERGE_METHODS.iter().find(|(id, _)| *id == method).map_or("", |(_, label)| label),
            208.,
            cx,
        );
        let discovery = self.discovery_sections(&state, client.is_some(), window, cx);
        let writing = self.writing_rows(&view, window, cx);
        let project_scope = view.scope.is_project();
        let mut row = |this: &mut Self, row: SettingRow| this.setting_row(&view, row, window, cx);
        let repositories = [
            row(
                self,
                SettingRow::new("automatic-pull")
                    .description(if project_scope {
                        "Keeps this project's default branch current when the checkout has no local changes or commits."
                    } else {
                        "Keeps the default branch current when the checkout has no local changes or commits. Projects can override it."
                    })
                    .setting("defaultAutoPull")
                    .switch(),
            ),
            row(
                self,
                SettingRow::new("remove-agent-credits-on-merge")
                    .description("Remove recognized agent credit lines from merge and squash messages on hosts that support it, keeping human co-authors. Includes auto-merge. Excludes merge queues, stack merges, and existing commits.")
                    .setting("removeAgentCreditsOnMerge")
                    .switch(),
            ),
            row(
                self,
                SettingRow::new("pull-request-merge-method")
                    .description(if project_scope {
                        "Pull requests in this project start with this method."
                    } else {
                        "Pull requests start with this method. Last selected reuses whatever you chose most recently on this device."
                    })
                    .setting("pullRequestMergeMethod")
                    .control(merge_method),
            ),
        ];
        let mut sections = vec![settings_section("repositories", "Repositories", repositories, &t)];
        sections.extend(discovery);
        sections.push(settings_section("writing-review", "Writing & review", writing, &t));
        self.settings_page("source-control", &view, sections, cx)
    }

    fn scan_button(&self, state: &Entity<SourceControlState>, cx: &mut Context<Self>) -> AnyElement {
        let pending = state.read(cx).pending;
        let client = self.settings_client();
        let state = state.clone();
        Button::new("rescan-source-control")
            .ghost()
            .xsmall()
            .icon(Icon::new(Lucide::RefreshCw).with_size(px(14.)))
            .loading(pending)
            .disabled(pending)
            .tooltip("Rescan Git and hosting integrations")
            .on_click(move |_, _, cx| {
                if let Some(client) = client.clone() {
                    state.update(cx, |state, cx| state.scan(client, cx));
                }
            })
            .into_any_element()
    }

    fn rescan(&self, state: &Entity<SourceControlState>, cx: &mut Context<Self>) {
        if let Some(client) = self.settings_client() {
            state.update(cx, |state, cx| state.scan(client, cx));
        }
    }

    fn discovery_sections(
        &mut self,
        state: &Entity<SourceControlState>,
        connected: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = *tokens(cx);
        if !connected {
            let note =
                muted("Connect an environment to inspect its version control tools and hosting integrations.", &t)
                    .px(px(16.))
                    .py(px(12.));
            return vec![settings_section("Server environment", "Server environment", [note.into_any_element()], &t)];
        }
        let (discovery, pending, error) = {
            let state = state.read(cx);
            (state.discovery.clone(), state.pending, state.error.clone())
        };
        let Some(discovery) = discovery else {
            if pending || error.is_none() {
                return vec![
                    section_with_action(
                        "Version Control",
                        "Version Control",
                        Some(self.scan_button(state, cx)),
                        skeleton_rows(&t),
                        &t,
                    ),
                    settings_section("Source control providers", "Source control providers", skeleton_rows(&t), &t),
                ];
            }
            return vec![self.empty_discovery(state, error, pending, cx)];
        };
        let empty = Vec::new();
        let vcs = discovery["versionControlSystems"].as_array().unwrap_or(&empty);
        let providers = discovery["sourceControlProviders"].as_array().unwrap_or(&empty);
        if vcs.is_empty() && providers.is_empty() {
            return vec![self.empty_discovery(state, error, pending, cx)];
        }
        let mut sections = Vec::new();
        if !vcs.is_empty() {
            let rows: Vec<AnyElement> = vcs.iter().map(|item| self.discovery_row(state, item, None, cx)).collect();
            sections.push(section_with_action(
                "Version Control",
                "Version Control",
                Some(self.scan_button(state, cx)),
                rows,
                &t,
            ));
        }
        if !providers.is_empty() {
            let mut rows = Vec::new();
            for item in providers {
                let kind = item["kind"].as_str().unwrap_or_default();
                let details = host(kind)
                    .filter(|host| host.credentials.is_some() || !host.fields.is_empty())
                    .map(|host| (state.read(cx).expanded.contains(kind), host));
                let panel = match details {
                    Some((true, host)) => Some(Some(self.host_panel(state, host, item, window, cx))),
                    Some((false, _)) => Some(None),
                    None => None,
                };
                rows.push(self.discovery_row(state, item, panel, cx));
            }
            let action = vcs.is_empty().then(|| self.scan_button(state, cx));
            sections.push(section_with_action(
                "Source control providers",
                "Source control providers",
                action,
                rows,
                &t,
            ));
        }
        sections
    }

    fn empty_discovery(
        &self,
        state: &Entity<SourceControlState>,
        error: Option<String>,
        pending: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *tokens(cx);
        let client = self.settings_client();
        let scan_state = state.clone();
        let body = v_flex()
            .items_center()
            .gap(px(16.))
            .px(px(24.))
            .py(px(40.))
            .child(
                div()
                    .size(px(40.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(t.border)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(Lucide::GitPullRequest).with_size(px(16.))),
            )
            .child(
                v_flex()
                    .items_center()
                    .gap(px(4.))
                    .child(div().text_base().font_weight(FontWeight::MEDIUM).child(if error.is_some() {
                        "Could not scan the server environment"
                    } else {
                        "Nothing detected yet"
                    }))
                    .child(div().max_w(px(384.)).text_sm().text_color(t.muted_foreground).text_center().child(error.unwrap_or_else(|| {
                        "Install Git on the server, add optional hosting integrations or credentials your workspace needs, then rescan.".into()
                    }))),
            )
            .child(
                Button::new("scan-source-control")
                    .outline()
                    .small()
                    .icon(Icon::new(Lucide::RefreshCw).with_size(px(14.)))
                    .label("Scan")
                    .loading(pending)
                    .disabled(pending)
                    .on_click(move |_, _, cx| {
                        if let Some(client) = client.clone() {
                            scan_state.update(cx, |state, cx| state.scan(client, cx));
                        }
                    }),
            );
        settings_section("Server environment", "Server environment", [body.into_any_element()], &t)
    }

    /// `DiscoveryItemRow`: mark, label, version, badges and summary, the read-only availability
    /// switch, and for hosts with settings a fold. `panel` is `Some(None)` for a closed fold.
    fn discovery_row(
        &self,
        state: &Entity<SourceControlState>,
        item: &Value,
        panel: Option<Option<AnyElement>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *tokens(cx);
        let kind = item["kind"].as_str().unwrap_or_default().to_owned();
        let label = item["label"].as_str().unwrap_or(&kind).to_owned();
        let provider = item.get("auth").is_some();
        let available = item["status"] == "available";
        let not_ready = !provider && item["implemented"] != true;
        let authenticated = item["auth"]["status"] == "authenticated";
        let enabled = if provider { available && authenticated } else { available && !not_ready };
        let dot = if not_ready || (provider && !enabled) {
            t.muted_foreground.opacity(0.35)
        } else if !available {
            t.warning
        } else {
            t.success_foreground
        };
        let icon = if provider { host(&kind).map(|host| host.icon) } else { vcs_icon(&kind) };
        let mark = div()
            .relative()
            .size(px(20.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .children(icon.map(|path| source_control_icon(path, &t)))
            .child(
                div()
                    .absolute()
                    .left(px(-2.))
                    .top(px(-2.))
                    .size(px(8.))
                    .rounded_full()
                    .border_2()
                    .border_color(t.background)
                    .bg(dot),
            );
        let version = some(&item["version"]).map(str::to_owned);
        let unauthenticated = item["auth"]["status"] == "unauthenticated";
        let expanded = matches!(panel, Some(Some(_)));
        let toggle_state = state.clone();
        let toggle_kind = kind.clone();
        let header = h_flex()
            .px(px(16.))
            .py(px(12.))
            .gap(px(12.))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap(px(8.))
                            .child(mark)
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(t.foreground)
                                    .child(label.clone()),
                            )
                            .children(version.map(|version| {
                                div().font_family(t.fonts.mono).text_xs().text_color(t.muted_foreground).child(version)
                            }))
                            .when(not_ready, |el| {
                                el.child(badge("Coming Soon", t.warning_foreground, t.warning.opacity(0.12)))
                            })
                            .when(provider && unauthenticated, |el| {
                                el.child(badge("Not authenticated", t.foreground, t.secondary))
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .line_height(px(18.))
                            .text_color(t.muted_foreground.opacity(0.8))
                            .child(self.item_summary(state, item, &kind, &label, cx)),
                    ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(8.))
                    .when(panel.is_some(), |el| {
                        el.child(
                            Button::new(SharedString::from(format!("toggle-{kind}")))
                                .ghost()
                                .xsmall()
                                .icon(
                                    Icon::new(if expanded { Lucide::ChevronUp } else { Lucide::ChevronDown })
                                        .with_size(px(14.)),
                                )
                                .on_click(move |_, _, cx| {
                                    toggle_state.update(cx, |state, cx| {
                                        if !state.expanded.remove(&toggle_kind) {
                                            state.expanded.insert(toggle_kind.clone());
                                        }
                                        cx.notify();
                                    })
                                }),
                        )
                    })
                    .when(!not_ready, |el| {
                        el.child(
                            Switch::new(SharedString::from(format!("available-{kind}")))
                                .checked(enabled)
                                .disabled(true),
                        )
                    }),
            );
        v_flex()
            .id(SharedString::from(format!("source-control-{kind}")))
            .when(not_ready, |el| el.opacity(0.8))
            .child(header)
            .children(panel.flatten().map(|panel| div().px(px(16.)).pb(px(16.)).pt(px(4.)).child(panel)))
            .into_any_element()
    }

    /// `itemSummary`.
    fn item_summary(
        &self,
        state: &Entity<SourceControlState>,
        item: &Value,
        kind: &str,
        label: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *tokens(cx);
        let text = |text: String| div().child(text).into_any_element();
        if item.get("auth").is_none() && item["implemented"] != true {
            return text(format!("Support for {label} is coming soon."));
        }
        let install_hint = item["installHint"].as_str().unwrap_or_default();
        if item["status"] != "available" {
            return text(format!("Not available on this server: {install_hint}"));
        }
        let auth = &item["auth"];
        if auth.is_null() {
            return text("Available".into());
        }
        let detail = some(&auth["detail"]);
        match auth["status"].as_str() {
            Some("authenticated") => {
                let mut parts = vec![text("Authenticated".into())];
                if let Some(account) = some(&auth["account"]) {
                    parts.push(text("as".into()));
                    let revealed = state.read(cx).revealed.contains(kind);
                    let (reveal_state, reveal_kind) = (state.clone(), kind.to_owned());
                    parts.push(
                        div()
                            .id(SharedString::from(format!("account-{kind}")))
                            .cursor_pointer()
                            .font_family(t.fonts.mono)
                            .text_size(px(10.))
                            .hover(|el| el.text_color(t.foreground))
                            .child(if revealed { account.to_owned() } else { redacted(account) })
                            .tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(if revealed {
                                    "Click to hide account"
                                } else {
                                    "Click to reveal account"
                                })
                                .build(window, cx)
                            })
                            .on_click(move |_, _, cx| {
                                reveal_state.update(cx, |state, cx| {
                                    if !state.revealed.remove(&reveal_kind) {
                                        state.revealed.insert(reveal_kind.clone());
                                    }
                                    cx.notify();
                                })
                            })
                            .into_any_element(),
                    );
                }
                if let Some(detail) = detail {
                    parts.push(text(format!("· {detail}")));
                }
                h_flex().flex_wrap().gap_x(px(4.)).children(parts).into_any_element()
            }
            Some("unauthenticated") if item.get("executable").is_none() => text(format!("Available. {install_hint}")),
            Some("unauthenticated")
                if auth["accounts"].as_array().is_some_and(|a| a.iter().any(|a| a["authenticated"] == true)) =>
            {
                text(detail.map_or_else(|| format!("Every {label} host is turned off."), str::to_owned))
            }
            Some("unauthenticated") => {
                let executable = item["executable"].as_str().unwrap_or_default();
                let lead =
                    format!("{label} is not authenticated on this server. Sign in or configure credentials using the ");
                prose(
                    &[
                        (&lead, false),
                        (executable, true),
                        (" tool on the server host to enable change request features.", false),
                    ],
                    &t,
                )
                .into_any_element()
            }
            _ => text(format!("Could not verify {label}. {}", detail.unwrap_or(install_hint))),
        }
    }

    /// The fold under a host with settings.
    fn host_panel(
        &mut self,
        state: &Entity<SourceControlState>,
        host: &'static HostDefinition,
        item: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *tokens(cx);
        let Some(credentials) = &host.credentials else {
            return self.host_fields(state, host, window, cx);
        };
        let mut parts = Vec::new();
        if let (Some(app), true) = (&credentials.app_sign_in, item["appSignIn"] == true) {
            parts.push(self.app_sign_in(state, host.kind, app, some(&item["auth"]["account"]), cx));
            parts.push(
                h_flex()
                    .gap(px(12.))
                    .text_size(px(10.))
                    .text_color(t.muted_foreground)
                    .child(div().h(px(1.)).flex_1().bg(t.border))
                    .child("Or use your own credentials")
                    .child(div().h(px(1.)).flex_1().bg(t.border))
                    .into_any_element(),
            );
        }
        parts.push(self.token_form(state, host.kind, credentials, window, cx));
        if item["status"] == "available" {
            let accounts = item["auth"]["accounts"].as_array().cloned().unwrap_or_default();
            parts.push(self.account_choices(state, host.kind, credentials, &accounts, cx));
        }
        v_flex().gap(px(24.)).children(parts).into_any_element()
    }

    fn host_settings(&self, kind: &str) -> Value {
        self.settings_info().settings.get("sourceControlHosts").get(kind).cloned().unwrap_or_else(|| json!({}))
    }

    /// `SourceControlAppSignInSettings`: the device-code sign-in card.
    fn app_sign_in(
        &self,
        state: &Entity<SourceControlState>,
        kind: &'static str,
        app: &'static AppSignIn,
        account: Option<&str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *tokens(cx);
        let saved = self.host_settings(kind);
        let sign_ins = saved["signIns"].clone();
        let signed_in = sign_ins[app.host].as_str().is_some_and(|session| !session.is_empty());
        let token_overrides = has_secret(&saved["tokens"][app.host]);
        let (sign_in, copied, revealed) = {
            let state = state.read(cx);
            (state.sign_in.clone(), state.copied, state.signed_in_revealed)
        };
        let failed = matches!(sign_in, SignIn::Failed(_)) && !signed_in;
        let input = json!({ "provider": kind, "host": app.host });
        let subtitle: AnyElement;
        let action: AnyElement;
        if signed_in {
            subtitle = match account {
                Some(account) if !token_overrides => {
                    let reveal = state.clone();
                    h_flex()
                        .gap(px(4.))
                        .child("Signed in as")
                        .child(
                            div()
                                .id("signed-in-account")
                                .cursor_pointer()
                                .font_family(t.fonts.mono)
                                .text_size(px(10.))
                                .child(if revealed { account.to_owned() } else { redacted(account) })
                                .on_click(move |_, _, cx| {
                                    reveal.update(cx, |state, cx| {
                                        state.signed_in_revealed = !state.signed_in_revealed;
                                        cx.notify();
                                    })
                                }),
                        )
                        .into_any_element()
                }
                _ if token_overrides => {
                    div().child("Signed in, but the token saved below is used while it can be read.").into_any_element()
                }
                _ => div().child("Signed in").into_any_element(),
            };
            let (state, sign_ins) = (state.clone(), sign_ins.clone());
            action = Button::new("source-control-sign-out")
                .outline()
                .xsmall()
                .label("Sign out")
                .on_click(cx.listener(move |this, _, _, cx| {
                    state.update(cx, |state, _| state.sign_in = SignIn::Idle);
                    // The whole map: other hosts' redaction markers keep their sessions.
                    let mut sessions = sign_ins.as_object().cloned().unwrap_or_default();
                    sessions.insert(app.host.into(), json!(""));
                    let state = state.clone();
                    this.update_environment_settings(
                        json!({ "sourceControlHosts": { kind: { "signIns": sessions } } }),
                        cx,
                        move |this, ok, cx| {
                            if ok {
                                this.rescan(&state, cx);
                            }
                        },
                    );
                }))
                .into_any_element();
        } else if let SignIn::Waiting { .. } = sign_in {
            subtitle = div().child("Approve the app on GitHub to finish signing in.").into_any_element();
            let (state, input) = (state.clone(), input.clone());
            let client = self.settings_client();
            action = Button::new("source-control-sign-in-cancel")
                .ghost()
                .xsmall()
                .label("Cancel")
                .on_click(move |_, _, cx| {
                    state.update(cx, |state, cx| {
                        state.sign_in = SignIn::Idle;
                        cx.notify();
                    });
                    if let Some(client) = client.clone() {
                        let input = input.clone();
                        cx.background_spawn(async move {
                            let _ = client.request("sourceControl.signInCancel", input).await;
                        })
                        .detach();
                    }
                })
                .into_any_element();
        } else {
            subtitle =
                div().child("A far higher GitHub API rate limit than a personal token or gh login.").into_any_element();
            let starting = matches!(sign_in, SignIn::Starting);
            let state = state.clone();
            action = Button::new("source-control-sign-in")
                .primary()
                .xsmall()
                .when(starting, |el| el.child(Spinner::new().xsmall()))
                .label(if matches!(sign_in, SignIn::Failed(_)) { "Try again" } else { "Sign in with GitHub" })
                .disabled(starting)
                .on_click(cx.listener(move |this, _, _, cx| this.start_sign_in(&state, input.clone(), cx)))
                .into_any_element();
        }
        let mark = div().relative().size(px(40.)).flex_none().child(img("icons/app/t3.svg").size_full()).when(
            signed_in,
            |el| {
                el.child(
                    div()
                        .absolute()
                        .right(px(-4.))
                        .bottom(px(-4.))
                        .size(px(16.))
                        .rounded_full()
                        .bg(t.success_foreground)
                        .border_2()
                        .border_color(t.card)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::new(Lucide::Check).with_size(px(10.)).text_color(gpui_kit::white())),
                )
            },
        );
        let mut card = v_flex()
            .rounded(px(12.))
            .border_1()
            .border_color(if failed { t.error.opacity(0.4) } else { t.border })
            .bg(t.card)
            .overflow_hidden()
            .child(
                h_flex()
                    .p(px(12.))
                    .gap(px(12.))
                    .child(mark)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(t.foreground)
                                    .child(app.app_name),
                            )
                            .child(div().text_xs().text_color(t.muted_foreground).child(subtitle)),
                    )
                    .child(div().flex_none().child(action)),
            );
        if let (SignIn::Waiting { user_code, verification_uri, expires_at }, false) = (&sign_in, signed_in) {
            card = card.child(self.device_code(state, user_code, verification_uri, *expires_at, copied, &t));
        }
        if signed_in {
            card = card.child(
                h_flex()
                    .flex_wrap()
                    .gap_x(px(12.))
                    .gap_y(px(4.))
                    .border_t_1()
                    .border_color(t.border)
                    .bg(t.muted.opacity(0.4))
                    .px(px(12.))
                    .py(px(8.))
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child("Reaches repositories in accounts and organizations that installed the app."),
                    )
                    .child(external_link("install-app", "Install on an organization", app.install_url, &t))
                    .child(external_link("manage-app", "Manage on GitHub", app.revoke_url, &t)),
            );
        }
        if let (SignIn::Failed(message), false) = (&sign_in, signed_in) {
            card = card.child(
                div()
                    .border_t_1()
                    .border_color(t.error.opacity(0.2))
                    .bg(t.error.opacity(0.05))
                    .px(px(12.))
                    .py(px(8.))
                    .text_xs()
                    .text_color(t.error_foreground)
                    .child(message.clone()),
            );
        }
        card.into_any_element()
    }

    /// `signIn`: start the device flow, show its code, then wait for the user to approve it.
    fn start_sign_in(&mut self, state: &Entity<SourceControlState>, input: Value, cx: &mut Context<Self>) {
        let Some(client) = self.settings_client() else { return };
        state.update(cx, |state, cx| {
            state.sign_in = SignIn::Starting;
            cx.notify();
        });
        let state = state.clone();
        cx.spawn(async move |this, cx| {
            let started = client.request("sourceControl.signInStart", input.clone()).await;
            let prompt = match started {
                Ok(prompt) => prompt,
                Err(error) => {
                    let _ = state.update(cx, |state, cx| {
                        state.sign_in = SignIn::Failed(failure_message(&error));
                        cx.notify();
                    });
                    return;
                }
            };
            let user_code = prompt["userCode"].as_str().unwrap_or_default().to_owned();
            let _ = state.update(cx, |state, cx| {
                state.sign_in = SignIn::Waiting {
                    user_code: user_code.clone(),
                    verification_uri: prompt["verificationUri"].as_str().unwrap_or_default().to_owned(),
                    expires_at: prompt["expiresAt"].as_f64().unwrap_or_default() as i64,
                };
                cx.notify();
            });
            let finished = client.request("sourceControl.signInWait", input).await;
            let _ = state.update(cx, |state, cx| {
                // A newer attempt, a cancel, or a sign-out owns the state by now.
                if matches!(&state.sign_in, SignIn::Waiting { user_code: code, .. } if *code == user_code) {
                    state.sign_in = match &finished {
                        Ok(_) => SignIn::Idle,
                        Err(error) => SignIn::Failed(failure_message(error)),
                    };
                    cx.notify();
                }
            });
            if finished.is_ok() {
                let _ = this.update(cx, |this, cx| this.rescan(&state, cx));
            }
        })
        .detach();
    }

    fn device_code(
        &self,
        state: &Entity<SourceControlState>,
        code: &str,
        uri: &str,
        expires_at: i64,
        copied: bool,
        t: &Tokens,
    ) -> AnyElement {
        let cells = code.chars().map(|c| {
            if c == '-' {
                div().px(px(2.)).text_color(t.muted_foreground).child("–")
            } else {
                div()
                    .h(px(40.))
                    .w(px(32.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.background)
                    .child(c.to_string())
            }
        });
        let expiry = chrono::DateTime::from_timestamp_millis(expires_at)
            .map(|at| {
                let at = at.with_timezone(&chrono::Local);
                let (pm, hour) = chrono::Timelike::hour12(&at);
                format!("{hour}:{:02} {}", chrono::Timelike::minute(&at), if pm { "PM" } else { "AM" })
            })
            .unwrap_or_default();
        let copy = {
            let (state, code) = (state.clone(), code.to_owned());
            move |cx: &mut gpui_kit::App| {
                cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                state.update(cx, |state, cx| {
                    state.copied = true;
                    state._copied = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_millis(1500)).await;
                        let _ = this.update(cx, |state, cx| {
                            state.copied = false;
                            cx.notify();
                        });
                    }));
                    cx.notify();
                });
            }
        };
        let copy_open = copy.clone();
        let uri = uri.to_owned();
        h_flex()
            .flex_wrap()
            .justify_between()
            .gap(px(16.))
            .border_t_1()
            .border_color(t.border)
            .bg(t.muted.opacity(0.4))
            .px(px(12.))
            .py(px(16.))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        h_flex()
                            .gap(px(4.))
                            .font_family(t.fonts.mono)
                            .text_lg()
                            .font_weight(FontWeight::MEDIUM)
                            .children(cells),
                    )
                    .child(
                        Button::new("copy-sign-in-code")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(if copied { Lucide::Check } else { Lucide::Copy }).with_size(px(14.)))
                            .on_click(move |_, _, cx| copy(cx)),
                    ),
            )
            .child(
                v_flex()
                    .items_end()
                    .gap(px(8.))
                    .child(
                        Button::new("open-sign-in")
                            .primary()
                            .small()
                            .label("Copy code & open GitHub")
                            .child(Icon::new(Lucide::ExternalLink).with_size(px(14.)))
                            .on_click(move |_, _, cx| {
                                copy_open(cx);
                                cx.open_url(&uri);
                            }),
                    )
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .text_xs()
                            .text_color(t.muted_foreground)
                            .child(div().size(px(6.)).rounded_full().bg(t.warning))
                            .child(format!("Waiting for approval · code expires at {expiry}")),
                    ),
            )
            .into_any_element()
    }

    /// `SourceControlTokenSettings`: a token per hostname, used before the CLI's login.
    fn token_form(
        &self,
        state: &Entity<SourceControlState>,
        kind: &'static str,
        credentials: &'static Credentials,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *tokens(cx);
        let tokens_map = self.host_settings(kind)["tokens"].clone();
        let (host_input, token_input, saving) = {
            let state = state.read(cx);
            (state.token_host.clone(), state.token.clone(), state.saving)
        };
        let typed_host = host_input.read(cx).value().trim().to_lowercase();
        let draft = token_input.read(cx).value().trim().to_owned();
        let is_saved = has_secret(&tokens_map[&typed_host]);
        if state.read(cx).token_saved != is_saved {
            state.update(cx, |state, _| state.token_saved = is_saved);
            let placeholder = if is_saved { "Stored secret, enter a new value to replace" } else { "Not set" };
            token_input.update(cx, |input, cx| input.set_placeholder(placeholder, window, cx));
        }
        let saved_hosts: Vec<String> = tokens_map
            .as_object()
            .map(|tokens| tokens.iter().filter(|(_, value)| has_secret(value)).map(|(host, _)| host.clone()).collect())
            .unwrap_or_default();
        let link_host = if typed_host.is_empty() { credentials.public_host.to_owned() } else { typed_host.clone() };
        let save = {
            let (state, tokens_map) = (state.clone(), tokens_map.clone());
            move |this: &mut Self, host: String, token: String, cx: &mut Context<Self>| {
                state.update(cx, |state, _| state.saving = true);
                // The whole map: other tokens' redaction markers keep their values.
                let mut tokens = tokens_map.as_object().cloned().unwrap_or_default();
                tokens.insert(host, json!(token));
                let state = state.clone();
                this.update_environment_settings(
                    json!({ "sourceControlHosts": { kind: { "tokens": tokens } } }),
                    cx,
                    move |this, ok, cx| {
                        state.update(cx, |state, _| state.saving = false);
                        if ok {
                            this.clear_token_draft(&state, cx);
                            this.rescan(&state, cx);
                        }
                    },
                );
            }
        };
        let remove = save.clone();
        let remove_host = typed_host.clone();
        let can_save = !typed_host.is_empty() && !draft.is_empty() && !saving;
        v_flex()
            .gap(px(12.))
            .child({
                let tail = format!(
                    " login, so GitHub works without the {}. Give it {}.",
                    credentials.cli_label, credentials.token_access
                );
                v_flex()
                    .max_w(px(672.))
                    .gap(px(2.))
                    .text_xs()
                    .line_height(px(19.5))
                    .text_color(t.muted_foreground)
                    .child(prose(
                        &[
                            ("A token saved here is used before ", false),
                            (credentials.token_env_var, true),
                            (" and the ", false),
                            (credentials.cli_name, true),
                            (&tail, false),
                        ],
                        &t,
                    ))
                    .child(external_link("new-token", "Create a token", &(credentials.new_token_url)(&link_host), &t))
            })
            .child(
                h_flex()
                    .gap(px(12.))
                    .child(field_label(
                        "Host",
                        div().w(px(192.)).child(Input::new(&host_input).small().disabled(saving)),
                        &t,
                    ))
                    .child(
                        field_label(
                            "Token",
                            div().flex_1().child(Input::new(&token_input).small().disabled(saving)),
                            &t,
                        )
                        .flex_1(),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .gap(px(12.))
                    .child(div().text_xs().text_color(t.muted_foreground).child(if saved_hosts.is_empty() {
                        format!(
                            "No token saved; the server uses {} or the {} login.",
                            credentials.token_env_var, credentials.cli_name
                        )
                    } else {
                        format!("Saved for {}.", saved_hosts.join(", "))
                    }))
                    .child(
                        h_flex()
                            .flex_none()
                            .gap(px(8.))
                            .when(is_saved, |el| {
                                el.child(
                                    Button::new("remove-token")
                                        .outline()
                                        .xsmall()
                                        .label("Remove")
                                        .disabled(saving)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            remove(this, remove_host.clone(), String::new(), cx)
                                        })),
                                )
                            })
                            .child(
                                Button::new("save-token")
                                    .primary()
                                    .xsmall()
                                    .label("Save")
                                    .disabled(!can_save)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if can_save {
                                            save(this, typed_host.clone(), draft.clone(), cx)
                                        }
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn clear_token_draft(&self, state: &Entity<SourceControlState>, cx: &mut Context<Self>) {
        let token = state.read(cx).token.clone();
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, cx| token.update(cx, |input, cx| input.set_value("", window, cx)));
        }
    }

    /// `SourceControlAccountSettings`: which CLI login each host uses, or the host turned off.
    fn account_choices(
        &self,
        state: &Entity<SourceControlState>,
        kind: &'static str,
        credentials: &'static Credentials,
        accounts: &[Value],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *tokens(cx);
        let groups = group_host_accounts(accounts);
        if groups.is_empty() {
            return div()
                .text_xs()
                .text_color(t.muted_foreground)
                .child(prose(
                    &[
                        ("Sign in with ", false),
                        (credentials.login_command, true),
                        (" on the server host, then rescan to choose accounts here.", false),
                    ],
                    &t,
                ))
                .into_any_element();
        }
        let hosts = self.host_settings(kind)["hosts"].clone();
        let (revealed, saving) = {
            let state = state.read(cx);
            (state.accounts_revealed, state.saving)
        };
        let login = move |account: &str| -> AnyElement {
            div()
                .truncate()
                .font_family(t.fonts.mono)
                .text_size(px(10.))
                .child(if revealed { account.to_owned() } else { redacted(account) })
                .into_any_element()
        };
        let save = {
            let (state, hosts) = (state.clone(), hosts.clone());
            move |this: &mut Self,
                  host: &str,
                  enabled: Option<bool>,
                  account: Option<Option<&str>>,
                  cx: &mut Context<Self>| {
                state.update(cx, |state, _| state.saving = true);
                let choices = next_host_choices(&hosts, host, enabled, account);
                let state = state.clone();
                this.update_environment_settings(
                    json!({ "sourceControlHosts": { kind: { "hosts": choices } } }),
                    cx,
                    move |this, ok, cx| {
                        state.update(cx, |state, _| state.saving = false);
                        if ok {
                            this.rescan(&state, cx);
                        }
                    },
                );
            }
        };
        let reveal_state = state.clone();
        let mut column = v_flex().gap(px(16.)).child(
            h_flex()
                .justify_between()
                .gap(px(12.))
                .child(div().flex_1().max_w(px(672.)).text_xs().text_color(t.muted_foreground).child(prose(
                    &[
                        ("Choose which ", false),
                        (credentials.cli_name, true),
                        (" login each GitHub host uses, or turn a host off.", false),
                    ],
                    &t,
                )))
                .child(
                    Button::new("reveal-accounts")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(if revealed { Lucide::EyeOff } else { Lucide::Eye }).with_size(px(14.)))
                        .on_click(move |_, _, cx| {
                            reveal_state.update(cx, |state, cx| {
                                state.accounts_revealed = !state.accounts_revealed;
                                cx.notify();
                            })
                        }),
                ),
        );
        for group in groups {
            let choice = &hosts[&group.host];
            let enabled = choice["enabled"].as_bool().unwrap_or(true);
            let pinned = choice["account"].as_str().map(str::to_owned);
            let stale = pinned.as_ref().is_some_and(|account| !group.selectable.contains(account));
            let current = pinned.clone().filter(|_| !stale).unwrap_or_else(|| ACTIVE_ACCOUNT.into());
            let host_id = group.host.clone();
            let toggle = save.clone();
            let mut block = v_flex().gap(px(8.)).child(
                h_flex()
                    .justify_between()
                    .gap(px(12.))
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(t.foreground)
                                    .child(group.host.clone()),
                            )
                            .when(group.selectable.len() == 1, |el| {
                                el.child(
                                    h_flex()
                                        .gap(px(4.))
                                        .text_xs()
                                        .text_color(t.muted_foreground)
                                        .child("Signed in as")
                                        .child(login(&group.selectable[0])),
                                )
                            }),
                    )
                    .child(
                        Switch::new(SharedString::from(format!("use-host-{}", group.host)))
                            .checked(enabled)
                            .disabled(saving)
                            .on_click(
                                cx.listener(move |this, on: &bool, _, cx| toggle(this, &host_id, Some(*on), None, cx)),
                            ),
                    ),
            );
            if group.selectable.len() > 1 {
                let active_label = match &group.active {
                    Some(active) => format!(
                        "Active {} account ({})",
                        credentials.cli_name,
                        if revealed { active.clone() } else { redacted(active) }
                    ),
                    None => format!("Active {} account", credentials.cli_name),
                };
                let label = if current == ACTIVE_ACCOUNT {
                    active_label.clone()
                } else if revealed {
                    current.clone()
                } else {
                    redacted(&current)
                };
                let mut options = vec![(SharedString::from(ACTIVE_ACCOUNT), SharedString::from(active_label))];
                options.extend(group.selectable.iter().map(|account| {
                    (
                        SharedString::from(account.clone()),
                        SharedString::from(if revealed { account.clone() } else { redacted(account) }),
                    )
                }));
                let (pick, host_id) = (save.clone(), group.host.clone());
                let options = options
                    .into_iter()
                    .map(|(value, label)| (value.clone(), label, value == current.as_str()))
                    .collect();
                let picker = self.menu_select(
                    format!("account-{}", group.host),
                    options,
                    label,
                    256.,
                    cx,
                    move |this, value: SharedString, _, cx| {
                        let account = (value.as_ref() != ACTIVE_ACCOUNT).then_some(value.as_ref());
                        pick(this, &host_id, None, Some(account), cx);
                    },
                );
                block = block.child(
                    h_flex()
                        .justify_between()
                        .gap(px(12.))
                        .child(div().text_xs().text_color(t.muted_foreground).child("Account"))
                        .child(div().w(px(256.)).when(saving || !enabled, |el| el.opacity(0.5)).child(picker)),
                );
            }
            if stale {
                let (reset, host_id) = (save.clone(), group.host.clone());
                block = block.child(
                    h_flex()
                        .justify_between()
                        .gap(px(12.))
                        .child(div().text_xs().text_color(t.warning_foreground).child(format!(
                            "The chosen login is no longer signed in, so the active {} login is used.",
                            credentials.cli_name
                        )))
                        .child(
                            Button::new(SharedString::from(format!("use-active-{}", group.host)))
                                .outline()
                                .xsmall()
                                .label("Use active login")
                                .disabled(saving)
                                .on_click(
                                    cx.listener(move |this, _, _, cx| reset(this, &host_id, None, Some(None), cx)),
                                ),
                        ),
                );
            }
            for (account, error) in &group.broken {
                block = block.child(
                    h_flex()
                        .flex_wrap()
                        .gap(px(4.))
                        .text_xs()
                        .text_color(t.muted_foreground.opacity(0.7))
                        .child(login(account))
                        .child(format!(
                            "can't be used: {}",
                            error
                                .clone()
                                .unwrap_or_else(|| format!("{} reports this login as invalid.", credentials.cli_name))
                        )),
                );
            }
            if let Some(variable) = &group.environment_variable {
                block = block.child(div().text_xs().text_color(t.warning_foreground).child(format!(
                    "{variable} is set on the server, so it overrides the account chosen here until it is unset."
                )));
            }
            column = column.child(block);
        }
        column.into_any_element()
    }

    /// `SourceControlHostSettings`: the host's own fields, each saved on blur, and Remove.
    fn host_fields(
        &mut self,
        state: &Entity<SourceControlState>,
        host: &'static HostDefinition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *tokens(cx);
        let saved = self.host_settings(host.kind);
        let kind = host.kind;
        let mut column = v_flex().gap(px(12.));
        for field in host.fields {
            let key = field.key;
            let stored = &saved[key];
            let commit = {
                let state = state.clone();
                move |this: &mut Self, text: String, _: &mut Window, cx: &mut Context<Self>| {
                    let state = state.clone();
                    this.update_environment_settings(
                        json!({ "sourceControlHosts": { kind: { key: text } } }),
                        cx,
                        move |this, ok, cx| {
                            if ok {
                                this.rescan(&state, cx);
                            }
                        },
                    );
                }
            };
            let input = if field.secret {
                let placeholder =
                    if has_secret(stored) { "Stored secret, enter a new value to replace" } else { "Not set" };
                self.secret_field(&format!("{kind}-{key}"), placeholder, window, cx, commit)
            } else {
                let text = stored.as_str().unwrap_or_default();
                self.text_field(&format!("{kind}-{key}"), text, field.placeholder, false, None, window, cx, commit)
            };
            column = column.child(
                v_flex()
                    .gap(px(6.))
                    .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(t.foreground).child(field.title))
                    .child(input.sized(px(384.)))
                    .child(div().max_w(px(672.)).text_xs().text_color(t.muted_foreground).child(field.description)),
            );
        }
        let has_saved = saved.as_object().is_some_and(|fields| fields.values().any(has_secret));
        if has_saved {
            let state = state.clone();
            let cleared: Map<String, Value> = saved
                .as_object()
                .map(|fields| fields.keys().map(|key| (key.clone(), json!(""))).collect())
                .unwrap_or_default();
            column = column.child(h_flex().justify_end().pt(px(8.)).child(
                Button::new(SharedString::from(format!("remove-{kind}"))).outline().xsmall().label("Remove").on_click(
                    cx.listener(move |this, _, _, cx| {
                        let state = state.clone();
                        this.update_environment_settings(
                            json!({ "sourceControlHosts": { kind: cleared.clone() } }),
                            cx,
                            move |this, ok, cx| {
                                if ok {
                                    this.rescan(&state, cx);
                                }
                            },
                        );
                    }),
                ),
            ));
        }
        column.into_any_element()
    }

    /// `SourceControlWritingSettingsSection`: writing style, review guides, and the writer model.
    fn writing_rows(&mut self, view: &SettingsView, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = *tokens(cx);
        let connected = self.settings_client().is_some();
        let style = view.value("sourceControlWritingStyle").clone();
        let mode = style["mode"].as_str().unwrap_or("repo_conventions").to_owned();
        let instructions = style["customInstructions"].as_str().unwrap_or_default().to_owned();
        let (_, mode_label, mode_description) =
            WRITING_STYLES.iter().find(|(id, ..)| *id == mode).copied().unwrap_or(WRITING_STYLES[0]);
        let instructions_field = self.text_field(
            "source-control-custom-instructions",
            &instructions,
            "Keep titles concise. Use short bullet points in descriptions.",
            true,
            None,
            window,
            cx,
            |this, text, window, cx| {
                this.update_settings(json!({ "sourceControlWritingStyle": { "customInstructions": text } }), window, cx)
            },
        );
        let typed = instructions_field.clone();
        let custom = mode == "custom";
        let style_options =
            WRITING_STYLES.iter().map(|(id, label, _)| (*id, SharedString::from(*label), *id == mode)).collect();
        let style_select = self.menu_select(
            "writing-style",
            style_options,
            mode_label,
            224.,
            cx,
            move |this, picked: &str, window, cx| {
                // Carries what was typed into the custom instructions along with the mode.
                let mut style = json!({ "mode": picked });
                if custom {
                    style["customInstructions"] = json!(typed.value(cx).trim());
                }
                this.update_settings(json!({ "sourceControlWritingStyle": style }), window, cx)
            },
        );

        let providers = &self.settings_info().providers;
        let usable: Vec<&crate::model::Provider> =
            providers.iter().filter(|p| p.is_selectable() && generates_text(p)).collect();
        let stored_text: Option<ModelSelection> =
            serde_json::from_value(view.value("textGenerationModelSelection").clone()).ok();
        let default_selection = resolve_text_generation(stored_text.as_ref(), providers);
        let writer: Option<ModelSelection> =
            serde_json::from_value(view.value("sourceControlWriterModelSelection").clone()).ok();
        let writer_usable = writer.as_ref().filter(|w| usable.iter().any(|p| p.instance_id == w.instance_id));
        let active = resolve_text_generation(writer_usable.or(stored_text.as_ref()), providers);
        let can_dedicate =
            default_selection.as_ref().is_some_and(|s| usable.iter().any(|p| p.instance_id == s.instance_id));
        let guides = view.bool("reviewGuidesEnabled");
        let guide_control = (guides && connected).then(|| {
            let guide: Option<ModelSelection> =
                serde_json::from_value(view.value("reviewGuideModelSelection").clone()).ok();
            let guide_usable = guide.as_ref().filter(|g| usable.iter().any(|p| p.instance_id == g.instance_id));
            let guide_selection = guide_usable.cloned().or_else(|| active.clone());
            self.dedicated_model_control(
                "review-guide-model",
                "reviewGuideModelSelection",
                guide.is_some(),
                guide_selection.filter(|_| guide.is_some() && can_dedicate),
                active.clone(),
                guide.is_none() && !can_dedicate,
                None,
                cx,
            )
        });
        let writer_control = connected.then(|| {
            let note =
                (writer.is_some() && !can_dedicate).then(|| muted("No text generation providers available.", &t));
            self.dedicated_model_control(
                "writer-model",
                "sourceControlWriterModelSelection",
                writer.is_some(),
                active.clone().filter(|_| writer.is_some() && can_dedicate),
                default_selection.clone(),
                false,
                note,
                cx,
            )
        });

        let mut row = |this: &mut Self, row: SettingRow| this.setting_row(view, row, window, cx);
        let mut style_row = SettingRow::new("source-control-writing-style")
            .description(mode_description)
            .setting("sourceControlWritingStyle")
            .control(style_select);
        if custom {
            style_row = style_row.body(textarea_body(&instructions_field));
        }
        let mut rows = vec![
            row(self, style_row),
            row(
                self,
                SettingRow::new("review-guides")
                    .description("Add a Guide tab to pull requests. An agent explains the changes and points out decisions that need review.")
                    .setting("reviewGuidesEnabled")
                    .switch(),
            ),
        ];
        if guides {
            let control = guide_control.unwrap_or_else(|| {
                muted("Connect an environment to choose its review guide model.", &t).into_any_element()
            });
            rows.push(row(
                self,
                SettingRow::new("review-guide-model")
                    .description("Model that reads a pull request to write its review guide. It reads the change itself over many steps, so a stronger model helps. Off uses the source control writer model.")
                    .setting("reviewGuideModelSelection")
                    .control(control),
            ));
            rows.push(row(
                self,
                SettingRow::new("review-guide-instructions")
                    .description("Your preferences for review guides: what to show first, dwell on, or skip. The guide's format and coverage stay the same.")
                    .setting("reviewGuideInstructions")
                    .textarea("Show service interfaces before implementations. Skip test files unless they change behavior. Keep summaries to two sentences."),
            ));
        }
        let follow = style["followChangeRequestTemplates"] != false;
        let follow = switch(&"follow-change-request-templates".into(), follow, false, &t).on_click(cx.listener(
            move |this, _, window, cx| {
                this.update_settings(
                    json!({ "sourceControlWritingStyle": { "followChangeRequestTemplates": !follow } }),
                    window,
                    cx,
                )
            },
        ));
        let writer_control = writer_control.unwrap_or_else(|| {
            muted("Connect an environment to choose its source control writer model.", &t).into_any_element()
        });
        let mut row = |this: &mut Self, row: SettingRow| this.setting_row(view, row, window, cx);
        let folded = [
            row(
                self,
                SettingRow::titled("follow-change-request-templates", "Follow change request templates")
                    .description("Use the repository's template for change request descriptions when available.")
                    .control(follow),
            ),
            row(
                self,
                SettingRow::new("source-control-writer-model")
                    .description("Model for source control text and branch or bookmark names. When disabled, use the text generation model configured for this environment.")
                    .setting("sourceControlWriterModelSelection")
                    .control(writer_control),
            ),
        ];
        rows.push(self.advanced_fold("source-control", folded, cx));
        rows
    }

    /// A model a feature can dedicate: the picker while on, and a switch that turns it on with
    /// `enable_with` or off (null), so the feature falls back to its parent model.
    #[allow(clippy::too_many_arguments)]
    fn dedicated_model_control(
        &self,
        id: &'static str,
        key: &'static str,
        on: bool,
        selection: Option<ModelSelection>,
        enable_with: Option<ModelSelection>,
        dimmed: bool,
        note: Option<gpui_kit::Div>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *tokens(cx);
        let set = move |this: &mut Self, value: Value, window: &mut Window, cx: &mut Context<Self>| {
            this.update_settings(json!({ key: value }), window, cx)
        };
        let traits_id: &'static str = if id == "writer-model" { "writer-traits" } else { "review-guide-traits" };
        h_flex()
            .flex_wrap()
            .justify_end()
            .gap(px(8.))
            .children(note)
            .when_some(selection, |el, selection| {
                el.child(self.model_picker(
                    id,
                    Some(&selection),
                    None,
                    generates_text,
                    cx,
                    move |this, pick, window, cx| {
                        set(this, json!({ "instanceId": pick.instance_id, "model": pick.model }), window, cx)
                    },
                ))
                .children(self.traits_picker(
                    traits_id,
                    &selection,
                    cx,
                    move |this, pick, window, cx| set(this, pick.to_json(), window, cx),
                ))
            })
            .child(switch(&id.into(), on, false, &t).on_click(cx.listener(move |this, _, window, cx| {
                match (on, &enable_with) {
                    (true, _) => set(this, Value::Null, window, cx),
                    (false, Some(selection)) => set(this, selection.to_json(), window, cx),
                    (false, None) => {}
                }
            })))
            .when(dimmed, |el| el.opacity(0.5))
            .into_any_element()
    }
}

/// Sentinel select value for "follow the CLI's active login"; logins never contain spaces.
const ACTIVE_ACCOUNT: &str = "active cli account";

fn vcs_icon(kind: &str) -> Option<&'static str> {
    match kind {
        "git" => Some("icons/app/source-control/git.svg"),
        "jj" => Some("icons/app/source-control/jj.svg"),
        _ => None,
    }
}

/// A host or VCS mark at 18px: GitHub's is one color and follows the text, the rest are drawn as is.
fn source_control_icon(path: &'static str, t: &Tokens) -> AnyElement {
    if path.ends_with("/github.svg") {
        svg().path(path).size(px(18.)).text_color(t.foreground.opacity(0.8)).into_any_element()
    } else {
        img(path).size(px(18.)).into_any_element()
    }
}

fn badge(text: &'static str, color: Hsla, bg: Hsla) -> AnyElement {
    div()
        .px(px(6.))
        .h(px(18.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .bg(bg)
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(text)
        .into_any_element()
}

/// A sentence with inline `<code>` spans as one text run, so it wraps like the web's paragraph.
fn prose(parts: &[(&str, bool)], t: &Tokens) -> StyledText {
    let mut text = String::new();
    let mut highlights = Vec::new();
    for (part, code) in parts {
        let start = text.len();
        text.push_str(part);
        if *code {
            let style =
                HighlightStyle { background_color: Some(t.muted), color: Some(t.foreground), ..Default::default() };
            highlights.push((start..text.len(), style));
        }
    }
    StyledText::new(text).with_highlights(highlights)
}

fn external_link(id: &'static str, label: &'static str, url: &str, t: &Tokens) -> AnyElement {
    let url = url.to_owned();
    h_flex()
        .id(id)
        .gap(px(4.))
        .cursor_pointer()
        .text_color(t.foreground)
        .hover(|el| el.underline())
        .child(label)
        .child(Icon::new(Lucide::ExternalLink).with_size(px(12.)))
        .on_click(move |_, _, cx| cx.open_url(&url))
        .into_any_element()
}

fn field_label(label: &'static str, input: impl IntoElement, t: &Tokens) -> gpui_kit::Div {
    v_flex()
        .gap(px(6.))
        .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(t.foreground).child(label))
        .child(input)
}

/// `SourceControlSectionSkeleton`: two placeholder rows while the first scan runs.
fn skeleton_rows(t: &Tokens) -> Vec<AnyElement> {
    let bar = |w: f32, h: f32| div().w(px(w)).h(px(h)).rounded_full().bg(t.muted_foreground.opacity(0.12));
    ["primary", "secondary"]
        .into_iter()
        .map(|_| {
            h_flex()
                .px(px(16.))
                .py(px(12.))
                .gap(px(12.))
                .child(
                    v_flex()
                        .flex_1()
                        .gap(px(8.))
                        .child(
                            h_flex()
                                .gap(px(8.))
                                .child(bar(18., 18.).rounded(px(4.)))
                                .child(bar(112., 16.))
                                .child(bar(56., 20.)),
                        )
                        .child(bar(320., 12.)),
                )
                .child(h_flex().gap(px(8.)).child(bar(28., 28.).rounded(px(6.))).child(bar(36., 20.)))
                .into_any_element()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_group_by_host() {
        let accounts = vec![
            json!({"host": "github.com", "account": "a", "active": false, "authenticated": true}),
            json!({"host": "github.com", "account": "b", "active": true, "authenticated": true}),
            json!({"host": "github.com", "account": "c", "active": false, "authenticated": false, "error": "expired"}),
            json!({"host": "ghe.example", "account": "env", "active": true, "authenticated": true, "environmentVariable": "GH_TOKEN"}),
        ];
        let groups = group_host_accounts(&accounts);
        assert_eq!(groups[0].active.as_deref(), Some("b"));
        assert_eq!(groups[0].selectable, ["a", "b"]);
        assert_eq!(groups[0].broken, [("c".to_owned(), Some("expired".to_owned()))]);
        assert_eq!(
            groups[1],
            HostAccounts {
                host: "ghe.example".into(),
                active: None,
                selectable: vec![],
                broken: vec![],
                environment_variable: Some("GH_TOKEN".into()),
            }
        );
    }

    #[test]
    fn host_choices_drop_hosts_on_their_defaults() {
        let current = json!({"github.com": {"enabled": true, "account": "a"}, "ghe.example": {"enabled": false}});
        assert_eq!(
            next_host_choices(&current, "github.com", None, Some(None)),
            json!({"ghe.example": {"enabled": false}}),
            "following the active login again is the default"
        );
        assert_eq!(
            next_host_choices(&current, "ghe.example", Some(true), None),
            json!({"github.com": {"enabled": true, "account": "a"}})
        );
        assert_eq!(
            next_host_choices(&current, "github.com", Some(false), None),
            json!({"github.com": {"enabled": false, "account": "a"}, "ghe.example": {"enabled": false}})
        );
    }

    #[test]
    fn redaction_keeps_shape_and_is_stable() {
        let hidden = redacted("octo-cat@example.com");
        assert_eq!(hidden.len(), "octo-cat@example.com".len());
        assert_eq!(hidden.chars().filter(|c| "@.-".contains(*c)).count(), 3);
        assert_ne!(hidden, "octo-cat@example.com");
        assert_eq!(hidden, redacted("octo-cat@example.com"));
    }
}
