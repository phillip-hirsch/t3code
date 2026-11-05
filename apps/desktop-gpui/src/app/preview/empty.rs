//! A blank browser tab (`PreviewEmptyState`: "Recently used" and live "Local servers") and
//! a page that failed to load (`PreviewUnreachable`).

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, px,
};
use serde_json::{Value, json};

use super::super::Workspace;
use super::super::right_panel::Surface;
use crate::preview::NavStatus;
use crate::theme::Tokens;

/// `PREVIEW_ERROR_CODE_MESSAGES`.
fn friendly_error(description: &str) -> &str {
    match description {
        "ERR_NAME_NOT_RESOLVED" | "ERR_NAME_RESOLUTION_FAILED" => "DNS address could not be found",
        "ERR_CONNECTION_REFUSED" => "Connection refused",
        "ERR_CONNECTION_RESET" => "Connection was reset",
        "ERR_CONNECTION_CLOSED" => "Connection was closed",
        "ERR_CONNECTION_TIMED_OUT" | "ERR_TIMED_OUT" => "Connection timed out",
        "ERR_INTERNET_DISCONNECTED" => "No internet connection",
        "ERR_CERT_AUTHORITY_INVALID" => "Certificate authority is not trusted",
        "ERR_CERT_COMMON_NAME_INVALID" => "Certificate hostname mismatch",
        "ERR_CERT_DATE_INVALID" => "Certificate is expired or not yet valid",
        "ERR_TOO_MANY_REDIRECTS" => "Too many redirects",
        "" => "Network error",
        other => other,
    }
}

/// A `DiscoveredLocalServer` as the card shows it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct LocalServer {
    host: String,
    port: u64,
    url: String,
    process_name: Option<String>,
    configured: bool,
}

/// `mergeServers`: configured servers first, then by port.
fn local_servers(list: &Value, configured: &[String]) -> Vec<LocalServer> {
    let key = |host: &str, port: u64| {
        let host = host.to_lowercase();
        let loopback = matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "[::1]" | "0.0.0.0");
        format!("{}:{port}", if loopback { "loopback" } else { host.as_str() })
    };
    let configured: Vec<String> = configured
        .iter()
        .filter_map(|raw| {
            let url = url::Url::parse(raw).ok()?;
            Some(key(url.host_str()?, u64::from(url.port_or_known_default()?)))
        })
        .collect();
    let mut servers: Vec<LocalServer> = list["servers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|server| {
            let (host, port) = (server["host"].as_str()?.to_owned(), server["port"].as_u64()?);
            Some(LocalServer {
                configured: configured.contains(&key(&host, port)),
                url: server["url"].as_str()?.to_owned(),
                process_name: server["processName"].as_str().map(str::to_owned),
                host,
                port,
            })
        })
        .collect();
    servers.sort_by_key(|s| (!s.configured, s.port));
    servers
}

#[derive(Default)]
pub(super) struct Discovery {
    servers: Vec<LocalServer>,
    /// The configured URLs subscribed with, and the stream.
    subscribed: Option<(Vec<String>, Task<()>)>,
}

impl Workspace {
    fn history_project(&self, thread_id: &str) -> Option<String> {
        self.shell.threads.iter().find(|t| t.id == thread_id).map(|t| t.project_id.clone())
    }

    /// `recordVisitForThread`.
    pub(super) fn record_visit(&mut self, thread_id: &str, url: &str) {
        let Some(project) = self.history_project(thread_id) else { return };
        if self.preview.history.record_visit(&project, url, crate::timestamp::now_ms()) {
            self.preview.history.save_in_background();
        }
    }

    /// A settled page names its history entry (`recordPreviewTitles`).
    pub(super) fn record_title(&mut self, thread_id: &str, status: &NavStatus) {
        let (NavStatus::Success { url, title }, Some(project)) = (status, self.history_project(thread_id)) else {
            return;
        };
        if self.preview.history.set_title(&project, url, title) {
            self.preview.history.save_in_background();
        }
    }

    /// The panel's blank tab, if one shows: its thread and the project's script preview URLs.
    fn blank_browser(&self) -> Option<(String, Vec<String>)> {
        let Some(Surface::Browser(tab)) = self.active_surface() else { return None };
        let thread_id = self.preview_thread_id()?;
        let session = self.preview.threads.get(thread_id).and_then(|p| p.get(tab));
        let blank = !self.preview.tabs.contains_key(tab.as_str())
            && session.is_none_or(|s| !s.server_hosted() && matches!(s.nav_status, NavStatus::Idle));
        if !blank {
            return None;
        }
        let project = self.target_project();
        // `getConfiguredPreviewUrls`, bounded like `boundConfiguredLocalServerUrls`.
        let urls = project
            .map(|p| {
                p.scripts.iter().filter_map(|s| s.preview_url.clone()).filter(|u| u.len() <= 2048).take(20).collect()
            })
            .unwrap_or_default();
        Some((thread_id.to_owned(), urls))
    }

