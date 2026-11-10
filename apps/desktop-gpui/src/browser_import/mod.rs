//! Importing another browser's cookies into a preview profile, as Electron's
//! `apps/desktop/src/preview/BrowserImport/` does: which browsers are installed with which
//! profiles, and reading a profile's cookies. Chromium's are decrypted with the browser's key
//! from the Keychain, the Secret Service or DPAPI; Firefox's and Safari's are stored in the clear.
//!
//! Everything here blocks on the file system, the OS keyring or a lock probe; callers run it off
//! the UI thread. Writing the cookies into a profile's WebView store is the caller's.

mod chromium;
mod firefox;
mod safari;
mod sources;

use sources::{Engine, Paths};

/// `BrowserImportSourceId`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SourceId {
    Chrome,
    Edge,
    Brave,
    Vivaldi,
    Opera,
    Arc,
    Helium,
    Firefox,
    Zen,
    Safari,
}

impl SourceId {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Chrome => "Chrome",
            Self::Edge => "Microsoft Edge",
            Self::Brave => "Brave",
            Self::Vivaldi => "Vivaldi",
            Self::Opera => "Opera",
            Self::Arc => "Arc",
            Self::Helium => "Helium",
            Self::Firefox => "Firefox",
            Self::Zen => "Zen",
            Self::Safari => "Safari",
        }
    }
}

/// `BrowserImportUnavailableReason` and `BrowserImportFailureReason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    NotInstalled,
    #[cfg_attr(
        all(windows, not(test)),
        expect(dead_code, reason = "Windows reads its key through DPAPI, which never prompts")
    )]
    NeedsKeychainApproval,
    #[cfg_attr(
        all(windows, not(test)),
        expect(dead_code, reason = "Windows reads its key through DPAPI, which never prompts")
    )]
    KeychainItemMissing,
    NeedsFullDiskAccess,
    BrowserRunning,
    UnsupportedPlatform,
    #[cfg_attr(
        all(windows, not(test)),
        expect(dead_code, reason = "Windows reads its key through DPAPI, which never prompts")
    )]
    KeychainUnavailable,
    UnknownSource,
    UnknownSourceProfile,
    SessionUnavailable,
    ProfileNotSaved,
    ProfileLimitReached,
    ReadFailed,
}

impl Reason {
    /// `BROWSER_IMPORT_FAILURE_COPY`.
    pub(crate) fn copy(self) -> &'static str {
        match self {
            Self::NotInstalled => "Not installed on this machine.",
            Self::NeedsKeychainApproval => "Needs Keychain access to read its cookies.",
            Self::KeychainItemMissing => {
                "No encryption key in your Keychain. Sign in to that browser once, then retry."
            }
            Self::NeedsFullDiskAccess => {
                "Give T3 Code Full Disk Access in System Settings \u{2192} Privacy & Security, then retry."
            }
            Self::BrowserRunning => "Quit the browser first so its cookie database can be read.",
            Self::UnsupportedPlatform => "Importing from this browser isn't possible on this platform.",
            Self::KeychainUnavailable => {
                "The system keyring could not be accessed. Make sure your desktop keyring is running and unlocked, then retry."
            }
            Self::UnknownSource => "That browser is no longer available to import from.",
            Self::UnknownSourceProfile => "That browser profile no longer exists.",
            Self::SessionUnavailable => "The target profile could not be opened.",
            Self::ProfileNotSaved => "The cookies were imported, but the new profile couldn't be saved. Try again.",
            Self::ProfileLimitReached => {
                "You've reached the profile limit. Delete a profile or import into an existing one."
            }
            Self::ReadFailed => "The browser's cookie database could not be read.",
        }
    }
}

/// `BrowserImportSourceProfile`: `directory` is relative to the browser's user data folder, or
/// absolute for a profile kept elsewhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceProfile {
    pub(crate) directory: String,
    pub(crate) name: String,
    pub(crate) cookie_count: Option<u64>,
}

/// `BrowserImportSource`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Source {
    pub(crate) id: SourceId,
    pub(crate) profiles: Vec<SourceProfile>,
    pub(crate) unavailable: Option<Reason>,
}

