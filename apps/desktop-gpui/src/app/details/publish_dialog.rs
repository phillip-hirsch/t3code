//! The Publish repository wizard (`PublishRepositoryDialog` in `GitActionsControl.tsx`):
//! Provider, then Repository, then a Summary of what was published.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, BoxShadow, Context, Div, ElementId, Entity, FocusHandle, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, div, hsla, px,
};
use serde_json::{Value, json};

use super::super::git::{Checkout, SOURCE_CONTROL_WRITE};
use super::super::source_control::{self, HOSTS, HostDefinition, Surface};
use super::super::titlebar::icon;
use super::super::{Target, Workspace};
use crate::rpc::RpcClient;
use crate::theme::{self, Tokens};

const STEPS: [&str; 3] = ["Provider", "Repository", "Summary"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Provider = 0,
    Repository = 1,
    Summary = 2,
}

/// `isStepDisabled`: only an earlier step is reachable, never the Summary, and nothing once published.
fn step_enabled(current: Step, index: usize) -> bool {
    current != Step::Summary && index < STEPS.len() - 1 && index <= current as usize
}

fn ready(discovery: Option<&Value>, kind: &str) -> bool {
    source_control::readiness(discovery, kind, Surface::Publish).ready
}

/// `publishProvider`: the picked host while it is ready, else the first ready one, else the picked
/// one, else the default host.
fn effective_provider(discovery: Option<&Value>, selected: Option<&str>) -> &'static HostDefinition {
    if let Some(kind) = selected.filter(|kind| ready(discovery, kind)) {
        return source_control::definition(kind);
    }
    source_control::sorted_by_readiness(discovery, Surface::Publish)
        .into_iter()
        .find(|host| ready(discovery, host.kind))
        .unwrap_or_else(|| source_control::definition(selected.unwrap_or_default()))
}

/// `publishRepositoryPrefill`: the signed-in account and a slash.
fn prefill(discovery: Option<&Value>, kind: &str) -> String {
    source_control::auth_account(discovery, kind).map(|account| format!("{account}/")).unwrap_or_default()
}

/// Both halves of `owner/name` are needed (`canSubmitPublishRepository`).
fn names_a_repository(repository: &str) -> bool {
    let mut parts = repository.trim().split('/');
    let owner = parts.next().unwrap_or_default().trim();
    let name = parts.collect::<Vec<_>>().join("/");
    !owner.is_empty() && !name.trim().is_empty()
}

/// The choices sent with `sourceControl.publishRepository`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PublishChoices {
    provider: &'static str,
    repository: String,
    public: bool,
    remote_name: String,
    https: bool,
}

impl PublishChoices {
    /// The input; a blank remote name is `origin`.
    fn input(&self, cwd: &str) -> Value {
        let remote = self.remote_name.trim();
        json!({
            "cwd": cwd,
            "provider": self.provider,
            "repository": self.repository.trim(),
            "visibility": if self.public { "public" } else { "private" },
            "remoteName": if remote.is_empty() { "origin" } else { remote },
            "protocol": if self.https { "https" } else { "ssh" },
        })
    }
}

/// `SourceControlPublishRepositoryResult`, as the Summary shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Published {
    name_with_owner: String,
    url: String,
    remote_name: String,
    branch: String,
    /// `status: "pushed"`; otherwise the remote was only added.
    pushed: bool,
}

impl Published {
    fn parse(value: &Value) -> Option<Self> {
        let text = |value: &Value| value.as_str().map(str::to_owned);
        Some(Self {
            name_with_owner: text(&value["repository"]["nameWithOwner"])?,
            url: text(&value["repository"]["url"])?,
            remote_name: text(&value["remoteName"])?,
            branch: text(&value["branch"])?,
            pushed: value["status"] == "pushed",
        })
    }

    /// The Summary's title and line.
    fn summary(&self, provider_label: &str) -> (&'static str, String) {
        if self.pushed {
            ("Repository published", format!("{} is now live on {provider_label}.", self.branch))
        } else {
            (
                "Repository created",
                format!("Remote \"{}\" is set up. Make a commit and push it to share your code.", self.remote_name),
            )
        }
    }
}

