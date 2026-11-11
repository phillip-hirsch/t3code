//! Web links in chat Markdown (`ChatMarkdown`'s `a`): the host mark before a link's text
//! (`MarkdownExternalLinkContent`), the URL tooltip, and a pull request link's hover card
//! (`PullRequestLinkPreview`), read with `pullRequests.preview` and kept per environment and
//! pull request.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::text::{
    InlineElement, InlineRenderContext, LinkHover, MarkdownNode, MarkdownParseContext, MarkdownPlugin,
};
use gpui_kit::component::{Icon, h_flex};
use gpui_kit::{
    Anchor, AnyElement, App, Bounds, Context, FontWeight, Image, ImageSource, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _, Styled as _, Task, WeakEntity, Window,
    anchored, deferred, div, img, point, prelude::FluentBuilder as _, px, relative, svg,
};

use super::Workspace;
use super::side_chat::{ChatPane, PaneOwner};
use crate::external_links::{self as logic, HoverBatch, HoverChange, LinkMark, Preview};
use crate::pull_request_detail::Reference;
use crate::theme::Tokens;

/// `PreviewCardTrigger`'s `delay` and `closeDelay`.
const CARD_OPEN: Duration = Duration::from_millis(350);
const CARD_CLOSE: Duration = Duration::from_millis(120);
/// Base UI's tooltip delay.
const TOOLTIP_OPEN: Duration = Duration::from_millis(600);
/// The preview query's `staleTimeMs`.
const PREVIEW_FRESH: Duration = Duration::from_secs(60);

/// Favicons by URL, shared by every message and window (`failedFaviconHosts` keeps failures
/// app-wide).
static FAVICONS: LazyLock<Favicons> = LazyLock::new(Favicons::default);

#[derive(Default)]
struct Favicons(Mutex<HashMap<String, Favicon>>);

#[derive(Clone)]
enum Favicon {
    Wanted,
    Loading,
    Loaded(Arc<Image>),
    Failed,
}

impl Favicons {
    /// The loaded favicon (`Ok(Some)`), still loading (`Ok(None)`), or failed (`Err`); the first
    /// ask queues a load, and says so.
    fn get(&self, url: &str) -> (Result<Option<Arc<Image>>, ()>, bool) {
        let Ok(mut map) = self.0.lock() else { return (Err(()), false) };
        match map.get(url) {
            Some(Favicon::Loaded(image)) => (Ok(Some(image.clone())), false),
            Some(Favicon::Failed) => (Err(()), false),
            Some(Favicon::Wanted | Favicon::Loading) => (Ok(None), false),
            None => {
                map.insert(url.to_owned(), Favicon::Wanted);
                (Ok(None), true)
            }
        }
    }

    fn take_wanted(&self) -> Vec<String> {
        let Ok(mut map) = self.0.lock() else { return Vec::new() };
        map.iter_mut()
            .filter(|(_, state)| matches!(state, Favicon::Wanted))
            .map(|(url, state)| {
                *state = Favicon::Loading;
                url.clone()
            })
            .collect()
    }

    fn finish(&self, url: String, image: Option<Arc<Image>>) {
        if let Ok(mut map) = self.0.lock() {
            map.insert(url, image.map_or(Favicon::Failed, Favicon::Loaded));
        }
    }
}

/// `MarkdownLinkFavicon` before a web link's text: its host's mark, its favicon, or a globe. It
/// leads the link, so a click or right click on it is the link's.
pub(super) struct LinkMarks {
    pub workspace: WeakEntity<Workspace>,
}

