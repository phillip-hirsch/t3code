//! Open VSX color themes (`openVsxThemes.ts`): search the registry, then download an
//! extension, check it against its published checksum and manifest, and read its themes
//! out of the package with the same size, ratio and path limits as the web.

use std::collections::HashMap;
use std::io::Read as _;
use std::time::Duration;

use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use crate::themes::{Collection, ThemeDefinition};
use crate::vscode_theme::{is_vscode_theme, pair_themes, parse_jsonc, parse_vscode_theme, resolve_label_collisions};

const SEARCH_URL: &str = "https://open-vsx.org/api/-/search";
const MAX_VSIX_BYTES: usize = 20 * 1024 * 1024;
const MAX_SEARCH_BYTES: usize = 512 * 1024;
const MAX_DETAIL_BYTES: usize = 256 * 1024;
const MAX_MANIFEST_BYTES: usize = 256 * 1024;
const MAX_THEME_BYTES: usize = 256 * 1024;
const MAX_ZIP_ENTRIES: usize = 5_000;
const MAX_UNCOMPRESSED_BYTES: u64 = 100 * 1024 * 1024;
const MAX_COMPRESSION_RATIO: u64 = 200;
const MAX_THEMES_PER_EXTENSION: usize = 40;
const MAX_INCLUDE_DEPTH: usize = 8;
const MAX_PACKAGE_PATH_LENGTH: usize = 1_024;
const MAX_COLOR_VALUE_LENGTH: usize = 128;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(10);
/// Import bounds each wait, not the whole download, so a slow but live 20 MB package still finishes.
const IMPORT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const IMPORT_READ_TIMEOUT: Duration = Duration::from_secs(30);
const SUPPORTED_LICENSES: &[&str] =
    &["0BSD", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "CC0-1.0", "ISC", "MIT", "MPL-2.0", "Unlicense"];
const USED_WORKBENCH_COLORS: &[&str] = &[
    "activityBar.background",
    "activityBarBadge.background",
    "badge.background",
    "button.background",
    "button.foreground",
    "contrastBorder",
    "descriptionForeground",
    "disabledForeground",
    "dropdown.background",
    "dropdown.border",
    "editor.background",
    "editor.foreground",
    "editor.findMatchBackground",
    "editor.findMatchForeground",
    "editor.findMatchHighlightBackground",
    "editor.findMatchHighlightForeground",
    "editor.selectionBackground",
    "editorCursor.foreground",
    "editorError.foreground",
    "editorGroup.border",
    "editorPane.background",
    "editorWarning.foreground",
    "editorWidget.background",
    "errorForeground",
    "focusBorder",
    "foreground",
    "input.border",
    "input.placeholderForeground",
    "list.activeSelectionBackground",
    "list.hoverBackground",
    "list.inactiveSelectionBackground",
    "menu.background",
    "panel.background",
    "panel.border",
    "progressBar.background",
    "quickInput.background",
    "scrollbarSlider.background",
    "sideBar.background",
    "sideBar.border",
    "sideBar.foreground",
    "terminal.background",
    "terminal.foreground",
    "terminal.selectionBackground",
    "terminalCursor.foreground",
    "textCodeBlock.background",
    "textLink.foreground",
];

/// `OpenVsxThemeSort`, with the web's sort labels.
pub const SORTS: &[(&str, &str)] = &[
    ("downloadCount", "Most downloaded"),
    ("rating", "Best rated"),
    ("timestamp", "Newest"),
    ("relevance", "Most relevant"),
];

#[derive(Clone, Debug, PartialEq)]
pub struct Extension {
    pub id: String,
    pub collection_id: String,
    pub name: String,
    pub publisher: String,
    pub description: String,
    pub download_count: f64,
    pub icon_url: Option<String>,
    pub source_url: Option<String>,
    pub manifest_url: String,
    pub sha256_url: String,
    pub vsix_url: String,
    pub version: String,
    pub license: String,
}

