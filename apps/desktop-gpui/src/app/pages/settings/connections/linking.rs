//! Linking another machine to an environment (`linkRequest.ts`, `LinkMachineSelect.tsx` and
//! `peerPairingCode.ts`): the machines this client knows, ranked for an agent's hint, and a
//! pairing code minted there with this client's own session. Machines only T3 Connect knows
//! are left out: this client has no T3 Connect account.

use gpui_kit::{AnyElement, App, IntoElement as _, ParentElement as _, Styled as _, div, px};
use serde_json::{Value, json};

use super::logic::{ACCESS_WRITE, ORCHESTRATION_OPERATE, ORCHESTRATION_READ, SETTINGS_WRITE, SOURCE_CONTROL_WRITE};
use super::ui::{self, text_xs};
use crate::app::Workspace;
use crate::backend::Endpoint;
use crate::theme::Tokens;

/// The machine choice for linking by an address typed in.
pub(in crate::app) const ANOTHER_ADDRESS: &str = "another-address";

/// `LinkableMachine`: a machine the user could link an environment to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct LinkableMachine {
    pub environment_id: String,
    pub label: String,
    /// `EnvironmentMachineKind`, for its icon.
    pub machine: String,
    pub connected: bool,
    /// The environment being linked from already has an unexpired link to it.
    pub linked: bool,
    /// Where it answers, most preferred first: the addresses it reports, then its saved
    /// routes. Empty for a machine reached only over SSH, whose forward lives in this client.
    pub urls: Vec<String>,
}

/// What this client knows about one environment it can reach, catalog order.
pub(in crate::app) struct KnownMachine {
    pub environment_id: String,
    pub label: String,
    pub machine: String,
    pub connected: bool,
    /// `serverConfig.directEndpoints`.
    pub reported: Vec<String>,
    /// The saved routes' direct addresses.
    pub routes: Vec<String>,
}

/// `linkableMachines`: every known machine but `host`, with `host`'s unexpired links marked.
pub(in crate::app) fn linkable_machines(known: Vec<KnownMachine>, host: &str, links: &[Value]) -> Vec<LinkableMachine> {
    let linked = |id: &str| links.iter().any(|link| link["environmentId"] == id && link["status"] != "expired");
    known
        .into_iter()
        .filter(|machine| machine.environment_id != host)
        .map(|machine| {
            let mut urls: Vec<String> = Vec::new();
            for url in machine.reported.into_iter().chain(machine.routes) {
                if !urls.contains(&url) {
                    urls.push(url);
                }
            }
            LinkableMachine {
                linked: linked(&machine.environment_id),
                environment_id: machine.environment_id,
                label: machine.label,
                machine: machine.machine,
                connected: machine.connected,
                urls,
            }
        })
        .collect()
}

const NO_MATCH: u8 = 4;

/// ` the words joined by spaces `, so a whole-word search can match on the spaces.
fn words(text: &str) -> String {
    let lower = text.to_lowercase();
    let joined: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    format!(" {} ", joined.join(" "))
}

/// `hintRank`: how well a machine's name matches the agent's hint, often the user's own words
/// ("my box"): exact, prefix, substring, then a hint that names it among other words.
fn hint_rank(label: &str, hint: &str) -> u8 {
    let name = label.trim().to_lowercase();
    let lower = hint.trim().to_lowercase();
    let wanted = ["my", "our", "the"]
        .iter()
        .find_map(|article| {
            let rest = lower.strip_prefix(article)?;
            let trimmed = rest.trim_start();
            (trimmed.len() < rest.len()).then_some(trimmed)
        })
        .unwrap_or(&lower);
    if name == wanted {
        0
    } else if name.starts_with(wanted) {
        1
    } else if name.contains(wanted) {
        2
    } else if words(wanted).contains(&words(&name)) {
        3
    } else {
        NO_MATCH
    }
}

/// `rankMachinesByHint`: the best matches for the hint first, each rank in catalog order.
pub(in crate::app) fn rank_machines_by_hint(
    mut machines: Vec<LinkableMachine>,
    hint: Option<&str>,
) -> Vec<LinkableMachine> {
    if let Some(hint) = hint.filter(|hint| !hint.trim().is_empty()) {
        machines.sort_by_key(|machine| hint_rank(&machine.label, hint));
    }
    machines
}

