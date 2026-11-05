//! Browser profiles: where this client's tabs keep their data, clearing it, and keeping the
//! server's browser on this client's profiles (`apps/web/src/browser/BrowserProfileReporter.ts`).

use std::path::PathBuf;

use std::time::Duration;

use gpui_kit::{Context, Task, Window};
use serde_json::json;

use crate::app::Workspace;
use crate::preview::{self, Partition};

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

impl Workspace {
    /// `clearBrowserProfileData`: a profile's cookies and cache, in this client's browser and in
    /// the server's when it runs one.
    pub(in crate::app) fn clear_browser_profile(
        &self,
        profile_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let server = self.client().filter(|_| self.server_info.capability("serverBrowser"));
        let storage = Storage::of(Some(&profile_id));
        cx.spawn_in(window, async move |_, cx| {
            // Clears through a hidden view in the profile's storage, which works whether or not a tab of it is open.
            let view = cx.update(|window, _| {
                let mut context = storage.context();
                let builder = storage
                    .builder(context.as_mut())
                    .with_visible(false)
                    // A focused view would take the keyboard focus with it when it is dropped.
                    .with_focused(false)
                    .with_url("about:blank");
                build(builder, window)
            });
            let view = view.map_err(|error| error.to_string())?.map_err(|error| error.to_string())?;
            let desktop = async {
                view.clear_all_browsing_data().map_err(|error| error.to_string())?;
                // WebView2 clears in the background and reports nothing back; dropping the view
                // early could cut that short.
                cx.background_executor().timer(CLEAR_GRACE).await;
                Ok::<(), String>(())
            };
            let server = async {
                let Some(client) = server else { return Ok(()) };
                let input = json!({"profileId": profile_id});
                client.request("preview.clearProfile", input).await.map(drop).map_err(|error| error.to_string())
            };
            let (desktop, server) = futures::join!(desktop, server);
            desktop.and(server)
        })
    }

    /// Reports this client's profiles, default profile and recording frame rate to the server's
    /// browser once per connection and again on each change. Agents' tabs use them.
    pub(in crate::app) fn report_browser_profiles(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client().filter(|_| self.server_info.capability("serverBrowser")) else { return };
        let report = preview::profile_report(&self.client_settings.values);
        if self.preview.reported_profiles.as_ref() == Some(&report) {
            return;
        }
        self.preview.reported_profiles = Some(report.clone());
        cx.spawn(async move |_, _| {
            // A session without `preview:operate` is refused; the web ignores that too.
            if let Err(error) = client.request("preview.reportProfiles", report).await {
                log::warn!("preview.reportProfiles failed: {error}");
            }
        })
        .detach();
    }
}

/// How long a hidden view stays alive after it asked WebView2 to clear its profile.
const CLEAR_GRACE: Duration = Duration::from_secs(2);

/// Builds a native view in `window`. Call it from a task's update: WebView2's constructor runs
/// the window's message loop, and while a task runs GPUI posts no other task to that loop, so
/// nothing re-enters the borrowed app (window events in that time are dropped).
pub(super) fn build(builder: wry::WebViewBuilder, window: &Window) -> wry::Result<wry::WebView> {
    builder.build_as_child(window)
}