fn short_hash(value: &str) -> String {
    Sha256::digest(value.as_bytes())[..6].iter().map(|byte| format!("{byte:02x}")).collect()
}

fn theme_id(extension_id: &str, source: &str) -> String {
    format!("ovx-theme-{}", short_hash(&format!("{extension_id}:{source}")))
}

fn collection_id(extension_id: &str) -> String {
    let normalized = format!("open-vsx:{}", extension_id.to_lowercase());
    let valid = normalized.len() <= 128
        && normalized.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b':' | b'-'));
    if valid { normalized } else { format!("open-vsx:{}", short_hash(extension_id)) }
}

fn trusted_url(value: &Value) -> Option<String> {
    let url = url::Url::parse(value.as_str()?).ok()?;
    (url.scheme() == "https" && url.host_str()?.eq_ignore_ascii_case("open-vsx.org")).then(|| url.to_string())
}

fn public_source_url(value: &Value) -> Option<String> {
    let raw = value.as_str().or_else(|| value.get("url")?.as_str())?;
    let url = url::Url::parse(raw).ok()?;
    (url.scheme() == "https" && url.username().is_empty() && url.password().is_none()).then(|| url.to_string())
}

fn theme_contributions(manifest: &Value) -> Vec<&Map<String, Value>> {
    manifest["contributes"]["themes"].as_array().into_iter().flatten().filter_map(Value::as_object).collect()
}

fn license_matches(manifest: &Value, license: &str) -> bool {
    manifest["license"].as_str().is_some_and(|value| value.trim().eq_ignore_ascii_case(license))
}

/// `extensionFromDetail`: None for an unsupported license.
fn extension_from_detail(value: &Value) -> Result<Option<Extension>, String> {
    let malformed = || "Open VSX returned malformed theme details.".to_owned();
    let files = value.get("files").filter(|files| files.is_object()).ok_or_else(malformed)?;
    let text = |key: &str| value[key].as_str().map(str::trim).unwrap_or_default().to_owned();
    let (namespace, name, version, license) = (text("namespace"), text("name"), text("version"), text("license"));
    let display = Some(text("displayName")).filter(|name| !name.is_empty()).unwrap_or_else(|| name.clone());
    let (Some(manifest_url), Some(sha256_url), Some(vsix_url)) =
        (trusted_url(&files["manifest"]), trusted_url(&files["sha256"]), trusted_url(&files["download"]))
    else {
        return Err(malformed());
    };
    if namespace.is_empty() || name.is_empty() || version.is_empty() {
        return Err(malformed());
    }
    if !SUPPORTED_LICENSES.contains(&license.as_str()) {
        return Ok(None);
    }
    let id = format!("{namespace}.{name}");
    Ok(Some(Extension {
        collection_id: collection_id(&id),
        id,
        name: display,
        publisher: namespace,
        description: value["description"].as_str().unwrap_or_default().to_owned(),
        download_count: value["downloadCount"].as_f64().filter(|n| n.is_finite()).unwrap_or(0.),
        icon_url: trusted_url(&files["icon"]),
        source_url: public_source_url(&value["repository"])
            .or_else(|| public_source_url(&value["homepage"]))
            .or_else(|| public_source_url(&value["url"])),
        manifest_url,
        sha256_url,
        vsix_url,
        version,
        license,
    }))
}

/// Why a capped download failed, kept typed until the caller picks what the user reads.
#[derive(Debug)]
enum Fetch {
    TooLarge,
    Status(reqwest::StatusCode),
    Http(reqwest::Error),
}

impl Fetch {
    /// `too_large` for an oversized body, else `failed`; the cause goes to the log.
    fn message(self, too_large: &str, failed: &str) -> String {
        match self {
            Fetch::TooLarge => too_large.to_owned(),
            Fetch::Status(status) => {
                log::warn!("open vsx: HTTP {status}");
                failed.to_owned()
            }
            Fetch::Http(error) => {
                log::warn!("open vsx: {error:?}");
                failed.to_owned()
            }
        }
    }
}

