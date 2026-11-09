//! Opening a remote environment's paths in an editor on this machine (`apps/web/src/remoteOpen.ts`
//! with the contracts' remote editor table and `buildRemoteOpenUrl`): the desktop's own server
//! runs its editors itself, a remote one is opened through an editor's SSH remote link, and one
//! with no SSH route cannot be opened.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::ssh::SshTarget;

/// `RemoteOpenHost`: the SSH destination a remote link names (an SSH alias, or a host the
/// server advertises).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteHost {
    pub host: String,
    pub username: Option<String>,
}

/// `RemoteOpenState`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteOpenState {
    /// The environment's own `shell.openInEditor` opens paths on its host.
    LocalExec,
    /// Paths open in an editor on this machine through `host`.
    RemoteLinks(RemoteHost),
    /// Remote, with no SSH route to open it through.
    RemoteUnavailable,
}

/// `resolveRemoteOpenState` for the desktop renderer: the desktop owns its primary, even at a
/// non-loopback WSL2 NAT address. A saved environment opens through its SSH profile (`ssh`, the
/// entry's first route), else the first host its server advertises (`remoteOpenTargets`).
pub fn resolve(primary: bool, ssh: Option<&SshTarget>, remote_open_targets: &Value) -> RemoteOpenState {
    if primary {
        return RemoteOpenState::LocalExec;
    }
    if let Some(target) = ssh.filter(|target| !target.alias.is_empty()) {
        let alias = target.alias.as_str();
        let (alias_user, host) = match alias.rfind('@') {
            Some(at) => (Some(&alias[..at]), &alias[at + 1..]),
            None => (None, alias),
        };
        let username = target.username.as_deref().map(str::trim).filter(|user| !user.is_empty()).or(alias_user);
        return RemoteOpenState::RemoteLinks(RemoteHost {
            host: host.to_owned(),
            username: username.map(str::to_owned),
        });
    }
    match first_advertised(remote_open_targets) {
        Some(host) => RemoteOpenState::RemoteLinks(host),
        None => RemoteOpenState::RemoteUnavailable,
    }
}

/// The first decodable `RemoteOpenTarget`; `ForwardCompatibleArray` drops the others.
fn first_advertised(targets: &Value) -> Option<RemoteHost> {
    let trimmed = |value: &Value| value.as_str().map(str::trim).filter(|text| !text.is_empty()).map(str::to_owned);
    targets.as_array()?.iter().find_map(|target| {
        if !matches!(target["kind"].as_str()?, "tailscale" | "mdns") {
            return None;
        }
        let host = trimmed(&target["host"])?;
        let username = match target.get("username") {
            None => None,
            Some(username) => Some(trimmed(username)?),
        };
        Some(RemoteHost { host, username })
    })
}

/// One of `EDITORS` with a remote scheme (`REMOTE_CAPABLE_EDITOR_IDS`).
struct RemoteEditor {
    id: &'static str,
    label: &'static str,
    commands: &'static [&'static str],
    /// `remoteSchemeForEditor`; the Gateway IDEs share `jetbrains`.
    scheme: &'static str,
    jetbrains_product_code: Option<&'static str>,
    /// `installNames` in `@t3tools/shared/editor`; the label when empty.
    install_names: &'static [&'static str],
}

const fn editor(
    id: &'static str,
    label: &'static str,
    command: &'static [&'static str],
    scheme: &'static str,
) -> RemoteEditor {
    RemoteEditor { id, label, commands: command, scheme, jetbrains_product_code: None, install_names: &[] }
}

const fn jetbrains(
    id: &'static str,
    label: &'static str,
    command: &'static [&'static str],
    code: &'static str,
) -> RemoteEditor {
    RemoteEditor {
        id,
        label,
        commands: command,
        scheme: "jetbrains",
        jetbrains_product_code: Some(code),
        install_names: &[],
    }
}

