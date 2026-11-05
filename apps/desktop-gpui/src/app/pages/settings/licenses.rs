//! Settings › Open source licenses (`OpenSourceLicenses.tsx`, `@t3tools/shared/thirdPartyLicenses`).
//!
//! The web loads one `third-party-licenses.json` built beside its bundle. This client ships two
//! things, so it lists two sources: the Rust crates and assets compiled into this binary
//! (`build.rs` writes them at build time) and the server's npm notices, read from the
//! `third-party-licenses.json` the connected server publishes beside its web client.
//! The list is virtualized: ~600 rows would otherwise rebuild on every window repaint.

use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, ElementId, Entity, Focusable as _, FontWeight, InteractiveElement as _,
    IntoElement, ListAlignment, ListState, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, Window, div, list, px,
};
use serde_json::Value;

use super::connections::ui::WebButton;
use crate::app::Workspace;
use crate::app::pages::settings::widgets::SettingsView;
use crate::theme::{self, Tokens};

/// One notice row (`ThirdPartyLicenseEntry`).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Entry {
    key: SharedString,
    name: SharedString,
    version: Option<SharedString>,
    license: SharedString,
    bundles: Vec<SharedString>,
    source_url: Option<SharedString>,
    notice: SharedString,
}

/// The crates and assets in this binary, from `build.rs`.
fn bundled() -> &'static [Entry] {
    static ENTRIES: OnceLock<Vec<Entry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        let manifest: Value =
            serde_json::from_str(include_str!(concat!(env!("OUT_DIR"), "/rust-licenses.json"))).unwrap_or_default();
        let texts: Vec<SharedString> = manifest["texts"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|text| SharedString::from(text.as_str().unwrap_or_default().to_owned()))
            .collect();
        manifest["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let name = entry["name"].as_str()?;
                let version = entry["version"].as_str();
                let bundle = entry["bundle"].as_str().unwrap_or("desktop");
                Some(Entry {
                    key: format!("{bundle}:{name}@{}", version.unwrap_or_default()).into(),
                    name: name.to_owned().into(),
                    version: version.map(|v| v.to_owned().into()),
                    license: entry["license"].as_str().unwrap_or_default().to_owned().into(),
                    bundles: vec![bundle.to_owned().into()],
                    source_url: entry["sourceUrl"].as_str().map(|url| url.to_owned().into()),
                    notice: texts.get(entry["text"].as_u64()? as usize)?.clone(),
                })
            })
            .collect()
    })
}

fn is_http_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

/// `decodeThirdPartyLicenseManifest`, keeping what this client runs: the server's notices and
/// the notification sounds it plays. Their other bundles (web, mobile…) do not ship here.
pub(super) fn decode_server_manifest(value: &Value) -> Result<Vec<Entry>, String> {
    let unsupported = || "The open-source license manifest has an unsupported format.".to_owned();
    if value["schemaVersion"] != 1 {
        return Err(unsupported());
    }
    let entries = value["entries"].as_array().ok_or_else(unsupported)?;
    let mut decoded = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let invalid = || format!("License entry {} has an invalid shape.", index + 1);
        let bundles: Vec<&str> = entry["bundles"]
            .as_array()
            .ok_or_else(invalid)?
            .iter()
            .map(Value::as_str)
            .collect::<Option<_>>()
            .ok_or_else(invalid)?;
        let text = |key: &str| entry[key].as_str().ok_or_else(invalid);
        let (name, license, notice) = (text("name")?, text("license")?, text("noticeText")?);
        let source_url = match &entry["sourceUrl"] {
            Value::Null => None,
            Value::String(url) if is_http_url(url) => Some(url.clone()),
            _ => return Err(invalid()),
        };
        let version = match &entry["version"] {
            Value::Null => None,
            Value::String(version) => Some(version.clone()),
            _ => return Err(invalid()),
        };
        if bundles.is_empty() || !matches!(entry["kind"].as_str(), Some("custom" | "package")) {
            return Err(invalid());
        }
        let ships = if bundles.contains(&"server") {
            "server"
        } else if name.starts_with("Notification Sound") {
            "assets"
        } else {
            continue;
        };
        decoded.push(Entry {
            key: format!("{ships}:{name}@{}", version.as_deref().unwrap_or_default()).into(),
            name: name.to_owned().into(),
            version: version.map(Into::into),
            license: license.to_owned().into(),
            bundles: vec![ships.into()],
            source_url: source_url.map(Into::into),
            notice: notice.to_owned().into(),
        });
    }
    Ok(decoded)
}