async fn get_capped(client: &reqwest::Client, url: &str, limit: usize) -> Result<Vec<u8>, Fetch> {
    let mut response = client.get(url).send().await.map_err(Fetch::Http)?;
    if !response.status().is_success() {
        return Err(Fetch::Status(response.status()));
    }
    if response.content_length().is_some_and(|length| length > limit as u64) {
        return Err(Fetch::TooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(Fetch::Http)? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() > limit {
            return Err(Fetch::TooLarge);
        }
    }
    Ok(bytes)
}

fn jsonc_object(bytes: &[u8], description: &str) -> Result<Value, String> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(parse_jsonc)
        .filter(Value::is_object)
        .ok_or_else(|| format!("{description} is not valid JSON."))
}

/// One search hit's details, kept only when it advertises themes under a supported license
/// and its package is small enough.
async fn detail(client: &reqwest::Client, namespace: &str, name: &str) -> Result<Option<Extension>, String> {
    let encode = |part: &str| url::form_urlencoded::byte_serialize(part.as_bytes()).collect::<String>();
    let url = format!("https://open-vsx.org/api/{}/{}", encode(namespace), encode(name));
    let unavailable = |error: Fetch| error.message("Theme details are too large.", "Theme details are unavailable.");
    let bytes = get_capped(client, &url, MAX_DETAIL_BYTES).await.map_err(unavailable)?;
    let unreadable = |_| "Open VSX returned unreadable theme details.".to_owned();
    let Some(extension) = extension_from_detail(&serde_json::from_slice(&bytes).map_err(unreadable)?)? else {
        return Ok(None);
    };
    let (manifest, package) = futures::join!(
        get_capped(client, &extension.manifest_url, MAX_MANIFEST_BYTES),
        client.head(&extension.vsix_url).send()
    );
    let manifest = jsonc_object(&manifest.map_err(unavailable)?, "Extension manifest")?;
    let Ok(package) = package else { return Ok(None) };
    let length =
        package.headers().get(reqwest::header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()?.parse::<usize>().ok());
    if !package.status().is_success() || length.is_some_and(|length| length > MAX_VSIX_BYTES) {
        return Ok(None);
    }
    Ok((!theme_contributions(&manifest).is_empty() && license_matches(&manifest, &extension.license))
        .then_some(extension))
}

/// `searchOpenVsxThemes`: at most eight importable theme extensions.
pub async fn search(query: String, sort: &'static str) -> Result<Vec<Extension>, String> {
    let query = query.trim().to_owned();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let request = async move {
        let client = reqwest::Client::builder().timeout(SEARCH_TIMEOUT).build().map_err(|e| e.to_string())?;
        let mut url = url::Url::parse(SEARCH_URL).map_err(|e| e.to_string())?;
        url.query_pairs_mut()
            .append_pair("query", &query)
            .append_pair("category", "Themes")
            .append_pair("sortBy", sort)
            .append_pair("sortOrder", "desc")
            .append_pair("size", "16");
        let bytes = get_capped(&client, url.as_str(), MAX_SEARCH_BYTES).await.map_err(|error| match error {
            Fetch::Http(error) if error.is_timeout() => "Open VSX took too long to respond.".to_owned(),
            error => error.message(
                "Open VSX returned an unexpectedly large response.",
                "Open VSX search is unavailable right now.",
            ),
        })?;
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| "Open VSX returned an unreadable response.".to_owned())?;
        let Some(hits) = value["extensions"].as_array() else {
            return Err("Open VSX returned an unreadable search response.".to_owned());
        };
        let identities: Vec<(String, String)> = hits
            .iter()
            .filter_map(|hit| Some((hit["namespace"].as_str()?.to_owned(), hit["name"].as_str()?.to_owned())))
            .filter(|(namespace, name)| !namespace.is_empty() && !name.is_empty())
            .take(16)
            .collect();
        let details =
            futures::future::join_all(identities.iter().map(|(namespace, name)| detail(&client, namespace, name)))
                .await;
        let completed: Vec<_> = details.into_iter().filter_map(Result::ok).collect();
        if !identities.is_empty() && completed.is_empty() {
            return Err("Open VSX theme details are unavailable right now.".to_owned());
        }
        Ok(completed.into_iter().flatten().take(8).collect())
    };
    crate::runtime().spawn(request).await.map_err(|error| error.to_string())?
}

