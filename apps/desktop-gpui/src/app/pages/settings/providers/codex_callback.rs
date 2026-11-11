//! Catches ChatGPT's sign-in redirect on this computer, as the Electron shell
//! does (`receiveCodexAuthCallback` in `packages/shared/src/codexAuthCallback.ts`).
//! The authorization code goes back to the environment; PKCE and tokens never
//! leave it.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use reqwest::Url;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

const INVALID_REQUEST: &str = "Invalid ChatGPT sign-in request.";
const EXPIRES_AFTER: Duration = Duration::from_secs(300);
/// Browsers open idle preconnect sockets; one may not hold up the redirect behind it for longer.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const RETURN_PAGE: &str = "<!doctype html><html><head><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta name=\"color-scheme\" content=\"light dark\"><title>T3 Code</title><style>body{font-family:system-ui;display:grid;place-items:center;min-height:90vh;margin:0}main{max-width:360px;padding:32px}h1{font-size:24px}p{line-height:1.6;opacity:.7}</style></head><body><main><h1>Return to T3 Code</h1><p>Your sign-in response has been received. T3 Code is finishing the connection. You can close this tab.</p></main></body></html>";

/// The parts of an OpenAI authorization URL the callback must match.
#[derive(Debug, PartialEq)]
pub(super) struct AuthorizationRequest {
    redirect_uri: Url,
    state: String,
}

fn single<'a>(pairs: &'a [(String, String)], key: &str) -> Result<&'a str> {
    let mut values = pairs.iter().filter(|(name, _)| name == key);
    match (values.next(), values.next()) {
        (Some((_, value)), None) if !value.is_empty() => Ok(value),
        _ => bail!(INVALID_REQUEST),
    }
}

fn is_token(value: &str, lengths: std::ops::RangeInclusive<usize>) -> bool {
    lengths.contains(&value.len()) && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `codexAuthorizationRequest` in `packages/shared/src/codexAuthHandoff.ts`.
pub(super) fn authorization_request(value: &str) -> Result<AuthorizationRequest> {
    let url = Url::parse(value).map_err(|_| anyhow!(INVALID_REQUEST))?;
    if value.len() > 16_384
        || url.scheme() != "https"
        || url.host_str() != Some("auth.openai.com")
        || url.port().is_some()
        || url.path() != "/api/accounts/authorize"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        bail!(INVALID_REQUEST);
    }
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let redirect = single(&pairs, "redirect_uri")?;
    let state = single(&pairs, "state")?;
    let client_id = single(&pairs, "client_id")?;
    let port_ok = redirect
        .strip_prefix("http://127.0.0.1:")
        .and_then(|rest| rest.strip_suffix("/auth/callback"))
        .is_some_and(|port| {
            !port.starts_with('0') && port.len() <= 5 && port.parse::<u32>().is_ok_and(|p| (1..=65_535).contains(&p))
        });
    if !port_ok
        || !is_token(state, 16..=128)
        || single(&pairs, "response_type")? != "code"
        || single(&pairs, "code_challenge_method")? != "S256"
        || !is_token(single(&pairs, "code_challenge")?, 43..=43)
        || !(client_id == "dynamic_agent_client"
            || client_id.strip_prefix("oaiapp_").is_some_and(|rest| is_token(rest, 1..=usize::MAX)))
    {
        bail!(INVALID_REQUEST);
    }
    Ok(AuthorizationRequest {
        redirect_uri: Url::parse(redirect).map_err(|_| anyhow!(INVALID_REQUEST))?,
        state: state.to_owned(),
    })
}

/// `codexCallbackUrl`: the redirect, checked against the sign-in it belongs to.
pub(super) fn callback_url(value: &str, request: &AuthorizationRequest) -> Result<Url> {
    const FOREIGN: &str = "This redirect URL does not belong to the current sign-in.";
    let pasted = value.trim_start();
    let has_scheme = pasted.split_once("://").is_some_and(|(scheme, _)| {
        scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
    });
    // Browsers can hide `http://` in the address bar, so a pasted callback may lack it.
    let callback = if has_scheme { Url::parse(pasted) } else { Url::parse(&format!("http://{pasted}")) }
        .map_err(|_| anyhow!(FOREIGN))?;
    let expected = &request.redirect_uri;
    let pairs: Vec<(String, String)> = callback.query_pairs().into_owned().collect();
    let all = |key: &str| pairs.iter().filter(|(name, _)| name == key).map(|(_, v)| v.as_str()).collect::<Vec<_>>();
    let (states, codes, errors, clients) = (all("state"), all("code"), all("error"), all("client_id"));
    let client_ok = match clients.as_slice() {
        [] => true,
        [client] => client.strip_prefix("oaiapp_").is_some_and(|rest| is_token(rest, 1..=usize::MAX)),
        _ => false,
    };
    let outcome_ok = matches!((codes.as_slice(), errors.as_slice()), ([code], []) if !code.is_empty())
        || matches!((codes.as_slice(), errors.as_slice()), ([], [error]) if !error.is_empty());
    if value.len() > 16_384
        || callback.origin() != expected.origin()
        || callback.path() != expected.path()
        || !callback.username().is_empty()
        || callback.password().is_some()
        || callback.fragment().is_some()
        || states.as_slice() != [request.state.as_str()]
        || !client_ok
        || !outcome_ok
    {
        bail!(FOREIGN);
    }
    Ok(callback)
}