    /// `subscribeDiscoveredLocalServers` while a blank tab shows, so the server only probes then.
    pub(in crate::app) fn sync_discovered_servers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.blank_browser().map(|(_, urls)| urls);
        let current = self.preview.discovery.subscribed.as_ref().map(|(urls, _)| urls.clone());
        if wanted == current {
            return;
        }
        let (Some(urls), Some(client)) = (wanted, self.client()) else {
            self.preview.discovery = Discovery::default();
            return;
        };
        let input = if urls.is_empty() { json!({}) } else { json!({"configuredUrls": urls}) };
        let stream = client.stream("subscribeDiscoveredLocalServers", input);
        let configured = urls.clone();
        let task = Self::pump(
            stream,
            window,
            cx,
            move |this, value| this.preview.discovery.servers = local_servers(value, &configured),
            |_, _, cx| cx.notify(),
        );
        self.preview.discovery.subscribed = Some((urls, task));
    }

    /// Opens a card's URL in the blank tab (`handleOpenServerUrl`).
    fn open_in_blank_tab(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        self.submit_address(&url, window, cx);
    }

    /// `PreviewEmptyState`: recents and live servers, else the "No preview yet" note.
    pub(super) fn render_preview_empty_state(
        &self,
        thread_id: &str,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let project = self.history_project(thread_id);
        let now = crate::timestamp::now_ms();
        let recents: Vec<crate::browser_history::Entry> = project
            .as_deref()
            .map(|p| {
                self.preview.history.recent(p, 8).iter().filter(|e| url::Url::parse(&e.url).is_ok()).cloned().collect()
            })
            .unwrap_or_default();
        let servers = &self.preview.discovery.servers;
        if recents.is_empty() && servers.is_empty() {
            return no_preview(tokens).into_any_element();
        }
        let list = |rows: Vec<AnyElement>| {
            v_flex()
                .overflow_hidden()
                .rounded(px(12.))
                .border_1()
                .border_color(tokens.border.opacity(0.7))
                .bg(tokens.background)
                .children(rows.into_iter().enumerate().map(|(i, row)| {
                    div().when(i > 0, |el| el.border_t_1().border_color(tokens.border.opacity(0.6))).child(row)
                }))
        };
        let heading = |glyph: &'static str, text: &'static str| {
            h_flex()
                .gap_2()
                .text_sm()
                .text_color(tokens.muted_foreground)
                .child(super::super::titlebar::icon(glyph).with_size(px(16.)))
                .child(div().font_weight(FontWeight::MEDIUM).child(text))
        };
        let favicon = || {
            div()
                .size(px(32.))
                .flex_none()
                .rounded(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .bg(tokens.muted)
                .child(Icon::new(IconName::Globe).size(px(16.)).text_color(tokens.muted_foreground))
        };
        let text_block = |title: SharedString, detail: SharedString| {
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(tokens.foreground)
                        .child(title),
                )
                .child(div().truncate().text_xs().text_color(tokens.muted_foreground).child(detail))
        };
        let recent_rows: Vec<AnyElement> = recents
            .iter()
            .map(|entry| {
                let parsed = url::Url::parse(&entry.url).ok();
                let label = parsed.as_ref().map_or_else(
                    || entry.url.clone(),
                    |u| {
                        let path = if u.path() == "/" { "" } else { u.path() };
                        let query = u.query().map(|q| format!("?{q}")).unwrap_or_default();
                        let hash = u.fragment().map(|f| format!("#{f}")).unwrap_or_default();
                        format!(
                            "{}{path}{query}{hash}",
                            u.host_str().map_or(String::new(), |h| match u.port() {
                                Some(port) => format!("{h}:{port}"),
                                None => h.to_owned(),
                            })
                        )
                    },
                );
                let ago = relative_time(now - entry.last_visited_at);
                let (title, detail) = match &entry.title {
                    Some(title) => (title.clone(), format!("{label} · {ago}")),
                    None => (label.clone(), ago),
                };
                let group: SharedString = format!("preview-recent-{}", entry.url).into();
                let (open, remove) = (entry.url.clone(), entry.url.clone());
                let project = project.clone();
                h_flex()
                    .id(ElementId::Name(group.clone()))
                    .group(group.clone())
                    .relative()
                    .w_full()
                    .gap_3()
                    .px_3()
                    .py_3()
                    .pr(px(40.))
                    .cursor_pointer()
                    .hover(|s| s.bg(tokens.accent.opacity(0.4)))
                    .on_click(cx.listener(move |this, _, window, cx| this.open_in_blank_tab(open.clone(), window, cx)))
                    .child(favicon())
                    .child(text_block(title.into(), detail.into()))
                    .child(
                        div()
                            .id(ElementId::Name(format!("{group}-remove").into()))
                            .absolute()
                            .right(px(12.))
                            .p_1()
                            .rounded(px(4.))
                            .invisible()
                            .group_hover(group, |s| s.visible())
                            .text_color(tokens.muted_foreground)
                            .hover(|s| s.bg(tokens.accent).text_color(tokens.foreground))
                            .tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(format!("Remove {label} from history"))
                                    .build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if let Some(project) = &project {
                                    this.preview.history.remove(project, &remove);
                                    this.preview.history.save_in_background();
                                    cx.notify();
                                }
                            }))
                            .child(Icon::new(IconName::X).size(px(14.))),
                    )
                    .into_any_element()
            })
            .collect();
        let server_rows: Vec<AnyElement> = servers
            .iter()
            .map(|server| {
                let url = server.url.clone();
                h_flex()
                    .id(ElementId::Name(format!("preview-server-{}:{}", server.host, server.port).into()))
                    .w_full()
                    .gap_3()
                    .px_3()
                    .py_3()
                    .cursor_pointer()
                    .hover(|s| s.bg(tokens.accent.opacity(0.4)))
                    .on_click(cx.listener(move |this, _, window, cx| this.open_in_blank_tab(url.clone(), window, cx)))
                    .child(favicon())
                    .child(text_block(
                        server.process_name.clone().unwrap_or_else(|| "Listening".into()).into(),
                        format!("{}:{}", server.host, server.port).into(),
                    ))
                    .into_any_element()
            })
            .collect();
        div()
            .id("preview-empty-state")
            .size_full()
            .overflow_y_scroll()
            .px_5()
            .py_8()
            .child(
                v_flex()
                    .mx_auto()
                    .w_full()
                    .max_w(px(576.))
                    .gap_6()
                    .when(!recent_rows.is_empty(), |el| {
                        el.child(
                            v_flex()
                                .gap_3()
                                .child(heading("icons/history.svg", "Recently used"))
                                .child(list(recent_rows)),
                        )
                    })
                    .when(!server_rows.is_empty(), |el| {
                        el.child(
                            v_flex()
                                .gap_3()
                                .child(heading("icons/radio-tower.svg", "Local servers"))
                                .child(list(server_rows))
                                .child(
                                    div()
                                        .px_1()
                                        .text_xs()
                                        .text_color(tokens.muted_foreground)
                                        .child("Select a live local server to open it in this browser tab."),
                                ),
                        )
                    }),
            )
            .into_any_element()
    }

    /// `PreviewUnreachable` over a page that failed to load.
    pub(super) fn render_unreachable(
        &self,
        tab_id: &str,
        status: &NavStatus,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let NavStatus::LoadFailed { url, code, description, .. } = status else { return None };
        let host = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(|h| u.port().map_or_else(|| h.to_owned(), |p| format!("{h}:{p}"))))
            .unwrap_or_else(|| url.clone());
        let label = if description.is_empty() {
            format!("ERR_{}", if *code == 0 { "FAILED".into() } else { code.abs().to_string() })
        } else {
            description.clone()
        };
        let details = self.preview.unreachable_details.contains(tab_id);
        let session =
            self.preview_thread_id().and_then(|t| self.preview.threads.get(t)).and_then(|p| p.get(tab_id)).cloned();
        let move_label = session.as_ref().filter(|s| !s.server_hosted()).and_then(|s| self.move_tab_label(s));
        let (toggle_tab, reload_tab) = (tab_id.to_owned(), tab_id.to_owned());
        Some(
            div()
                .id("preview-unreachable")
                .size_full()
                .overflow_y_scroll()
                .bg(tokens.background)
                .child(
                    v_flex()
                        .mx_auto()
                        .size_full()
                        .max_w(px(576.))
                        .px_8()
                        .py_16()
                        .child(
                            super::super::titlebar::icon("icons/file-x.svg")
                                .with_size(px(48.))
                                .mb_6()
                                .text_color(tokens.muted_foreground.opacity(0.7)),
                        )
                        .child(
                            div()
                                .mb_3()
                                .text_2xl()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(tokens.foreground)
                                .child("This site can\u{2019}t be reached"),
                        )
                        .child(
                            h_flex()
                                .flex_wrap()
                                .text_sm()
                                .text_color(tokens.muted_foreground)
                                .child(
                                    div().font_weight(FontWeight::SEMIBOLD).text_color(tokens.foreground).child(host),
                                )
                                .child(format!(": {}.", friendly_error(description))),
                        )
                        .when(details, |el| {
                            let item = |text: &'static str| {
                                h_flex().items_start().gap_2().child("•").child(div().flex_1().min_w_0().child(text))
                            };
                            el.child(
                                v_flex()
                                    .mt_6()
                                    .p_4()
                                    .gap_1()
                                    .rounded(px(8.))
                                    .border_1()
                                    .border_color(tokens.border)
                                    .bg(tokens.muted.opacity(0.4))
                                    .text_sm()
                                    .text_color(tokens.muted_foreground)
                                    .child(
                                        div()
                                            .mb_1()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(tokens.foreground)
                                            .child("Try:"),
                                    )
                                    .child(item("Checking your connection"))
                                    .child(item("Confirming the dev server is running"))
                                    .when(move_label.is_some(), |el| {
                                        el.child(item(
                                            "Opening it in the environment\u{2019}s browser, which reaches its network",
                                        ))
                                    })
                                    .child(item("Checking the proxy and the firewall")),
                            )
                        })
                        .child(
                            div()
                                .mt_8()
                                .text_xs()
                                .text_color(tokens.muted_foreground.opacity(0.7))
                                .child(label.to_uppercase()),
                        )
                        .child(
                            h_flex()
                                .mt_auto()
                                .pt_8()
                                .gap_2()
                                .child(
                                    Button::new("preview-unreachable-details")
                                        .outline()
                                        .small()
                                        .label(if details { "Hide details" } else { "Details" })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if !this.preview.unreachable_details.remove(&toggle_tab) {
                                                this.preview.unreachable_details.insert(toggle_tab.clone());
                                            }
                                            cx.notify();
                                        })),
                                )
                                .child(div().flex_1())
                                .when_some(move_label.zip(session), |el, (label, session)| {
                                    el.child(
                                        Button::new("preview-unreachable-move")
                                            .outline()
                                            .small()
                                            .label(label)
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.move_preview_tab(session.clone(), window, cx)
                                            })),
                                    )
                                })
                                .child(
                                    Button::new("preview-unreachable-reload")
                                        .primary()
                                        .small()
                                        .label("Reload")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if let Some(tab) = this.preview.tabs.get(&reload_tab) {
                                                let _ = tab.view.read(cx).raw().reload();
                                            }
                                        })),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}