/// `normalizePackagePath`: a path inside `extension/`, relative to the file `relative_to`.
fn normalize_path(path: &str, relative_to: &str) -> Result<String, String> {
    let bytes = path.as_bytes();
    if path.len() > MAX_PACKAGE_PATH_LENGTH
        || path.contains('\0')
        || path.starts_with('/')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
    {
        return Err("Theme path is not a safe relative package path.".into());
    }
    let mut segments: Vec<&str> = relative_to.split('/').collect();
    segments.pop();
    let normalized = path.replace('\\', "/");
    for segment in normalized.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.len() <= 1 {
                    return Err("Theme path escapes the extension package.".into());
                }
                segments.pop();
            }
            segment => segments.push(segment),
        }
    }
    if segments.first() != Some(&"extension") {
        segments.insert(0, "extension");
    }
    Ok(segments.join("/"))
}

struct ZipEntry {
    method: u16,
    compressed: usize,
    uncompressed: usize,
    local_offset: usize,
}

/// The package's central directory (`inspectZipDirectory` + `inspectZip`), by entry name.
struct Zip<'a> {
    bytes: &'a [u8],
    entries: HashMap<String, ZipEntry>,
}

impl<'a> Zip<'a> {
    fn open(bytes: &'a [u8]) -> Result<Self, String> {
        let invalid = || "That extension package has an invalid ZIP directory.".to_owned();
        let u16_at = |at: usize| bytes.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
        let u32_at = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64);
        let minimum = bytes.len().saturating_sub(65_557);
        let mut end = bytes.len().checked_sub(22).ok_or("That extension package has no ZIP directory.")?;
        while u32_at(end) != Some(0x06054b50) || end + 22 + u16_at(end + 20).unwrap_or(0) != bytes.len() {
            if end <= minimum {
                return Err("That extension package has no ZIP directory.".into());
            }
            end -= 1;
        }
        let size = u32_at(end + 12).ok_or_else(invalid)? as usize;
        let start = u32_at(end + 16).ok_or_else(invalid)? as usize;
        if start + size != end {
            return Err(invalid());
        }
        let (mut entries, mut total, mut offset) = (HashMap::new(), 0u64, start);
        while offset < end {
            if offset + 46 > end || u32_at(offset) != Some(0x02014b50) {
                return Err(invalid());
            }
            if entries.len() >= MAX_ZIP_ENTRIES {
                return Err("That extension package has too many files.".into());
            }
            let (compressed, uncompressed) =
                (u32_at(offset + 20).ok_or_else(invalid)?, u32_at(offset + 24).ok_or_else(invalid)?);
            if compressed == 0xffffffff || uncompressed == 0xffffffff {
                return Err("That extension package has unsupported ZIP64 metadata.".into());
            }
            total += uncompressed;
            if total > MAX_UNCOMPRESSED_BYTES {
                return Err("That extension package expands beyond the safe import limit.".into());
            }
            if uncompressed > 0 && (compressed == 0 || uncompressed / compressed > MAX_COMPRESSION_RATIO) {
                return Err("That extension package has an unsafe compression ratio.".into());
            }
            let name_length = u16_at(offset + 28).ok_or_else(invalid)?;
            let extra = u16_at(offset + 30).ok_or_else(invalid)?;
            let comment = u16_at(offset + 32).ok_or_else(invalid)?;
            let name = bytes.get(offset + 46..offset + 46 + name_length).ok_or_else(invalid)?;
            let name = String::from_utf8_lossy(name).into_owned();
            normalize_path(&name, "extension/")?;
            entries.insert(
                name,
                ZipEntry {
                    method: u16_at(offset + 10).ok_or_else(invalid)? as u16,
                    compressed: compressed as usize,
                    uncompressed: uncompressed as usize,
                    local_offset: u32_at(offset + 42).ok_or_else(invalid)? as usize,
                },
            );
            offset += 46 + name_length + extra + comment;
        }
        if offset != end {
            return Err(invalid());
        }
        Ok(Self { bytes, entries })
    }

    /// `readZipText`: one entry, at most `MAX_THEME_BYTES` once inflated.
    fn text(&self, path: &str, description: &str) -> Result<String, String> {
        let entry =
            self.entries.get(path).ok_or_else(|| format!("{description} is missing from the extension package."))?;
        let too_large = || format!("{description} is too large.");
        if entry.uncompressed > MAX_THEME_BYTES {
            return Err(too_large());
        }
        let unreadable = || format!("{description} could not be read.");
        let header = entry.local_offset;
        let field =
            |at: usize| self.bytes.get(header + at..header + at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
        let data = header + 30 + field(26).ok_or_else(unreadable)? + field(28).ok_or_else(unreadable)?;
        let raw = self.bytes.get(data..data + entry.compressed).ok_or_else(unreadable)?;
        let mut out = Vec::new();
        let limit = MAX_THEME_BYTES as u64 + 1;
        match entry.method {
            0 => out.extend_from_slice(raw),
            8 => {
                flate2::read::DeflateDecoder::new(raw).take(limit).read_to_end(&mut out).map_err(|error| {
                    log::warn!("open vsx: inflating {path}: {error}");
                    unreadable()
                })?;
            }
            _ => return Err(unreadable()),
        }
        if out.len() > MAX_THEME_BYTES {
            return Err(too_large());
        }
        String::from_utf8(out).map_err(|_| unreadable())
    }
}

