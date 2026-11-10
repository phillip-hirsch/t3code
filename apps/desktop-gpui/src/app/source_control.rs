//! The source control hosts a client can publish to or clone from (`sourceControlClients`, the
//! `source-control-*/src/shared/definition.ts` registry) and whether discovery says each one is
//! ready (`getPublishProviderReadiness` in `GitActionsControl.tsx`,
//! `buildAddProjectRemoteSourceReadiness` in `client-runtime/src/operations/projects.ts` and
//! `buildPaletteRemoteSourceReadiness` in `CommandPalette.logic.ts`).

use gpui_kit::{AnyElement, Hsla, IntoElement as _, Styled as _, img, px, svg};
use serde_json::Value;

/// One host's client-side definition.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HostDefinition {
    pub kind: &'static str,
    /// `pickerLabel`.
    pub label: &'static str,
    /// `repositoryPathHint`, the repository field's placeholder.
    pub path_hint: &'static str,
    /// `publishHost` without a signed-in host; `None` when it names the signed-in server.
    fixed_publish_host: Option<&'static str>,
    icon: &'static str,
    /// The mark is one color and follows the text (`isMonochromeSourceControlIcon`).
    monochrome: bool,
}

impl HostDefinition {
    /// `publishHost(signedInHost)`: the host the repository field's prefix shows.
    pub(crate) fn publish_host<'a>(&self, signed_in_host: Option<&'a str>) -> &'a str {
        self.fixed_publish_host.unwrap_or_else(|| signed_in_host.unwrap_or("your server"))
    }

    /// The host's mark at `size`px; a monochrome one takes `color`.
    pub(crate) fn mark(&self, size: f32, color: Hsla) -> AnyElement {
        if self.monochrome {
            svg().path(self.icon).size(px(size)).flex_none().text_color(color).into_any_element()
        } else {
            img(self.icon).size(px(size)).flex_none().into_any_element()
        }
    }
}

const fn host(
    kind: &'static str,
    label: &'static str,
    path_hint: &'static str,
    fixed_publish_host: Option<&'static str>,
    icon: &'static str,
    monochrome: bool,
) -> HostDefinition {
    HostDefinition { kind, label, path_hint, fixed_publish_host, icon, monochrome }
}

/// `builtInSourceControlDefinitions.definitions`, in registry order; GitHub first is the default.
pub(crate) const HOSTS: &[HostDefinition] = &[
    host("github", "GitHub", "owner/repo", Some("github.com"), "icons/app/source-control/github.svg", true),
    host(
        "cursor-origin",
        "Cursor Origin",
        "owner/repo",
        Some("origin.cursor.com"),
        "icons/app/source-control/cursor-origin.svg",
        true,
    ),
    host("gitlab", "GitLab", "group/project", Some("gitlab.com"), "icons/app/source-control/gitlab.svg", false),
    host("gitcafe", "GitCafe", "owner/repo", Some("git.cafe"), "icons/app/source-control/gitcafe.svg", false),
    host("forgejo", "Forgejo / Gitea", "owner/repo", None, "icons/app/source-control/forgejo.svg", false),
    host(
        "bitbucket",
        "Bitbucket",
        "workspace/repository",
        Some("bitbucket.org"),
        "icons/app/source-control/bitbucket.svg",
        false,
    ),
    host(
        "azure-devops",
        "Azure DevOps",
        "project/repository",
        Some("dev.azure.com"),
        "icons/app/source-control/azure-devops.svg",
        false,
    ),
];

/// `sourceControlClients.get(kind)`: an unknown kind reads as the default host.
pub(crate) fn definition(kind: &str) -> &'static HostDefinition {
    HOSTS.iter().find(|host| host.kind == kind).unwrap_or(&HOSTS[0])
}

/// `defaultCloneTransport`: GitHub, Cursor Origin and Forgejo clone over HTTPS, the rest over SSH.
pub(crate) fn clones_over_https(kind: &str) -> bool {
    matches!(definition(kind).kind, "github" | "cursor-origin" | "forgejo")
}

