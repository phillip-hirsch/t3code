//! `Sources.ts`: where each browser keeps its profiles and cookies, and whether it is running.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use super::{Reason, SourceId, SourceProfile};
use crate::sqlite::Snapshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Platform {
    Mac,
    Windows,
    Linux,
}

impl Platform {
    pub(super) const CURRENT: Self = if cfg!(target_os = "macos") {
        Self::Mac
    } else if cfg!(windows) {
        Self::Windows
    } else {
        Self::Linux
    };
}

/// `BrowserImportPathContext`: the folders browsers keep their data under.
pub(super) struct Paths {
    pub(super) platform: Platform,
    home: PathBuf,
    linux_config: PathBuf,
    linux_chrome_config: PathBuf,
    app_data: Option<PathBuf>,
    local_app_data: Option<PathBuf>,
}

impl Paths {
    pub(super) fn from_env() -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty()).map(PathBuf::from);
        let home = var("HOME").or_else(|| var("USERPROFILE")).unwrap_or_default();
        Self::new(Platform::CURRENT, home, var)
    }

    fn new(platform: Platform, home: PathBuf, var: impl Fn(&str) -> Option<PathBuf>) -> Self {
        let absolute = |name: &str| var(name).filter(|path| path.is_absolute());
        let linux_config = absolute("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        Self {
            platform,
            linux_chrome_config: absolute("CHROME_CONFIG_HOME").unwrap_or_else(|| linux_config.clone()),
            linux_config,
            app_data: var("APPDATA"),
            local_app_data: var("LOCALAPPDATA"),
            home,
        }
    }

    fn application_support(&self, segments: &[&str]) -> PathBuf {
        segments.iter().fold(self.home.join("Library").join("Application Support"), |path, s| path.join(s))
    }
}

/// A Chromium browser's folders and the keyring items holding its cookie key.
pub(super) struct Chromium {
    #[cfg_attr(not(target_os = "macos"), expect(dead_code, reason = "only macOS reads the Keychain"))]
    pub(super) keychain_service: &'static str,
    #[cfg_attr(not(target_os = "macos"), expect(dead_code, reason = "only macOS reads the Keychain"))]
    pub(super) keychain_account: &'static str,
    mac: &'static [&'static str],
    linux: Option<&'static [&'static str]>,
    /// Its libsecret `application` attribute, which is not always its own name.
    #[cfg_attr(any(windows, target_os = "macos"), expect(dead_code, reason = "only Linux reads the Secret Service"))]
    pub(super) linux_secret: Option<&'static str>,
    windows: Option<&'static [&'static str]>,
}

pub(super) enum Engine {
    Chromium(Chromium),
    Firefox,
    Safari,
}

pub(super) struct Definition {
    pub(super) id: SourceId,
    pub(super) engine: Engine,
}

const fn chromium(
    id: SourceId,
    keychain: (&'static str, &'static str),
    mac: &'static [&'static str],
    linux: Option<(&'static [&'static str], &'static str)>,
    windows: Option<&'static [&'static str]>,
) -> Definition {
    let (linux, linux_secret) = match linux {
        Some((segments, secret)) => (Some(segments), Some(secret)),
        None => (None, None),
    };
    let engine =
        Chromium { keychain_service: keychain.0, keychain_account: keychain.1, mac, linux, linux_secret, windows };
    Definition { id, engine: Engine::Chromium(engine) }
}