/// `sanitizeThemeObject`: only the workbench colors the import reads, and the include.
fn sanitize(value: &Value) -> Value {
    let colors: Map<String, Value> = value["colors"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, color)| {
            USED_WORKBENCH_COLORS.contains(&key.as_str())
                && color.as_str().is_some_and(|c| c.len() <= MAX_COLOR_VALUE_LENGTH)
        })
        .map(|(key, color)| (key.clone(), color.clone()))
        .collect();
    let mut object = Map::new();
    if let Some(include) = value["include"].as_str() {
        object.insert("include".into(), include.into());
    }
    object.insert("colors".into(), Value::Object(colors));
    Value::Object(object)
}

/// `loadThemeObject`: a theme file merged over the files it includes.
fn load_theme(
    zip: &Zip,
    path: &str,
    cache: &mut HashMap<String, Value>,
    budget: &mut usize,
    ancestors: &mut Vec<String>,
) -> Result<Value, String> {
    if ancestors.len() >= MAX_INCLUDE_DEPTH {
        return Err("Theme includes are nested too deeply.".into());
    }
    if ancestors.iter().any(|ancestor| ancestor == path) {
        return Err("Theme includes contain a cycle.".into());
    }
    if let Some(cached) = cache.get(path) {
        return Ok(cached.clone());
    }
    *budget += 1;
    if *budget > MAX_THEMES_PER_EXTENSION * MAX_INCLUDE_DEPTH {
        return Err("That extension references too many theme files.".into());
    }
    let value = sanitize(&jsonc_object(zip.text(path, path)?.as_bytes(), path)?);
    let Some(include) = value["include"].as_str() else {
        cache.insert(path.to_owned(), value.clone());
        return Ok(value);
    };
    let include = normalize_path(include, path)?;
    ancestors.push(path.to_owned());
    let base = load_theme(zip, &include, cache, budget, ancestors)?;
    ancestors.pop();
    let mut colors = base["colors"].as_object().cloned().unwrap_or_default();
    colors.extend(value["colors"].as_object().cloned().unwrap_or_default());
    let mut resolved = base.as_object().cloned().unwrap_or_default();
    resolved.extend(value.as_object().cloned().unwrap_or_default());
    resolved.insert("colors".into(), Value::Object(colors));
    let resolved = Value::Object(resolved);
    cache.insert(path.to_owned(), resolved.clone());
    Ok(resolved)
}

