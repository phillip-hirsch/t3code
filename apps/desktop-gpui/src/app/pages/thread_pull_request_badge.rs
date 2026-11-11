//! A thread row's pull request: the sidebar's badge with its hover list
//! (`ThreadPullRequestBadgeControl`, `ThreadPullRequestsMiniList`) and the palette's status slots
//! (`ThreadRowLeadingStatus`, `ThreadSearchPullRequestNumber`), with the host read a row with no
//! synced link needs (`useLinkedThreadPullRequest`).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, ClickEvent, Context, ElementId, FontWeight, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Task, WeakEntity, Window, div, px,
    rgb,
};

use super::super::Workspace;
use super::super::right_panel::Surface;
use super::super::sidebar::guest_foreground;
use super::pull_requests::tone;
use crate::inbox::{Facts, Pill};
use crate::model::ThreadShell;
use crate::pull_request_detail::Reference;
use crate::pull_requests::{Entry, State};
use crate::theme::{self, Tokens};
use crate::thread_pull_requests::{
    self as links, BadgeView, Glyph, LinkedPullRequest, PullRequestLink, Shown, Source, Tone,
};

/// `staleTimeMs`: a row's read is asked again after this, unless its change request merged.
const STALE: Duration = Duration::from_secs(60);

/// The host's answers for thread rows read by their branch pull request.
#[derive(Default)]
pub(in crate::app) struct RowReads {
    reads: HashMap<String, RowRead>,
}

struct RowRead {
    /// When the last answer, or failure, landed.
    at: Option<Instant>,
    shown: Option<Shown>,
    _task: Option<Task<()>>,
}

impl RowReads {
    /// Not in flight, and never read, stale, or failed a minute ago; a merge is final.
    fn due(&self, key: &str) -> bool {
        self.reads.get(key).is_none_or(|read| {
            read._task.is_none()
                && read.at.is_none_or(|at| at.elapsed() >= STALE)
                && read.shown.as_ref().is_none_or(|shown| shown.tone != Tone::Merged)
        })
    }
}

/// The reference a branch pull request is read by, on the thread's environment.
fn read_reference(environment_id: &str, linked: &LinkedPullRequest) -> Reference {
    Reference {
        environment_id: environment_id.to_owned(),
        project_id: linked.project_id.clone(),
        host: crate::pull_request_detail::parse_change_request(&linked.url).map(|link| link.host),
        repository: linked.repository.clone(),
        number: linked.number,
    }
}

/// A listed pull request as a row shows it (`savedPullRequestSummaryAtom`).
fn shown_of_entry(entry: &Entry) -> Shown {
    let state = match entry.state {
        State::Merged => "merged",
        State::Closed => "closed",
        State::Open | State::Unknown => "open",
    };
    Shown {
        number: entry.number,
        url: entry.url.clone(),
        title: entry.title.clone(),
        tone: Tone::of(state, entry.is_draft),
        provider: entry.provider.clone(),
    }
}

/// `PULL_REQUEST_STATE_PRESENTATION`'s icon and tone.
fn tone_glyph(state: Tone, t: &Tokens) -> (Lucide, Hsla) {
    match state {
        Tone::Merged => (Lucide::GitMerge, tone(t, 0x7f22fe, 0xc4b4ff, 0.9)),
        Tone::Closed => (Lucide::GitPullRequestClosed, tone(t, 0xe7000b, 0xffa2a2, 0.9)),
        Tone::Draft => (Lucide::GitPullRequestDraft, tone(t, 0x71717b, 0x9f9fa9, 0.8)),
        Tone::Open => (Lucide::GitPullRequestArrow, tone(t, 0x009966, 0x5ee9b5, 0.9)),
    }
}

fn glyph(glyph: Glyph, t: &Tokens) -> (Lucide, Hsla) {
    match glyph {
        Glyph::Stack(state) => (Lucide::Layers, tone_glyph(state, t).1),
        Glyph::State(state) => tone_glyph(state, t),
        Glyph::Pending => (Lucide::GitPullRequestArrow, t.muted_foreground),
    }
}