/// `linkRequestInitialMachine`: the machine the request names, else the hint's best match.
pub(in crate::app) fn initial_machine(
    environment_id: Option<&str>,
    hint: Option<&str>,
    ranked: &[LinkableMachine],
) -> Option<String> {
    if let Some(id) = environment_id {
        return Some(id.to_owned());
    }
    let hint = hint?;
    let best = ranked.first()?;
    (hint_rank(&best.label, hint) < NO_MATCH).then(|| best.environment_id.clone())
}

/// `mcpClientScopes`: what a link at `access` may do there.
pub(in crate::app) fn mcp_client_scopes(access: &str) -> &'static [&'static str] {
    match access {
        "read-only" => &[ORCHESTRATION_READ],
        "full-access" => &[ORCHESTRATION_READ, ORCHESTRATION_OPERATE, SETTINGS_WRITE, SOURCE_CONTROL_WRITE],
        _ => &[ORCHESTRATION_READ, ORCHESTRATION_OPERATE],
    }
}

/// `PeerPairingCodeError`'s messages.
const MINT_NOT_ALLOWED: &str = "Your session there cannot create pairing codes. Paste a code from there instead.";
const MINT_UNAVAILABLE: &str = "Could not get a pairing code from there. Paste a code from there instead.";

/// `createPeerPairingCodeCommand`: a fresh one-use code minted at `endpoint` with this client's
/// session there, carrying exactly what a link at `access` needs. Each call mints its own.
pub(in crate::app) async fn mint_link_code(endpoint: Endpoint, access: &str) -> Result<String, &'static str> {
    let body = json!({ "label": "T3 Code link", "scopes": mcp_client_scopes(access) });
    let request = async move {
        let response = super::super::http()
            .post(format!("{}/api/auth/pairing-token", endpoint.http_base))
            .bearer_auth(&endpoint.bearer_token)
            .json(&body)
            .timeout(std::time::Duration::from_secs(8))
            .send()
            .await
            .map_err(|_| MINT_UNAVAILABLE)?;
        let status = response.status();
        if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(MINT_NOT_ALLOWED);
        }
        let value: Value = response.json().await.map_err(|_| MINT_UNAVAILABLE)?;
        match value["credential"].as_str() {
            Some(code) if status.is_success() && !code.is_empty() => Ok(code.to_owned()),
            _ => Err(MINT_UNAVAILABLE),
        }
    };
    crate::runtime().spawn(request).await.map_err(|_| MINT_UNAVAILABLE)?
}

impl Workspace {
    /// The environments this client knows, primary first, then the saved catalog's order.
    fn known_machines(&self, cx: &App) -> Vec<KnownMachine> {
        let reported = |environment_id: &str| -> Vec<String> {
            let config = self.env_info(environment_id).map(|info| &info.config);
            let endpoints = config.and_then(|config| config["directEndpoints"].as_array());
            endpoints.into_iter().flatten().filter_map(|e| e["httpBaseUrl"].as_str().map(str::to_owned)).collect()
        };
        let mut known = Vec::new();
        if let Some(primary) = self.sessions.first().filter(|s| s.id.is_some()) {
            let id = primary.id.as_deref().unwrap_or_default();
            known.push(KnownMachine {
                environment_id: id.to_owned(),
                label: primary.server_info.environment_label.clone().unwrap_or_else(|| id.to_owned()),
                machine: primary.server_info.machine_kind().to_owned(),
                connected: primary.is_connected(),
                reported: reported(id),
                routes: primary.endpoint.iter().map(|e| e.http_base.clone()).collect(),
            });
        }
        let saved = self.saved_environments.read(cx);
        for environment in saved.environments() {
            let id = environment.environment_id.as_str();
            if known.iter().any(|k| k.environment_id == id) {
                continue;
            }
            let session = self.session(id);
            let machine = match session.filter(|s| s.server_info.config.is_object()) {
                Some(session) => session.server_info.machine_kind().to_owned(),
                None => saved.session(id).and_then(|s| s.machine.clone()).unwrap_or_else(|| "server".into()),
            };
            known.push(KnownMachine {
                environment_id: id.to_owned(),
                label: environment.label.clone(),
                machine,
                connected: session.is_some_and(crate::app::sessions::EnvironmentSession::is_connected),
                reported: reported(id),
                routes: environment.routes.iter().filter_map(|r| r.http_base().map(str::to_owned)).collect(),
            });
        }
        known
    }