/// `BROWSER_IMPORT_SOURCES`, in the menu's order. Chrome 127+'s App-Bound Encryption keeps other
/// apps from reading Windows Chromium cookies, so only Helium, which lacks it, reads there.
pub(super) const DEFINITIONS: [Definition; 10] = [
    chromium(
        SourceId::Chrome,
        ("Chrome Safe Storage", "Chrome"),
        &["Google", "Chrome"],
        Some((&["google-chrome"], "chrome")),
        None,
    ),
    chromium(
        SourceId::Edge,
        ("Microsoft Edge Safe Storage", "Microsoft Edge"),
        &["Microsoft Edge"],
        Some((&["microsoft-edge"], "msedge")),
        None,
    ),
    chromium(
        SourceId::Brave,
        ("Brave Safe Storage", "Brave"),
        &["BraveSoftware", "Brave-Browser"],
        Some((&["BraveSoftware", "Brave-Browser"], "brave")),
        None,
    ),
    // Vivaldi stores its Linux libsecret key under Chrome's application name.
    chromium(
        SourceId::Vivaldi,
        ("Vivaldi Safe Storage", "Vivaldi"),
        &["Vivaldi"],
        Some((&["vivaldi"], "chrome")),
        None,
    ),
    chromium(
        SourceId::Opera,
        ("Opera Safe Storage", "Opera"),
        &["com.operasoftware.Opera"],
        Some((&["opera"], "opera")),
        None,
    ),
    chromium(SourceId::Arc, ("Arc Safe Storage", "Arc"), &["Arc", "User Data"], None, None),
    // Helium keeps Chromium's libsecret application name on Linux.
    chromium(
        SourceId::Helium,
        ("Helium Storage Key", "Helium"),
        &["net.imput.helium"],
        Some((&["net.imput.helium"], "chromium")),
        Some(&["imput", "Helium", "User Data"]),
    ),
    Definition { id: SourceId::Safari, engine: Engine::Safari },
    Definition { id: SourceId::Firefox, engine: Engine::Firefox },
    Definition { id: SourceId::Zen, engine: Engine::Firefox },
];

fn supports(definition: &Definition, platform: Platform) -> bool {
    match (&definition.engine, platform) {
        (Engine::Chromium(_) | Engine::Safari, Platform::Mac) | (Engine::Firefox, _) => true,
        (Engine::Chromium(chromium), Platform::Linux) => chromium.linux.is_some(),
        (Engine::Chromium(chromium), Platform::Windows) => chromium.windows.is_some(),
        (Engine::Safari, _) => false,
    }
}

pub(super) fn user_data_directory(definition: &Definition, paths: &Paths) -> Option<PathBuf> {
    let join = |root: &Path, segments: &[&str]| segments.iter().fold(root.to_path_buf(), |path, s| path.join(s));
    match (&definition.engine, paths.platform) {
        (Engine::Chromium(chromium), Platform::Mac) => Some(paths.application_support(chromium.mac)),
        (Engine::Chromium(chromium), Platform::Windows) => {
            Some(join(paths.local_app_data.as_ref()?, chromium.windows?))
        }
        (Engine::Chromium(chromium), Platform::Linux) => {
            let root = if definition.id == SourceId::Chrome { &paths.linux_chrome_config } else { &paths.linux_config };
            Some(join(root, chromium.linux?))
        }
        (Engine::Safari, Platform::Mac) => {
            Some(join(&paths.home, &["Library", "Containers", "com.apple.Safari", "Data", "Library", "Cookies"]))
        }
        (Engine::Safari, _) => None,
        (Engine::Firefox, platform) => {
            let firefox = definition.id == SourceId::Firefox;
            Some(match platform {
                Platform::Mac => paths.application_support(&[if firefox { "Firefox" } else { "zen" }]),
                Platform::Windows => {
                    let app_data = paths.app_data.as_ref()?;
                    if firefox { join(app_data, &["Mozilla", "Firefox"]) } else { app_data.join("zen") }
                }
                Platform::Linux if firefox => join(&paths.home, &[".mozilla", "firefox"]),
                Platform::Linux => paths.linux_config.join("zen"),
            })
        }
    }
}

/// Snap and Flatpak installs keep their Linux profiles outside the usual folder.
fn alternate_linux_roots(definition: &Definition, paths: &Paths) -> Vec<PathBuf> {
    let home = &paths.home;
    match definition.id {
        SourceId::Firefox => vec![home.join("snap").join("firefox").join("common").join(".mozilla").join("firefox")],
        SourceId::Zen => vec![home.join(".zen"), home.join(".var").join("app").join("app.zen_browser.zen").join("zen")],
        _ => Vec::new(),
    }
}

fn profile_path(root: &Path, directory: &str) -> PathBuf {
    let directory = Path::new(directory);
    if directory.is_absolute() { directory.to_path_buf() } else { root.join(directory) }
}

fn cookie_database_candidates(definition: &Definition, root: &Path, directory: &str) -> Vec<PathBuf> {
    let profile = profile_path(root, directory);
    match definition.engine {
        Engine::Firefox => vec![profile.join("cookies.sqlite")],
        Engine::Safari => vec![profile.join("Cookies.binarycookies")],
        // Chromium 96+ keeps it under `Network/`; an upgrade leaves the older file behind.
        Engine::Chromium(_) => vec![profile.join("Network").join("Cookies"), profile.join("Cookies")],
    }
}