/// Binds the redirect's loopback port. Run on the tokio runtime, then open the
/// browser and await [`receive`].
pub(super) async fn listen(authorization_url: String) -> Result<(TcpListener, AuthorizationRequest)> {
    let request = authorization_request(&authorization_url)?;
    let port = request.redirect_uri.port().ok_or_else(|| anyhow!(INVALID_REQUEST))?;
    let listener = TcpListener::bind(("127.0.0.1", port)).await.map_err(|error| {
        log::warn!("chatgpt callback: binding port {port}: {error}");
        anyhow!(
            "The ChatGPT callback port is in use on this computer. Close the other sign-in and try again, or paste the redirect URL in T3 Code."
        )
    })?;
    Ok((listener, request))
}

/// Answers requests on the port until one carries this sign-in's redirect.
pub(super) async fn receive(listener: TcpListener, request: AuthorizationRequest) -> Result<String> {
    tokio::time::timeout(EXPIRES_AFTER, async {
        loop {
            let (mut stream, _) = listener.accept().await?;
            let mut buffer = vec![0; 16_384 + 1024];
            let read = tokio::time::timeout(READ_TIMEOUT, stream.read(&mut buffer)).await.ok().and_then(Result::ok).unwrap_or(0);
            let head = String::from_utf8_lossy(&buffer[..read]);
            let target = head.lines().next().and_then(|line| line.strip_prefix("GET ")).and_then(|r| r.split(' ').next());
            let callback =
                target.and_then(|target| request.redirect_uri.join(target).ok()).and_then(|url| callback_url(url.as_str(), &request).ok());
            let response = match &callback {
                Some(_) => format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncache-control: no-store\r\nreferrer-policy: no-referrer\r\nx-content-type-options: nosniff\r\ncontent-security-policy: default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{RETURN_PAGE}",
                    RETURN_PAGE.len()
                ),
                None => {
                    let body = "This response does not belong to the active sign-in.";
                    format!("HTTP/1.1 400 Bad Request\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
                }
            };
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
            if let Some(callback) = callback {
                return Ok(callback.to_string());
            }
        }
    })
    .await
    .map_err(|_| anyhow!("Sign-in expired. Try again."))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authorization_url(redirect: &str) -> String {
        let mut url = Url::parse("https://auth.openai.com/api/accounts/authorize").unwrap();
        url.query_pairs_mut()
            .append_pair("client_id", "dynamic_agent_client")
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", redirect)
            .append_pair("state", &"a".repeat(43))
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &"b".repeat(43));
        url.to_string()
    }

    const REDIRECT: &str = "http://127.0.0.1:54213/auth/callback";

    #[test]
    fn accepts_openai_loopback_requests_only() {
        assert!(authorization_request(&authorization_url(REDIRECT)).is_ok());
        let duplicated = format!("{}&redirect_uri=http://localhost:1/auth/callback", authorization_url(REDIRECT));
        assert!(authorization_request(&duplicated).is_err());
        for redirect in ["http://localhost:54213/auth/callback", "https://attacker.example/auth/callback"] {
            assert!(authorization_request(&authorization_url(redirect)).is_err());
        }
        let foreign = authorization_url(REDIRECT).replace("auth.openai.com", "attacker.example");
        assert!(authorization_request(&foreign).is_err());
    }

    #[test]
    fn callbacks_must_match_the_sign_in() {
        let request = authorization_request(&authorization_url(REDIRECT)).unwrap();
        let state = "a".repeat(43);
        let good = format!("127.0.0.1:54213/auth/callback?state={state}&code=one-time-code&client_id=oaiapp_test");
        assert_eq!(callback_url(&good, &request).unwrap().as_str(), format!("http://{good}"));
        assert!(callback_url(&format!("http://{good}&state=another"), &request).is_err());
        let both = format!("http://127.0.0.1:54213/auth/callback?state={state}&code=c&error=e");
        assert!(callback_url(&both, &request).is_err());
        let other_port = format!("http://127.0.0.1:1/auth/callback?state={state}&code=c");
        assert!(callback_url(&other_port, &request).is_err());
        let denied = format!("http://127.0.0.1:54213/auth/callback?state={state}&error=access_denied");
        assert!(callback_url(&denied, &request).is_ok());
    }
}