/// `filterThirdPartyLicenseEntries`: every term must appear in the name, version, license or bundles.
pub(super) fn filter_entries(entries: &[Entry], query: &str) -> Vec<usize> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            if terms.is_empty() {
                return true;
            }
            let mut searchable =
                format!("{} {} {}", entry.name, entry.version.as_deref().unwrap_or_default(), entry.license);
            for bundle in &entry.bundles {
                searchable.push(' ');
                searchable.push_str(bundle);
            }
            let searchable = searchable.to_lowercase();
            terms.iter().all(|term| searchable.contains(term.as_str()))
        })
        .map(|(index, _)| index)
        .collect()
}

/// `formatLicenseBundles`.
pub(super) fn format_bundles(bundles: &[SharedString]) -> String {
    let label = |bundle: &str| {
        match bundle {
            "android" => "Android",
            "assets" => "Assets",
            "desktop" => "Desktop",
            "device-tools" => "Device tools",
            "ios" => "iOS",
            "mobile" => "Mobile",
            "server" => "Server",
            "web" => "Web",
            other => other,
        }
        .to_owned()
    };
    bundles.iter().map(|bundle| label(bundle)).collect::<Vec<_>>().join(", ")
}

/// Bundled entries plus the server's, by name.
fn merge(server: &[Entry]) -> Vec<Entry> {
    let mut entries: Vec<Entry> = bundled().iter().chain(server).cloned().collect();
    entries.sort_by_key(|entry| (entry.name.to_lowercase(), entry.key.clone()));
    entries
}

enum ServerNotices {
    Loading,
    Ready,
    Failed(SharedString),
}

pub(super) struct LicensesState {
    entries: Rc<Vec<Entry>>,
    /// Indexes into `entries` matching the search.
    visible: Rc<Vec<usize>>,
    server: ServerNotices,
    search: Entity<InputState>,
    search_open: bool,
    open: Option<SharedString>,
    list: ListState,
    _load: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl LicensesState {
    fn new(endpoint: Option<crate::backend::Endpoint>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search licenses"));
        let subscription = cx.subscribe_in(&search, window, |this, input, event: &InputEvent, _, cx| match event {
            InputEvent::Change => this.refilter(cx),
            InputEvent::Blur if input.read(cx).value().is_empty() => {
                this.search_open = false;
                cx.notify();
            }
            _ => {}
        });
        let entries: Vec<Entry> = merge(&[]);
        let mut state = Self {
            visible: Rc::new((0..entries.len()).collect()),
            list: ListState::new(entries.len(), ListAlignment::Top, px(600.)),
            entries: Rc::new(entries),
            server: ServerNotices::Loading,
            search,
            search_open: false,
            open: None,
            _load: None,
            _subscriptions: vec![subscription],
        };
        state.load_server(endpoint, cx);
        state
    }

    /// Fetches the server's manifest; each visit (and Try again) loads it again, like the web.
    fn load_server(&mut self, endpoint: Option<crate::backend::Endpoint>, cx: &mut Context<Self>) {
        self.server = ServerNotices::Loading;
        self._load = Some(cx.spawn(async move |this, cx| {
            let result = fetch_manifest(endpoint).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(server) => {
                        this.entries = Rc::new(merge(&server));
                        this.server = ServerNotices::Ready;
                    }
                    Err(error) => this.server = ServerNotices::Failed(error.into()),
                }
                this.refilter(cx);
            });
        }));
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().to_string();
        self.visible = Rc::new(filter_entries(&self.entries, &query));
        self.list.reset(self.visible.len());
        cx.notify();
    }

    fn toggle(&mut self, key: SharedString, cx: &mut Context<Self>) {
        self.open = if self.open.as_ref() == Some(&key) { None } else { Some(key) };
        self.list.remeasure();
        cx.notify();
    }
}

async fn fetch_manifest(endpoint: Option<crate::backend::Endpoint>) -> Result<Vec<Entry>, String> {
    let endpoint = endpoint.ok_or("The license manifest could not load.")?;
    let request = async move {
        let url = format!("{}/third-party-licenses.json", endpoint.http_base.trim_end_matches('/'));
        let response =
            crate::app::pages::settings::http().get(url).timeout(Duration::from_secs(30)).send().await.map_err(
                |error| {
                    if error.is_timeout() {
                        "The license manifest took too long to load.".to_owned()
                    } else {
                        error.to_string()
                    }
                },
            )?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("The license manifest request failed with status {}.", status.as_u16()));
        }
        let body: Value = response.json().await.map_err(|error| error.to_string())?;
        decode_server_manifest(&body)
    };
    crate::runtime().spawn(request).await.map_err(|error| error.to_string())?
}

