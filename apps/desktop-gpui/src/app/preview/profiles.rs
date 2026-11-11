//! Browser profiles: where this client's tabs keep their data, clearing it, and keeping the
//! server's browser on this client's profiles (`apps/web/src/browser/BrowserProfileReporter.ts`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::{App, Context, SharedString, Task, Window};
use gpui_webview::WebView;
use serde_json::{Value, json};

use crate::app::Workspace;
use crate::preview::{self, Partition};
use crate::rpc::RpcClient;

/// A profile's storage, kept apart like Electron's session partitions.
pub(super) enum Storage {
    Default,
    Incognito,
    /// `dir` is the user data folder (`WebView2`, `WebKitGTK`); `store` the data store (macOS).
    Folder {
        dir: PathBuf,
        #[cfg_attr(
            all(not(target_os = "macos"), not(test)),
            expect(dead_code, reason = "only WKWebView keeps a profile by store identifier")
        )]
        store: [u8; 16],
    },
}

/// Why a custom profile can't be opened here: it would share the default profile's data.
pub(in crate::app) const CUSTOM_PROFILES_UNAVAILABLE: &str =
    "Browser profiles need macOS 14 or later. On this Mac they would share the default profile's cookies.";

/// Whether this machine can keep a custom profile's data apart. `WKWebView` takes a store of its
/// own from macOS 14 on; before that wry silently gives it the default store.
#[cfg(target_os = "macos")]
pub(in crate::app) fn custom_profiles_available() -> bool {
    objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion().majorVersion >= 14
}

/// `WebView2` and `WebKitGTK` keep each profile in its own data folder.
#[cfg(not(target_os = "macos"))]
pub(in crate::app) fn custom_profiles_available() -> bool {
    true
}

impl Storage {
    pub(super) fn of(profile_id: Option<&str>) -> Result<Self, &'static str> {
        Self::resolve(preview::partition(profile_id), crate::settings::userdata_dir(), custom_profiles_available())
    }

    fn resolve(partition: Partition, userdata: Option<PathBuf>, custom_profiles: bool) -> Result<Self, &'static str> {
        match partition {
            Partition::Default => Ok(Self::Default),
            Partition::Incognito => Ok(Self::Incognito),
            Partition::Folder(_) if !custom_profiles => Err(CUSTOM_PROFILES_UNAVAILABLE),
            // Without a home folder a profile must not fall back to the default's cookies.
            Partition::Folder(store) => Ok(userdata.map_or(Self::Incognito, |userdata| Self::Folder {
                dir: userdata.join("browser-profiles").join(crate::hex::lower_hex(&store)),
                store,
            })),
        }
    }

    pub(super) fn context(&self) -> Option<wry::WebContext> {
        match self {
            Self::Folder { dir, .. } => Some(wry::WebContext::new(Some(dir.clone()))),
            Self::Default | Self::Incognito => None,
        }
    }

    pub(super) fn builder<'a>(&self, context: Option<&'a mut wry::WebContext>) -> wry::WebViewBuilder<'a> {
        let builder = match context {
            Some(context) => wry::WebViewBuilder::new_with_web_context(context),
            None => wry::WebViewBuilder::new(),
        };
        let builder = builder.with_incognito(matches!(self, Self::Incognito));
        // WKWebView ignores the data folder; only a store identifier keeps the profile apart.
        #[cfg(target_os = "macos")]
        if let Self::Folder { store, .. } = self {
            use wry::WebViewBuilderExtDarwin as _;
            return builder.with_data_store_identifier(*store);
        }
        builder
    }

    /// A hidden view in this storage, to reach its cookies whether or not a tab of it is open.
    pub(super) fn hidden_view(&self, window: &mut Window, cx: &mut App) -> Result<WebView, String> {
        let mut context = self.context();
        let builder = self
            .builder(context.as_mut())
            .with_visible(false)
            // A focused view would take the keyboard focus with it when it is dropped.
            .with_focused(false)
            .with_url("about:blank");
        super::build_view(builder, window, cx).map_err(|error| error.to_string())
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
        // A profile this Mac can't open has no store of its own here, and the default's isn't its to clear.
        let storage = Storage::of(Some(&profile_id)).ok();
        cx.spawn_in(window, async move |_, cx| {
            let view = match storage {
                Some(storage) => Some(
                    cx.update(|window, cx| storage.hidden_view(window, cx)).map_err(|error| error.to_string())??,
                ),
                None => None,
            };
            let desktop = async {
                let Some(view) = view else { return Ok(()) };
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

/// How long a hidden view stays alive after it asked WebView2 to clear or write its profile's data.
pub(super) const CLEAR_GRACE: Duration = Duration::from_secs(2);

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

    #[test]
    fn a_custom_profile_gets_its_own_store_or_is_refused() {
        let home = Some(PathBuf::from("home"));
        let work = || preview::partition(Some("profile-work"));
        let Ok(Storage::Folder { dir, store }) = Storage::resolve(work(), home.clone(), true) else { panic!() };
        assert_eq!(dir, PathBuf::from("home").join("browser-profiles").join(crate::hex::lower_hex(&store)));
        assert!(matches!(Storage::resolve(work(), None, true), Ok(Storage::Incognito)), "never the default's data");
        assert_eq!(Storage::resolve(work(), home.clone(), false).err(), Some(CUSTOM_PROFILES_UNAVAILABLE));
        assert!(matches!(Storage::resolve(preview::partition(None), home.clone(), false), Ok(Storage::Default)));
        let incognito = preview::partition(Some(preview::INCOGNITO_PROFILE_ID));
        assert!(matches!(Storage::resolve(incognito, home, false), Ok(Storage::Incognito)));
    }
}