impl Source {
    pub(crate) fn name(&self) -> &'static str {
        self.id.name()
    }

    /// The Add profile menu lists it: it is installed on a platform it can be read on.
    pub(crate) fn listed(&self) -> bool {
        !matches!(self.unavailable, Some(Reason::NotInstalled | Reason::UnsupportedPlatform))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SameSite {
    /// Left for the browser to decide; guessing None would widen where a cookie is sent.
    Unspecified,
    NoRestriction,
    Lax,
    Strict,
}

/// A cookie as the source browser stored it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImportedCookie {
    /// The host without a leading dot, which names the cookie's site.
    pub(crate) host: String,
    /// The `.`-prefixed domain of a cookie its subdomains also get; `None` for a host-only one.
    pub(crate) domain: Option<String>,
    pub(crate) name: String,
    pub(crate) value: String,
    pub(crate) path: String,
    pub(crate) secure: bool,
    pub(crate) http_only: bool,
    /// Unix seconds; `None` for a session cookie.
    pub(crate) expires: Option<i64>,
    pub(crate) same_site: SameSite,
}

impl ImportedCookie {
    /// `cookieScope`: a host stored with a leading dot is a domain cookie.
    fn scoped(host: &str) -> (String, Option<String>) {
        let bare = bare_host(host).to_owned();
        (bare, host.starts_with('.').then(|| host.to_owned()))
    }
}

pub(crate) fn bare_host(host: &str) -> &str {
    host.strip_prefix('.').unwrap_or(host)
}

/// `CookieReadResult`: the readable cookies, and how many (and whose) could not be decrypted.
#[derive(Debug, Default)]
pub(crate) struct CookieRead {
    pub(crate) cookies: Vec<ImportedCookie>,
    pub(crate) undecryptable: usize,
    pub(crate) undecryptable_hosts: Vec<String>,
}

/// `BrowserImport.listSources`: every browser this client knows, in the menu's order.
pub(crate) fn list_sources() -> Vec<Source> {
    let paths = Paths::from_env();
    sources::DEFINITIONS
        .iter()
        .map(|definition| {
            let unavailable = sources::unavailable_reason(definition, &paths);
            let profiles = if unavailable.is_none() { sources::list_profiles(definition, &paths) } else { Vec::new() };
            Source { id: definition.id, profiles, unavailable }
        })
        .collect()
}

/// `BrowserImport.importCookies`' read: the cookies of the source's profile `directory`, which
/// must be one the source lists, so a stale or crafted path cannot leave its user data folder.
pub(crate) fn read_cookies(id: SourceId, directory: &str) -> Result<CookieRead, Reason> {
    let paths = Paths::from_env();
    let definition = sources::DEFINITIONS.iter().find(|definition| definition.id == id).ok_or(Reason::UnknownSource)?;
    if let Some(reason) = sources::unavailable_reason(definition, &paths) {
        return Err(reason);
    }
    let profiles = sources::list_profiles(definition, &paths);
    let profile = profiles.iter().find(|profile| profile.directory == directory).ok_or(Reason::UnknownSourceProfile)?;
    // Re-resolved, since a Chromium jar may have moved under `Network/`.
    let database =
        sources::resolve_cookie_database(definition, &paths, &profile.directory).ok_or(Reason::ReadFailed)?;
    match &definition.engine {
        Engine::Safari => safari::read(&database).map(CookieRead::readable),
        Engine::Firefox => firefox::read(&database).map(CookieRead::readable),
        Engine::Chromium(chromium) => {
            let local_state = sources::user_data_directory(definition, &paths).map(|root| root.join("Local State"));
            chromium::read(chromium, &database, local_state.as_deref())
        }
    }
}

impl CookieRead {
    fn readable(cookies: Vec<ImportedCookie>) -> Self {
        Self { cookies, ..Self::default() }
    }
}

/// `checkSystemPermission("full-disk-access")`: whether this app may read Safari's cookies.
pub(crate) fn full_disk_access_granted() -> bool {
    sources::safari_access_granted(&Paths::from_env())
}
