//! Importing another browser's cookies into a profile (`importBrowserCookiesIntoTarget` in
//! `IntegrationsSettings.tsx`, and Electron's `BrowserImport.writeCookies`): the source is read off
//! the UI thread, then its cookies are written into the profile's WebView store.

use std::time::Duration;

use futures::channel::oneshot;
use gpui_kit::{AsyncWindowContext, Context, Task, Window};
use serde_json::{Value, json};
use wry::cookie::{Cookie, SameSite as CookieSameSite, time::OffsetDateTime};

use super::profiles::{CLEAR_GRACE, Storage};
use crate::app::Workspace;
use crate::browser_import::{self, CookieRead, ImportedCookie, Reason, SameSite, Source, SourceId};
use crate::preview::{self, Profile, ProfileKind};

/// Cookies written between yields, so a large jar doesn't stall the window.
const WRITE_BATCH: usize = 50;
/// `writeCookies` reports at most this many skipped sites.
const SKIPPED_DOMAINS: usize = 20;

/// What Settings knows of the browsers to import from.
#[derive(Default)]
pub(in crate::app) struct CookieImport {
    /// The last listing; `None` until the first arrives.
    pub(in crate::app) sources: Option<Vec<Source>>,
    listing: Option<Task<()>>,
    /// An import is under way; profile edits wait for it.
    pub(in crate::app) in_flight: bool,
}

/// `WizardTarget`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) enum ImportTarget {
    /// A profile created for these cookies once they are written.
    New {
        profile_id: String,
    },
    Existing {
        profile_id: String,
        name: String,
    },
}

impl ImportTarget {
    fn profile_id(&self) -> &str {
        match self {
            Self::New { profile_id } | Self::Existing { profile_id, .. } => profile_id,
        }
    }
}

/// `ImportOutcome`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) enum ImportOutcome {
    Imported {
        imported: usize,
        skipped: usize,
        skipped_domains: Vec<String>,
        target_name: String,
    },
    /// `message`, such as a managed policy's refusal, replaces the reason's copy.
    Blocked {
        reason: Reason,
        message: Option<String>,
    },
}

impl ImportOutcome {
    fn blocked(reason: Reason) -> Self {
        Self::Blocked { reason, message: None }
    }
}

/// `managedCapabilityDenial(policy, "browserCookieImport")`.
pub(in crate::app) fn cookie_import_denial(policy: &Value) -> Option<String> {
    let organization = policy["organization"].as_str()?;
    (policy.pointer("/limits/browserCookieImport") == Some(&Value::Bool(false)))
        .then(|| format!("{organization}'s policy does not allow importing browser cookies."))
}

/// The new profile's entry: named after the source, made unique. The list may have filled up
/// while the cookies were written, so the limit is checked against the settings as they are now.
fn registration(
    settings: &crate::settings::Settings,
    profile_id: &str,
    source_name: &str,
) -> Result<Option<(Value, String)>, Reason> {
    if preview::user_profiles(settings).iter().any(|profile| profile.id == profile_id) {
        return Ok(None);
    }
    let taken = preview::resolve_profiles(&preview::user_profiles(settings));
    let profile = Profile {
        id: profile_id.to_owned(),
        name: preview::unique_profile_name(source_name, &taken),
        kind: ProfileKind::Persistent,
    };
    let profiles = preview::profiles_with_added(settings, &profile).ok_or(Reason::ProfileLimitReached)?;
    Ok(Some((profiles, profile.name)))
}