fn resolve_in(definition: &Definition, root: &Path, directory: &str) -> Option<PathBuf> {
    cookie_database_candidates(definition, root, directory).into_iter().find(|path| path.is_file())
}

pub(super) fn resolve_cookie_database(definition: &Definition, paths: &Paths, directory: &str) -> Option<PathBuf> {
    resolve_in(definition, &user_data_directory(definition, paths)?, directory)
}

/// `parseFirefoxProfiles`: only `[ProfileN]` sections are profiles (`[Install…]` ones pick
/// defaults). A relative path must stay inside `root`; `IsRelative=0` allows any absolute one.
pub(super) fn parse_firefox_profiles(ini: &str) -> Vec<SourceProfile> {
    #[derive(Default)]
    struct Section {
        name: Option<String>,
        path: Option<String>,
        is_relative: Option<String>,
    }
    fn flush(section: Option<Section>, profiles: &mut Vec<SourceProfile>) {
        let Some(Section { name, path: Some(candidate), is_relative }) = section else { return };
        if !matches!(is_relative.as_deref(), None | Some("0" | "1")) || candidate.contains('\0') {
            return;
        }
        let directory = if is_relative.as_deref() == Some("0") {
            Path::new(&candidate).is_absolute().then(|| candidate.clone())
        } else {
            contained_relative(&candidate)
        };
        if let Some(directory) = directory {
            let name = name.map(|name| name.trim().to_owned()).filter(|name| !name.is_empty());
            profiles.push(SourceProfile {
                name: name.unwrap_or_else(|| directory.clone()),
                directory,
                cookie_count: None,
            });
        }
    }

    let mut profiles = Vec::new();
    let mut current: Option<Section> = None;
    for line in ini.lines().map(str::trim) {
        if line.starts_with('[') {
            flush(current.take(), &mut profiles);
            let profile = line
                .strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
                .and_then(|label| label.get(..7).filter(|p| p.eq_ignore_ascii_case("profile")).map(|_| &label[7..]))
                .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()));
            current = profile.then(Section::default);
            continue;
        }
        let Some(section) = current.as_mut() else { continue };
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim().to_owned();
        match key.trim().to_ascii_lowercase().as_str() {
            "name" => section.name = Some(value),
            "path" => section.path = Some(value),
            "isrelative" => section.is_relative = Some(value),
            _ => {}
        }
    }
    flush(current, &mut profiles);
    profiles
}

/// `candidate` normalized, or `None` when it is absolute or climbs out of the folder it is in.
fn contained_relative(candidate: &str) -> Option<String> {
    let mut parts = Vec::new();
    for component in Path::new(candidate).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::Normal(part) => parts.push(part),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if parts.is_empty() {
        return Some(".".into());
    }
    Some(parts.iter().collect::<PathBuf>().to_string_lossy().into_owned())
}

/// Browser metadata is the user's to write; a profile folder is one plain name.
fn is_safe_profile_directory(directory: &str) -> bool {
    !directory.is_empty() && directory != "." && directory != ".." && !directory.contains(['/', '\\', '\0'])
}

/// Chromium's `Local State` names its profiles under `profile.info_cache`.
pub(super) fn parse_local_state_profiles(contents: &str) -> Vec<SourceProfile> {
    let Ok(state) = serde_json::from_str::<serde_json::Value>(contents) else { return Vec::new() };
    let Some(cache) = state.pointer("/profile/info_cache").and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    cache
        .iter()
        .filter(|(directory, _)| is_safe_profile_directory(directory))
        .map(|(directory, info)| {
            let name = info["name"].as_str().map(str::trim).filter(|name| !name.is_empty());
            SourceProfile {
                directory: directory.clone(),
                name: name.unwrap_or(directory).to_owned(),
                cookie_count: None,
            }
        })
        .collect()
}