/// `resolveThreadStatusPill`'s text and dot colors.
fn pill_colors(pill: Pill, t: &Tokens) -> (Hsla, Hsla) {
    let pair = |light: u32, dot: u32, dark: u32, alpha: f32| (tone(t, light, dark, alpha), tone(t, dot, dark, alpha));
    match pill {
        Pill::PendingApproval => pair(0xe17100, 0xfe9a00, 0xffd230, 0.9),
        Pill::AwaitingInput => pair(0x4f39f6, 0x615fff, 0xa3b3ff, 0.9),
        Pill::GuestRequest => (guest_foreground(t), guest_foreground(t)),
        Pill::Working | Pill::Connecting => pair(0x0084d1, 0x00a6f4, 0x74d4ff, 0.8),
        Pill::Waiting => (t.sidebar.muted_foreground, t.sidebar.muted_foreground),
        Pill::Failed => pair(0xe7000b, 0xfb2c36, 0xffa2a2, 0.9),
        Pill::PlanReady => pair(0x7f22fe, 0x8e51ff, 0xc4b4ff, 0.9),
        Pill::Completed => pair(0x009966, 0x00bc7d, 0x5ee9b5, 0.9),
    }
}

/// One line of the badge's hover list.
#[derive(Clone)]
struct MiniItem {
    number: u64,
    url: String,
    title: String,
    /// `None` until the link syncs.
    tone: Option<Glyph>,
    depth: usize,
    /// "stack · 3" or "chain · 2" on a chain's base line.
    chain: Option<String>,
}

/// `ThreadPullRequestsMiniList`'s lines, or the one change request a thread without links shows.
fn mini_items(
    pull_requests: &[PullRequestLink],
    view: &BadgeView,
    shown: Option<&Shown>,
    subject: Option<(u64, &str)>,
) -> Vec<MiniItem> {
    let lines = links::list_lines(pull_requests);
    if lines.is_empty() {
        let Some((number, url)) = subject else { return Vec::new() };
        let title = shown.map_or_else(|| view.label.clone(), |shown| shown.title.clone());
        let item = MiniItem { number, url: url.to_owned(), title, tone: Some(view.glyph), depth: 0, chain: None };
        return vec![item];
    }
    lines
        .iter()
        .map(|line| {
            let snapshot = line.link.snapshot.as_ref();
            MiniItem {
                number: line.link.number,
                url: line.link.url.clone(),
                title: snapshot.map_or_else(|| line.link.repository.clone(), |s| s.title.clone()),
                tone: snapshot.map(|s| Glyph::State(Tone::of(&s.state, s.is_draft))),
                depth: line.depth.min(3),
                chain: line.stack.map(|(native, size)| format!("{} · {size}", if native { "stack" } else { "chain" })),
            }
        })
        .collect()
}

/// The thread a badge belongs to.
#[derive(Clone)]
struct RowThread {
    environment_id: String,
    thread_id: String,
}

/// One badge control's content and where it acts.
struct Badge {
    id: SharedString,
    view: BadgeView,
    items: Vec<MiniItem>,
    row: RowThread,
    url: Option<String>,
    /// Opens the change request in the thread's panel rather than the Pull requests page.
    in_panel: bool,
    /// Drawn as the composer's control rather than a sidebar row's.
    composer: bool,
}

fn mini_list(
    items: &[MiniItem],
    row: &RowThread,
    in_panel: bool,
    view: &WeakEntity<Workspace>,
    t: &Tokens,
) -> AnyElement {
    let lines = items.iter().enumerate().map(|(index, item)| {
        let (icon, color) = item.tone.map_or((Lucide::GitPullRequestArrow, t.muted_foreground), |g| glyph(g, t));
        let (url, row, view) = (item.url.clone(), row.clone(), view.clone());
        h_flex()
            .id(ElementId::Name(format!("mini-pr-{index}").into()))
            .ml(px(12. * item.depth as f32))
            .min_w_0()
            .gap(px(8.))
            .px(px(4.))
            .py(px(4.))
            .rounded(px(4.))
            .cursor_pointer()
            .hover(|s| s.bg(t.accent))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |event: &ClickEvent, window, cx| {
                cx.stop_propagation();
                let _ = view.update(cx, |this, cx| this.open_row_pull_request(&row, &url, in_panel, event, window, cx));
            })
            .child(Icon::new(icon).with_size(px(12.)).flex_none().text_color(color))
            .child(div().flex_none().font_family(t.fonts.mono).child(format!("#{}", item.number)))
            .child(div().min_w_0().truncate().text_color(t.foreground.opacity(0.75)).child(item.title.clone()))
            .children(
                item.chain.clone().map(|chain| div().ml_auto().flex_none().pl(px(4.)).text_size(px(10.)).child(chain)),
            )
    });
    v_flex()
        .id("thread-pull-requests-mini-list")
        .w(px(320.))
        .max_h(px(360.))
        .overflow_y_scroll()
        .gap(px(4.))
        .mx(px(-4.))
        .text_xs()
        .text_color(t.muted_foreground)
        .children(lines)
        .into_any_element()
}