fn contribution_type(ui_theme: &Value) -> Option<&'static str> {
    match ui_theme.as_str()? {
        "vs" => Some("light"),
        "vs-dark" => Some("dark"),
        "hc-black" => Some("hc-black"),
        "hc-light" => Some("hc-light"),
        _ => None,
    }
}

/// The themes inside a verified package (the second half of `importOpenVsxThemeExtension`).
fn themes_from_package(extension: &Extension, package: &[u8]) -> Result<Vec<ThemeDefinition>, String> {
    let zip = Zip::open(package)?;
    let manifest =
        jsonc_object(zip.text("extension/package.json", "Extension manifest")?.as_bytes(), "Extension manifest")?;
    let publisher = manifest["publisher"].as_str().unwrap_or_default();
    let name = manifest["name"].as_str().unwrap_or_default();
    if !publisher.eq_ignore_ascii_case(&extension.publisher)
        || !format!("{publisher}.{name}").eq_ignore_ascii_case(&extension.id)
        || manifest["version"].as_str() != Some(extension.version.as_str())
    {
        return Err("That extension package does not match the selected Open VSX theme.".into());
    }
    if !license_matches(&manifest, &extension.license) {
        return Err("That extension package does not match its advertised license.".into());
    }
    let contributions = theme_contributions(&manifest);
    if contributions.is_empty() {
        return Err("That extension does not contain color themes.".into());
    }
    if contributions.len() > MAX_THEMES_PER_EXTENSION {
        return Err("That extension contains too many color themes to import safely.".into());
    }
    let (mut cache, mut budget) = (HashMap::new(), 0);
    let mut parsed = Vec::new();
    for contribution in contributions {
        let path = contribution.get("path").and_then(Value::as_str).ok_or("theme path is missing")?;
        let theme = (|| {
            let path = normalize_path(path, "extension/")?;
            let mut value = load_theme(&zip, &path, &mut cache, &mut budget, &mut Vec::new())?;
            let label = contribution
                .get("label")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .unwrap_or(&extension.name);
            value["displayName"] = label.into();
            if let Some(kind) = contribution.get("uiTheme").and_then(contribution_type) {
                value["type"] = kind.into();
            }
            if !is_vscode_theme(&value) {
                return Err("not a VS Code color theme".to_owned());
            }
            Ok((parse_vscode_theme(&value)?, path))
        })();
        parsed.push(theme.map_err(|error| {
            log::warn!("open vsx: {} theme {path}: {error}", extension.id);
            "One or more color themes in that extension could not be imported safely."
        })?);
    }
    let extension_id = extension.id.to_lowercase();
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let counts = parsed.iter().fold(HashMap::<&str, usize>::new(), |mut counts, (_, path)| {
        *counts.entry(path.as_str()).or_default() += 1;
        counts
    });
    let identities: Vec<String> = parsed
        .iter()
        .map(|(_, path)| {
            if counts[path.as_str()] == 1 {
                return path.clone();
            }
            let occurrence = seen.entry(path.as_str()).or_default();
            *occurrence += 1;
            if *occurrence == 1 { path.clone() } else { format!("{path}\0{}", *occurrence - 1) }
        })
        .collect();
    let entries =
        parsed.iter().map(|(theme, path)| (theme.clone(), path.rsplit('/').next().map(str::to_owned))).collect();
    let resolved: Vec<ThemeDefinition> = resolve_label_collisions(entries)
        .into_iter()
        .zip(&identities)
        .map(|(theme, identity)| ThemeDefinition { id: theme_id(&extension_id, identity), ..theme })
        .collect();
    let paired_id = |light: &ThemeDefinition, dark: &ThemeDefinition| {
        let mut ids = [light.id.clone(), dark.id.clone()];
        ids.sort();
        theme_id(&extension_id, &ids.join(":"))
    };
    let paired = pair_themes(resolved, Some(&paired_id));
    let collection =
        Collection { id: extension.collection_id.clone(), label: extension.name.chars().take(48).collect() };
    let themes = resolve_label_collisions(paired.into_iter().map(|theme| (theme, None)).collect());
    Ok(themes.into_iter().map(|theme| ThemeDefinition { collection: Some(collection.clone()), ..theme }).collect())
}