/// In `EDITORS` order, which is the pickers' order.
const REMOTE_EDITORS: [RemoteEditor; 17] = [
    editor("cursor", "Cursor", &["cursor"], "cursor"),
    RemoteEditor { install_names: &["Visual Studio Code"], ..editor("vscode", "VS Code", &["code"], "vscode") },
    RemoteEditor {
        install_names: &["Visual Studio Code - Insiders"],
        ..editor("vscode-insiders", "VS Code Insiders", &["code-insiders"], "vscode-insiders")
    },
    editor("vscodium", "VSCodium", &["codium"], "vscodium"),
    editor("zed", "Zed", &["zed", "zeditor"], "zed"),
    RemoteEditor {
        install_names: &["IntelliJ IDEA", "IntelliJ IDEA CE", "IntelliJ IDEA Ultimate"],
        ..jetbrains("idea", "IntelliJ IDEA", &["idea"], "IU")
    },
    jetbrains("aqua", "Aqua", &["aqua"], "QA"),
    jetbrains("clion", "CLion", &["clion"], "CL"),
    jetbrains("datagrip", "DataGrip", &["datagrip"], "DB"),
    jetbrains("dataspell", "DataSpell", &["dataspell"], "DS"),
    jetbrains("goland", "GoLand", &["goland"], "GO"),
    jetbrains("phpstorm", "PhpStorm", &["phpstorm"], "PS"),
    RemoteEditor { install_names: &["PyCharm", "PyCharm CE"], ..jetbrains("pycharm", "PyCharm", &["pycharm"], "PY") },
    RemoteEditor { install_names: &["Rider", "JetBrains Rider"], ..jetbrains("rider", "Rider", &["rider"], "RD") },
    jetbrains("rubymine", "RubyMine", &["rubymine"], "RM"),
    jetbrains("rustrover", "RustRover", &["rustrover"], "RR"),
    jetbrains("webstorm", "WebStorm", &["webstorm"], "WS"),
];

/// `REMOTE_FALLBACK_EDITORS`: listed until the probe finds any.
pub const FALLBACK_EDITORS: &[&str] = &["vscode"];

fn remote_editor(id: &str) -> Option<&'static RemoteEditor> {
    REMOTE_EDITORS.iter().find(|editor| editor.id == id)
}

/// JavaScript's `encodeURIComponent`.
fn encode_component(input: &str) -> String {
    crate::preview::encode_uri_component(input)
}

/// `buildRemoteOpenUrl`: the deep link that opens `absolute_path` on `host` in `editor`, or
/// `None` for an editor without a remote scheme.
pub fn build_url(editor: &str, host: &RemoteHost, absolute_path: &str, is_file: bool) -> Option<String> {
    let definition = remote_editor(editor)?;
    let scheme = definition.scheme;
    let posix_path = absolute_path.replace('\\', "/");
    if let Some(code) = definition.jetbrains_product_code {
        let mut params = url::form_urlencoded::Serializer::new(String::new());
        params.append_pair("h", &host.host).append_pair("launchIde", "true").append_pair("ideHint", code);
        params.append_pair("projectHint", &posix_path);
        if let Some(user) = &host.username {
            params.append_pair("u", user);
        }
        return Some(format!("{scheme}://gateway/ssh/environment?{}", params.finish()));
    }
    // Windows server paths (`C:\...`) appear as `/C:/...` in vscode-remote URIs.
    let rooted = if posix_path.starts_with('/') { posix_path } else { format!("/{posix_path}") };
    let encode_path = |path: &str| path.split('/').map(encode_component).collect::<Vec<_>>().join("/");
    let encoded_host = encode_component(&host.host);
    if editor == "zed" {
        // Zed roots paths on the system drive, so the Windows drive prefix goes.
        let bytes = absolute_path.as_bytes();
        let system_drive = bytes.len() >= 3
            && bytes[0].eq_ignore_ascii_case(&b'c')
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/');
        let zed_path = if system_drive { &rooted[3..] } else { rooted.as_str() };
        // Zed reads this segment as ssh URL userinfo, so the `@` stays literal.
        let destination = match &host.username {
            Some(user) => format!("{}@{encoded_host}", encode_component(user)),
            None => encoded_host,
        };
        return Some(format!("{scheme}://ssh/{destination}{}", encode_path(zed_path)));
    }
    let destination = match &host.username {
        Some(user) => format!("{user}@{}", host.host),
        None => host.host.clone(),
    };
    // VS Code opens a bare vscode-remote path as a folder; a trailing position opens the file.
    let position = if is_file { ":1:1" } else { "" };
    Some(format!(
        "{scheme}://vscode-remote/ssh-remote+{}{}{position}",
        encode_component(&destination),
        encode_path(&rooted)
    ))
}