fn sorted_entries(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .map(|entries| entries.flatten().filter_map(|entry| entry.file_name().into_string().ok()).collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// Only cookies outside Firefox containers are imported, so only those are counted.
fn count_cookies(definition: &Definition, root: &Path, directory: &str) -> Option<u64> {
    let database = resolve_in(definition, root, directory)?;
    let sql = match definition.engine {
        Engine::Firefox => "SELECT count(*) FROM moz_cookies WHERE originAttributes = ''",
        Engine::Chromium(_) => "SELECT count(*) FROM cookies",
        Engine::Safari => return None,
    };
    let snapshot = Snapshot::open(&database).ok()?;
    let count: i64 = snapshot.connection().query_row(sql, [], |row| row.get(0)).ok()?;
    u64::try_from(count).ok()
}

fn with_counts(definition: &Definition, root: &Path, profiles: Vec<SourceProfile>) -> Vec<SourceProfile> {
    profiles
        .into_iter()
        .map(|profile| SourceProfile { cookie_count: count_cookies(definition, root, &profile.directory), ..profile })
        .collect()
}

fn list_profiles_in(definition: &Definition, paths: &Paths, root: &Path) -> Vec<SourceProfile> {
    match definition.engine {
        Engine::Safari => list_safari_profiles(root),
        Engine::Firefox => {
            let declared = std::fs::read_to_string(root.join("profiles.ini"))
                .map(|ini| parse_firefox_profiles(&ini))
                .unwrap_or_default();
            // Declared profiles may be stale or never launched; then look for unlisted ones.
            let with_database: Vec<_> = declared
                .into_iter()
                .filter(|profile| resolve_in(definition, root, &profile.directory).is_some())
                .collect();
            if !with_database.is_empty() {
                return with_counts(definition, root, with_database);
            }
            let linux = paths.platform == Platform::Linux;
            let scanned = if linux { root.to_path_buf() } else { root.join("Profiles") };
            let found = sorted_entries(&scanned)
                .into_iter()
                .filter_map(|entry| {
                    let directory = if linux {
                        entry.clone()
                    } else {
                        Path::new("Profiles").join(&entry).to_string_lossy().into_owned()
                    };
                    resolve_in(definition, root, &directory).map(|_| SourceProfile {
                        directory,
                        name: entry,
                        cookie_count: None,
                    })
                })
                .collect();
            with_counts(definition, root, found)
        }
        Engine::Chromium(_) => {
            let declared = std::fs::read_to_string(root.join("Local State"))
                .map(|contents| parse_local_state_profiles(&contents))
                .unwrap_or_default();
            if !declared.is_empty() {
                return with_counts(definition, root, declared);
            }
            let found = sorted_entries(root)
                .into_iter()
                .filter(|entry| is_safe_profile_directory(entry) && resolve_in(definition, root, entry).is_some())
                .map(|directory| SourceProfile { name: directory.clone(), directory, cookie_count: None })
                .collect();
            with_counts(definition, root, found)
        }
    }
}

/// `listSourceProfiles`. Linux Firefox profiles from a Snap or Flatpak root keep absolute paths,
/// so reads and lock checks use that installation.
pub(super) fn list_profiles(definition: &Definition, paths: &Paths) -> Vec<SourceProfile> {
    let Some(root) = user_data_directory(definition, paths) else { return Vec::new() };
    if !matches!(definition.engine, Engine::Firefox) || paths.platform != Platform::Linux {
        return list_profiles_in(definition, paths, &root);
    }
    let mut seen = HashSet::new();
    let mut profiles = Vec::new();
    for directory in std::iter::once(root.clone()).chain(alternate_linux_roots(definition, paths)) {
        for profile in list_profiles_in(definition, paths, &directory) {
            let absolute = profile_path(&directory, &profile.directory);
            if seen.insert(absolute.clone()) {
                let directory =
                    if directory == root { profile.directory } else { absolute.to_string_lossy().into_owned() };
                profiles.push(SourceProfile { directory, ..profile });
            }
        }
    }
    profiles
}

fn is_safari_profile_uuid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Safari's profiles: `SafariTabs.db` names them; each keeps its jar in its own website data
/// store. Without readable metadata, the stores on disk are listed by id.
fn list_safari_profiles(root: &Path) -> Vec<SourceProfile> {
    let Some(library) = root.parent() else { return Vec::new() };
    let declared = read_safari_profiles(&library.join("Safari").join("SafariTabs.db")).unwrap_or_default();
    let default_name = declared.iter().find(|(_, uuid)| uuid == "DefaultProfile").map_or("Safari", |(title, _)| {
        title.as_deref().map(str::trim).filter(|title| !title.is_empty()).unwrap_or("Personal")
    });
    let mut profiles = vec![SourceProfile { directory: ".".into(), name: default_name.to_owned(), cookie_count: None }];
    let stores = library.join("WebKit").join("WebsiteDataStore");
    let store = |uuid: &str| stores.join(uuid).join("Cookies").to_string_lossy().into_owned();
    for (title, uuid) in declared.iter().filter(|(_, uuid)| is_safari_profile_uuid(uuid)) {
        let name = title.as_deref().map(str::trim).filter(|title| !title.is_empty()).unwrap_or(uuid);
        profiles.push(SourceProfile {
            directory: store(&uuid.to_lowercase()),
            name: name.to_owned(),
            cookie_count: None,
        });
    }
    // With readable metadata, profiles deleted but left on disk stay deleted.
    if declared.is_empty() {
        for entry in sorted_entries(&stores).into_iter().filter(|entry| is_safari_profile_uuid(entry)) {
            let directory = store(&entry);
            if Path::new(&directory).join("Cookies.binarycookies").is_file() {
                profiles.push(SourceProfile { directory, name: entry, cookie_count: None });
            }
        }
    }
    profiles
}

fn read_safari_profiles(metadata: &Path) -> Option<Vec<(Option<String>, String)>> {
    let snapshot = Snapshot::open(metadata).ok()?;
    let mut statement = snapshot
        .connection()
        .prepare(
            "SELECT title, external_uuid FROM bookmarks WHERE parent = 0 AND type = 1 AND subtype = 2 \
             AND deleted = 0 ORDER BY order_index",
        )
        .ok()?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?))).ok()?;
    rows.collect::<Result<Vec<_>, _>>().ok()
}