/// One notice row (`LicenseNoticeRow`).
fn notice_row(state: &Entity<LicensesState>, entry: &Entry, open: bool, t: &Tokens) -> AnyElement {
    let key = entry.key.clone();
    let toggle_state = state.clone();
    let row = h_flex()
        .id(ElementId::Name(format!("license-{key}").into()))
        .min_h(px(36.))
        .hover(|s| s.bg(t.muted.opacity(0.35)))
        .child(
            h_flex()
                .id(ElementId::Name(format!("license-toggle-{key}").into()))
                .flex_1()
                .min_w_0()
                .min_h(px(36.))
                .gap(px(10.))
                .px(px(16.))
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    let key = key.clone();
                    toggle_state.update(cx, |s, cx| s.toggle(key, cx));
                })
                .child(
                    Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                        .size(px(14.))
                        .text_color(t.muted_foreground),
                )
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(8.))
                        .child(div().truncate().font_weight(FontWeight::MEDIUM).child(entry.name.clone()))
                        .when_some(entry.version.clone(), |el, version| {
                            el.child(
                                div()
                                    .flex_none()
                                    .font_family(t.fonts.mono)
                                    .text_xs()
                                    .text_color(t.muted_foreground)
                                    .child(version),
                            )
                        }),
                )
                .child(
                    div()
                        .flex_none()
                        .max_w(gpui_kit::relative(0.42))
                        .truncate()
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .child(format!("{} \u{b7} {}", entry.license, format_bundles(&entry.bundles))),
                ),
        )
        .when_some(entry.source_url.clone(), |el, url| {
            let (hover, fg) = (t.accent, t.foreground);
            el.child(
                div()
                    .id(ElementId::Name(format!("license-source-{}", entry.key).into()))
                    .mr(px(16.))
                    .size(px(20.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .text_color(t.muted_foreground)
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover).text_color(fg))
                    .child(Icon::new(IconName::ExternalLink).size(px(12.)))
                    .tooltip(super::connections::ui::tooltip("Project source"))
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            )
        });
    v_flex()
        .child(row)
        .when(open, |el| {
            el.child(
                div().px(px(40.)).pt(px(4.)).pb(px(16.)).child(
                    div()
                        .max_w(px(600.))
                        .font_family(t.fonts.mono)
                        .text_xs()
                        .line_height(px(20.))
                        .text_color(t.foreground.opacity(0.8))
                        .child(entry.notice.clone()),
                ),
            )
        })
        .into_any_element()
}