impl MarkdownPlugin for LinkMarks {
    fn name(&self) -> &'static str {
        "t3-link-mark"
    }

    fn parse(
        &self,
        node: &gpui_kit::component::text::markdown_ast::Node,
        cx: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        let (url, children) = super::markdown_ui::link_destination(node, cx)?;
        let host = logic::web_link_host(url)?;
        // An image link has no text to lead (`hastHasText`).
        if super::markdown_ui::link_text(children).trim().is_empty() {
            return None;
        }
        Some(MarkdownNode::new("t3-link-mark", logic::link_mark(&host)).leading())
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<InlineElement> {
        let mark = node.data::<LinkMark>()?;
        let color = context.text_style().color;
        let globe = || Icon::new(IconName::Globe).size_full().text_color(color).into_any_element();
        let icon = match mark {
            LinkMark::Host(path, true) => svg().path(*path).size_full().text_color(color).into_any_element(),
            LinkMark::Host(path, false) => img(*path).size_full().into_any_element(),
            LinkMark::Globe => globe(),
            LinkMark::Favicon(url) => match FAVICONS.get(url) {
                (Ok(Some(image)), _) => img(ImageSource::Image(image)).size_full().rounded(px(2.)).into_any_element(),
                (Ok(None), queued) => {
                    if queued {
                        let workspace = self.workspace.clone();
                        cx.defer(move |cx| {
                            let _ = workspace.update(cx, Workspace::load_link_favicons);
                        });
                    }
                    div().into_any_element()
                }
                (Err(()), _) => globe(),
            },
        };
        // `ms-[0.25em] me-[0.2em] size-[14px] [vertical-align:-0.125em]`.
        let (size, font) = (px(14.), context.font_size());
        let element = div().flex_none().size(size).ml(font * 0.25).mr(font * 0.2).child(icon);
        Some(InlineElement::new(element).with_baseline(size - font * 0.125))
    }
}

/// The link under the pointer and what it shows.
#[derive(Default)]
pub(super) struct LinkHoverState {
    /// What this event did to every text view's links, resolved once they all reported.
    batch: Option<HoverBatch<(Bounds<Pixels>, ChatPane)>>,
    hover: Option<Hover>,
    next_id: u64,
    /// `pullRequests.preview` answers by environment and pull request.
    previews: HashMap<(SharedString, String), PreviewRead>,
}

struct Hover {
    id: u64,
    url: SharedString,
    bounds: Bounds<Pixels>,
    /// The pane whose timeline holds the link, and with its thread the card's owner: the card
    /// goes once the pane shows another thread or closes.
    pane: ChatPane,
    owner: Option<PaneOwner>,
    /// The pull request the link names, in the message's environment; `None` shows the URL tooltip.
    target: Option<(SharedString, Reference)>,
    over_link: bool,
    over_card: bool,
    shown: bool,
    _timer: Option<Task<()>>,
}

enum PreviewRead {
    Loading,
    Loaded(Rc<Preview>, Instant),
    Failed,
}