impl Workspace {
    /// `useSupportsMultiplePullRequests`: the environment reports its server config.
    fn supports_pull_request_links(&self, environment_id: &str) -> bool {
        self.env_info(environment_id).is_some()
    }

    /// `useLinkedThreadPullRequest`: the current link's snapshot, else the host's answer for the
    /// branch's pull request (or the listed one), asked for again when stale while the row draws.
    fn row_pull_request(&self, thread: &ThreadShell, cx: &mut Context<Self>) -> Option<Shown> {
        let linked = match links::source(thread, self.supports_pull_request_links(&thread.environment_id))? {
            Source::Link(link) => return Shown::of_link(link),
            Source::Read(linked) => linked,
        };
        let reference = read_reference(&thread.environment_id, linked);
        let key = reference.key();
        let reads = &self.pull_requests.row_reads;
        if reads.due(&key) && self.env_client(&reference.environment_id).is_some() {
            let (this, reference) = (cx.entity().downgrade(), reference.clone());
            cx.defer(move |cx| {
                let _ = this.update(cx, |this, cx| this.read_row_pull_request(reference, cx));
            });
        }
        reads
            .reads
            .get(&key)
            .and_then(|read| read.shown.clone())
            .or_else(|| self.pull_requests.entry_for(&reference).map(|entry| shown_of_entry(&entry)))
    }

    /// `pullRequests.summary` for a row's branch pull request; its answer redraws the sidebar.
    fn read_row_pull_request(&mut self, reference: Reference, cx: &mut Context<Self>) {
        let key = reference.key();
        if !self.pull_requests.row_reads.due(&key) {
            return;
        }
        let Some(client) = self.env_client(&reference.environment_id) else { return };
        let input = reference.to_json();
        let landed = key.clone();
        let task = cx.spawn(async move |this, cx| {
            let answer = client.request("pullRequests.summary", input).await;
            let _ = this.update(cx, |this, cx| {
                let Some(read) = this.pull_requests.row_reads.reads.get_mut(&landed) else { return };
                read._task = None;
                read.at = Some(Instant::now());
                match answer.as_ref().map(Shown::of_summary) {
                    Ok(Some(shown)) => read.shown = Some(shown),
                    Ok(None) => log::warn!("unreadable pull request summary"),
                    Err(error) => log::debug!("pull request summary failed: {error}"),
                }
                cx.notify();
                this.notify_sidebar(cx);
            });
        });
        let read =
            self.pull_requests.row_reads.reads.entry(key).or_insert(RowRead { at: None, shown: None, _task: None });
        read._task = Some(task);
    }

    /// `handlePrClick`: Ctrl or Cmd, a middle click, or "Open pull requests in T3 Code" off open
    /// the browser; otherwise the thread's Pull request surface when `in_panel`, else the Pull
    /// requests page, falling back to the browser for a change request no project holds.
    fn open_row_pull_request(
        &mut self,
        row: &RowThread,
        url: &str,
        in_panel: bool,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = event.modifiers();
        let middle = matches!(event, ClickEvent::Mouse(click) if click.down.button == MouseButton::Middle);
        if modifiers.control
            || modifiers.platform
            || middle
            || !self.client_settings.values.bool("pullRequestLinksOpenInApp")
        {
            return self.open_link(url, event, window, cx);
        }
        if !in_panel {
            if !self.open_change_request_link(url, cx) {
                self.open_link(url, event, window, cx);
            }
            return;
        }
        let thread =
            self.shell.threads.iter().find(|t| t.id == row.thread_id && *t.environment_id == *row.environment_id);
        let (Some(thread), Some(link)) = (thread, crate::pull_request_detail::parse_change_request(url)) else {
            return self.open_link(url, event, window, cx);
        };
        // `findProjectForChangeRequest` on the thread's environment, else the thread's own project.
        let projects = self
            .shell
            .projects
            .iter()
            .filter(|p| p.environment_id == thread.environment_id)
            .map(|p| (p.id.as_str(), p.repository_identity.as_ref().and_then(|i| i.canonical_key.as_deref())));
        let project = crate::pull_request_detail::project_for_link(projects, &link.host, &link.repository)
            .unwrap_or(&thread.project_id)
            .to_owned();
        let reference = Reference {
            environment_id: row.environment_id.clone(),
            project_id: project,
            host: Some(link.host),
            repository: link.repository,
            number: link.number,
        };
        self.select_thread_in(row.environment_id.clone().into(), row.thread_id.clone(), window, cx);
        self.open_surface(Surface::PullRequest(reference), window, cx);
    }