impl Workspace {
    pub(super) fn render_settings_licenses(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let endpoint = self.settings_endpoint().cloned();
        let state =
            window.use_keyed_state("settings-licenses", cx, |window, cx| LicensesState::new(endpoint, window, cx));
        let t = *theme::tokens(cx);
        let s = state.read(cx);
        let (filtered, total) = (s.visible.len(), s.entries.len());
        let count = div().whitespace_nowrap().text_xs().text_color(t.muted_foreground).child(if filtered == total {
            format!("{total} notices")
        } else {
            format!("{filtered} of {total}")
        });
        let action = if s.search_open {
            let close = state.clone();
            h_flex()
                .gap(px(8.))
                .child(count)
                .child(
                    div()
                        .w(px(176.))
                        .on_key_down(move |event, window, cx| {
                            if event.keystroke.key == "escape" {
                                close.update(cx, |s, cx| {
                                    s.search.update(cx, |input, cx| input.set_value("", window, cx));
                                    s.search_open = false;
                                    s.refilter(cx);
                                });
                                cx.stop_propagation();
                            }
                        })
                        .child(Input::new(&s.search).small().prefix(Icon::new(IconName::Search).size(px(12.)))),
                )
                .into_any_element()
        } else {
            let open = state.clone();
            h_flex()
                .gap(px(6.))
                .child(count)
                .child(
                    div()
                        .id("license-search")
                        .size(px(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .text_color(t.muted_foreground)
                        .cursor_pointer()
                        .hover(|s| s.bg(t.accent))
                        .child(Icon::new(IconName::Search).size(px(12.)))
                        .tooltip(super::connections::ui::tooltip("Search licenses"))
                        .on_click(move |_, window, cx| {
                            open.update(cx, |s, cx| {
                                s.search_open = true;
                                let focus = s.search.read(cx).focus_handle(cx);
                                window.focus(&focus, cx);
                                cx.notify();
                            });
                        }),
                )
                .into_any_element()
        };
        let footer = match &s.server {
            ServerNotices::Ready => None,
            ServerNotices::Loading => Some(
                div()
                    .px(px(16.))
                    .py(px(12.))
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .child("Loading server notices\u{2026}")
                    .into_any_element(),
            ),
            ServerNotices::Failed(message) => {
                let (retry, endpoint) = (state.clone(), self.settings_endpoint().cloned());
                Some(
                    v_flex()
                        .items_start()
                        .gap(px(12.))
                        .px(px(16.))
                        .py(px(20.))
                        .child(
                            v_flex()
                                .gap(px(4.))
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child("Server notices are unavailable"),
                                )
                                .child(
                                    div().text_xs().text_color(t.muted_foreground.opacity(0.8)).child(message.clone()),
                                ),
                        )
                        .child(WebButton::new("license-retry", "Try again").on_click(move |_, _, cx| {
                            let endpoint = endpoint.clone();
                            retry.update(cx, |s, cx| s.load_server(endpoint, cx));
                        }))
                        .into_any_element(),
                )
            }
        };
        let body = if filtered == 0 && total > 0 {
            div()
                .px(px(16.))
                .py(px(32.))
                .flex()
                .justify_center()
                .text_sm()
                .text_color(t.muted_foreground)
                .child("No licenses match that search.")
                .into_any_element()
        } else {
            let (entries, visible, open, item_state) =
                (s.entries.clone(), s.visible.clone(), s.open.clone(), state.clone());
            list(s.list.clone(), move |index, _, cx| {
                let t = *theme::tokens(cx);
                let entry = &entries[visible[index]];
                let row = notice_row(&item_state, entry, open.as_ref() == Some(&entry.key), &t);
                div()
                    .when(index > 0, |el| el.border_t_1().border_color(t.border.opacity(0.5)))
                    .child(row)
                    .into_any_element()
            })
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        let card = v_flex()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .rounded(px(12.))
            .border_1()
            .border_color(t.border.opacity(0.6))
            .bg(t.card.opacity(0.4))
            .text_sm()
            .child(body)
            .children(footer.map(|footer| div().border_t_1().border_color(t.border.opacity(0.5)).child(footer)));
        let section = v_flex()
            .flex_1()
            .min_h_0()
            .gap(px(10.))
            .child(
                h_flex()
                    .min_h(px(28.))
                    .px(px(16.))
                    .justify_between()
                    .gap(px(16.))
                    .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child("Third-party notices"))
                    .child(action),
            )
            .child(card);
        // The list scrolls inside the card, so the page itself does not.
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .max_w(px(896.))
            .mx_auto()
            .px(px(24.))
            .pt(px(24.))
            .pb(px(24.))
            .gap(px(32.))
            .child(self.render_scope_sentence(view, cx))
            .child(section)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_build_lists_linked_crates_and_assets() {
        let entries = bundled();
        assert!(entries.iter().any(|e| e.name == "gpui-kit" && !e.notice.is_empty()));
        assert!(entries.iter().any(|e| e.name == "Inter" && e.bundles == ["assets"]));
        assert!(entries.iter().all(|e| !e.notice.is_empty()));
    }

    #[test]
    fn server_manifest_keeps_what_this_client_runs() {
        let entry = |name: &str, bundles: &[&str]| {
            json!({"kind": "package", "name": name, "version": "1.0.0", "license": "MIT",
                   "noticeText": "MIT License", "sourceUrl": null, "bundles": bundles})
        };
        let manifest = json!({"schemaVersion": 1, "entries": [
            entry("effect", &["server", "web"]),
            entry("react", &["web"]),
            entry("Notification Sound 1 by deadrobotmusic", &["assets", "desktop", "web"]),
        ]});
        let decoded = decode_server_manifest(&manifest).unwrap();
        let names: Vec<(&str, String)> =
            decoded.iter().map(|e| (e.name.as_ref(), format_bundles(&e.bundles))).collect();
        assert_eq!(names, [("effect", "Server".into()), ("Notification Sound 1 by deadrobotmusic", "Assets".into())]);
        assert!(decode_server_manifest(&json!({"schemaVersion": 2, "entries": []})).is_err());
        let bad = json!({"schemaVersion": 1, "entries": [{"kind": "package", "name": "x", "bundles": []}]});
        assert_eq!(decode_server_manifest(&bad).unwrap_err(), "License entry 1 has an invalid shape.");
    }

    #[test]
    fn search_needs_every_term() {
        let entry = |name: &str, license: &str, bundle: &str| Entry {
            key: name.to_owned().into(),
            name: name.to_owned().into(),
            version: Some("1.2.3".into()),
            license: license.to_owned().into(),
            bundles: vec![bundle.to_owned().into()],
            source_url: None,
            notice: "".into(),
        };
        let entries = [entry("serde", "MIT OR Apache-2.0", "desktop"), entry("effect", "MIT", "server")];
        assert_eq!(filter_entries(&entries, ""), [0, 1]);
        assert_eq!(filter_entries(&entries, "mit server"), [1]);
        assert_eq!(filter_entries(&entries, "  SERDE  1.2 "), [0]);
        assert!(filter_entries(&entries, "gpl").is_empty());
        assert_eq!(
            format_bundles(&["desktop".into(), "device-tools".into(), "other".into()]),
            "Desktop, Device tools, other"
        );
    }
}