// ---------------------------------------------------------------------------------------------
// probeRemoteEditors

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Platform {
    MacOs,
    Windows,
    Linux,
}

impl Platform {
    fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// Electron's `probeRemoteEditors`: the remote-capable editors installed on this machine, found
/// as the server finds editors (`resolveEditorCommand`). Reads the filesystem, so it runs off the
/// UI thread.
pub fn probe_installed() -> Vec<&'static str> {
    let platform = Platform::current();
    let env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let search_path = crate::desktop::shell_env::search_path();
    let lookup = CommandLookup {
        platform,
        extensions: path_extensions(platform, env("PATHEXT").as_deref()),
        search_path: search_path.as_deref(),
    };
    let read_dir = |dir: &Path| -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|entries| entries.flatten().filter_map(|entry| entry.file_name().into_string().ok()).collect())
            .unwrap_or_default()
    };
    REMOTE_EDITORS
        .iter()
        .filter(|editor| {
            let prefers_install = editor.id == "zed" && platform != Platform::Windows;
            (!prefers_install && editor.commands.iter().any(|command| lookup.available(command)))
                || install_candidates(editor, platform, &env, &read_dir)
                    .iter()
                    .any(|candidate| lookup.available(candidate))
        })
        .map(|editor| editor.id)
        .collect()
}

/// `resolveWindowsPathExtensions`.
fn path_extensions(platform: Platform, pathext: Option<&str>) -> Vec<String> {
    if platform != Platform::Windows {
        return Vec::new();
    }
    let mut parsed: Vec<String> = Vec::new();
    for entry in pathext.unwrap_or_default().split(';').map(str::trim).filter(|entry| !entry.is_empty()) {
        let extension = entry.to_uppercase();
        let extension = if extension.starts_with('.') { extension } else { format!(".{extension}") };
        if !parsed.contains(&extension) {
            parsed.push(extension);
        }
    }
    if parsed.is_empty() { [".COM", ".EXE", ".BAT", ".CMD"].map(str::to_owned).to_vec() } else { parsed }
}

/// `isCommandAvailable`: a path is checked as it is, a bare name on the search path.
struct CommandLookup<'a> {
    platform: Platform,
    extensions: Vec<String>,
    search_path: Option<&'a OsStr>,
}

impl CommandLookup<'_> {
    fn available(&self, command: &str) -> bool {
        let candidates = self.candidates(command);
        if command.contains(['/', '\\']) {
            return candidates.iter().any(|candidate| self.executable(Path::new(candidate)));
        }
        let Some(search_path) = self.search_path else { return false };
        let mut seen: Vec<PathBuf> = Vec::new();
        std::env::split_paths(search_path).any(|entry| {
            let text = entry.to_string_lossy();
            let unquoted = text.trim().trim_matches('"');
            if unquoted.is_empty() {
                return false;
            }
            let dir = PathBuf::from(unquoted);
            if seen.contains(&dir) {
                return false;
            }
            let found = candidates.iter().any(|candidate| self.executable(&dir.join(candidate)));
            seen.push(dir);
            found
        })
    }

    /// `resolveCommandCandidates`: on Windows, the name with each executable extension.
    fn candidates(&self, command: &str) -> Vec<String> {
        if self.platform != Platform::Windows {
            return vec![command.to_owned()];
        }
        let extension =
            Path::new(command).extension().and_then(OsStr::to_str).map(|ext| format!(".{}", ext.to_uppercase()));
        if let Some(extension) = extension.filter(|ext| self.extensions.contains(ext)) {
            let stem = &command[..command.len() - extension.len()];
            let mut candidates = vec![command.to_owned()];
            for variant in [format!("{stem}{extension}"), format!("{stem}{}", extension.to_lowercase())] {
                if !candidates.contains(&variant) {
                    candidates.push(variant);
                }
            }
            return candidates;
        }
        let mut candidates = Vec::new();
        for extension in &self.extensions {
            for variant in [format!("{command}{extension}"), format!("{command}{}", extension.to_lowercase())] {
                if !candidates.contains(&variant) {
                    candidates.push(variant);
                }
            }
        }
        candidates
    }

    /// `isExecutableFile`.
    fn executable(&self, path: &Path) -> bool {
        let Ok(metadata) = std::fs::metadata(path) else { return false };
        if !metadata.is_file() {
            return false;
        }
        if self.platform == Platform::Windows {
            let extension = path.extension().and_then(OsStr::to_str).map(|ext| format!(".{}", ext.to_uppercase()));
            return extension.is_some_and(|ext| self.extensions.contains(&ext));
        }
        can_execute(&metadata)
    }
}

