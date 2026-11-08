//! `projectCommitUrl`: a commit's page on the project's code host, built the way each host's
//! source control definition does (`commitUrl` in `packages/source-control-*`).

use crate::model::RepositoryIdentity;

/// `FULL_COMMIT_SHA`: a SHA-1 or SHA-256 commit id.
fn full_sha(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `remoteWebOrigin`: an HTTP remote on `host` names the server's web origin, port included;
/// SSH remotes do not.
fn remote_web_origin(host: &str, remote_url: Option<&str>) -> String {
    let remote = remote_url.and_then(|remote| url::Url::parse(remote).ok());
    let origin = remote.filter(|remote| matches!(remote.scheme(), "http" | "https")).and_then(|remote| {
        let hostname = remote.host_str()?;
        let with_port = remote.port().map_or_else(|| hostname.to_owned(), |port| format!("{hostname}:{port}"));
        (hostname.eq_ignore_ascii_case(host) || with_port.eq_ignore_ascii_case(host))
            .then(|| remote.origin().ascii_serialization())
    });
    origin.unwrap_or_else(|| format!("https://{host}"))
}

/// `buildCommitUrl`: `route/sha` below the repository's web page.
fn build(
    host: &str,
    repository: &str,
    sha: &str,
    remote_url: Option<&str>,
    web_url: Option<&str>,
    route: &str,
) -> Option<String> {
    if repository.is_empty() || !full_sha(sha) {
        return None;
    }
    let base = match web_url.filter(|web| !web.is_empty()) {
        Some(web) => web.to_owned(),
        None => format!("{}/{repository}", remote_web_origin(host, remote_url)),
    };
    let mut url = url::Url::parse(&base).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let path = format!("{}/{route}/{}", url.path().trim_end_matches('/'), sha.to_lowercase());
    url.set_path(&path);
    url.set_username("").ok()?;
    url.set_password(None).ok()?;
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

/// GitHub's `isRepositoryPath`: `owner/name` of word characters, dots and dashes.
fn github_repository(repository: &str) -> bool {
    let segments: Vec<&str> = repository.split('/').collect();
    segments.len() == 2
        && segments.iter().all(|segment| {
            !segment.is_empty()
                && *segment != "."
                && *segment != ".."
                && segment.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        })
}

/// Azure DevOps' `canonicalRepositoryKey`: every spelling of a Services repository, on
/// `dev.azure.com`.
fn azure_canonical(key: &str) -> Option<String> {
    let parts: Vec<&str> = key.split('/').collect();
    let canonical = match parts.as_slice() {
        ["ssh.dev.azure.com" | "vs-ssh.visualstudio.com", "v3", org, project, repo]
            if [org, project, repo].iter().all(|s| !s.is_empty()) =>
        {
            format!("dev.azure.com/{org}/{project}/_git/{repo}")
        }
        [host, rest @ ..] => {
            let rest = match rest {
                ["defaultcollection", rest @ ..] => rest,
                rest => rest,
            };
            match (host.strip_suffix(".visualstudio.com"), rest) {
                (Some(org), [project, "_git", repo])
                    if !org.is_empty() && !org.contains('.') && !project.is_empty() && !repo.is_empty() =>
                {
                    format!("dev.azure.com/{org}/{project}/_git/{repo}")
                }
                _ => key.to_owned(),
            }
        }
        [] => key.to_owned(),
    };
    canonical.starts_with("dev.azure.com/").then_some(canonical)
}

/// The commit's URL on the project's host, when the host is known and links commits.
pub(super) fn commit_url(identity: &RepositoryIdentity, sha: &str) -> Option<String> {
    let provider = identity.provider.as_deref()?;
    let key = identity.canonical_key.as_deref()?;
    let (host, repository) = key.split_once('/').unwrap_or((key, ""));
    if host.is_empty() {
        return None;
    }
    let remote = identity.locator.as_ref().map(|locator| locator.remote_url.trim());
    let web = identity.web_url.as_deref();
    match provider {
        "github" => github_repository(repository).then(|| build(host, repository, sha, remote, web, "commit"))?,
        "gitlab" => build(host, repository, sha, remote, web, "-/commit"),
        "bitbucket" => build(host, repository, sha, remote, web, "commits"),
        "forgejo" | "gitcafe" => build(host, repository, sha, remote, web, "commit"),
        "azure-devops" => {
            let key = format!("{host}/{repository}").to_lowercase();
            let key = azure_canonical(&key).unwrap_or(key);
            let (host, repository) = key.split_once('/').unwrap_or((&key, ""));
            build(host, repository, sha, remote, web, "commit")
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const SHA: &str = "ABCDEF0123456789abcdef0123456789abcdef01";

    fn identity(provider: &str, key: &str, remote: &str, web: Option<&str>) -> RepositoryIdentity {
        let mut value = json!({
            "canonicalKey": key,
            "provider": provider,
            "locator": {"source": "git-remote", "remoteName": "origin", "remoteUrl": remote},
        });
        if let Some(web) = web {
            value["webUrl"] = web.into();
        }
        serde_json::from_value(value).expect("an identity")
    }

    #[test]
    fn commits_link_to_their_hosts_page() {
        let sha = SHA.to_lowercase();
        let github = identity("github", "github.com/pingdotgg/t3code", "git@github.com:pingdotgg/t3code.git", None);
        assert_eq!(commit_url(&github, SHA), Some(format!("https://github.com/pingdotgg/t3code/commit/{sha}")));
        // An HTTP remote names the server's own origin, port included.
        let gitlab =
            identity("gitlab", "git.example:8443/group/sub/app", "https://git.example:8443/group/sub/app.git", None);
        assert_eq!(commit_url(&gitlab, SHA), Some(format!("https://git.example:8443/group/sub/app/-/commit/{sha}")));
        let bitbucket = identity("bitbucket", "bitbucket.org/team/app", "git@bitbucket.org:team/app.git", None);
        assert_eq!(commit_url(&bitbucket, SHA), Some(format!("https://bitbucket.org/team/app/commits/{sha}")));
        // A recorded web URL wins, without its credentials or query.
        let forgejo = identity(
            "forgejo",
            "code.example/me/app",
            "ssh://code.example/me/app",
            Some("https://u:p@code.example/me/app/?x=1"),
        );
        assert_eq!(commit_url(&forgejo, SHA), Some(format!("https://code.example/me/app/commit/{sha}")));
        let azure = identity(
            "azure-devops",
            "contoso.visualstudio.com/DefaultCollection/Pay/_git/Web",
            "https://contoso.visualstudio.com/Pay/_git/Web",
            None,
        );
        assert_eq!(commit_url(&azure, SHA), Some(format!("https://dev.azure.com/contoso/pay/_git/web/commit/{sha}")));
    }

    #[test]
    fn commits_without_a_known_host_or_full_sha_have_no_link() {
        let github = identity("github", "github.com/pingdotgg/t3code", "git@github.com:pingdotgg/t3code.git", None);
        assert_eq!(commit_url(&github, "abcdef0"), None);
        assert_eq!(commit_url(&identity("github", "github.com/a/b/c", "", None), SHA), None);
        assert_eq!(commit_url(&identity("unknown", "git.example/a/b", "", None), SHA), None);
        let mut bare = github;
        bare.provider = None;
        assert_eq!(commit_url(&bare, SHA), None);
    }
}