    /// A stack or several links open the thread's Linked pull requests surface.
    fn open_row_pull_requests(&mut self, row: &RowThread, window: &mut Window, cx: &mut Context<Self>) {
        self.select_thread_in(row.environment_id.clone().into(), row.thread_id.clone(), window, cx);
        self.open_surface(Surface::PullRequests, window, cx);
    }

    /// The sidebar row's pull request badge (`ThreadPullRequestBadgeControl`): its state glyph and
    /// number, or a stack's or several links' aggregate, with the linked list on hover. It opens
    /// the pull request (in the thread's panel while a thread is open), or the list.
    pub(in crate::app) fn render_row_pull_request_badge(
        &self,
        thread: &ThreadShell,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let supports = self.supports_pull_request_links(&thread.environment_id);
        let shown = self.row_pull_request(thread, cx);
        let view = links::row_badge(thread, supports, shown.as_ref())?;
        let current = links::current(&thread.pull_requests).filter(|_| supports);
        let number = shown.as_ref().map(|s| s.number).or(current.map(|l| l.number));
        let url = shown.as_ref().map(|s| s.url.clone()).or(current.map(|l| l.url.clone()));
        let items = mini_items(&thread.pull_requests, &view, shown.as_ref(), number.zip(url.as_deref()));
        let badge = Badge {
            id: format!("thread-pr-badge-{}-{}", thread.environment_id, thread.id).into(),
            view,
            items,
            row: RowThread { environment_id: thread.environment_id.to_string(), thread_id: thread.id.clone() },
            url,
            in_panel: matches!(self.target, Some(super::super::Target::Thread { .. })),
            composer: false,
        };
        Some(self.badge_control(badge, cx))
    }

    /// The composer's control beside the branch trigger (`BranchToolbarBranchSelector`'s
    /// `ThreadPullRequestBadgeControl`): the thread's linked change requests, else the change
    /// request of the branch the trigger shows while the thread (or draft) is on it.
    pub(in crate::app) fn render_composer_pull_request_badge(
        &self,
        trigger_branch: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (environment_id, thread, thread_branch) = match self.target.as_ref()? {
            super::super::Target::Thread { env, id } => {
                let thread = self.shell.threads.iter().find(|t| &t.id == id && &t.environment_id == env)?;
                (env, Some(thread), thread.branch.as_deref())
            }
            super::super::Target::Draft { env, .. } => (env, None, self.draft_workspace.choice.branch.as_deref()),
        };
        let supports = self.supports_pull_request_links(environment_id);
        let pull_requests = thread.map_or(&[][..], |t| t.pull_requests.as_slice());
        let current = links::current(pull_requests).filter(|_| supports);
        // `resolveBranchToolbarPrBranch`: the checkout's change request while the trigger shows the thread's branch.
        let branch_pr = thread_branch.filter(|b| trigger_branch == Some(*b)).and_then(|branch| {
            let status = self.vcs.status.as_ref().filter(|s| s.local.ref_name.as_deref() == Some(branch))?;
            let pr = status.remote.pr.as_ref()?;
            Some(Shown {
                number: pr.number,
                url: pr.url.clone(),
                title: pr.title.clone(),
                tone: Tone::of(&pr.state, pr.is_draft),
                provider: status.local.source_control_provider.as_ref().map_or_else(String::new, |p| p.kind.clone()),
            })
        });
        let shown = thread.and_then(|t| self.row_pull_request(t, cx)).or(branch_pr.filter(|_| current.is_none()));
        let number = current.map(|l| l.number).or(shown.as_ref().map(|s| s.number));
        let url = current.map(|l| l.url.clone()).or(shown.as_ref().map(|s| s.url.clone()));
        let badge = links::badge(pull_requests).filter(|_| supports);
        let view = links::badge_view(badge, number, url.as_deref(), shown.as_ref())?;
        let items = mini_items(pull_requests, &view, shown.as_ref(), number.zip(url.as_deref()));
        let badge = Badge {
            id: "composer-pr-badge".into(),
            view,
            items,
            row: RowThread {
                environment_id: environment_id.to_string(),
                thread_id: thread.map_or_else(String::new, |t| t.id.clone()),
            },
            url,
            in_panel: thread.is_some(),
            composer: true,
        };
        Some(self.badge_control(badge, cx))
    }

