//! Browser profiles: where this client's tabs keep their data, clearing it, and keeping the
//! server's browser on this client's profiles (`apps/web/src/browser/BrowserProfileReporter.ts`).

use std::path::PathBuf;

use gpui_kit::{Context, Task, Window};
use raw_window_handle::HasWindowHandle as _;
use serde_json::json;

use super::ParentWindow;
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
        let server = self.client().filter(|_| self.server_info().capability("serverBrowser"));
        let parent = window.window_handle().map(|handle| ParentWindow(handle.as_raw()));
        let storage = Storage::of(Some(&profile_id));
        cx.spawn_in(window, async move |_, _| {
            let parent = parent.map_err(|error| error.to_string())?;
            let server = async {
                let Some(client) = server else { return Ok(()) };
                let input = json!({"profileId": profile_id});
                client.request("preview.clearProfile", input).await.map(drop).map_err(|error| error.to_string())
            };
            let (desktop, server) = futures::join!(clear_desktop(storage, &parent), server);
            desktop.and(server)
        })
    }

    /// Reports this client's profiles, default profile and recording frame rate to the server's
    /// browser once per connection and again on each change. Agents' tabs use them.
    pub(in crate::app) fn report_browser_profiles(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client().filter(|_| self.server_info().capability("serverBrowser")) else { return };
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

/// Clears through a hidden view in the profile's storage, which works whether or not a tab of it is open.
async fn clear_desktop(storage: Storage, parent: &ParentWindow) -> Result<(), String> {
    let mut context = storage.context();
    let view = storage
        .builder(context.as_mut())
        .with_visible(false)
        // A focused view would take the keyboard focus with it when it is dropped.
        .with_focused(false)
        .with_url("about:blank")
        .build_as_child_async(parent)
        .await
        .map_err(|error| error.to_string())?;
    #[cfg(windows)]
    return super::native::clear_cookies_and_cache(&view).await;
    #[cfg(not(windows))]
    return view.clear_all_browsing_data().map_err(|error| error.to_string());
}