/// `formatRelativeElapsed`.
fn relative_time(elapsed_ms: i64) -> String {
    match elapsed_ms.max(0) / 60_000 {
        0 => "just now".into(),
        minutes @ 1..60 => format!("{minutes}m ago"),
        minutes @ 60..1440 => format!("{}h ago", minutes / 60),
        minutes => format!("{}d ago", minutes / 1440),
    }
}

/// "No preview yet".
fn no_preview(tokens: &Tokens) -> impl IntoElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .p_6()
        .text_center()
        .child(
            div()
                .size(px(36.))
                .mb_2()
                .rounded(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .bg(tokens.muted)
                .child(Icon::new(IconName::Globe).size(px(18.)).text_color(tokens.muted_foreground)),
        )
        .child(div().text_base().font_weight(FontWeight::MEDIUM).text_color(tokens.foreground).child("No preview yet"))
        .child(div().max_w(px(320.)).text_sm().text_color(tokens.muted_foreground).child(
            "Type a URL above, or run a dev script. Local servers this thread starts will show up here automatically.",
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_servers_come_first_then_ports_ascend() {
        let list = json!({"servers": [
            {"host": "127.0.0.1", "port": 5173, "url": "http://127.0.0.1:5173/", "processName": "node"},
            {"host": "localhost", "port": 8080, "url": "http://localhost:8080/", "processName": null},
            {"host": "127.0.0.1", "port": 3000, "url": "http://127.0.0.1:3000/", "processName": "bun"},
        ]});
        let servers = local_servers(&list, &["http://localhost:8080/app".into()]);
        let ports: Vec<u64> = servers.iter().map(|s| s.port).collect();
        assert_eq!(ports, [8080, 3000, 5173]);
        assert_eq!(friendly_error("ERR_CONNECTION_REFUSED"), "Connection refused");
        assert_eq!(friendly_error(""), "Network error");
    }
}