/// `importOpenVsxThemeExtension`: downloads, verifies and reads an extension's themes.
pub async fn import(extension: Extension) -> Result<Vec<ThemeDefinition>, String> {
    let request = async move {
        let client = reqwest::Client::builder()
            .connect_timeout(IMPORT_CONNECT_TIMEOUT)
            .read_timeout(IMPORT_READ_TIMEOUT)
            .build()
            .map_err(|error| error.to_string())?;
        let manifest = get_capped(&client, &extension.manifest_url, MAX_MANIFEST_BYTES).await.map_err(|error| {
            error.message(
                "That Open VSX extension manifest is too large.",
                "That Open VSX extension has no readable manifest.",
            )
        })?;
        let advertised = theme_contributions(&jsonc_object(&manifest, "Extension manifest")?).len();
        if advertised == 0 {
            return Err("That extension does not contain color themes.".to_owned());
        }
        if advertised > MAX_THEMES_PER_EXTENSION {
            return Err("That extension contains too many color themes to import safely.".to_owned());
        }
        let package = get_capped(&client, &extension.vsix_url, MAX_VSIX_BYTES).await.map_err(|error| {
            error.message(
                "That theme extension is too large to import safely.",
                "That Open VSX theme could not be downloaded.",
            )
        })?;
        let checksum = get_capped(&client, &extension.sha256_url, 256).await.map_err(|error| {
            error
                .message("That Open VSX checksum response is invalid.", "That Open VSX theme has no readable checksum.")
        })?;
        let expected = String::from_utf8_lossy(&checksum).split_whitespace().next().unwrap_or_default().to_owned();
        if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("That Open VSX theme has an invalid checksum.".to_owned());
        }
        let actual: String = Sha256::digest(&package).iter().map(|byte| format!("{byte:02x}")).collect();
        if !actual.eq_ignore_ascii_case(&expected) {
            return Err("That Open VSX theme failed its integrity check.".to_owned());
        }
        themes_from_package(&extension, &package)
    };
    crate::runtime().spawn(request).await.map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use serde_json::json;

    use super::*;

    /// A ZIP with deflated entries, as `vsce` packs them.
    fn zip(files: &[(&str, &str)]) -> Vec<u8> {
        let (mut out, mut directory) = (Vec::new(), Vec::new());
        for (name, text) in files {
            let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(text.as_bytes()).unwrap();
            let data = encoder.finish().unwrap();
            let offset = out.len() as u32;
            let sizes = [(data.len() as u32).to_le_bytes(), (text.len() as u32).to_le_bytes()].concat();
            out.extend([0x50, 0x4b, 3, 4, 20, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend(&sizes);
            out.extend((name.len() as u16).to_le_bytes());
            out.extend([0, 0]);
            out.extend(name.as_bytes());
            out.extend(&data);
            directory.extend([0x50, 0x4b, 1, 2, 20, 0, 20, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            directory.extend(&sizes);
            directory.extend((name.len() as u16).to_le_bytes());
            directory.extend([0; 12]);
            directory.extend(offset.to_le_bytes());
            directory.extend(name.as_bytes());
        }
        let start = out.len() as u32;
        out.extend(&directory);
        out.extend([0x50, 0x4b, 5, 6, 0, 0, 0, 0]);
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((directory.len() as u32).to_le_bytes());
        out.extend(start.to_le_bytes());
        out.extend([0, 0]);
        out
    }

    fn extension() -> Extension {
        let detail = json!({
            "namespace": "acme", "name": "solar", "displayName": "Solar Themes", "version": "1.0.0", "license": "MIT",
            "files": {
                "manifest": "https://open-vsx.org/m", "sha256": "https://open-vsx.org/s", "download": "https://open-vsx.org/d",
                "icon": "https://evil.example/icon.png",
            },
            "repository": "https://github.com/acme/solar",
        });
        extension_from_detail(&detail).unwrap().unwrap()
    }

    #[test]
    fn details_keep_trusted_urls_and_supported_licenses() {
        let extension = extension();
        assert_eq!((extension.id.as_str(), extension.collection_id.as_str()), ("acme.solar", "open-vsx:acme.solar"));
        assert_eq!(extension.icon_url, None, "only open-vsx.org files load");
        assert_eq!(extension.source_url.as_deref(), Some("https://github.com/acme/solar"));
        let gpl = json!({"namespace": "a", "name": "b", "version": "1", "license": "GPL-3.0",
            "files": {"manifest": "https://open-vsx.org/m", "sha256": "https://open-vsx.org/s", "download": "https://open-vsx.org/d"}});
        assert_eq!(extension_from_detail(&gpl), Ok(None));
        assert!(extension_from_detail(&json!({"files": {}})).is_err());
    }

    #[test]
    fn package_paths_stay_inside_the_extension() {
        assert_eq!(normalize_path("./themes/a.json", "extension/").unwrap(), "extension/themes/a.json");
        assert_eq!(normalize_path("../base.json", "extension/themes/a.json").unwrap(), "extension/base.json");
        assert!(normalize_path("../../x", "extension/").is_err());
        assert!(normalize_path("C:/x", "extension/").is_err());
        assert!(normalize_path("/etc", "extension/").is_err());
    }

    #[test]
    fn a_package_yields_paired_themes_in_one_collection() {
        let manifest = json!({
            "publisher": "acme", "name": "solar", "version": "1.0.0", "license": "MIT",
            "contributes": {"themes": [
                {"label": "Solar Light", "uiTheme": "vs", "path": "./themes/light.json"},
                {"label": "Solar Dark", "uiTheme": "vs-dark", "path": "./themes/dark.json"},
            ]},
        });
        let light = r##"{ "include": "./base.json", "colors": { "editor.background": "#fdf6e3", } }"##;
        let base = r##"{ /* shared */ "colors": { "focusBorder": "#268bd2", "editor.foreground": "#333333" } }"##;
        let dark = r##"{ "colors": { "editor.background": "#002b36", "editor.foreground": "#eeeeee" } }"##;
        let package = zip(&[
            ("extension/package.json", &manifest.to_string()),
            ("extension/themes/light.json", light),
            ("extension/themes/base.json", base),
            ("extension/themes/dark.json", dark),
        ]);
        let themes = themes_from_package(&extension(), &package).unwrap();
        assert_eq!(themes.len(), 1);
        let theme = &themes[0];
        assert_eq!((theme.label.as_str(), theme.modes()), ("Solar", vec![false, true]));
        assert!(theme.id.starts_with("ovx-theme-"));
        assert_eq!(theme.collection.as_ref().map(|c| c.id.as_str()), Some("open-vsx:acme.solar"));
        assert_eq!(
            crate::theme_color::to_hex(theme.roles.get("accent")).as_deref(),
            Some("#268bd2"),
            "the include applies"
        );

        let mismatched = zip(&[("extension/package.json", &json!({"publisher": "other"}).to_string())]);
        assert!(themes_from_package(&extension(), &mismatched).unwrap_err().contains("does not match"));
        assert!(Zip::open(b"not a zip").is_err());
    }
}
