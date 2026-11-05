//! Browser profiles: where this client's tabs keep their data, clearing it, and keeping the
//! server's browser on this client's profiles (`apps/web/src/browser/BrowserProfileReporter.ts`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::{Context, SharedString, Task, Window};
use serde_json::{Value, json};

use crate::app::Workspace;
use crate::preview::{self, Partition};
use crate::rpc::RpcClient;

/// A profile's WebView2 storage: its own user data folder, like Electron's session partitions.
pub(super) enum Storage {
    Default,
    Incognito,
    Folder(PathBuf),
}

impl Storage {
    pub(super) fn of(profile_id: Option<&str>) -> Self {
        match preview::partition(profile_id) {
            Partition::Default => Self::Default,
            Partition::Incognito => Self::Incognito,
            // Without a home folder a profile must not fall back to the default's cookies.
            Partition::Folder(name) => crate::settings::userdata_dir()
                .map_or(Self::Incognito, |dir| Self::Folder(dir.join("browser-profiles").join(name))),
        }
    }

    pub(super) fn context(&self) -> Option<wry::WebContext> {
        match self {
            Self::Folder(dir) => Some(wry::WebContext::new(Some(dir.clone()))),
            Self::Default | Self::Incognito => None,
        }
    }

    pub(super) fn builder<'a>(&self, context: Option<&'a mut wry::WebContext>) -> wry::WebViewBuilder<'a> {
        let builder = match context {
            Some(context) => wry::WebViewBuilder::new_with_web_context(context),
            None => wry::WebViewBuilder::new(),
        };
        builder.with_incognito(matches!(self, Self::Incognito))
    }
}

/// What each environment's connection was last sent with `preview.reportProfiles`.
#[derive(Default)]
pub(super) struct ReportedProfiles(HashMap<SharedString, Value>);

impl ReportedProfiles {
    /// Records `report` as sent to `environment`; false when its connection already has it.
    fn record(&mut self, environment: &SharedString, report: &Value) -> bool {
        if self.0.get(environment) == Some(report) {
            return false;
        }
        self.0.insert(environment.clone(), report.clone());
        true
    }

    /// `environment` (re)connected: its new connection is sent the profiles again.
    pub(super) fn forget(&mut self, environment: &str) {
        self.0.remove(environment);
    }
}

impl Workspace {
    /// `clearBrowserProfileData`: a profile's cookies and cache, in this client's browser and in
    /// every connected environment's browser.
    pub(in crate::app) fn clear_browser_profile(
        &self,
        profile_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let servers = self.browser_servers();
        let storage = Storage::of(Some(&profile_id));
        cx.spawn_in(window, async move |_, cx| {
            // Clears through a hidden view in the profile's storage, which works whether or not a tab of it is open.
            let view = cx.update(|window, cx| {
                let mut context = storage.context();
                let builder = storage
                    .builder(context.as_mut())
                    .with_visible(false)
                    // A focused view would take the keyboard focus with it when it is dropped.
                    .with_focused(false)
                    .with_url("about:blank");
                super::build_view(builder, window, cx)
            });
            let view = view.map_err(|error| error.to_string())?.map_err(|error| error.to_string())?;
            let desktop = async {
                view.raw().clear_all_browsing_data().map_err(|error| error.to_string())?;
                // WebView2 clears in the background and reports nothing back; dropping the view
                // early could cut that short.
                cx.background_executor().timer(CLEAR_GRACE).await;
                Ok::<(), String>(())
            };
            let server = async {
                let input = json!({"profileId": profile_id});
                let cleared = servers.iter().map(|(_, client)| client.request("preview.clearProfile", input.clone()));
                futures::future::try_join_all(cleared).await.map(drop).map_err(|error| error.to_string())
            };
            let (desktop, server) = futures::join!(desktop, server);
            desktop.and(server)
        })
    }

    /// Every connected environment that runs a browser of its own (`serverBrowser`).
    fn browser_servers(&self) -> Vec<(SharedString, RpcClient)> {
        self.sessions()
            .iter()
            .filter(|session| session.server_info.capability("serverBrowser"))
            .filter_map(|session| Some((session.id.clone()?, session.client.clone()?)))
            .collect()
    }

    /// Reports this client's profiles, default profile and recording frame rate to every connected
    /// environment's browser, once per connection and again on each change. Agents' tabs use them.
    pub(in crate::app) fn report_browser_profiles(&mut self, cx: &mut Context<Self>) {
        let report = preview::profile_report(&self.client_settings.values);
        for (environment, client) in self.browser_servers() {
            if !self.preview.reported_profiles.record(&environment, &report) {
                continue;
            }
            let report = report.clone();
            cx.spawn(async move |_, _| {
                // A session without `preview:operate` is refused; the web ignores that too.
                if let Err(error) = client.request("preview.reportProfiles", report).await {
                    log::warn!("preview.reportProfiles failed: {error}");
                }
            })
            .detach();
        }
    }
}

/// How long a hidden view stays alive after it asked WebView2 to clear its profile.
const CLEAR_GRACE: Duration = Duration::from_secs(2);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_are_reported_once_per_environment_connection() {
        let mut reported = ReportedProfiles::default();
        let (a, b): (SharedString, SharedString) = ("a".into(), "b".into());
        let report = json!({"profiles": []});
        assert!(reported.record(&a, &report));
        assert!(!reported.record(&a, &report), "the same connection already has it");
        assert!(reported.record(&b, &report), "another environment is told separately");
        assert!(reported.record(&a, &json!({"profiles": [1]})), "a change is sent again");
        reported.forget("a");
        assert!(reported.record(&a, &json!({"profiles": [1]})), "a reconnect is sent again");
    }
}