    /// The machines `host` could link to, given `host`'s links.
    pub(in crate::app) fn linkable_machines(&self, host: &str, links: &[Value], cx: &App) -> Vec<LinkableMachine> {
        linkable_machines(self.known_machines(cx), host, links)
    }

    /// `access:write` there: the grant that links, unlinks and answers link requests.
    pub(in crate::app) fn manages_access(&self, environment_id: &str) -> bool {
        self.env_grants(environment_id, ACCESS_WRITE)
    }

    /// Where this client can mint a pairing code on `environment_id`: connected there, with a
    /// session that may create pairing links. Checked again right before minting.
    pub(in crate::app) fn mint_endpoint(&self, environment_id: &str) -> Option<Endpoint> {
        let session = self.session(environment_id).filter(|s| s.is_connected())?;
        self.manages_access(environment_id).then(|| session.endpoint.clone()).flatten()
    }
}

/// `LinkMachineBadge`: linked already, or not connected from this client.
pub(in crate::app) fn machine_badge(machine: &LinkableMachine, t: &Tokens) -> Option<AnyElement> {
    let (label, color, border) = if machine.linked {
        ("Linked", ui::success(), ui::success().opacity(0.32))
    } else if !machine.connected {
        ("Not connected", t.muted_foreground, t.border)
    } else {
        return None;
    };
    Some(
        text_xs(div().flex_none().px(px(4.)).rounded(px(4.)).border_1().border_color(border).text_color(color))
            .child(label)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(id: &str, label: &str, reported: &[&str], routes: &[&str]) -> KnownMachine {
        KnownMachine {
            environment_id: id.into(),
            label: label.into(),
            machine: "server".into(),
            connected: true,
            reported: reported.iter().map(|s| (*s).to_owned()).collect(),
            routes: routes.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn machine(id: &str, label: &str) -> LinkableMachine {
        linkable_machines(vec![known(id, label, &[], &[])], "host", &[]).remove(0)
    }

    #[test]
    fn machines_skip_the_host_and_merge_their_addresses() {
        let links =
            [json!({"environmentId": "vps", "status": "active"}), json!({"environmentId": "old", "status": "expired"})];
        let machines = linkable_machines(
            vec![
                known("host", "Laptop", &[], &[]),
                known("vps", "VPS", &["https://vps.ts.net/"], &["https://vps.ts.net/", "http://10.0.0.2:3773"]),
                known("old", "Old box", &[], &[]),
            ],
            "host",
            &links,
        );
        let summary: Vec<(&str, bool, usize)> =
            machines.iter().map(|m| (m.environment_id.as_str(), m.linked, m.urls.len())).collect();
        assert_eq!(summary, [("vps", true, 2), ("old", false, 0)], "an expired link no longer counts");
        assert_eq!(machines[0].urls, ["https://vps.ts.net/", "http://10.0.0.2:3773"]);
    }

    #[test]
    fn hints_rank_exact_prefix_substring_then_words() {
        let machines = vec![
            machine("a", "Hetzner build box"),
            machine("b", "box"),
            machine("c", "Office Mac"),
            machine("d", "Boxer"),
        ];
        let ranked = rank_machines_by_hint(machines.clone(), Some("my box"));
        let ids: Vec<&str> = ranked.iter().map(|m| m.environment_id.as_str()).collect();
        assert_eq!(ids, ["b", "d", "a", "c"]);
        assert_eq!(initial_machine(None, Some("my box"), &ranked).as_deref(), Some("b"));
        assert_eq!(hint_rank("box", "the box on hetzner"), 3, "a hint that names it among other words");
        assert_eq!(
            initial_machine(None, Some("raspberry"), &rank_machines_by_hint(machines.clone(), Some("raspberry"))),
            None
        );
        assert_eq!(initial_machine(Some("c"), Some("box"), &ranked).as_deref(), Some("c"), "a named machine wins");
        assert_eq!(rank_machines_by_hint(machines.clone(), Some("  ")), machines, "a blank hint keeps catalog order");
    }

    #[test]
    fn link_codes_carry_the_access_scopes() {
        assert_eq!(mcp_client_scopes("read-only"), ["orchestration:read"]);
        assert_eq!(mcp_client_scopes("approval-required"), ["orchestration:read", "orchestration:operate"]);
        assert_eq!(mcp_client_scopes("full-access").len(), 4);
    }
}
