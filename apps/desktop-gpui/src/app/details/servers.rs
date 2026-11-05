//! The details card's Servers section (`ThreadServersPanel.tsx`): local servers the
//! thread's terminals and agent started, from `subscribeDiscoveredLocalServers`. A click
//! opens one in the browser panel, Ctrl-click in the system browser; hover shows Stop.

use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, WindowExt as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, ClickEvent, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, px,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{CardColors, ROW_HEIGHT, Section, row, section_heading};
use crate::app::{Target, Workspace};
use crate::rpc::StreamEvent;
use crate::theme::Tokens;

/// `SERVER_PAGE_COUNT`.
const PAGE: usize = 4;
const SERVER_ROW_HEIGHT: f32 = 44.;
const HEADING_HEIGHT: f32 = 36.;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(in crate::app) struct LocalServer {
    port: u16,
    url: String,
    #[serde(default)]
    process_name: Option<String>,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    terminal: Option<Value>,
    #[serde(default)]
    agent: Option<Value>,
}

impl LocalServer {
    /// `useThreadDiscoveredPorts`: started by one of the thread's terminals or its agent.
    fn belongs_to(&self, thread_id: &str) -> bool {
        [&self.terminal, &self.agent].into_iter().flatten().any(|owner| owner["threadId"] == thread_id)
    }

    fn key(&self) -> (Option<u32>, u16) {
        (self.pid, self.port)
    }

    /// "Agent · node" or "Terminal · node".
    fn subtitle(&self) -> String {
        let source = if self.terminal.is_none() { "Agent" } else { "Terminal" };
        match &self.process_name {
            Some(name) => format!("{source} · {name}"),
            None => source.to_owned(),
        }
    }
}

/// The discovered servers, subscribed only while the section can show.
#[derive(Default)]
pub(in crate::app) struct ServersWatch {
    servers: Rc<Vec<LocalServer>>,
    task: Option<Task<()>>,
    stopping: Option<(Option<u32>, u16)>,
    visible: Option<usize>,
}

impl Workspace {
    fn servers_wanted(&self, window: &Window) -> bool {
        matches!(self.target, Some(Target::Thread(_)))
            && self.details_shown(window)
            && self.details_item_shown("servers")
            && self.client().is_some()
    }