    /// A [`Badge`] as a control: a sidebar row's link-like glyph and number, or the composer's
    /// `ComposerControl` (`size="xs"`).
    fn badge_control(&self, badge: Badge, cx: &mut Context<Self>) -> AnyElement {
        let Badge { id, view, items, row, url, in_panel, composer } = badge;
        let t = *theme::tokens(cx);
        let (icon, color) = glyph(view.glyph, &t);
        let weak = cx.entity().downgrade();
        let (tip_row, tip_items) = (row.clone(), items);
        let opens_list = view.opens_list;
        let label: SharedString = view.label.into();
        let text_color = if composer { t.muted_foreground } else { t.sidebar.secondary_label };
        h_flex()
            .id(id)
            .flex_none()
            .border_1()
            .border_color(gpui_kit::transparent_black())
            .cursor_pointer()
            .focusable()
            .tab_stop(true)
            .focus_visible(move |s| s.border_color(t.ring))
            .map(|el| {
                if composer {
                    let height = px(t.breakpoint.pick(28., 24.));
                    el.h(height).px(px(7.)).gap(px(4.)).rounded(px(8.)).hover(|s| s.bg(t.accent))
                } else {
                    el.gap(px(2.)).rounded(px(2.)).hover(gpui_kit::Styled::underline)
                }
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                cx.stop_propagation();
                if opens_list {
                    this.open_row_pull_requests(&row, window, cx);
                } else if let Some(url) = &url {
                    this.open_row_pull_request(&row, url, in_panel, event, window, cx);
                }
            }))
            .hoverable_tooltip(move |window, cx| {
                let (items, row, weak, label) = (tip_items.clone(), tip_row.clone(), weak.clone(), label.clone());
                Tooltip::element(move |_, cx| {
                    let t = *theme::tokens(cx);
                    if items.is_empty() {
                        return div().child(label.clone()).into_any_element();
                    }
                    mini_list(&items, &row, in_panel, &weak, &t)
                })
                .build(window, cx)
            })
            .child(Icon::new(icon).with_size(px(12.)).flex_none().text_color(color))
            .child(div().text_xs().text_color(text_color).child(view.text))
            .into_any_element()
    }

    /// `ThreadRowLeadingStatus`: the change request's state glyph (the open thread's checkout
    /// branch's too), a pending link's glyph, and the thread's status pill.
    pub(in crate::app) fn render_thread_leading_status(
        &self,
        thread: &ThreadShell,
        requests: &crate::inbox::ChildRequests,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let t = *theme::tokens(cx);
        let shown = self.row_pull_request(thread, cx).or_else(|| self.checkout_pull_request(thread));
        let supports = self.supports_pull_request_links(&thread.environment_id);
        let pending = links::current(&thread.pull_requests).filter(|_| supports && shown.is_none());
        let pill = Facts::new(thread, requests).pill();
        if shown.is_none() && pending.is_none() && pill.is_none() {
            return None;
        }
        let key = format!("{}-{}", thread.environment_id, thread.id);
        let state = shown.map(|shown| {
            let (icon, color) = tone_glyph(shown.tone, &t);
            let (lead, title): (SharedString, SharedString) = (shown.lead().into(), shown.title.into());
            div()
                .id(SharedString::from(format!("leading-pr-{key}")))
                .flex_none()
                .flex()
                .child(Icon::new(icon).with_size(px(12.)).text_color(color))
                .tooltip(move |window, cx| {
                    let (lead, title) = (lead.clone(), title.clone());
                    Tooltip::element(move |_, cx| {
                        let border = theme::tokens(cx).border.opacity(0.7);
                        h_flex()
                            .max_w(px(544.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(div().flex_none().pr(px(8.)).font_weight(FontWeight::MEDIUM).child(lead.clone()))
                            .child(div().flex_none().h(px(16.)).border_l_1().border_color(border))
                            .child(div().min_w_0().truncate().pl(px(8.)).child(title.clone()))
                    })
                    .build(window, cx)
                })
        });
        let pending = pending.map(|link| {
            let short = crate::pull_request_detail::terminology(
                crate::pull_request_detail::parse_change_request(&link.url).map_or("", |l| l.provider),
            )
            .1;
            let label: SharedString = format!("{short} #{}, status pending", link.number).into();
            div()
                .id(SharedString::from(format!("leading-pending-{key}")))
                .flex_none()
                .flex()
                .child(Icon::new(Lucide::GitPullRequestArrow).with_size(px(12.)).text_color(t.muted_foreground))
                .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
        });
        let pill = pill.map(|pill| {
            let (text, dot) = pill_colors(pill, &t);
            div()
                .id(SharedString::from(format!("leading-pill-{key}")))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(4.))
                .text_size(px(10.))
                .line_height(px(14.))
                .text_color(text)
                .child(div().size(px(6.)).rounded_full().bg(dot))
                .child(pill.label())
                .tooltip(move |window, cx| Tooltip::new(pill.label()).build(window, cx))
        });
        Some(h_flex().flex_none().gap(px(6.)).children(state).children(pending).children(pill).into_any_element())
    }

    /// The open thread's checkout pull request (`resolveDisplayedThreadPr`'s `gitStatus.pr`),
    /// for a thread on its branch without a linked one.
    fn checkout_pull_request(&self, thread: &ThreadShell) -> Option<Shown> {
        let open = matches!(&self.target, Some(super::super::Target::Thread { env, id })
            if *id == thread.id && *env == thread.environment_id);
        if !open || thread.linked_pull_request.is_some() {
            return None;
        }
        let status = self
            .vcs
            .status
            .as_ref()
            .filter(|s| thread.branch.is_some() && s.local.ref_name.as_deref() == thread.branch.as_deref())?;
        let pr = status.remote.pr.as_ref()?;
        Some(Shown {
            number: pr.number,
            url: pr.url.clone(),
            title: pr.title.clone(),
            tone: Tone::of(&pr.state, pr.is_draft),
            provider: crate::pull_request_detail::parse_change_request(&pr.url).map_or("", |l| l.provider).to_owned(),
        })
    }

    /// `ThreadSearchPullRequestNumber`: the badge's look without its controls, on a palette row
    /// whose query names one of the thread's pull requests. A settled thread's is muted.
    pub(in crate::app) fn render_thread_search_number(
        &self,
        thread: &ThreadShell,
        query: &str,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !links::matches_search(thread, query) {
            return None;
        }
        let supports = self.supports_pull_request_links(&thread.environment_id);
        let shown = self.row_pull_request(thread, cx);
        let view = links::search_badge(thread, query, supports, shown.as_ref())?;
        let t = *theme::tokens(cx);
        let (icon, color) = glyph(view.glyph, &t);
        let color =
            if thread.settled_override.as_deref() == Some("settled") { t.sidebar.secondary_label } else { color };
        let label: SharedString = view.label.into();
        Some(
            h_flex()
                .id(SharedString::from(format!("search-pr-{}-{}", thread.environment_id, thread.id)))
                .flex_none()
                .gap(px(2.))
                .text_xs()
                .text_color(color)
                .child(Icon::new(icon).with_size(px(12.)).flex_none().text_color(color))
                .child(view.text)
                .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                .into_any_element(),
        )
    }
}

/// `terminalStatusFromRunningIds`' teal.
pub(in crate::app) fn terminal_running_color(t: &Tokens) -> Hsla {
    if t.dark { Hsla { a: 0.9, ..rgb(0x46ecd5).into() } } else { rgb(0x009689).into() }
}