/// `isSourceInstalled`: installers create empty folders, so a cookie database must exist.
fn is_installed(definition: &Definition, paths: &Paths) -> bool {
    list_profiles(definition, paths)
        .iter()
        .any(|profile| resolve_cookie_database(definition, paths, &profile.directory).is_some())
}

/// `unavailableReason`.
pub(super) fn unavailable_reason(definition: &Definition, paths: &Paths) -> Option<Reason> {
    if !supports(definition, paths.platform) {
        return Some(Reason::UnsupportedPlatform);
    }
    if !is_installed(definition, paths) {
        return Some(Reason::NotInstalled);
    }
    if is_running(definition, paths) {
        return Some(Reason::BrowserRunning);
    }
    // Listing a jar needs no Full Disk Access, but reading it does.
    if matches!(definition.engine, Engine::Safari)
        && resolve_cookie_database(definition, paths, ".").is_some_and(|jar| super::safari::access_denied(&jar))
    {
        return Some(Reason::NeedsFullDiskAccess);
    }
    None
}

/// `safariPermissionCheck`: any Safari jar opens.
pub(super) fn safari_access_granted(paths: &Paths) -> bool {
    let Some(safari) = DEFINITIONS.iter().find(|definition| definition.id == SourceId::Safari) else { return false };
    if paths.platform != Platform::Mac {
        return false;
    }
    if let Some(jar) = resolve_cookie_database(safari, paths, ".") {
        return std::fs::File::open(jar).is_ok();
    }
    // A grant may reveal named profiles TCC was hiding.
    list_profiles(safari, paths).iter().any(|profile| {
        resolve_cookie_database(safari, paths, &profile.directory).is_some_and(|jar| std::fs::File::open(jar).is_ok())
    })
}

/// `isSourceRunning`. Safari writes its jar atomically, so it can be read while open.
fn is_running(definition: &Definition, paths: &Paths) -> bool {
    let Some(root) = user_data_directory(definition, paths) else { return false };
    match definition.engine {
        Engine::Safari => false,
        Engine::Chromium(_) => chromium_running(definition, paths, &root),
        Engine::Firefox => {
            let profiles = list_profiles(definition, paths);
            let probe = locks::FirefoxProbe::new();
            profiles.iter().any(|profile| probe.held(&profile_path(&root, &profile.directory)))
        }
    }
}

/// Windows Chromium holds its cookie databases open without sharing.
#[cfg(windows)]
fn chromium_running(definition: &Definition, paths: &Paths, root: &Path) -> bool {
    list_profiles(definition, paths)
        .iter()
        .filter_map(|profile| resolve_in(definition, root, &profile.directory))
        .any(|database| locks::windows_lock_held(&database))
}