#[cfg(unix)]
fn can_execute(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn can_execute(_: &std::fs::Metadata) -> bool {
    true
}

/// `isVersionedBundle`: a Toolbox install such as `PyCharm 2025.1.app`.
fn is_versioned_bundle(entry: &str, name: &str) -> bool {
    let Some(version) =
        entry.strip_prefix(name).and_then(|rest| rest.strip_prefix(' ')).and_then(|rest| rest.strip_suffix(".app"))
    else {
        return false;
    };
    version.starts_with(|c: char| c.is_ascii_digit()) && version.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// The install locations `resolveEditorCommand` checks after the search path, in its order.
fn install_candidates(
    editor: &RemoteEditor,
    platform: Platform,
    env: &dyn Fn(&str) -> Option<String>,
    read_dir: &dyn Fn(&Path) -> Vec<String>,
) -> Vec<String> {
    let label = [editor.label];
    let names: &[&str] = if editor.install_names.is_empty() { &label } else { editor.install_names };
    let command = editor.commands[0];
    let jetbrains = editor.jetbrains_product_code.is_some();
    let zed = editor.id == "zed";
    let home = env("HOME").map(PathBuf::from);
    let mut candidates: Vec<PathBuf> = Vec::new();
    match platform {
        Platform::MacOs => {
            let roots = home.iter().map(|home| home.join("Applications")).chain([PathBuf::from("/Applications")]);
            for root in roots {
                let entries = read_dir(&root);
                let mut bundles: Vec<String> = names.iter().map(|name| format!("{name}.app")).collect();
                for entry in entries {
                    let matches =
                        names.iter().any(|name| entry == format!("{name}.app") || is_versioned_bundle(&entry, name));
                    if matches && !bundles.contains(&entry) {
                        bundles.push(entry);
                    }
                }
                for bundle in bundles {
                    let contents = root.join(bundle).join("Contents");
                    if jetbrains || zed {
                        candidates.push(contents.join("MacOS").join(if zed { "cli" } else { command }));
                    } else {
                        candidates.push(contents.join("Resources/app/bin").join(command));
                        candidates.push(contents.join("Resources/app/bin/code"));
                    }
                }
            }
            if let Some(home) = home.as_ref().filter(|_| jetbrains) {
                candidates.push(home.join("Library/Application Support/JetBrains/Toolbox/scripts").join(command));
            }
        }
        Platform::Windows => {
            let local_app_data = env("LOCALAPPDATA").map(PathBuf::from);
            let roots: Vec<PathBuf> = local_app_data
                .iter()
                .map(|local| local.join("Programs"))
                .chain(
                    ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"]
                        .into_iter()
                        .filter_map(env)
                        .map(PathBuf::from),
                )
                .collect();
            if jetbrains {
                if let Some(local) = &local_app_data {
                    candidates.push(local.join("JetBrains/Toolbox/scripts").join(format!("{command}.cmd")));
                }
                for directory in roots.iter().flat_map(|root| [root.clone(), root.join("JetBrains")]) {
                    for entry in read_dir(&directory) {
                        if names.iter().any(|name| entry == *name || entry.starts_with(&format!("{name} "))) {
                            let bin = directory.join(entry).join("bin");
                            candidates.push(bin.join(format!("{command}64.exe")));
                            candidates.push(bin.join(format!("{command}.exe")));
                        }
                    }
                }
            } else {
                let name = match editor.id {
                    "vscode" => "Microsoft VS Code",
                    "vscode-insiders" => "Microsoft VS Code Insiders",
                    _ => editor.label,
                };
                for root in &roots {
                    let install = root.join(name);
                    candidates.push(install.join("resources/app/bin").join(format!("{command}.cmd")));
                    candidates.push(install.join("resources/app/bin/code.cmd"));
                    candidates.push(install.join("bin").join(format!("{command}.cmd")));
                    candidates.push(install.join("bin/code.cmd"));
                    if zed {
                        candidates.push(install.join("bin").join("zed.exe"));
                        candidates.push(install.join("zed.exe"));
                    }
                }
            }
        }
        Platform::Linux => {
            let mut dirs: Vec<PathBuf> = home.iter().map(|home| home.join(".local/bin")).collect();
            dirs.extend(["/usr/local/bin", "/usr/bin", "/snap/bin"].map(PathBuf::from));
            if jetbrains {
                let data_home = env("XDG_DATA_HOME")
                    .map(PathBuf::from)
                    .or_else(|| home.as_ref().map(|home| home.join(".local/share")));
                if let Some(data_home) = data_home {
                    dirs.push(data_home.join("JetBrains/Toolbox/scripts"));
                }
            }
            if zed {
                // Linux packages use zeditor because OpenZFS owns the zed command.
                for name in ["zeditor", "zed"] {
                    candidates.push(PathBuf::from(name));
                    candidates.extend(dirs.iter().map(|dir| dir.join(name)));
                }
            } else {
                for dir in &dirs {
                    candidates.extend(editor.commands.iter().map(|name| dir.join(name)));
                }
            }
        }
    }
    let mut candidates: Vec<String> = candidates.into_iter().map(|path| path.to_string_lossy().into_owned()).collect();
    // OpenZFS also installs a `zed` executable, so macOS tries Zed's bundle first.
    if zed && platform == Platform::MacOs {
        candidates.extend(editor.commands.iter().map(|command| (*command).to_owned()));
    }
    candidates
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn host(name: &str, username: Option<&str>) -> RemoteHost {
        RemoteHost { host: name.into(), username: username.map(str::to_owned) }
    }

    fn ssh(alias: &str, username: Option<&str>) -> SshTarget {
        SshTarget { username: username.map(str::to_owned), ..SshTarget::alias(alias) }
    }

    fn links(state: RemoteOpenState) -> RemoteHost {
        match state {
            RemoteOpenState::RemoteLinks(host) => host,
            other => panic!("expected remote links, got {other:?}"),
        }
    }

    #[test]
    fn the_desktop_primary_opens_on_its_host() {
        let targets = json!([{"kind": "tailscale", "host": "sol.tail1234.ts.net"}]);
        assert_eq!(resolve(true, Some(&ssh("sol", None)), &targets), RemoteOpenState::LocalExec);
    }

    #[test]
    fn an_ssh_profile_names_one_login_and_outranks_advertised_hosts() {
        let advertised = json!([{"kind": "tailscale", "host": "sol.tail1234.ts.net", "username": "server-user"}]);
        assert_eq!(links(resolve(false, Some(&ssh("sol", None)), &advertised)), host("sol", None));
        for (alias, username) in [
            ("server", Some("admin-user")),
            ("admin-user@server", None),
            ("old-user@server", Some("admin-user")),
            ("admin-user@server", Some("")),
            ("admin-user@server", Some("  ")),
        ] {
            let resolved = links(resolve(false, Some(&ssh(alias, username)), &advertised));
            assert_eq!(resolved, host("server", Some("admin-user")), "{alias} / {username:?}");
            let urls = ["vscode", "zed", "idea"].map(|editor| build_url(editor, &resolved, "/work/project", false));
            assert_eq!(
                urls.map(Option::unwrap),
                [
                    "vscode://vscode-remote/ssh-remote+admin-user%40server/work/project",
                    "zed://ssh/admin-user@server/work/project",
                    "jetbrains://gateway/ssh/environment?h=server&launchIde=true&ideHint=IU&projectHint=%2Fwork%2Fproject&u=admin-user",
                ]
            );
        }
    }

    #[test]
    fn without_ssh_the_first_valid_advertised_host_is_used() {
        let advertised = json!([
            {"kind": "relay", "host": "future"},
            {"kind": "tailscale", "host": "  "},
            {"kind": "tailscale", "host": "server.tail1234.ts.net", "username": "admin-user"},
            {"kind": "mdns", "host": "server.local"},
        ]);
        let resolved = links(resolve(false, None, &advertised));
        assert_eq!(
            build_url("cursor", &resolved, "/work/project", false).unwrap(),
            "cursor://vscode-remote/ssh-remote+admin-user%40server.tail1234.ts.net/work/project"
        );
        for advertised in [json!([]), Value::Null, json!([{"kind": "mdns"}])] {
            assert_eq!(resolve(false, None, &advertised), RemoteOpenState::RemoteUnavailable);
        }
        assert_eq!(resolve(false, Some(&ssh("", None)), &Value::Null), RemoteOpenState::RemoteUnavailable);
    }

    #[test]
    fn windows_paths_are_rooted_and_zed_drops_the_system_drive() {
        let sol = host("sol", None);
        assert_eq!(
            build_url("vscode", &sol, r"C:\Users\theo", false).unwrap(),
            "vscode://vscode-remote/ssh-remote+sol/C%3A/Users/theo"
        );
        assert_eq!(build_url("zed", &sol, r"C:\Users\theo", false).unwrap(), "zed://ssh/sol/Users/theo");
        assert_eq!(build_url("zed", &sol, "/C:/project", false).unwrap(), "zed://ssh/sol/C%3A/project");
    }

    #[test]
    fn only_vscode_family_file_links_carry_a_position() {
        let sol = host("sol", None);
        let cases = [
            ("vscode", true, "vscode://vscode-remote/ssh-remote+sol/tmp/my%20file.md:1:1"),
            ("cursor", true, "cursor://vscode-remote/ssh-remote+sol/tmp/my%20file.md:1:1"),
            ("vscode", false, "vscode://vscode-remote/ssh-remote+sol/tmp/my%20file.md"),
            ("zed", true, "zed://ssh/sol/tmp/my%20file.md"),
            (
                "pycharm",
                true,
                "jetbrains://gateway/ssh/environment?h=sol&launchIde=true&ideHint=PY&projectHint=%2Ftmp%2Fmy+file.md",
            ),
        ];
        for (editor, is_file, expected) in cases {
            assert_eq!(build_url(editor, &sol, "/tmp/my file.md", is_file).as_deref(), Some(expected), "{editor}");
        }
        assert_eq!(build_url("trae", &sol, "/tmp", false), None);
        assert_eq!(build_url("file-manager", &sol, "/tmp", false), None);
    }

    #[test]
    fn install_candidates_follow_resolve_editor_command() {
        let env = |name: &str| (name == "HOME").then(|| "/Users/me".to_owned());
        let read_dir = |dir: &Path| match dir.to_str() {
            Some("/Applications") => vec!["PyCharm 2025.1.app".to_owned(), "PyCharm Beta.app".to_owned()],
            _ => Vec::new(),
        };
        let pycharm = remote_editor("pycharm").unwrap();
        assert_eq!(
            install_candidates(pycharm, Platform::MacOs, &env, &read_dir),
            [
                "/Users/me/Applications/PyCharm.app/Contents/MacOS/pycharm",
                "/Users/me/Applications/PyCharm CE.app/Contents/MacOS/pycharm",
                "/Applications/PyCharm.app/Contents/MacOS/pycharm",
                "/Applications/PyCharm CE.app/Contents/MacOS/pycharm",
                "/Applications/PyCharm 2025.1.app/Contents/MacOS/pycharm",
                "/Users/me/Library/Application Support/JetBrains/Toolbox/scripts/pycharm",
            ]
        );
        let zed = remote_editor("zed").unwrap();
        let mac_zed = install_candidates(zed, Platform::MacOs, &env, &|_| Vec::new());
        assert_eq!(mac_zed.last().map(String::as_str), Some("zeditor"));
        assert_eq!(mac_zed.first().map(String::as_str), Some("/Users/me/Applications/Zed.app/Contents/MacOS/cli"));
        let linux_zed = install_candidates(zed, Platform::Linux, &env, &|_| Vec::new());
        assert_eq!(&linux_zed[..2], ["zeditor", "/Users/me/.local/bin/zeditor"]);
    }

    #[test]
    fn windows_lookups_try_each_executable_extension() {
        assert_eq!(path_extensions(Platform::Windows, Some("exe; .cmd;;.EXE")), [".EXE", ".CMD"]);
        assert_eq!(path_extensions(Platform::Windows, None), [".COM", ".EXE", ".BAT", ".CMD"]);
        let lookup = CommandLookup {
            platform: Platform::Windows,
            extensions: vec![".EXE".into(), ".CMD".into()],
            search_path: None,
        };
        assert_eq!(lookup.candidates("code"), ["code.EXE", "code.exe", "code.CMD", "code.cmd"]);
        assert_eq!(lookup.candidates("code.cmd"), ["code.cmd", "code.CMD"]);
    }
}
