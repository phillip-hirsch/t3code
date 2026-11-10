//! `SourceControlAppAccessNotice` under the panel's tab bar: says when the environment's forge
//! app sign-in cannot reach the pull request's repository, so requests there use the user's login.

use gpui_kit::{HighlightStyle, StyledText, img};

use super::*;
use crate::app::pages::settings::code::{AppSignIn, app_sign_in_of};

/// The `sourceControl.appAccess` question for one repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AppAccessQuery {
    provider: String,
    host: String,
    repository: String,
}

/// `SourceControlAppAccessNotice`'s gate: `provider` has an app sign-in covering `host` (else its
/// public host), and the environment's `settings` hold a session for that host.
fn app_access_query(
    provider: String,
    host: Option<&str>,
    repository: &str,
    settings: &Value,
) -> Option<(&'static AppSignIn, AppAccessQuery)> {
    let (public_host, app) = app_sign_in_of(&provider)?;
    let host = host.unwrap_or(public_host).to_lowercase();
    if app.host != host {
        return None;
    }
    let session = &settings["sourceControlHosts"][provider.as_str()]["signIns"][host.as_str()];
    let signed_in = session.as_str().is_some_and(|session| !session.is_empty());
    signed_in.then(|| (app, AppAccessQuery { provider, host, repository: repository.to_owned() }))
}

impl Workspace {
    /// The question the notice needs, asked only when the pull request's environment is signed in
    /// to a forge app that covers its host.
    fn pr_app_access_query(&self, panel: &Panel) -> Option<(&'static AppSignIn, AppAccessQuery)> {
        let reference = &panel.reference;
        let provider = match panel.detail.as_ref() {
            Some(detail) => detail.provider.clone(),
            None => self.pr_project(reference)?.repository_identity.as_ref()?.provider.clone()?,
        };
        app_access_query(
            provider,
            reference.host.as_deref(),
            &reference.repository,
            &self.pull_request_info(reference)?.settings,
        )
    }

    /// Asks whether the app reaches the repository of the pull request `key` names: when the
    /// question changed, or `again` (a refresh), which keeps the last answer until the new one.
    pub(super) fn read_pr_app_access_of(&mut self, key: &str, again: bool, cx: &mut Context<Self>) {
        let Some(panel) = self.pr_panel_ref(key) else { return };
        let Some((_, query)) = self.pr_app_access_query(panel) else { return };
        if !again && panel.app_access.as_ref().is_some_and(|(asked, _)| *asked == query) {
            return;
        }
        let Some(client) = self.pull_request_client_of(key) else { return };
        let input = json!({ "provider": query.provider, "host": query.host, "repository": query.repository });
        let (answer_key, asked) = (key.to_owned(), query.clone());
        let read = cx.spawn(async move |this, cx| {
            let result = client.request("sourceControl.appAccess", input).await;
            let _ = this.update(cx, |this, cx| {
                let Some(panel) = this.pr_panel(&answer_key) else { return };
                let status = result.ok().and_then(|answer| answer["status"].as_str().map(str::to_owned));
                panel.app_access = Some((asked, status));
                cx.notify();
            });
        });
        let Some(panel) = self.pr_panel(key) else { return };
        if panel.app_access.as_ref().is_none_or(|(asked, _)| *asked != query) {
            panel.app_access = Some((query, None));
        }
        panel._app_access_read = Some(read);
    }

    /// The muted note, while the shown pull request's repository is `not-covered`.
    pub(super) fn render_pr_app_access(&self, t: &Tokens) -> Option<AnyElement> {
        let panel = self.pull_requests.panel.as_ref()?;
        let (app, query) = self.pr_app_access_query(panel)?;
        match &panel.app_access {
            Some((asked, Some(status))) if *asked == query && status == "not-covered" => {}
            _ => return None,
        }
        let owner = query.repository.split('/').next().unwrap_or(&query.repository).to_owned();
        let lead = format!("{} isn't installed on ", app.app_name);
        let owner_range = lead.len()..lead.len() + owner.len();
        let text = format!("{lead}{owner}, so requests here use your own GitHub login and its lower rate limit.");
        let highlight = HighlightStyle { color: Some(t.foreground), ..Default::default() };
        let url = app.install_url;
        Some(
            h_flex()
                .flex_none()
                .gap(px(10.))
                .px(px(16.))
                .py(px(8.))
                .bg(t.muted.opacity(0.4))
                .border_b_1()
                .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
                .text_xs()
                .text_color(t.muted_foreground)
                .child(img("icons/app/t3.svg").flex_none().size(px(16.)))
                .child(
                    div().flex_1().min_w_0().child(StyledText::new(text).with_highlights([(owner_range, highlight)])),
                )
                .child(
                    gpui_kit::base::Button::new("pr-app-access-install")
                        .flex_none()
                        .gap(px(2.))
                        .whitespace_nowrap()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.foreground)
                        .cursor_pointer()
                        .hover(gpui_kit::Styled::underline)
                        .focus_visible(outline_ring(t))
                        .child(format!("Install on {owner}"))
                        .child(Icon::new(Lucide::ExternalLink).with_size(px(12.)))
                        .on_click(move |_, _, cx| cx.open_url(url)),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_access_question_is_asked_only_for_a_signed_in_covered_host() {
        let signed_in = json!({ "sourceControlHosts": { "github": { "signIns": { "github.com": "session" } } } });
        let signed_out = json!({ "sourceControlHosts": { "github": { "signIns": { "github.com": "" } } } });
        let asked = |provider: &str, host: Option<&str>, settings: &Value| {
            app_access_query(provider.to_owned(), host, "acme/app", settings).map(|(_, query)| query)
        };
        let expected =
            AppAccessQuery { provider: "github".into(), host: "github.com".into(), repository: "acme/app".into() };
        assert_eq!(asked("github", None, &signed_in), Some(expected.clone()));
        assert_eq!(asked("github", Some("GitHub.com"), &signed_in), Some(expected));
        assert_eq!(asked("github", Some("github.example.com"), &signed_in), None);
        assert_eq!(asked("github", None, &signed_out), None);
        assert_eq!(asked("github", None, &json!({})), None);
        assert_eq!(asked("gitlab", None, &signed_in), None);
    }
}