/// Whether a host can be used now, and what to do about it when it can't.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Readiness {
    pub ready: bool,
    pub hint: Option<String>,
}

/// Where the hint sends the user: the surfaces word the Settings path differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Surface {
    /// The publish wizard: "Open Settings -> Source control".
    Publish,
    /// Add project's clone sources: "Open Source Control settings".
    AddProject,
}

/// `Option<string>` on the wire: `{"_tag": "Some", "value": …}`.
fn some(value: &Value) -> Option<&str> {
    (value["_tag"] == "Some").then(|| value["value"].as_str()).flatten()
}

/// The discovery item for `kind` in a `server.discoverSourceControl` result.
pub(crate) fn discovered<'a>(discovery: Option<&'a Value>, kind: &str) -> Option<&'a Value> {
    discovery?["sourceControlProviders"].as_array()?.iter().find(|item| item["kind"] == kind)
}

/// The signed-in account (`auth.account`) discovery reports for `kind`.
pub(crate) fn auth_account<'a>(discovery: Option<&'a Value>, kind: &str) -> Option<&'a str> {
    some(&discovered(discovery, kind)?["auth"]["account"])
}

/// The signed-in host (`auth.host`) discovery reports for `kind`.
pub(crate) fn auth_host<'a>(discovery: Option<&'a Value>, kind: &str) -> Option<&'a str> {
    some(&discovered(discovery, kind)?["auth"]["host"])
}

/// `getPublishProviderReadiness` / `buildAddProjectRemoteSourceReadiness` for one host: missing
/// from discovery (or discovery not loaded), not installed, or signed out is not ready.
pub(crate) fn readiness(discovery: Option<&Value>, kind: &str, surface: Surface) -> Readiness {
    let settings = match surface {
        Surface::Publish => "Open Settings -> Source control",
        Surface::AddProject => "Open Source Control settings",
    };
    let Some(item) = discovered(discovery, kind) else {
        return Readiness { ready: false, hint: Some(format!("Provider status unavailable. {settings} and rescan.")) };
    };
    if item["status"] != "available" {
        return Readiness { ready: false, hint: item["installHint"].as_str().map(str::to_owned) };
    }
    if item["auth"]["status"] == "unauthenticated" {
        let hint = some(&item["auth"]["detail"]).map_or_else(
            || {
                let label = item["label"].as_str().unwrap_or(definition(kind).label);
                format!("{label} is not authenticated. {settings} for setup guidance.")
            },
            str::to_owned,
        );
        return Readiness { ready: false, hint: Some(hint) };
    }
    Readiness { ready: true, hint: None }
}

/// `buildPaletteRemoteSourceReadiness`: the add-project wording, with a server hint's
/// "Open source control settings" pointed at the Settings path.
pub(crate) fn palette_readiness(discovery: Option<&Value>, kind: &str) -> Readiness {
    let mut entry = readiness(discovery, kind, Surface::AddProject);
    entry.hint = entry.hint.map(|hint| hint.replace("Open source control settings", "Open Settings -> Source control"));
    entry
}