/// Chromium's `SingletonLock` links to `hostname-pid` while a browser owns the folder.
#[cfg(unix)]
fn chromium_running(_: &Definition, _: &Paths, root: &Path) -> bool {
    match std::fs::read_link(root.join("SingletonLock")) {
        Ok(target) => locks::singleton_lock_held(&target.to_string_lossy(), &locks::hostname(), locks::process_alive),
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

pub(super) mod locks {
    #[cfg(any(unix, test))]
    use std::collections::HashSet;
    use std::path::Path;

    /// `chromiumSingletonLockIsHeld`: anything but a provably dead local owner counts as held.
    #[cfg(any(unix, test))]
    pub(in crate::browser_import) fn singleton_lock_held(
        target: &str,
        current_host: &str,
        alive: impl Fn(u32) -> bool,
    ) -> bool {
        let Some((host, pid)) = target.rsplit_once('-').filter(|(host, _)| !host.is_empty()) else { return true };
        // A shared home's other machines' PIDs mean nothing here.
        match parse_pid(pid) {
            Some(pid) if host == current_host => alive(pid),
            _ => true,
        }
    }

    /// `firefoxSymlinkLockIsHeld`: Firefox links `lock` to `address:+pid`; only a local
    /// owner's PID can be checked.
    #[cfg(any(unix, test))]
    pub(in crate::browser_import) fn firefox_symlink_lock_held(
        target: &str,
        local_addresses: &HashSet<String>,
        alive: impl Fn(u32) -> bool,
    ) -> bool {
        let Some((owner, pid)) = target.rsplit_once(':') else { return true };
        if !local_addresses.contains(owner) {
            return true;
        }
        parse_pid(pid.strip_prefix('+').unwrap_or(pid)).is_none_or(alive)
    }

    #[cfg(any(unix, test))]
    fn parse_pid(text: &str) -> Option<u32> {
        if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        text.parse().ok().filter(|pid| *pid > 0 && i32::try_from(*pid).is_ok())
    }

    /// Opening for writing is refused with a sharing or lock violation while a browser holds it.
    #[cfg(windows)]
    pub(super) fn windows_lock_held(path: &Path) -> bool {
        const ERROR_SHARING_VIOLATION: i32 = 32;
        const ERROR_LOCK_VIOLATION: i32 = 33;
        match std::fs::OpenOptions::new().read(true).write(true).open(path) {
            Ok(_) => false,
            Err(error) => matches!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)),
        }
    }

    #[cfg(unix)]
    pub(super) fn hostname() -> String {
        nix::unistd::gethostname().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Only "no such process" proves a lock stale; a permission error may hide a live browser.
    #[cfg(unix)]
    pub(super) fn process_alive(pid: u32) -> bool {
        let Ok(pid) = i32::try_from(pid) else { return true };
        !matches!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None), Err(nix::errno::Errno::ESRCH))
    }

    /// Whether a Firefox profile is in use. Firefox leaves its lock files behind on exit, so the
    /// locks are tested, not the files.
    pub(super) struct FirefoxProbe {
        #[cfg(unix)]
        local_addresses: HashSet<String>,
    }

    impl FirefoxProbe {
        #[cfg(windows)]
        pub(super) fn new() -> Self {
            Self {}
        }

        #[cfg(windows)]
        pub(super) fn held(&self, directory: &Path) -> bool {
            windows_lock_held(&directory.join("parent.lock"))
        }

        /// Firefox names the address its hostname resolves to, else the loopback one.
        #[cfg(unix)]
        pub(super) fn new() -> Self {
            use std::net::ToSocketAddrs as _;
            let mut local_addresses: HashSet<String> = ["127.0.0.1", "::1"].map(String::from).into();
            if let Ok(resolved) = (hostname().as_str(), 0).to_socket_addrs() {
                local_addresses.extend(resolved.map(|address| address.ip().to_string()));
            }
            Self { local_addresses }
        }

        #[cfg(unix)]
        pub(super) fn held(&self, directory: &Path) -> bool {
            if let Ok(target) = std::fs::read_link(directory.join("lock"))
                && firefox_symlink_lock_held(&target.to_string_lossy(), &self.local_addresses, process_alive)
            {
                return true;
            }
            let parent_lock = directory.join(".parentlock");
            parent_lock.is_file() && fcntl_lock_held(&parent_lock)
        }
    }

    /// Asks Python whether the `.parentlock` `fcntl` lock is held, as Electron does. Without a
    /// working interpreter (Apple's shim with no developer tools) it reads as free: "held" would
    /// block the import for good, and the database is read from a snapshot anyway.
    #[cfg(unix)]
    fn fcntl_lock_held(path: &Path) -> bool {
        const SCRIPT: &str = "import fcntl,os,sys\nfd=os.open(sys.argv[1],os.O_WRONLY)\ntry:\n  \
                              fcntl.lockf(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)\nexcept BlockingIOError:\n  \
                              print('held')\nelse:\n  print('free')";
        // A Dock launch has a bare PATH, so the absolute interpreter goes first.
        for interpreter in ["/usr/bin/python3", "python3"] {
            let output = std::process::Command::new(interpreter)
                .args(["-c", SCRIPT])
                .arg(path)
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output();
            match output.as_ref().map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned()).as_deref() {
                Ok("held") => return true,
                Ok("free") => return false,
                _ => {}
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("t3-gpui-import-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn paths(platform: Platform, home: &Path) -> Paths {
        Paths::new(platform, home.to_path_buf(), |name| match name {
            "XDG_CONFIG_HOME" => Some(PathBuf::from("relative/is/ignored")),
            "LOCALAPPDATA" => Some(home.join("AppData").join("Local")),
            "APPDATA" => Some(home.join("AppData").join("Roaming")),
            _ => None,
        })
    }

    fn definition(id: SourceId) -> &'static Definition {
        DEFINITIONS.iter().find(|definition| definition.id == id).unwrap()
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }

    #[test]
    fn browsers_live_where_each_platform_keeps_them() {
        let home = Path::new("/home/me");
        let folder = |id, platform| user_data_directory(definition(id), &paths(platform, home));
        assert_eq!(
            folder(SourceId::Chrome, Platform::Linux),
            Some(home.join(".config/google-chrome")),
            "no relative XDG"
        );
        assert_eq!(
            folder(SourceId::Brave, Platform::Mac),
            Some(home.join("Library/Application Support/BraveSoftware/Brave-Browser"))
        );
        assert_eq!(
            folder(SourceId::Helium, Platform::Windows),
            Some(home.join("AppData/Local/imput/Helium/User Data"))
        );
        assert_eq!(folder(SourceId::Firefox, Platform::Windows), Some(home.join("AppData/Roaming/Mozilla/Firefox")));
        assert_eq!(folder(SourceId::Zen, Platform::Linux), Some(home.join(".config/zen")));
        assert_eq!(folder(SourceId::Safari, Platform::Linux), None);

        let supported = |platform| {
            DEFINITIONS.iter().filter(|definition| supports(definition, platform)).map(|d| d.id).collect::<Vec<_>>()
        };
        assert_eq!(supported(Platform::Windows), [SourceId::Helium, SourceId::Firefox, SourceId::Zen]);
        assert!(!supported(Platform::Linux).contains(&SourceId::Arc), "Arc is macOS only");
        assert_eq!(supported(Platform::Mac).len(), 10);
    }

    #[test]
    fn firefox_profiles_come_from_profile_sections_and_stay_inside_the_folder() {
        let custom = if cfg!(windows) { r"C:\custom\work" } else { "/custom/work" };
        let ini = format!(
            "[Install4F96D1932A9F858E]\nDefault=Profiles/install.default\n\
             [Profile0]\nName= default \nIsRelative=1\nPath=Profiles/abc.default\n\
             [Profile1]\nName=escape\nIsRelative=1\nPath=../outside\n\
             [Profile2]\nIsRelative=0\nPath={custom}\n\
             [Profile3]\nName=bad\nIsRelative=2\nPath=Profiles/bad\n\
             [Profile4]\nName=relative but marked absolute\nIsRelative=0\nPath=Profiles/x\n\
             [General]\nPath=Profiles/general\n\
             [profile5]\nPath=Profiles/./nested/../five\n"
        );
        let profiles = parse_firefox_profiles(&ini);
        let listed: Vec<(&str, &str)> = profiles.iter().map(|p| (p.directory.as_str(), p.name.as_str())).collect();
        let five = Path::new("Profiles").join("five").to_string_lossy().into_owned();
        let abc = Path::new("Profiles").join("abc.default").to_string_lossy().into_owned();
        assert_eq!(listed, [(abc.as_str(), "default"), (custom, custom), (five.as_str(), five.as_str())]);
    }

    #[test]
    fn chromium_profiles_reject_traversal_and_fall_back_to_folders_with_cookies() {
        let state =
            r#"{"profile":{"info_cache":{"Default":{"name":" Work "},"Profile 2":{},"..":{"name":"x"},"a/b":{}}}}"#;
        let declared = parse_local_state_profiles(state);
        let listed: Vec<(&str, &str)> = declared.iter().map(|p| (p.directory.as_str(), p.name.as_str())).collect();
        assert_eq!(listed, [("Default", "Work"), ("Profile 2", "Profile 2")]);
        assert!(parse_local_state_profiles("not json").is_empty());

        let home = temp_dir("chromium");
        let paths = paths(Platform::Linux, &home);
        let chrome = definition(SourceId::Chrome);
        let root = user_data_directory(chrome, &paths).unwrap();
        std::fs::create_dir_all(root.join("Empty")).unwrap();
        assert_eq!(unavailable_reason(chrome, &paths), Some(Reason::NotInstalled), "an empty folder is not an install");
        touch(&root.join("Default").join("Network").join("Cookies"));
        touch(&root.join("Profile 3").join("Cookies"));
        let found = list_profiles(chrome, &paths);
        assert_eq!(found.iter().map(|p| p.directory.as_str()).collect::<Vec<_>>(), ["Default", "Profile 3"]);
        assert_eq!(
            resolve_cookie_database(chrome, &paths, "Default"),
            Some(root.join("Default").join("Network").join("Cookies")),
            "Chromium 96+'s jar first"
        );
        assert_eq!(unavailable_reason(chrome, &paths), None, "no SingletonLock: not running");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn linux_firefox_lists_snap_profiles_by_absolute_path() {
        let home = temp_dir("firefox");
        let paths = paths(Platform::Linux, &home);
        let firefox = definition(SourceId::Firefox);
        let root = user_data_directory(firefox, &paths).unwrap();
        touch(&root.join("abc.default").join("cookies.sqlite"));
        let snap = ["snap", "firefox", "common", ".mozilla", "firefox"]
            .iter()
            .fold(home.clone(), |path, part| path.join(part));
        touch(&snap.join("snap.default").join("cookies.sqlite"));
        let found = list_profiles(firefox, &paths);
        let directories: Vec<&str> = found.iter().map(|p| p.directory.as_str()).collect();
        let snap_profile = snap.join("snap.default").to_string_lossy().into_owned();
        assert_eq!(directories, ["abc.default", snap_profile.as_str()]);
        assert_eq!(
            resolve_cookie_database(firefox, &paths, &snap_profile),
            Some(snap.join("snap.default").join("cookies.sqlite"))
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn locks_count_as_held_unless_a_local_owner_is_provably_gone() {
        let alive = |pid: u32| pid == 42;
        assert!(locks::singleton_lock_held("box-42", "box", alive));
        assert!(!locks::singleton_lock_held("my-box-7", "my-box", alive), "a dead local owner is stale");
        assert!(locks::singleton_lock_held("other-7", "box", alive), "another machine's lock");
        for malformed in ["box", "-7", "box-", "box-x7", "box-0"] {
            assert!(locks::singleton_lock_held(malformed, "box", alive), "{malformed}");
        }

        let local: HashSet<String> = ["127.0.0.1".to_owned()].into();
        assert!(locks::firefox_symlink_lock_held("127.0.0.1:+42", &local, alive));
        assert!(!locks::firefox_symlink_lock_held("127.0.0.1:+7", &local, alive));
        assert!(!locks::firefox_symlink_lock_held("127.0.0.1:7", &local, alive), "the + is optional");
        assert!(locks::firefox_symlink_lock_held("10.0.0.9:+7", &local, alive), "a remote owner");
        assert!(locks::firefox_symlink_lock_held("127.0.0.1:+x", &local, alive));
    }

    #[test]
    fn safari_profile_ids_are_uuids() {
        assert!(is_safari_profile_uuid("1A2B3C4D-1111-2222-3333-444455556666"));
        assert!(!is_safari_profile_uuid("DefaultProfile"));
        assert!(!is_safari_profile_uuid("1A2B3C4D-1111-2222-3333-44445555666"));
        assert!(!is_safari_profile_uuid("../../../../etc-1111-2222-3333-444455556666"));
    }
}