impl Workspace {
    /// Starts loading the favicons the chat asked for.
    fn load_link_favicons(&mut self, cx: &mut Context<Self>) {
        for url in FAVICONS.take_wanted() {
            cx.spawn(async move |this, cx| {
                let image = super::markdown_images::fetch(url.clone()).await.and_then(|(mime, bytes)| {
                    let format = mime
                        .as_deref()
                        .and_then(gpui_kit::ImageFormat::from_mime_type)
                        .or_else(|| super::markdown_images::sniff(&bytes))?;
                    Some(Arc::new(Image::from_bytes(format, bytes)))
                });
                FAVICONS.finish(url, image);
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
    }

    /// A text view's link hover report, resolved with the rest of this event's. `pane` shows the
    /// view's timeline.
    pub(super) fn link_hovered(&mut self, hover: Option<LinkHover>, pane: ChatPane, cx: &mut Context<Self>) {
        // Only web links show anything (`faviconHost`); any other link is no link here.
        let hover = hover.filter(|hover| logic::web_link_host(&hover.url).is_some());
        let first = self.link_hover.batch.is_none();
        let batch = self.link_hover.batch.get_or_insert(HoverBatch { entered: None, left: false });
        match hover {
            Some(hover) => batch.entered = Some((hover.url.to_string(), (hover.bounds, pane))),
            None => batch.left = true,
        }
        if first {
            let view = cx.weak_entity();
            cx.defer(move |cx| {
                let _ = view.update(cx, Self::resolve_link_hover);
            });
        }
    }

    fn resolve_link_hover(&mut self, cx: &mut Context<Self>) {
        let Some(batch) = self.link_hover.batch.take() else { return };
        let current = self.link_hover.hover.as_ref().map(|hover| (hover.url.as_ref(), &hover.pane));
        match logic::hover_change(current, batch) {
            HoverChange::Stay((bounds, _)) => {
                if let Some(hover) = self.link_hover.hover.as_mut() {
                    hover.over_link = true;
                    if !hover.shown {
                        hover.bounds = bounds;
                    }
                }
            }
            HoverChange::Enter(url, (bounds, pane)) => {
                let shown = self.link_hover.hover.as_ref().is_some_and(|hover| hover.shown);
                let owner = self.pane_owner(pane);
                let target = owner.as_ref().and_then(|owner| self.link_preview_target(&owner.env, &url));
                let id = self.link_hover.next_id;
                self.link_hover.next_id += 1;
                let delay = if target.is_some() { CARD_OPEN } else { TOOLTIP_OPEN };
                self.link_hover.hover = Some(Hover {
                    id,
                    url: url.into(),
                    bounds,
                    pane,
                    owner,
                    target,
                    over_link: true,
                    over_card: false,
                    shown: false,
                    _timer: Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(delay).await;
                        let _ = this.update(cx, |this, cx| this.show_link_hover(id, cx));
                    })),
                });
                if shown {
                    cx.notify();
                }
            }
            HoverChange::Leave => {
                let Some(hover) = self.link_hover.hover.as_mut() else { return };
                hover.over_link = false;
                // A card stays a moment so the pointer can reach it; a tooltip or a card yet to
                // open goes at once.
                if hover.shown && hover.target.is_some() {
                    self.close_link_hover_later(cx);
                } else {
                    self.close_link_hover(cx);
                }
            }
            HoverChange::Nothing => {}
        }
    }

    /// `resolvePullRequestPreviewTarget` in the environment of the thread the link is in.
    fn link_preview_target(&self, environment: &SharedString, url: &str) -> Option<(SharedString, Reference)> {
        let environment = environment.clone();
        let loaded = self.env_info(&environment).is_some_and(|info| !info.config.is_null());
        let projects =
            self.shell.projects.iter().filter(|project| project.environment_id == environment).map(|project| {
                (
                    project.id.as_str(),
                    project.repository_identity.as_ref().and_then(|identity| identity.canonical_key.as_deref()),
                )
            });
        let reference = logic::preview_target(&environment, url, loaded, projects)?;
        Some((environment, reference))
    }

    fn show_link_hover(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(hover) = self.link_hover.hover.as_mut().filter(|hover| hover.id == id) else { return };
        if !hover.over_link && !hover.over_card {
            return;
        }
        hover.shown = true;
        hover._timer = None;
        if let Some((environment, reference)) = hover.target.clone() {
            self.read_link_preview(environment, reference, cx);
        }
        cx.notify();
    }

    /// Drops the card or tooltip once the pane and thread holding its link go.
    pub(super) fn release_left_link_hover(&mut self) {
        let owner = self.link_hover.hover.as_ref().and_then(|hover| hover.owner.as_ref());
        if owner.is_none_or(|owner| !self.owner_shown(owner)) {
            self.link_hover.hover = None;
        }
    }

    /// Hides the card or tooltip: a click on its link opens what it previewed.
    pub(super) fn close_link_hover(&mut self, cx: &mut Context<Self>) {
        if self.link_hover.hover.take().is_some_and(|hover| hover.shown) {
            cx.notify();
        }
    }