/// The cookie as wry hands it to the platform's store. `Cookie::domain` drops one leading dot,
/// so a domain cookie carries two to keep its own; a host-only one names its host.
fn wry_cookie(cookie: &ImportedCookie) -> Cookie<'static> {
    let domain = cookie.domain.as_ref().map_or_else(|| cookie.host.clone(), |domain| format!(".{domain}"));
    let mut builder = Cookie::build((cookie.name.clone(), cookie.value.clone()))
        .domain(domain)
        .path(cookie.path.clone())
        .secure(cookie.secure)
        .http_only(cookie.http_only);
    if let Some(expires) = cookie.expires.and_then(|seconds| OffsetDateTime::from_unix_timestamp(seconds).ok()) {
        builder = builder.expires(expires);
    }
    let same_site = match cookie.same_site {
        SameSite::Unspecified => None,
        SameSite::NoRestriction => Some(CookieSameSite::None),
        SameSite::Lax => Some(CookieSameSite::Lax),
        SameSite::Strict => Some(CookieSameSite::Strict),
    };
    if let Some(same_site) = same_site {
        builder = builder.same_site(same_site);
    }
    builder.build()
}

/// What `writeCookies` reports: undecryptable cookies count as skipped, with their sites.
struct Written {
    imported: usize,
    skipped: usize,
    skipped_domains: Vec<String>,
}

impl Written {
    fn new(read: &CookieRead) -> Self {
        Self { imported: 0, skipped: read.undecryptable, skipped_domains: read.undecryptable_hosts.clone() }
    }

    fn skip(&mut self, host: &str) {
        self.skipped += 1;
        if !self.skipped_domains.iter().any(|domain| domain == host) {
            self.skipped_domains.push(host.to_owned());
        }
    }
}

/// Writes `read` into `storage`; a cookie the store rejects is skipped, not the import.
async fn write_cookies(storage: Storage, read: CookieRead, cx: &mut AsyncWindowContext) -> Result<Written, Reason> {
    let view = cx
        .update(|window, cx| storage.hidden_view(window, cx))
        .map_err(|_| Reason::SessionUnavailable)?
        .map_err(|error| {
            log::warn!("cookie import: the profile's view could not be opened: {error}");
            Reason::SessionUnavailable
        })?;
    let mut written = Written::new(&read);
    for batch in read.cookies.chunks(WRITE_BATCH) {
        for cookie in batch {
            match view.raw().set_cookie(&wry_cookie(cookie)) {
                Ok(()) => written.imported += 1,
                Err(error) => {
                    log::debug!("cookie import: {} for {} was rejected: {error}", cookie.name, cookie.host);
                    written.skip(&cookie.host);
                }
            }
        }
        cx.background_executor().timer(Duration::ZERO).await;
    }
    // The store settles in the background, so the view outlives the report a little.
    cx.spawn(async move |cx| {
        cx.background_executor().timer(CLEAR_GRACE).await;
        drop(view);
    })
    .detach();
    written.skipped_domains.truncate(SKIPPED_DOMAINS);
    Ok(written)
}