    /// Starts or drops the subscription as the section comes and goes; called each render.
    pub(in crate::app) fn sync_servers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.servers_wanted(window);
        if wanted == self.servers.task.is_some() {
            return;
        }
        if !wanted {
            self.servers = ServersWatch::default();
            return;
        }
        let Some(client) = self.client() else { return };
        let stream = client.stream("subscribeDiscoveredLocalServers", json!({}));
        self.servers.task = Some(cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = stream.next().await {
                let value = match event {
                    StreamEvent::Item(value) => value,
                    StreamEvent::End(Err(error)) => {
                        log::warn!("subscribeDiscoveredLocalServers: {error}");
                        break;
                    }
                    StreamEvent::End(Ok(())) => break,
                };
                let servers = match Vec::<LocalServer>::deserialize(&value["servers"]) {
                    Ok(servers) => servers,
                    Err(error) => {
                        log::warn!("subscribeDiscoveredLocalServers: {error}");
                        continue;
                    }
                };
                let updated = this.update(cx, |this, cx| {
                    if *this.servers.servers != servers {
                        this.servers.servers = Rc::new(servers);
                        cx.notify();
                    }
                });
                if updated.is_err() {
                    return;
                }
            }
            super::resubscribe_after_backoff(&this, cx, |this| this.servers.task = None).await;
        }));
    }

    fn open_server(&mut self, url: String, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Target::Thread(thread_id)) = self.target.clone() else { return };
        let m = event.modifiers();
        if m.control || m.platform {
            cx.open_url(&url);
        } else {
            self.open_preview_tab(thread_id, Some(url), window, cx);
        }
    }

    /// `preview.stopLocalServer`.
    fn stop_server(&mut self, server: &LocalServer, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(pid), Some(client)) = (server.pid, self.client()) else { return };
        if self.servers.stopping.is_some() {
            return;
        }
        self.servers.stopping = Some(server.key());
        let input = json!({"pid": pid, "port": server.port});
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("preview.stopLocalServer", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.servers.stopping = None;
                if let Err(error) = result {
                    let toast = Notification::error(error.to_string()).title("Could not stop server");
                    window.push_notification(toast, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Hidden while the thread runs none.
    pub(super) fn servers_section(
        &self,
        tokens: &Tokens,
        colors: &CardColors,
        cx: &mut Context<Self>,
    ) -> Option<Section> {
        let Some(Target::Thread(thread_id)) = &self.target else { return None };
        let servers: Vec<&LocalServer> = self.servers.servers.iter().filter(|s| s.belongs_to(thread_id)).collect();
        if servers.is_empty() {
            return None;
        }
        let can_stop = self.server_info.capability("localServerStop");
        let visible = self.servers.visible.unwrap_or(PAGE);
        let hidden = servers.len().saturating_sub(visible);
        let rows: Vec<AnyElement> = servers
            .iter()
            .take(visible)
            .map(|server| {
                let key = format!("{}:{}", server.pid.unwrap_or_default(), server.port);
                let stoppable = can_stop && server.pid.is_some();
                let stopping = self.servers.stopping == Some(server.key());
                let (url, stop) = (server.url.clone(), (*server).clone());
                let group: SharedString = format!("server-{key}").into();
                div()
                    .id(ElementId::Name(group.clone()))
                    .group(group.clone())
                    .relative()
                    .h(px(SERVER_ROW_HEIGHT))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(|s| s.bg(colors.hover))
                    .child(
                        row(colors)
                            .h_full()
                            .child(Icon::new(Lucide::Globe).with_size(px(16.)).text_color(tokens.muted_foreground))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(2.))
                                    .when(stoppable, |el| el.pr(px(24.)))
                                    .child(
                                        div()
                                            .truncate()
                                            .text_sm()
                                            .line_height(px(16.))
                                            .text_color(tokens.foreground.opacity(0.85))
                                            .child(format!("localhost:{}", server.port)),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(11.))
                                            .line_height(px(16.))
                                            .font_weight(gpui_kit::FontWeight::NORMAL)
                                            .text_color(tokens.muted_foreground)
                                            .child(server.subtitle()),
                                    ),
                            ),
                    )
                    .tooltip(|window, cx| {
                        Tooltip::new("Open in the browser panel. Ctrl-click for your browser.").build(window, cx)
                    })
                    .on_click(
                        cx.listener(move |this, event, window, cx| this.open_server(url.clone(), event, window, cx)),
                    )
                    .when(stoppable, |el| {
                        el.child(
                            div()
                                .id(ElementId::Name(format!("server-stop-{key}").into()))
                                .absolute()
                                .right(px(4.))
                                .top(px((SERVER_ROW_HEIGHT - 24.) / 2.))
                                .size(px(24.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(8.))
                                .text_color(tokens.error)
                                .when(!stopping, |el| el.invisible().group_hover(group.clone(), |s| s.visible()))
                                .hover(|s| s.bg(tokens.error.opacity(0.1)))
                                .child(if stopping {
                                    Spinner::new().xsmall().into_any_element()
                                } else {
                                    div().size(px(9.)).rounded(px(1.5)).bg(tokens.error).into_any_element()
                                })
                                .tooltip(|window, cx| Tooltip::new("Stop server").build(window, cx))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.stop_server(&stop, window, cx);
                                })),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        let shown = rows.len();
        let pager = (servers.len() > PAGE).then(|| {
            let label = if hidden > 0 { format!("Show {} more", hidden.min(PAGE)) } else { "Show less".into() };
            row(colors)
                .id("servers-more")
                .cursor_pointer()
                .text_color(tokens.muted_foreground)
                .hover(|s| s.bg(colors.hover))
                .child(Icon::new(if hidden > 0 { Lucide::ChevronDown } else { Lucide::ChevronUp }).with_size(px(16.)))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.servers.visible = Some(if hidden > 0 { visible + PAGE } else { PAGE });
                    cx.notify();
                }))
        });
        let height = HEADING_HEIGHT + shown as f32 * SERVER_ROW_HEIGHT + if pager.is_some() { ROW_HEIGHT } else { 0. };
        Some(Section {
            element: v_flex()
                .child(section_heading("Servers".into(), tokens))
                .children(rows)
                .children(pager)
                .into_any_element(),
            height,
            min_density: super::Density::Full,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servers_belong_to_the_thread_their_terminal_or_agent_runs_in() {
        let server: Vec<LocalServer> = serde_json::from_value(json!([
            {"host": "127.0.0.1", "port": 5173, "url": "http://127.0.0.1:5173", "processName": "node", "pid": 7,
             "terminal": {"threadId": "t1", "terminalId": "a"}},
            {"host": "127.0.0.1", "port": 3000, "url": "http://127.0.0.1:3000", "processName": null, "pid": null,
             "terminal": null, "agent": {"threadId": "t2"}},
        ]))
        .unwrap();
        assert!(server[0].belongs_to("t1") && !server[0].belongs_to("t2"));
        assert!(server[1].belongs_to("t2"));
        assert_eq!(server[0].subtitle(), "Terminal · node");
        assert_eq!(server[1].subtitle(), "Agent");
    }
}