    fn close_link_hover_later(&mut self, cx: &mut Context<Self>) {
        let Some(hover) = self.link_hover.hover.as_mut() else { return };
        let id = hover.id;
        hover._timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CARD_CLOSE).await;
            let _ = this.update(cx, |this, cx| {
                if this.link_hover.hover.as_ref().is_some_and(|h| h.id == id && !h.over_link && !h.over_card) {
                    this.close_link_hover(cx);
                }
            });
        }));
    }

    fn hover_link_card(&mut self, id: u64, hovered: bool, cx: &mut Context<Self>) {
        let Some(hover) = self.link_hover.hover.as_mut().filter(|hover| hover.id == id) else { return };
        hover.over_card = hovered;
        if !hovered && !hover.over_link {
            self.close_link_hover_later(cx);
        }
    }

    /// Reads the pull request's preview unless a fresh answer or a read is in hand.
    fn read_link_preview(&mut self, environment: SharedString, reference: Reference, cx: &mut Context<Self>) {
        let key = (environment.clone(), reference.key());
        match self.link_hover.previews.get(&key) {
            Some(PreviewRead::Loading) => return,
            Some(PreviewRead::Loaded(_, at)) if at.elapsed() < PREVIEW_FRESH => return,
            _ => {}
        }
        let Some(client) = self.env_client(&environment) else {
            self.link_hover.previews.insert(key, PreviewRead::Failed);
            return;
        };
        self.link_hover.previews.insert(key.clone(), PreviewRead::Loading);
        let input = reference.to_json();
        cx.spawn(async move |this, cx| {
            let result = client.request("pullRequests.preview", input).await;
            let read = match result.map(serde_json::from_value::<Preview>) {
                Ok(Ok(preview)) => PreviewRead::Loaded(Rc::new(preview), Instant::now()),
                Ok(Err(error)) => {
                    log::debug!("unreadable pull request preview: {error}");
                    PreviewRead::Failed
                }
                Err(error) => {
                    log::debug!("pullRequests.preview: {error}");
                    PreviewRead::Failed
                }
            };
            let _ = this.update(cx, |this, cx| {
                // A stale answer would replace a newer one; keep the old preview instead.
                let keep = matches!(
                    (&read, this.link_hover.previews.get(&key)),
                    (PreviewRead::Failed, Some(PreviewRead::Loaded(..)))
                );
                if !keep {
                    this.link_hover.previews.insert(key.clone(), read);
                }
                let showing = this.link_hover.hover.as_ref().is_some_and(|hover| {
                    hover.shown && hover.target.as_ref().is_some_and(|(e, r)| *e == key.0 && r.key() == key.1)
                });
                if showing {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The hover card or URL tooltip of the link under the pointer.
    pub(super) fn render_link_hover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let hover = self.link_hover.hover.as_ref().filter(|hover| hover.shown)?;
        if !hover.owner.as_ref().is_some_and(|owner| self.owner_shown(owner)) {
            return None;
        }
        let t = *crate::theme::tokens(cx);
        let bounds = hover.bounds;
        let card = match &hover.target {
            Some((environment, reference)) => {
                let preview = match self.link_hover.previews.get(&(environment.clone(), reference.key())) {
                    Some(PreviewRead::Loaded(preview, _)) => Some(preview.clone()),
                    _ => None,
                };
                let failed = matches!(
                    self.link_hover.previews.get(&(environment.clone(), reference.key())),
                    Some(PreviewRead::Failed)
                );
                let summary = self.pull_requests.entry_for(reference);
                match (preview, summary) {
                    (Some(preview), _) => Some(card_body(&CardFacts::from_preview(&preview), &t)),
                    (None, Some(entry)) => Some(card_body(&CardFacts::from_entry(&entry), &t)),
                    // Nothing to show yet; a failed read shows the URL instead (`showUrlTooltip`).
                    (None, None) if !failed => return None,
                    (None, None) => None,
                }
            }
            None => None,
        };
        let id = hover.id;
        let element = match card {
            Some(body) => {
                let view = cx.weak_entity();
                div()
                    .id(SharedString::from(format!("link-card-{id}")))
                    .occlude()
                    .w(px(320.))
                    .p(px(12.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.popover)
                    .shadow_lg()
                    .on_hover(move |hovered, _, cx| {
                        let _ = view.update(cx, |this, cx| this.hover_link_card(id, *hovered, cx));
                    })
                    .child(body)
                    .into_any_element()
            }
            None => url_tooltip(hover.url.clone(), &t),
        };
        let gap = if hover.target.is_some() { px(6.) } else { px(4.) };
        // The side with room: above the link, else below it.
        let (position, corner) = if bounds.top() > px(160.) {
            (point(bounds.center().x, bounds.top() - gap), Anchor::BottomCenter)
        } else {
            (point(bounds.center().x, bounds.bottom() + gap), Anchor::TopCenter)
        };
        Some(
            deferred(anchored().anchor(corner).position(position).snap_to_window_with_margin(px(8.)).child(element))
                .with_priority(3)
                .into_any_element(),
        )
    }
}

/// What the card shows, from a preview or from a list row already in hand.
struct CardFacts {
    repository: String,
    number: u64,
    state: crate::pull_requests::State,
    draft: bool,
    title: String,
    /// `None`: no author row (`card.author === undefined`); `Some(None)` is a deleted account.
    author: Option<Option<crate::pull_request_detail::Actor>>,
    created_at: Option<String>,
}

impl CardFacts {
    fn from_preview(preview: &Preview) -> Self {
        Self {
            repository: preview.repository.clone(),
            number: preview.number,
            state: preview.state,
            draft: preview.is_draft,
            title: preview.title.clone(),
            author: Some(preview.author.clone()),
            created_at: Some(preview.created_at.clone()),
        }
    }

    fn from_entry(entry: &crate::pull_requests::Entry) -> Self {
        Self {
            repository: entry.repository.clone(),
            number: entry.number,
            state: entry.state,
            draft: entry.is_draft,
            title: entry.title.clone(),
            author: entry.author.as_ref().map(|actor| {
                Some(crate::pull_request_detail::Actor {
                    login: actor.login.clone(),
                    name: actor.name.clone(),
                    avatar_url: actor.avatar_url.clone(),
                    ..Default::default()
                })
            }),
            created_at: None,
        }
    }
}

/// `PullRequestLinkPreview`'s popup content.
fn card_body(facts: &CardFacts, t: &Tokens) -> AnyElement {
    let (glyph, tone, label) = super::pages::pr_state_glyph(facts.state, facts.draft, t);
    let dot = || div().flex_none().child("·");
    let meta = h_flex()
        .min_w_0()
        .gap(px(6.))
        .text_size(px(11.))
        .line_height(px(16.))
        .text_color(t.muted_foreground)
        .child(div().min_w_0().truncate().child(facts.repository.clone()))
        .child(div().flex_none().child(format!("#{}", facts.number)))
        .child(dot())
        .child(h_flex().flex_none().gap(px(4.)).child(Icon::new(glyph).size(px(12.)).text_color(tone)).child(label));
    let title = div()
        .mt(px(4.))
        .text_size(px(14.))
        .line_height(relative(1.375))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.foreground)
        .child(facts.title.clone());
    let author = facts.author.as_ref().map(|actor| {
        let actor = actor.as_ref();
        let name = match actor {
            None => "ghost".to_owned(),
            Some(actor) => match actor.name.as_deref().filter(|name| *name != actor.login) {
                Some(name) => format!("{name} (@{})", actor.login),
                None => actor.login.clone(),
            },
        };
        let opened = facts.created_at.as_deref().and_then(crate::timestamp::parse_ms).map(|created| {
            format!("opened {}", crate::timestamp::relative_label(crate::timestamp::now_ms() - created))
        });
        h_flex()
            .mt(px(8.))
            .min_w_0()
            .gap(px(6.))
            .text_size(px(12.))
            .text_color(t.muted_foreground)
            .child(super::pages::pr_avatar(actor, 16., t))
            .child(div().min_w_0().truncate().child(name))
            .when_some(opened, |row, opened| row.child(dot()).child(div().flex_none().child(opened)))
    });
    div().min_w_0().child(meta).child(title).children(author).into_any_element()
}

/// `TooltipPopup` with the link's URL (`max-w-80 wrap-anywhere`).
fn url_tooltip(url: SharedString, t: &Tokens) -> AnyElement {
    div()
        .max_w(px(320.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(6.))
        .border_1()
        .border_color(t.border)
        .bg(t.popover)
        .shadow_md()
        .text_size(px(12.))
        .line_height(relative(1.375))
        .text_color(t.foreground)
        .child(url)
        .into_any_element()
}