pub(super) struct PublishWizard {
    workspace: WeakEntity<Workspace>,
    env: SharedString,
    checkout: Checkout,
    /// The target the wizard opened over, where a failure goes once the wizard is closed.
    origin: Option<Target>,
    discovery: Option<Value>,
    step: Step,
    selected: Option<&'static str>,
    repository: Entity<InputState>,
    /// `publishRepositoryOverride`: typed text wins over the account prefill.
    repository_edited: bool,
    remote: Entity<InputState>,
    public: bool,
    https: bool,
    advanced: bool,
    pending: bool,
    error: Option<String>,
    published: Option<Published>,
    /// One per [`HOSTS`] entry.
    provider_focus: Vec<FocusHandle>,
    visibility_focus: [FocusHandle; 2],
    _discover: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl PublishWizard {
    fn new(
        workspace: &Entity<Workspace>,
        env: SharedString,
        checkout: Checkout,
        client: RpcClient,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let repository = cx.new(|cx| InputState::new(window, cx));
        let remote = cx.new(|cx| InputState::new(window, cx).default_value("origin").placeholder("origin"));
        let subscriptions = vec![
            cx.subscribe_in(&repository, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.repository_edited = true;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.submit(window, cx),
                _ => {}
            }),
            // Grants and the checkout's running action live on the workspace.
            cx.observe(workspace, |_, _, cx| cx.notify()),
        ];
        let discover = cx.spawn_in(window, async move |this, cx| {
            let discovery = client.request("server.discoverSourceControl", json!({})).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match discovery {
                    Ok(discovery) => this.discovery = Some(discovery),
                    // As the web's query: no discovery reads as every host unavailable.
                    Err(error) => log::warn!("server.discoverSourceControl failed: {error}"),
                }
                this.sync_prefill(window, cx);
                this.focus_provider(window, cx);
                cx.notify();
            });
        });
        let origin = workspace.read(cx).target.clone();
        Self {
            workspace: workspace.downgrade(),
            env,
            checkout,
            origin,
            discovery: None,
            step: Step::Provider,
            selected: None,
            repository,
            repository_edited: false,
            remote,
            public: false,
            https: false,
            advanced: false,
            pending: false,
            error: None,
            published: None,
            provider_focus: HOSTS.iter().map(|_| cx.focus_handle().tab_stop(true)).collect(),
            visibility_focus: [cx.focus_handle().tab_stop(true), cx.focus_handle().tab_stop(true)],
            _discover: discover,
            _subscriptions: subscriptions,
        }
    }

    fn discovery(&self) -> Option<&Value> {
        self.discovery.as_ref()
    }

    fn provider(&self) -> &'static HostDefinition {
        effective_provider(self.discovery(), self.selected)
    }

    fn provider_ready(&self) -> bool {
        ready(self.discovery(), self.provider().kind)
    }

    /// `publishRepositoryAction.isAllowed`, read live: grants change on reconnect.
    fn allowed(&self, cx: &App) -> bool {
        self.workspace.upgrade().is_some_and(|workspace| workspace.read(cx).env_grants(&self.env, SOURCE_CONTROL_WRITE))
    }

    /// Another git action on this checkout holds it.
    fn checkout_busy(&self, cx: &App) -> bool {
        self.workspace.upgrade().is_some_and(|workspace| workspace.read(cx).vcs.is_running(&self.checkout))
    }

    fn can_submit(&self, cx: &App) -> bool {
        self.allowed(cx)
            && self.provider_ready()
            && !self.pending
            && !self.checkout_busy(cx)
            && names_a_repository(&self.repository.read(cx).value())
    }

    /// Shows the account prefill until the person types their own repository.
    fn sync_prefill(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let provider = self.provider();
        self.repository.update(cx, |input, cx| {
            input.set_placeholder(provider.path_hint, window, cx);
        });
        if self.repository_edited {
            return;
        }
        let value = prefill(self.discovery(), provider.kind);
        self.repository.update(cx, |input, cx| {
            if *input.value() != *value {
                input.set_value(value, window, cx);
            }
        });
    }

    fn select(&mut self, kind: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(kind);
        self.repository_edited = false;
        self.sync_prefill(window, cx);
        cx.notify();
    }

    fn focus_provider(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.step != Step::Provider {
            return;
        }
        let kind = self.provider().kind;
        if let Some(index) = HOSTS.iter().position(|host| host.kind == kind) {
            self.provider_focus[index].focus(window, cx);
        }
    }

    /// The radio group's arrow keys: move to the next ready host, looping, and pick it.
    fn step_provider(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let forward = match event.keystroke.key.as_str() {
            "right" | "down" => true,
            "left" | "up" => false,
            _ => return,
        };
        let options: Vec<_> = source_control::sorted_by_readiness(self.discovery(), Surface::Publish)
            .into_iter()
            .filter(|host| ready(self.discovery(), host.kind))
            .collect();
        let Some(current) = options.iter().position(|host| host.kind == self.provider().kind) else { return };
        let next = options[(current + if forward { 1 } else { options.len() - 1 }) % options.len()];
        cx.stop_propagation();
        self.select(next.kind, window, cx);
        self.focus_provider(window, cx);
    }

    fn step_visibility(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(event.keystroke.key.as_str(), "right" | "down" | "left" | "up") || self.pending {
            return;
        }
        cx.stop_propagation();
        self.public = !self.public;
        self.visibility_focus[usize::from(self.public)].focus(window, cx);
        cx.notify();
    }

    fn go_to(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        self.step = step;
        match step {
            Step::Provider => self.focus_provider(window, cx),
            Step::Repository => self.repository.update(cx, |input, cx| input.focus(window, cx)),
            Step::Summary => {}
        }
        cx.notify();
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |this, cx| this.open_settings("source-control", cx));
        }
    }

    fn submit(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.step != Step::Repository || !self.can_submit(cx) {
            return;
        }
        let Some(workspace) = self.workspace.upgrade() else { return };
        let choices = PublishChoices {
            provider: self.provider().kind,
            repository: self.repository.read(cx).value().to_string(),
            public: self.public,
            remote_name: self.remote.read(cx).value().to_string(),
            https: self.https,
        };
        let (env, checkout, origin, wizard) =
            (self.env.clone(), self.checkout.clone(), self.origin.clone(), cx.entity().downgrade());
        let started =
            workspace.update(cx, |this, cx| this.publish_repository(&env, checkout, &choices, origin, wizard, cx));
        if started {
            self.pending = true;
            self.error = None;
            cx.notify();
        }
    }

    /// The publish's answer, while the wizard is still open.
    fn finish(&mut self, result: Result<Value, String>, cx: &mut Context<Self>) {
        self.pending = false;
        match result.and_then(|value| Published::parse(&value).ok_or_else(|| "Publish result unavailable.".to_owned()))
        {
            Ok(published) => {
                self.published = Some(published);
                self.step = Step::Summary;
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }
}

// MARK: Rendering

fn white(alpha: f32) -> Hsla {
    hsla(0., 0., 1., alpha)
}

fn black(alpha: f32) -> Hsla {
    hsla(0., 0., 0., alpha)
}

/// `focus-visible:ring-2 ring-inset ring-ring`.
fn focus_ring(color: Hsla) -> BoxShadow {
    BoxShadow::new(px(0.), px(0.), color).spread_radius(px(2.)).inset()
}

/// A radio card (`RadioPrimitive.Root` in the wizard): primary-ringed when picked.
fn radio_card(
    id: impl Into<ElementId>,
    focus: &FocusHandle,
    selected: bool,
    enabled: bool,
    t: &Tokens,
) -> gpui_kit::Stateful<Div> {
    let ring = t.ring;
    let card = h_flex()
        .id(id.into())
        .track_focus(focus)
        .gap(px(12.))
        .px(px(12.))
        .rounded(px(8.))
        .border_1()
        .text_left()
        .focus_visible(move |style| style.shadow(vec![focus_ring(ring)]));
    let card = if selected {
        let (border, bg, inset) = if t.dark {
            (gpui_kit::transparent_black(), t.primary.opacity(0.1), (1., t.primary.opacity(0.3)))
        } else {
            (t.primary, t.background, (2., t.primary.opacity(0.35)))
        };
        card.border_color(border)
            .bg(bg)
            .shadow(vec![BoxShadow::new(px(0.), px(0.), inset.1).spread_radius(px(inset.0)).inset()])
    } else if t.dark {
        let hover = t.accent;
        card.border_color(gpui_kit::transparent_black())
            .bg(white(0.035))
            .when(enabled, |el| el.hover(move |s| s.bg(hover)))
    } else {
        let (hover_border, hover_bg) = (t.foreground.opacity(0.2), t.muted.opacity(0.5));
        card.border_color(t.border)
            .bg(t.background)
            .when(enabled, |el| el.hover(move |s| s.border_color(hover_border).bg(hover_bg)))
    };
    if enabled { card.cursor_pointer() } else { card.cursor_not_allowed() }
}

impl PublishWizard {
    fn render_steps(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let label = self.provider().label;
        let summaries = [Some(label.to_owned()), self.published.as_ref().map(|p| p.name_with_owner.clone()), None];
        let current = self.step as usize;
        let ring = t.ring;
        h_flex()
            .gap(px(4.))
            .p(px(4.))
            .rounded(px(12.))
            .bg(if t.dark { white(0.04) } else { t.background })
            .border_1()
            .border_color(if t.dark { white(0.05) } else { black(0.05) })
            .children(STEPS.iter().zip(summaries).enumerate().map(|(index, (step, summary))| {
                let (here, done) = (index == current, index < current);
                let enabled = step_enabled(self.step, index);
                let badge = div()
                    .size(px(20.))
                    .flex_none()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .border_1()
                    .map(|badge| {
                        if done {
                            badge
                                .bg(t.primary)
                                .border_color(t.primary)
                                .child(icon("icons/check.svg").with_size(px(16.)).text_color(t.primary_foreground))
                        } else if here {
                            badge
                                .bg(t.primary.opacity(0.1))
                                .border_color(t.primary.opacity(0.3))
                                .text_color(t.primary)
                                .child((index + 1).to_string())
                        } else {
                            badge
                                .bg(if t.dark { white(0.05) } else { t.card })
                                .border_color(if t.dark { white(0.1) } else { black(0.1) })
                                .text_color(t.muted_foreground)
                                .child((index + 1).to_string())
                        }
                    });
                let text = match summary.filter(|_| done) {
                    Some(summary) => format!("{step}: {summary}"),
                    None => (*step).to_owned(),
                };
                h_flex()
                    .id(SharedString::from(format!("publish-step-{index}")))
                    .flex_1()
                    .min_w_0()
                    .gap(px(8.))
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(8.))
                    .when(here, |el| {
                        el.bg(t.card).border_1().border_color(if t.dark { white(0.05) } else { black(0.05) })
                    })
                    .when(enabled, |el| {
                        let card = t.card;
                        el.cursor_pointer()
                            .tab_index(0)
                            .focus_visible(move |style| style.shadow(vec![focus_ring(ring)]))
                            .when(!here, |el| el.hover(move |s| s.bg(card)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if step_enabled(this.step, index) {
                                    let step = if index == 0 { Step::Provider } else { Step::Repository };
                                    this.go_to(step, window, cx);
                                }
                            }))
                    })
                    .child(badge)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if here { t.foreground } else { t.muted_foreground })
                            .child(text),
                    )
            }))
    }

    fn render_providers(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let discovery = self.discovery();
        let picked = self.provider().kind;
        let cards = source_control::sorted_by_readiness(discovery, Surface::Publish).into_iter().map(|host| {
            let readiness = source_control::readiness(discovery, host.kind, Surface::Publish);
            let index = HOSTS.iter().position(|h| h.kind == host.kind).unwrap_or_default();
            let focus = &self.provider_focus[index];
            let label = div()
                .min_w_0()
                .flex_1()
                .truncate()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.foreground)
                .child(host.label);
            let id = SharedString::from(format!("publish-provider-{}", host.kind));
            if !readiness.ready {
                let hint: SharedString = readiness
                    .hint
                    .unwrap_or_else(|| "Open Settings -> Source control to configure this provider.".into())
                    .into();
                let warning = t.warning;
                let setup = div()
                    .id(SharedString::from(format!("publish-setup-{}", host.kind)))
                    .flex_none()
                    .h(px(20.))
                    .px(px(5.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(warning.opacity(0.32))
                    .bg(warning.opacity(0.08))
                    .text_size(px(11.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.warning_foreground)
                    .cursor_pointer()
                    .tab_index(0)
                    .focus_visible({
                        let ring = t.ring;
                        move |style| style.shadow(vec![focus_ring(ring)])
                    })
                    .hover(move |s| s.border_color(warning.opacity(0.4)).bg(warning.opacity(0.16)))
                    .child("Setup Required")
                    .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
                    .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx)));
                return div()
                    .id(id)
                    .child(
                        h_flex()
                            .gap(px(12.))
                            .px(px(12.))
                            .py(px(12.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(if t.dark { gpui_kit::transparent_black() } else { t.border })
                            .bg(if t.dark { white(0.035) } else { t.background })
                            .opacity(0.64)
                            .cursor_not_allowed()
                            .child(host.mark(20., t.muted_foreground))
                            .child(label)
                            .child(setup),
                    )
                    .into_any_element();
            }
            let kind = host.kind;
            radio_card(id, focus, kind == picked, true, t)
                .py(px(12.))
                .child(host.mark(20., t.foreground))
                .child(label)
                .on_click(cx.listener(move |this, _, window, cx| this.select(kind, window, cx)))
                .into_any_element()
        });
        let cards: Vec<_> = cards.collect();
        v_flex()
            .gap(px(8.))
            .child(div().text_xs().font_weight(FontWeight::MEDIUM).child("Provider"))
            .child(
                div()
                    .id("publish-providers")
                    .grid()
                    .grid_cols(2)
                    .gap(px(8.))
                    .on_key_down(cx.listener(Self::step_provider))
                    .children(cards),
            )
            .into_any_element()
    }

    fn render_repository(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let provider = self.provider();
        let host = provider.publish_host(source_control::auth_host(self.discovery(), provider.kind));
        let pending = self.pending;
        let repository =
            v_flex().gap(px(8.)).child(div().text_xs().font_weight(FontWeight::MEDIUM).child("Repository")).child(
                h_flex()
                    .items_stretch()
                    .overflow_hidden()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(t.input)
                    .bg(t.background)
                    .child(
                        h_flex()
                            .flex_none()
                            .gap(px(6.))
                            .px(px(10.))
                            .border_r_1()
                            .border_color(t.input)
                            .bg(t.muted.opacity(0.5))
                            .font_family(t.fonts.mono)
                            .text_xs()
                            .text_color(t.muted_foreground)
                            .child(provider.mark(14., t.muted_foreground))
                            .child(format!("{host}/")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family(t.fonts.mono)
                            .child(Input::new(&self.repository).appearance(false).disabled(pending)),
                    ),
            );
        let visibility_option = |public: bool, cx: &mut Context<Self>| {
            let (label, description, glyph) = if public {
                ("Public", "Anyone on the web", "icons/globe.svg")
            } else {
                ("Private", "Only invited people", "icons/lock.svg")
            };
            radio_card(
                if public { "publish-public" } else { "publish-private" },
                &self.visibility_focus[usize::from(public)],
                self.public == public,
                !pending,
                t,
            )
            .py(px(10.))
            .child(icon(glyph).with_size(px(16.)).text_color(t.muted_foreground))
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(t.foreground).child(label))
                    .child(div().text_xs().text_color(t.muted_foreground).child(description)),
            )
            .when(!pending, |el| {
                el.on_click(cx.listener(move |this, _, _, cx| {
                    this.public = public;
                    cx.notify();
                }))
            })
        };
        let visibility =
            v_flex().gap(px(8.)).child(div().text_xs().font_weight(FontWeight::MEDIUM).child("Visibility")).child(
                div()
                    .id("publish-visibility")
                    .grid()
                    .grid_cols(2)
                    .gap(px(8.))
                    .on_key_down(cx.listener(Self::step_visibility))
                    .child(visibility_option(false, cx))
                    .child(visibility_option(true, cx)),
            );
        let open = self.advanced;
        let hover = t.foreground;
        let ring = t.ring;
        let toggle = h_flex()
            .id("publish-advanced")
            .gap(px(4.))
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.muted_foreground)
            .cursor_pointer()
            .tab_index(0)
            .rounded(px(4.))
            .focus_visible(move |style| style.shadow(vec![focus_ring(ring)]))
            .hover(move |style| style.text_color(hover))
            .child(icon(if open { "icons/chevron-down.svg" } else { "icons/chevron-right.svg" }).with_size(px(14.)))
            .child("Advanced")
            .on_click(cx.listener(|this, _, _, cx| {
                this.advanced = !this.advanced;
                cx.notify();
            }));
        let protocol = |https: bool, cx: &mut Context<Self>| {
            Button::new(if https { "publish-https" } else { "publish-ssh" })
                .small()
                .label(if https { "HTTPS" } else { "SSH" })
                .map(|button| if self.https == https { button.primary() } else { button.outline() })
                .disabled(pending)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.https = https;
                    cx.notify();
                }))
        };
        let small_field = |label: &'static str, control: AnyElement| {
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(6.))
                .child(div().text_xs().font_weight(FontWeight::MEDIUM).child(label))
                .child(control)
        };
        let options = open.then(|| {
            h_flex()
                .mt_3()
                .gap_3()
                .items_start()
                .child(small_field("Remote", Input::new(&self.remote).disabled(pending).into_any_element()))
                .child(small_field(
                    "Protocol",
                    h_flex().gap_2().child(protocol(false, cx)).child(protocol(true, cx)).into_any_element(),
                ))
        });
        let status = if pending {
            Some(
                h_flex()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(if t.dark { gpui_kit::transparent_black() } else { t.input })
                    .bg(if t.dark { white(0.035) } else { t.muted.opacity(0.4) })
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child(Spinner::new().xsmall())
                    .child(format!("Publishing repository to {}...", provider.label))
                    .into_any_element(),
            )
        } else {
            self.error.clone().map(|error| {
                v_flex()
                    .px_3()
                    .py_2()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(t.error.opacity(0.4))
                    .bg(t.error.opacity(0.1))
                    .text_xs()
                    .text_color(t.error)
                    .child(div().font_weight(FontWeight::MEDIUM).child("Publish failed"))
                    .child(div().mt(px(2.)).text_color(t.error.opacity(0.9)).child(error))
                    .into_any_element()
            })
        };
        v_flex()
            .gap(px(20.))
            .child(repository)
            .child(visibility)
            .child(v_flex().child(toggle).children(options))
            .children(status)
            .into_any_element()
    }

    fn render_summary(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let Some(published) = &self.published else {
            return div()
                .text_xs()
                .text_color(t.muted_foreground)
                .child("Publish result unavailable.")
                .into_any_element();
        };
        let provider = self.provider();
        let (title, line) = published.summary(provider.label);
        let url = published.url.clone();
        v_flex()
            .gap_4()
            .child(
                v_flex()
                    .items_center()
                    .gap_2()
                    .py_1()
                    .child(
                        div()
                            .size(px(32.))
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(t.success_foreground.opacity(0.15))
                            .child(icon("icons/check.svg").with_size(px(16.)).text_color(t.success_foreground)),
                    )
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(t.foreground).child(title))
                    .child(div().max_w(px(320.)).text_center().text_xs().text_color(t.muted_foreground).child(line)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(if t.dark { gpui_kit::transparent_black() } else { t.input })
                    .bg(if t.dark { white(0.035) } else { t.muted.opacity(0.4) })
                    .child(provider.mark(14., t.muted_foreground))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(t.fonts.mono)
                            .text_xs()
                            .text_color(t.foreground)
                            .child(published.name_with_owner.clone()),
                    ),
            )
            .child(
                Button::new("publish-open")
                    .outline()
                    .small()
                    .w_full()
                    .label(format!("Open on {}", provider.label))
                    .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
            )
            .into_any_element()
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let footer = h_flex().justify_end().gap(px(8.)).px(px(24.)).py(px(16.));
        if self.step == Step::Summary {
            return footer.child(
                Button::new("publish-done")
                    .primary()
                    .small()
                    .label("Done")
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            );
        }
        let back = Button::new("publish-back")
            .outline()
            .small()
            .label(if self.step == Step::Provider { "Cancel" } else { "Back" })
            .disabled(self.pending)
            .on_click(cx.listener(|this, _, window, cx| match this.step {
                Step::Provider => window.close_dialog(cx),
                _ => this.go_to(Step::Provider, window, cx),
            }));
        let next = if self.step == Step::Provider {
            Button::new("publish-next").primary().small().label("Next").disabled(!self.provider_ready()).on_click(
                cx.listener(|this, _, window, cx| {
                    if this.provider_ready() {
                        this.go_to(Step::Repository, window, cx);
                    }
                }),
            )
        } else {
            Button::new("publish-confirm")
                .primary()
                .small()
                .label(if self.pending { "Publishing..." } else { "Publish" })
                .loading(self.pending)
                .disabled(!self.can_submit(cx))
                .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
        };
        footer.child(back).child(next)
    }
}