/// The hosts ready ones first, then by label (`sortAddProjectProviderSources`, the publish
/// wizard's `sortedPublishProviderOptions`).
pub(crate) fn sorted_by_readiness(discovery: Option<&Value>, surface: Surface) -> Vec<&'static HostDefinition> {
    let mut hosts: Vec<_> = HOSTS.iter().collect();
    hosts.sort_by_cached_key(|host| (!readiness(discovery, host.kind, surface).ready, host.label.to_lowercase()));
    hosts
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn item(kind: &str, status: &str, auth: Value) -> Value {
        json!({"kind": kind, "label": definition(kind).label, "status": status, "installHint": format!("Install {kind}"), "auth": auth})
    }

    fn signed_in(account: &str) -> Value {
        json!({"status": "authenticated", "account": {"_tag": "Some", "value": account}, "host": {"_tag": "None"}, "detail": {"_tag": "None"}})
    }

    #[test]
    fn readiness_follows_discovery_with_each_surfaces_wording() {
        let discovery = json!({"sourceControlProviders": [
            item("github", "available", signed_in("octo")),
            item("gitlab", "missing", signed_in("me")),
            item("forgejo", "available", json!({"status": "unauthenticated", "account": {"_tag": "None"}, "host": {"_tag": "None"}, "detail": {"_tag": "None"}})),
            item("bitbucket", "available", json!({"status": "unauthenticated", "detail": {"_tag": "Some", "value": "Run bb login"}})),
            item("azure-devops", "available", json!({"status": "unknown"})),
        ]});
        let d = Some(&discovery);
        assert_eq!(readiness(d, "github", Surface::Publish), Readiness { ready: true, hint: None });
        assert!(readiness(d, "azure-devops", Surface::Publish).ready, "an unknown sign-in still counts as ready");
        assert_eq!(readiness(d, "gitlab", Surface::Publish).hint.as_deref(), Some("Install gitlab"));
        assert_eq!(
            readiness(d, "forgejo", Surface::Publish).hint.as_deref(),
            Some("Forgejo / Gitea is not authenticated. Open Settings -> Source control for setup guidance.")
        );
        assert_eq!(
            readiness(d, "forgejo", Surface::AddProject).hint.as_deref(),
            Some("Forgejo / Gitea is not authenticated. Open Source Control settings for setup guidance.")
        );
        assert_eq!(readiness(d, "bitbucket", Surface::Publish).hint.as_deref(), Some("Run bb login"));
        assert_eq!(
            readiness(d, "gitcafe", Surface::Publish),
            Readiness {
                ready: false,
                hint: Some("Provider status unavailable. Open Settings -> Source control and rescan.".into())
            }
        );
        assert_eq!(
            readiness(None, "github", Surface::AddProject).hint.as_deref(),
            Some("Provider status unavailable. Open Source Control settings and rescan."),
            "nothing is ready before discovery answers"
        );
        assert_eq!(auth_account(d, "github"), Some("octo"));
        assert_eq!(auth_account(d, "forgejo"), None);
    }

    #[test]
    fn palette_points_server_hints_at_the_settings_path() {
        let discovery = json!({"sourceControlProviders": [{
            "kind": "gitlab", "label": "GitLab", "status": "missing",
            "installHint": "Install glab, then Open source control settings.", "auth": {"status": "unknown"},
        }]});
        assert_eq!(
            palette_readiness(Some(&discovery), "gitlab").hint.as_deref(),
            Some("Install glab, then Open Settings -> Source control.")
        );
        // The built-in wording is capitalized differently, so it is left as is (as on the web).
        assert_eq!(
            palette_readiness(None, "gitlab").hint.as_deref(),
            Some("Provider status unavailable. Open Source Control settings and rescan.")
        );
    }

    #[test]
    fn ready_hosts_sort_first_then_by_label() {
        let discovery = json!({"sourceControlProviders": [
            item("gitlab", "available", signed_in("me")),
            item("bitbucket", "available", signed_in("me")),
        ]});
        let kinds: Vec<_> =
            sorted_by_readiness(Some(&discovery), Surface::Publish).iter().map(|host| host.kind).collect();
        assert_eq!(kinds, ["bitbucket", "gitlab", "azure-devops", "cursor-origin", "forgejo", "gitcafe", "github"]);
    }

    #[test]
    fn publish_host_names_the_signed_in_server_where_it_has_no_fixed_one() {
        assert_eq!(definition("forgejo").publish_host(Some("git.example.org")), "git.example.org");
        assert_eq!(definition("forgejo").publish_host(None), "your server");
        assert_eq!(definition("gitlab").publish_host(Some("gitlab.example.org")), "gitlab.com");
        assert_eq!(definition("nope").kind, "github", "an unknown kind is the default host");
    }
}