impl Workspace {
    /// `listBrowserImportSources`, off the UI thread; a listing already running is reused.
    pub(in crate::app) fn refresh_browser_import_sources(&mut self, cx: &mut Context<Self>) {
        if self.preview.cookie_import.listing.is_some() {
            return;
        }
        let listing = cx.background_executor().spawn(async { browser_import::list_sources() });
        self.preview.cookie_import.listing = Some(cx.spawn(async move |this, cx| {
            let sources = listing.await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.preview.cookie_import;
                state.sources = Some(sources);
                state.listing = None;
                cx.notify();
            });
        }));
    }

    /// `refreshImportSource`: a fresh listing's entry for `id`, which the menu also keeps.
    pub(in crate::app) fn browser_import_source(
        &mut self,
        id: SourceId,
        cx: &mut Context<Self>,
    ) -> Task<Option<Source>> {
        let listing = cx.background_executor().spawn(async { browser_import::list_sources() });
        cx.spawn(async move |this, cx| {
            let sources = listing.await;
            let source = sources.iter().find(|source| source.id == id).cloned();
            let _ = this.update(cx, |this, cx| {
                this.preview.cookie_import.sources = Some(sources);
                cx.notify();
            });
            source
        })
    }

    /// `runWizardImport` and `importBrowserCookiesIntoTarget`: reads the source's profile,
    /// writes its cookies into the target, then registers a new target. A new profile that cannot
    /// be registered is cleared, so imported cookies never outlive it.
    pub(in crate::app) fn import_browser_cookies(
        &mut self,
        source: SourceId,
        directory: String,
        target: ImportTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<ImportOutcome> {
        // Checked again here: the menu's denial may predate a policy the primary now reports.
        if let Some(message) = cookie_import_denial(&self.primary_info().managed_policy) {
            return Task::ready(ImportOutcome::Blocked { reason: Reason::ReadFailed, message: Some(message) });
        }
        if self.preview.cookie_import.in_flight || !self.import_target_listed(&target) {
            return Task::ready(ImportOutcome::blocked(Reason::ReadFailed));
        }
        self.preview.cookie_import.in_flight = true;
        cx.notify();
        let storage = Storage::of(Some(target.profile_id()));
        let read = cx.background_executor().spawn(async move { browser_import::read_cookies(source, &directory) });
        cx.spawn_in(window, async move |this, cx| {
            let outcome = match read.await {
                Err(reason) => ImportOutcome::blocked(reason),
                Ok(read) => match write_cookies(storage, read, cx).await {
                    Err(reason) => ImportOutcome::blocked(reason),
                    Ok(written) => Self::finish_import(&this, source, target, written, cx).await,
                },
            };
            let _ = this.update(cx, |this, cx| {
                this.preview.cookie_import.in_flight = false;
                cx.notify();
            });
            outcome
        })
    }

    /// An existing target must still be listed; a new one is made only once it holds cookies.
    fn import_target_listed(&self, target: &ImportTarget) -> bool {
        match target {
            ImportTarget::New { .. } => true,
            ImportTarget::Existing { profile_id, .. } => {
                preview::resolve_profiles(&preview::user_profiles(&self.client_settings.values))
                    .iter()
                    .any(|profile| &profile.id == profile_id && profile.kind != ProfileKind::Incognito)
            }
        }
    }

    async fn finish_import(
        this: &gpui_kit::WeakEntity<Self>,
        source: SourceId,
        target: ImportTarget,
        written: Written,
        cx: &mut AsyncWindowContext,
    ) -> ImportOutcome {
        let imported = |target_name: String| ImportOutcome::Imported {
            imported: written.imported,
            skipped: written.skipped,
            skipped_domains: written.skipped_domains.clone(),
            target_name,
        };
        let profile_id = match target {
            ImportTarget::Existing { profile_id, name } => {
                // Removed while the cookies were written.
                let listed = this.read_with(cx, |this, _| {
                    this.import_target_listed(&ImportTarget::Existing { profile_id, name: name.clone() })
                });
                return if listed.unwrap_or(false) {
                    imported(name)
                } else {
                    ImportOutcome::blocked(Reason::ReadFailed)
                };
            }
            ImportTarget::New { .. } if written.imported == 0 => return imported(source.name().to_owned()),
            ImportTarget::New { profile_id } => profile_id,
        };
        let registered = this.update_in(cx, |this, window, cx| {
            match registration(&this.client_settings.values, &profile_id, source.name())? {
                None => {
                    let profiles = preview::user_profiles(&this.client_settings.values);
                    let name =
                        profiles.into_iter().find(|profile| profile.id == profile_id).map(|profile| profile.name);
                    Ok((name.unwrap_or_else(|| source.name().to_owned()), None))
                }
                Some((profiles, name)) => {
                    let (saved, rx) = oneshot::channel();
                    this.update_settings_then(json!({"browserProfiles": profiles}), window, cx, move |_, ok, _| {
                        let _ = saved.send(ok);
                    });
                    Ok((name, Some(rx)))
                }
            }
        });
        let failure = match registered {
            Ok(Ok((name, None))) => return imported(name),
            Ok(Ok((name, Some(saved)))) => match saved.await {
                Ok(true) => return imported(name),
                _ => Reason::ProfileNotSaved,
            },
            Ok(Err(reason)) => reason,
            Err(_) => Reason::ProfileNotSaved,
        };
        let clear = this.update_in(cx, |this, window, cx| this.clear_browser_profile(profile_id, window, cx));
        if let Ok(clear) = clear
            && let Err(error) = clear.await
        {
            log::warn!("cookie import: the unsaved profile's cookies could not be cleared: {error}");
        }
        ImportOutcome::blocked(failure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(domain: Option<&str>, same_site: SameSite) -> ImportedCookie {
        ImportedCookie {
            host: "example.com".into(),
            domain: domain.map(str::to_owned),
            name: "sid".into(),
            value: "abc".into(),
            path: "/app".into(),
            secure: true,
            http_only: true,
            expires: Some(1_700_000_000),
            same_site,
        }
    }

    #[test]
    fn cookies_keep_their_scope_in_the_webview_store() {
        let domain = wry_cookie(&cookie(Some(".example.com"), SameSite::NoRestriction));
        assert_eq!(domain.domain(), Some(".example.com"), "subdomains still get it");
        assert_eq!(domain.same_site(), Some(CookieSameSite::None));
        assert_eq!((domain.path(), domain.secure(), domain.http_only()), (Some("/app"), Some(true), Some(true)));
        assert_eq!(domain.expires_datetime().map(OffsetDateTime::unix_timestamp), Some(1_700_000_000));

        let host_only = wry_cookie(&ImportedCookie { expires: None, ..cookie(None, SameSite::Unspecified) });
        assert_eq!(host_only.domain(), Some("example.com"));
        assert_eq!(host_only.same_site(), None, "the store decides, as the source did");
        assert_eq!(host_only.expires_datetime(), None, "a session cookie");
    }

    #[test]
    fn a_new_profile_is_named_after_its_source_within_the_limit() {
        let settings = |profiles: Value| {
            crate::settings::Settings::layered(
                &crate::settings::contract().client_defaults,
                &json!({"browserProfiles": profiles}),
            )
        };
        let with_chrome = settings(json!([{"id": "p1", "name": "Chrome", "kind": "persistent"}]));
        let (profiles, name) = registration(&with_chrome, "p2", "Chrome").unwrap().unwrap();
        assert_eq!(name, "Chrome 2");
        assert_eq!(profiles[1], json!({"id": "p2", "name": "Chrome 2", "kind": "persistent"}));
        assert_eq!(registration(&with_chrome, "p1", "Chrome"), Ok(None), "already registered");

        let full: Vec<Value> =
            (0..preview::PROFILE_MAX_COUNT).map(|i| json!({"id": format!("p{i}"), "name": format!("P{i}")})).collect();
        assert_eq!(registration(&settings(Value::Array(full)), "new", "Chrome"), Err(Reason::ProfileLimitReached));
    }

    #[test]
    fn only_an_organization_policy_that_turns_import_off_denies_it() {
        let policy = json!({"organization": "Acme", "limits": {"browserCookieImport": false}});
        assert_eq!(
            cookie_import_denial(&policy).as_deref(),
            Some("Acme's policy does not allow importing browser cookies.")
        );
        assert_eq!(
            cookie_import_denial(&json!({"organization": "Acme", "limits": {"browserCookieImport": true}})),
            None
        );
        assert_eq!(cookie_import_denial(&Value::Null), None);
    }

    #[test]
    fn skipped_sites_are_listed_once() {
        let read = CookieRead { undecryptable: 1, undecryptable_hosts: vec!["a.com".into()], ..CookieRead::default() };
        let mut written = Written::new(&read);
        written.skip("a.com");
        written.skip("b.com");
        assert_eq!((written.skipped, written.skipped_domains), (3, vec!["a.com".to_owned(), "b.com".to_owned()]));
    }
}