impl Render for PublishWizard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let header = v_flex()
            .gap(px(8.))
            .p(px(24.))
            .pb(px(12.))
            .child(div().text_xl().line_height(px(27.5)).font_weight(FontWeight::SEMIBOLD).child("Publish repository"))
            .child(
                div()
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .child("Pick where to host it, then point us at a repo to push to."),
            )
            .child(self.render_steps(&t, cx));
        let body = match self.step {
            Step::Provider => self.render_providers(&t, cx),
            Step::Repository => self.render_repository(&t, cx),
            Step::Summary => self.render_summary(&t, cx),
        };
        let panel = v_flex()
            .min_w_0()
            .px(px(24.))
            .py(px(20.))
            .bg(if t.dark { white(0.02) } else { t.background.opacity(0.8) })
            .border_y_1()
            .border_color(if t.dark { white(0.05) } else { black(0.05) })
            .child(body);
        // `WizardPopup` scrolls its whole content inside the window.
        let max_h = window.viewport_size().height - px(64.);
        v_flex()
            .id("publish-wizard")
            .max_h(max_h)
            .overflow_y_scroll()
            .child(header)
            .child(panel)
            .child(self.render_footer(cx))
    }
}

impl Workspace {
    /// `PublishRepositoryDialog` on the watched checkout.
    pub(super) fn open_publish_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_write_source_control() {
            return;
        }
        let Some(checkout) = self.vcs.checkout() else { return };
        let Some(env) = checkout.0.clone() else { return };
        let Some(client) = self.env_client(&env) else { return };
        let workspace = cx.entity();
        // The wizard reads the workspace, which this call is still updating.
        window.defer(cx, move |window, cx| {
            let wizard = cx.new(|cx| PublishWizard::new(&workspace, env, checkout, client, window, cx));
            window.open_dialog(cx, move |dialog, _, _| {
                dialog
                    .entrance(gpui_kit::component::dialog::DialogEntrance::None)
                    .p_0()
                    .w(px(576.))
                    .centered()
                    // Enter belongs to the repository field, which publishes from the Repository step.
                    .on_ok(|_, _, _| false)
                    .child(wizard.clone())
            });
        });
    }

    /// `sourceControl.publishRepository` for the wizard: it holds `checkout` like any git action and
    /// answers the wizard while it is open; a failure after it closed goes to `origin`.
    fn publish_repository(
        &mut self,
        env: &str,
        checkout: Checkout,
        choices: &PublishChoices,
        origin: Option<Target>,
        wizard: WeakEntity<PublishWizard>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.env_grants(env, SOURCE_CONTROL_WRITE) {
            return false;
        }
        let Some(client) = self.env_client(env) else { return false };
        if !self.vcs.start_on(checkout.clone(), "Publishing repository...") {
            return false;
        }
        let input = choices.input(&checkout.1);
        cx.spawn(async move |this, cx| {
            let result =
                client.request("sourceControl.publishRepository", input).await.map_err(|error| error.message());
            let _ = this.update(cx, |this, cx| {
                let delivered = wizard.upgrade().inspect(|wizard| {
                    wizard.update(cx, |wizard, cx| wizard.finish(result.clone(), cx));
                });
                let error = result.err().filter(|_| delivered.is_none());
                this.finish_vcs(client, checkout, origin, error, cx);
            });
        })
        .detach();
        cx.notify();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discovery(items: &[(&str, &str, Option<&str>)]) -> Value {
        let providers: Vec<_> = items
            .iter()
            .map(|(kind, auth, account)| {
                let account = match account {
                    Some(account) => json!({"_tag": "Some", "value": account}),
                    None => json!({"_tag": "None"}),
                };
                json!({"kind": kind, "label": kind, "status": "available", "installHint": "install",
                    "auth": {"status": auth, "account": account, "host": {"_tag": "None"}, "detail": {"_tag": "None"}}})
            })
            .collect();
        json!({"sourceControlProviders": providers})
    }

    #[test]
    fn the_picked_provider_holds_only_while_it_is_ready() {
        let d = discovery(&[("gitlab", "authenticated", Some("me")), ("github", "unauthenticated", None)]);
        let d = Some(&d);
        assert_eq!(effective_provider(d, None).kind, "gitlab", "the first ready host when nothing is picked");
        assert_eq!(effective_provider(d, Some("github")).kind, "gitlab", "a signed-out pick falls back");
        let both = discovery(&[("gitlab", "authenticated", None), ("bitbucket", "unknown", None)]);
        assert_eq!(effective_provider(Some(&both), Some("gitlab")).kind, "gitlab");
        assert_eq!(effective_provider(Some(&both), None).kind, "bitbucket", "ready hosts sort by label");
        assert_eq!(effective_provider(None, Some("forgejo")).kind, "forgejo", "nothing ready keeps the pick");
        assert_eq!(effective_provider(None, None).kind, "github", "else the default host");
        assert_eq!(prefill(d, "gitlab"), "me/");
        assert_eq!(prefill(d, "github"), "");
    }

    #[test]
    fn only_earlier_steps_are_reachable_and_none_once_published() {
        assert!(step_enabled(Step::Repository, 0));
        assert!(step_enabled(Step::Repository, 1));
        assert!(!step_enabled(Step::Provider, 1), "a later step waits for Next");
        assert!(!step_enabled(Step::Repository, 2), "the Summary comes only from publishing");
        assert!(!step_enabled(Step::Summary, 0));
    }

    #[test]
    fn a_repository_needs_an_owner_and_a_name() {
        assert!(names_a_repository("me/app"));
        assert!(names_a_repository(" group/sub/project "));
        assert!(!names_a_repository("me/"), "the bare prefill can't publish");
        assert!(!names_a_repository("/app"));
        assert!(!names_a_repository("app"));
        assert!(!names_a_repository("me/ "));
    }

    #[test]
    fn the_publish_input_defaults_a_blank_remote_to_origin() {
        let choices = PublishChoices {
            provider: "github",
            repository: " me/app ".into(),
            public: false,
            remote_name: "  ".into(),
            https: false,
        };
        assert_eq!(
            choices.input("/repo"),
            json!({"cwd": "/repo", "provider": "github", "repository": "me/app", "visibility": "private",
                "remoteName": "origin", "protocol": "ssh"})
        );
        let custom = PublishChoices { public: true, remote_name: "upstream".into(), https: true, ..choices };
        assert_eq!(custom.input("/repo")["visibility"], "public");
        assert_eq!(custom.input("/repo")["remoteName"], "upstream");
        assert_eq!(custom.input("/repo")["protocol"], "https");
    }

    #[test]
    fn the_summary_says_whether_it_pushed_or_only_added_the_remote() {
        let result = json!({
            "repository": {"provider": "github", "nameWithOwner": "me/app", "url": "https://github.com/me/app", "sshUrl": "git@github.com:me/app.git"},
            "remoteName": "origin", "remoteUrl": "git@github.com:me/app.git", "branch": "main", "status": "pushed",
        });
        let pushed = Published::parse(&result).unwrap();
        assert_eq!(pushed.summary("GitHub"), ("Repository published", "main is now live on GitHub.".to_owned()));
        let mut added = result;
        added["status"] = "remote_added".into();
        assert_eq!(
            Published::parse(&added).unwrap().summary("GitHub"),
            (
                "Repository created",
                "Remote \"origin\" is set up. Make a commit and push it to share your code.".to_owned()
            )
        );
        assert_eq!(Published::parse(&json!({"status": "pushed"})), None);
    }
}
