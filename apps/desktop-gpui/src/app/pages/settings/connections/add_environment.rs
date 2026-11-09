//! `AddEnvironmentDialog`: pairs a new environment, or another route to a saved one, by a
//! "Remote link" host and pairing code or over SSH with the system `ssh`. T3 Connect routes
//! need an account this client does not have.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, Context, ElementId, Entity, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, Keystroke, MouseButton, ParentElement as _, Pixels, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, anchored, canvas, deferred,
    div, px,
};

use super::ui::{self, Size, WebButton, text_sm, text_xs};
use super::{error_toast, success_toast};
use crate::app::environments::{SavedEnvironments, Saving};
use crate::app::keymap::{self, ShortcutContext};
use crate::environments::{self, Address, SavedEnvironment};
use crate::keybindings::Rule;
use crate::ssh::config::{self, DiscoveredHost, HostSource};
use crate::ssh::{self, SshTarget};
use crate::theme::{self, Tokens};

/// The saved environment "Add route" pairs another address for.
#[derive(Clone)]
pub(super) struct RouteTarget {
    pub environment_id: String,
    pub label: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Remote,
    Ssh,
}

/// Waits off the UI thread for an edit to be on disk; a dropped list saved nothing.
async fn finished(saving: anyhow::Result<Saving>) -> Result<(), String> {
    match saving {
        Ok(saving) => saving.finished().await,
        Err(_) => Err("The environment list is no longer available.".into()),
    }
}

/// `formatDesktopSshConnectionError`.
fn ssh_error_message(message: &str) -> String {
    match message.trim() {
        "" => "Failed to connect SSH host.".to_owned(),
        message => message.to_owned(),
    }
}

/// `savedDesktopSshEnvironmentKeys`: the aliases and addresses saved SSH routes use.
fn saved_ssh_keys(saved: &[SavedEnvironment]) -> Vec<String> {
    let targets = saved.iter().flat_map(|environment| &environment.routes).filter_map(|route| match &route.address {
        Address::Ssh(target) => Some(target),
        Address::Direct(_) => None,
    });
    targets.flat_map(|target| [target.alias.clone(), target.display()]).collect()
}

/// `unsavedDiscoveredSshHosts`: hosts no saved SSH route uses yet.
fn unsaved_hosts(hosts: &[DiscoveredHost], keys: &[String]) -> Vec<DiscoveredHost> {
    let saved = |key: &str| keys.iter().any(|saved| saved == key);
    hosts
        .iter()
        .filter(|host| !saved(&host.alias) && (host.source == HostSource::SshConfig || !saved(&host.hostname)))
        .cloned()
        .collect()
}

/// The highlighted suggestion after an arrow key: down from none is the first, up the last.
fn step_highlight(current: Option<usize>, count: usize, down: bool) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(match (current, down) {
        (None, true) => 0,
        (None, false) => count - 1,
        (Some(index), true) => (index + 1) % count,
        (Some(index), false) => (index + count - 1) % count,
    })
}

pub(super) struct AddForm {
    mode: Mode,
    host: Entity<InputState>,
    code: Entity<InputState>,
    ssh_host: Entity<InputState>,
    ssh_user: Entity<InputState>,
    ssh_port: Entity<InputState>,
    busy: bool,
    error: Option<SharedString>,
    saved: WeakEntity<SavedEnvironments>,
    /// The workspace's own environment, which is never saved again.
    primary_id: Option<String>,
    route: Option<RouteTarget>,
    /// A pairing that could not be saved, with the host and code it used. The code is spent,
    /// so submitting the same fields again retries only the save.
    unsaved: Option<(String, String, SavedEnvironment)>,
    /// The last hosts discovered, kept on screen while discovery runs again.
    hosts: Option<Vec<DiscoveredHost>>,
    suggestions_open: bool,
    highlighted: Option<usize>,
    keymap: Rc<[Rule]>,
    /// Where the host field is, so its suggestions line up under it.
    host_bounds: Rc<Cell<Bounds<Pixels>>>,
    _discovery: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl AddForm {
    fn new(
        saved: WeakEntity<SavedEnvironments>,
        primary_id: Option<String>,
        route: Option<RouteTarget>,
        keymap: Rc<[Rule]>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let host = input("backend.example.com", window, cx);
        let code = input("PAIRCODE", window, cx);
        let ssh_host = input("Search hosts or type devbox", window, cx);
        let ssh_user = input("root", window, cx);
        let ssh_port = input("22", window, cx);
        let on_host = |this: &mut Self,
                       _: &Entity<InputState>,
                       event: &InputEvent,
                       window: &mut Window,
                       cx: &mut Context<Self>| {
            if let InputEvent::Change = event {
                // `handleSavedBackendHostChange`: a pasted pairing URL fills both fields.
                if let Some((host, code)) = environments::parse_pairing_url(&this.host.read(cx).value()) {
                    this.host.update(cx, |input, cx| input.set_value(host, window, cx));
                    this.code.update(cx, |input, cx| input.set_value(code, window, cx));
                }
                cx.notify();
            }
        };
        let on_ssh_host = |this: &mut Self,
                           _: &Entity<InputState>,
                           event: &InputEvent,
                           window: &mut Window,
                           cx: &mut Context<Self>| {
            match event {
                InputEvent::Change => {
                    this.suggestions_open = true;
                    this.highlighted = None;
                }
                InputEvent::Blur => this.suggestions_open = false,
                // A highlighted suggestion takes Enter first (`on_suggestion_key`).
                InputEvent::PressEnter { .. } => this.submit_ssh_field(window, cx),
                InputEvent::Focus => {}
            }
            cx.notify();
        };
        let on_ssh_field = |this: &mut Self,
                            _: &Entity<InputState>,
                            event: &InputEvent,
                            window: &mut Window,
                            cx: &mut Context<Self>| {
            if let InputEvent::PressEnter { .. } = event {
                this.submit_ssh_field(window, cx);
            }
        };
        let form = cx.weak_entity();
        let subscriptions = vec![
            cx.subscribe_in(&host, window, on_host),
            cx.subscribe_in(&ssh_host, window, on_ssh_host),
            cx.subscribe_in(&ssh_user, window, on_ssh_field),
            cx.subscribe_in(&ssh_port, window, on_ssh_field),
            // Ahead of the field's and the dialog's own keys, while the suggestions show.
            cx.intercept_keystrokes(move |event, window, cx| {
                let consumed = form.update(cx, |form, cx| form.on_suggestion_key(&event.keystroke, window, cx));
                if consumed.unwrap_or(false) {
                    cx.stop_propagation();
                }
            }),
        ];
        Self {
            mode: Mode::Remote,
            host,
            code,
            ssh_host,
            ssh_user,
            ssh_port,
            busy: false,
            error: None,
            saved,
            primary_id,
            route,
            unsaved: None,
            hosts: None,
            suggestions_open: false,
            highlighted: None,
            keymap,
            host_bounds: Rc::default(),
            _discovery: None,
            _subscriptions: subscriptions,
        }
    }

    fn submit_label(&self) -> &'static str {
        match (self.busy, self.route.is_some()) {
            (true, _) => "Adding\u{2026}",
            (false, true) => "Add route",
            (false, false) => "Add environment",
        }
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.busy || self.mode == mode {
            return;
        }
        self.mode = mode;
        if mode == Mode::Ssh {
            self.discover(cx);
        }
        cx.notify();
    }

    /// Reads the SSH config each time the SSH tab is shown; stale hosts stay meanwhile.
    fn discover(&mut self, cx: &mut Context<Self>) {
        self._discovery = Some(cx.spawn(async move |this, cx| {
            let hosts = cx
                .background_executor()
                .spawn(async { std::env::home_dir().map(|home| config::discover(&home)).unwrap_or_default() })
                .await;
            drop(this.update(cx, |this, cx| {
                this.hosts = Some(hosts);
                this._discovery = None;
                cx.notify();
            }));
        }));
    }

    /// The suggestions under the host field, filtered by what it holds.
    fn suggestions(&self, cx: &App) -> Vec<DiscoveredHost> {
        let Some(hosts) = &self.hosts else { return Vec::new() };
        let keys = self.saved.upgrade().map(|saved| saved_ssh_keys(saved.read(cx).environments())).unwrap_or_default();
        let unsaved = unsaved_hosts(hosts, &keys);
        let query = self.ssh_host.read(cx).value();
        config::filter_hosts(&unsaved, &query).into_iter().cloned().collect()
    }

    /// `hasSshHostSuggestionContent`: loading, or some host not saved yet.
    fn has_suggestion_content(&self, cx: &App) -> bool {
        match &self.hosts {
            None => self._discovery.is_some(),
            Some(hosts) => {
                let keys =
                    self.saved.upgrade().map(|saved| saved_ssh_keys(saved.read(cx).environments())).unwrap_or_default();
                !unsaved_hosts(hosts, &keys).is_empty()
            }
        }
    }

    fn suggestions_visible(&self, window: &Window, cx: &App) -> bool {
        self.mode == Mode::Ssh
            && self.suggestions_open
            && !self.busy
            && self.ssh_host.read(cx).focus_handle(cx).is_focused(window)
            && self.has_suggestion_content(cx)
    }

    /// `handleSavedBackendSshHostKeyDown` while the suggestions show: the thread-jump shortcut
    /// picks that host, arrows move the highlight, Enter picks it, Escape closes the list.
    fn on_suggestion_key(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.suggestions_visible(window, cx) {
            return false;
        }
        let suggestions = self.suggestions(cx);
        let command = keymap::resolve(&self.keymap, keystroke, &ShortcutContext::default());
        if let Some(index) = command.and_then(|command| keymap::jump_index(command, "thread"))
            && let Some(host) = suggestions.get(index)
        {
            self.select_suggestion(host.clone(), window, cx);
            return true;
        }
        if keystroke.modifiers.modified() {
            return false;
        }
        match keystroke.key.as_str() {
            "escape" => self.suggestions_open = false,
            "down" | "up" => {
                self.highlighted = step_highlight(self.highlighted, suggestions.len(), keystroke.key == "down");
            }
            "enter" => match self.highlighted.and_then(|index| suggestions.get(index)) {
                Some(host) => {
                    self.select_suggestion(host.clone(), window, cx);
                    return true;
                }
                None => return false,
            },
            _ => return false,
        }
        cx.notify();
        true
    }

    /// `handleSavedBackendSshFieldKeyDown`: Enter adds once a host is typed.
    fn submit_ssh_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ssh_host.read(cx).value().trim().is_empty() {
            self.submit(window, cx);
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        match self.mode {
            Mode::Remote => self.submit_remote(window, cx),
            Mode::Ssh => {
                let target = ssh::parse_manual_target(
                    &self.ssh_host.read(cx).value(),
                    &self.ssh_user.read(cx).value(),
                    &self.ssh_port.read(cx).value(),
                );
                match target {
                    Ok(target) => self.connect_ssh(target, window, cx),
                    Err(message) => {
                        self.error = Some(message.into());
                        cx.notify();
                    }
                }
            }
        }
    }

    /// `handleSelectSshHostSuggestion`: resolves the alias with `ssh -G`, shows what it
    /// resolved to, then connects.
    fn select_suggestion(&mut self, host: DiscoveredHost, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        self.suggestions_open = false;
        self.highlighted = None;
        self.ssh_host.update(cx, |input, cx| input.set_value(host.alias.clone(), window, cx));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let alias = host.alias.clone();
            let resolved = crate::runtime().spawn(async move { ssh::resolve_target(&alias).await }).await;
            let resolved = resolved.unwrap_or_else(|error| Err(ssh::SshError { message: error.to_string() }));
            drop(this.update_in(cx, |this, window, cx| match resolved {
                Ok(target) => {
                    let port = target.port.map(|port| port.to_string()).unwrap_or_default();
                    let user = target.username.clone().unwrap_or_default();
                    this.ssh_user.update(cx, |input, cx| input.set_value(user, window, cx));
                    this.ssh_port.update(cx, |input, cx| input.set_value(port, window, cx));
                    this.busy = false;
                    this.connect_ssh(target, window, cx);
                }
                Err(error) => {
                    this.busy = false;
                    this.error = Some(ssh_error_message(&error.message).into());
                    cx.notify();
                }
            }));
        })
        .detach();
    }

    /// `connectSavedBackendSshTarget`: provisions T3 Code over SSH and saves the route. The
    /// save happens even if the dialog closed meanwhile, as the pairing is already spent.
    fn connect_ssh(&mut self, target: SshTarget, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let (saved, primary, route) = (self.saved.clone(), self.primary_id.clone(), self.route.clone());
        let expected = route.as_ref().map(|route| route.environment_id.clone());
        let alias = target.alias.clone();
        cx.spawn_in(window, async move |this, cx| {
            let paired = crate::runtime().spawn(environments::pair_ssh(target, expected)).await;
            let paired = paired.unwrap_or_else(|error| Err(error.to_string()));
            let saving = match paired {
                // The workspace's own environment is already here.
                Ok(paired) if primary.as_deref() == Some(paired.environment_id.as_str()) => None,
                Ok(paired) => Some(saved.update(cx, |saved, cx| saved.register(paired, false, cx))),
                Err(error) => {
                    drop(this.update(cx, |this, cx| {
                        this.busy = false;
                        this.error = Some(ssh_error_message(&error).into());
                        cx.notify();
                    }));
                    return;
                }
            };
            let result = match saving {
                Some(saving) => finished(saving).await,
                None => Ok(()),
            };
            drop(this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        window.close_dialog(cx);
                        let (title, description) = match &route {
                            Some(route) => {
                                ("Route added", format!("{} can now be reached over SSH {alias}.", route.label))
                            }
                            None => ("Environment connected", format!("{alias} is ready over an SSH-managed tunnel.")),
                        };
                        success_toast(window, cx, title, description);
                    }
                    Err(error) => {
                        this.error = Some(ssh_error_message(&error).into());
                        cx.notify();
                    }
                }
            }));
        })
        .detach();
    }

    fn submit_remote(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fields =
            environments::parse_remote_pairing_fields(&self.host.read(cx).value(), &self.code.read(cx).value());
        let (host, code) = match fields {
            Ok(fields) => fields,
            Err(message) => return self.fail(message.to_owned(), window, cx),
        };
        if let Some((_, _, paired)) = self.unsaved.take().filter(|(h, c, _)| (h, c) == (&host, &code)) {
            return self.save(host, code, paired, window, cx);
        }
        let Some(catalog) = self.saved.read_with(cx, |saved, _| saved.environments().to_vec()).ok() else { return };
        let expected = self.route.as_ref().map(|route| route.environment_id.clone());
        self.busy = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let pairing = environments::pair(host.clone(), code.clone(), catalog, expected);
            let result = crate::runtime().spawn(pairing).await;
            let result = result.unwrap_or_else(|error| Err(error.to_string()));
            drop(this.update_in(cx, |this, window, cx| match result {
                Ok(paired) => this.save(host, code, paired, window, cx),
                Err(error) => this.fail(error, window, cx),
            }));
        })
        .detach();
    }

    /// Saves a redeemed pairing; on failure the form stays open with the error, ready to retry.
    fn save(
        &mut self,
        host: String,
        code: String,
        paired: SavedEnvironment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.primary_id.as_deref() == Some(&paired.environment_id) {
            // The workspace's own environment is already here (`register` skips platform entries).
            return self.added(window, cx);
        }
        let known_address_only = self.route.is_none();
        let saving = self.saved.update(cx, |saved, cx| saved.register(paired.clone(), known_address_only, cx));
        self.busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = finished(saving).await;
            drop(this.update_in(cx, |this, window, cx| match result {
                Ok(()) => this.added(window, cx),
                Err(error) => {
                    this.unsaved = Some((host, code, paired));
                    this.fail(error, window, cx);
                }
            }));
        })
        .detach();
    }

    fn added(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.busy = false;
        window.close_dialog(cx);
        match &self.route {
            Some(route) => {
                success_toast(window, cx, "Route added", format!("{} now has another way to connect.", route.label));
            }
            None => {
                success_toast(
                    window,
                    cx,
                    "Backend added",
                    "The environment is saved and will reconnect on app startup.",
                );
            }
        }
    }

    fn fail(&mut self, message: String, window: &mut Window, cx: &mut Context<Self>) {
        self.busy = false;
        self.error = Some(message.clone().into());
        error_toast(window, cx, "Could not add backend", message);
        cx.notify();
    }

    /// `renderConnectionModeCard`.
    fn mode_card(
        &self,
        mode: Mode,
        title: &'static str,
        description: &'static str,
        icon: &'static str,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.mode == mode;
        let (border, bg) = if selected {
            (t.primary.opacity(0.5), t.primary.opacity(0.05))
        } else {
            (t.border.opacity(0.6), gpui_kit::transparent_black())
        };
        let (icon_border, icon_bg, icon_color) = if selected {
            (t.primary.opacity(0.3), t.primary.opacity(0.1), t.primary)
        } else {
            (t.border.opacity(0.7), t.background, t.muted_foreground)
        };
        let (hover, ring) = (t.muted.opacity(0.4), t.ring);
        let group: SharedString = format!("add-environment-mode-{title}").into();
        h_flex()
            .id(ElementId::Name(group.clone()))
            .group(group.clone())
            .flex_1()
            .min_w_0()
            .min_h(px(96.))
            .items_start()
            .gap(px(12.))
            .p(px(16.))
            .rounded(px(8.))
            .border_1()
            .border_color(border)
            .bg(bg)
            .tab_index(0)
            .focus_visible(move |style| style.border_color(ring))
            .when(!selected && !self.busy, |el| el.cursor_pointer().hover(move |style| style.bg(hover)))
            .when(self.busy, |el| el.opacity(0.64))
            .on_click(cx.listener(move |this, _, _, cx| this.set_mode(mode, cx)))
            .child(
                div()
                    .mt(px(2.))
                    .size(px(32.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(icon_border)
                    .bg(icon_bg)
                    .text_color(icon_color)
                    .when(!selected, |el| {
                        let foreground = t.foreground;
                        el.group_hover(group, move |style| style.text_color(foreground))
                    })
                    .child(gpui_kit::svg().path(icon).size(px(16.)).flex_none().text_color(icon_color)),
            )
            .child(v_flex().min_w_0().child(text_sm(div().font_weight(FontWeight::MEDIUM)).child(title)).child(
                text_xs(div().mt(px(4.)).line_height(px(19.5)).text_color(t.muted_foreground)).child(description),
            ))
            .into_any_element()
    }

    fn submit_button(&self, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .w_full()
            .child(
                WebButton::new("add-environment-submit", self.submit_label())
                    .size(Size::Default)
                    .icon(IconName::Plus)
                    .disabled(self.busy)
                    .full_width()
                    .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
            )
            .into_any_element()
    }

    fn remote_body(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.busy;
        let field = |label: &'static str, input: &Entity<InputState>| {
            v_flex()
                .gap(px(6.))
                .child(text_xs(div().font_weight(FontWeight::MEDIUM)).child(label))
                .child(Input::new(input).disabled(busy))
        };
        v_flex()
            .gap(px(16.))
            .child(
                v_flex()
                    .gap(px(12.))
                    .child(
                        h_flex()
                            .items_start()
                            .gap(px(12.))
                            .child(field("Host", &self.host).flex_1().min_w_0())
                            .child(field("Pairing code", &self.code).w(px(160.)).flex_none()),
                    )
                    .child(
                        ui::text_2xs(div().mt(px(4.)).text_color(t.muted_foreground))
                            .child("Paste a full pairing URL here to fill both fields automatically."),
                    ),
            )
            .when_some(self.error.clone(), |el, error| el.child(text_xs(div().text_color(t.error)).child(error)))
            .child(self.submit_button(cx))
            .into_any_element()
    }

    /// The `AutocompletePopup` under the host field.
    fn suggestion_popup(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let width = self.host_bounds.get().size.width.max(px(240.));
        let card = v_flex()
            .id("ssh-host-suggestions")
            .occlude()
            .w(width)
            .max_h(px(288.))
            .overflow_y_scroll()
            .p(px(4.))
            .rounded(px(8.))
            .border_1()
            .border_color(t.border)
            .bg(t.popover)
            .shadow_lg();
        let card = if self.hosts.is_none() {
            card.child(
                text_xs(div().px(px(12.)).py(px(8.)).text_color(t.muted_foreground)).child("Loading hosts\u{2026}"),
            )
        } else {
            let suggestions = self.suggestions(cx);
            if suggestions.is_empty() {
                let query = self.ssh_host.read(cx).value().trim().to_owned();
                card.child(
                    text_sm(div().px(px(8.)).py(px(8.)).text_color(t.muted_foreground))
                        .child(format!("No hosts match \u{201c}{query}\u{201d}.")),
                )
            } else {
                let context = ShortcutContext::default();
                card.children(suggestions.into_iter().enumerate().map(|(index, host)| {
                    let address = host.hostname.clone();
                    let shortcut = (index < 9)
                        .then(|| keymap::label_for(&self.keymap, &format!("thread.jump.{}", index + 1), &context))
                        .flatten();
                    let highlighted = self.highlighted == Some(index);
                    let accent = t.accent;
                    h_flex()
                        .id(ElementId::Name(format!("ssh-host-{}-{}", host.alias, host.hostname).into()))
                        .h(px(32.))
                        .px(px(8.))
                        .gap(px(8.))
                        .rounded(px(4.))
                        .whitespace_nowrap()
                        .cursor_pointer()
                        .when(highlighted, |el| el.bg(accent))
                        .hover(move |style| style.bg(accent))
                        .child(
                            text_sm(div().min_w_0().truncate().font_weight(FontWeight::MEDIUM))
                                .child(host.alias.clone()),
                        )
                        .child(if address == host.alias {
                            div().flex_1()
                        } else {
                            text_xs(div().min_w_0().flex_1().truncate().text_color(t.muted_foreground)).child(address)
                        })
                        .when_some(shortcut, |el, shortcut| {
                            el.child(text_xs(div().flex_none().text_color(t.muted_foreground)).child(shortcut))
                        })
                        // On mouse down, before the field's blur closes the list.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.select_suggestion(host.clone(), window, cx);
                            }),
                        )
                }))
            }
        };
        div()
            .absolute()
            .top_full()
            .left_0()
            .mt(px(4.))
            .child(deferred(anchored().snap_to_window_with_margin(px(8.)).child(card)).with_priority(2))
            .into_any_element()
    }

    fn ssh_body(&self, window: &Window, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.busy;
        let field = |label: &'static str, input: &Entity<InputState>| {
            v_flex()
                .gap(px(6.))
                .child(text_xs(div().font_weight(FontWeight::MEDIUM)).child(label))
                .child(Input::new(input).disabled(busy))
        };
        let bounds = self.host_bounds.clone();
        let host_field = div()
            .id("ssh-host-field")
            .relative()
            // `openOnInputClick`.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.suggestions_open = true;
                    cx.notify();
                }),
            )
            .child(Input::new(&self.ssh_host).disabled(busy))
            .child(
                canvas(move |element_bounds, _, _| bounds.set(element_bounds), |_, (), _, _| {}).absolute().size_full(),
            )
            .when(self.suggestions_visible(window, cx), |el| el.child(self.suggestion_popup(t, cx)));
        let error = self.error.clone().map(|error| {
            text_sm(div())
                .px(px(14.))
                .py(px(12.))
                .rounded(px(8.))
                .border_1()
                .border_color(t.error.opacity(0.32))
                .bg(t.error.opacity(0.04))
                .text_color(t.error_foreground)
                .child(error)
        });
        v_flex()
            .gap(px(12.))
            .child(
                v_flex()
                    .gap(px(6.))
                    .child(text_xs(div().font_weight(FontWeight::MEDIUM)).child("SSH host or alias"))
                    .child(host_field),
            )
            .child(
                h_flex()
                    .items_start()
                    .gap(px(12.))
                    .child(field("Username", &self.ssh_user).flex_1().min_w_0())
                    .child(field("Port", &self.ssh_port).w(px(112.)).flex_none()),
            )
            .children(error)
            .child(self.submit_button(cx))
            .into_any_element()
    }
}

impl Render for AddForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let remote = self.mode_card(
            Mode::Remote,
            "Remote link",
            "Enter a backend host and pairing code.",
            "icons/chevrons-left-right-ellipsis.svg",
            &t,
            cx,
        );
        let ssh = self.mode_card(
            Mode::Ssh,
            "SSH",
            "Use local SSH config, agent, and tunnels for the backend.",
            "icons/terminal.svg",
            &t,
            cx,
        );
        let body = match self.mode {
            Mode::Remote => self.remote_body(&t, cx),
            Mode::Ssh => self.ssh_body(window, &t, cx),
        };
        v_flex().gap(px(16.)).child(h_flex().gap(px(12.)).child(remote).child(ssh)).child(body)
    }
}

/// Opens the dialog: a new environment, or with `route`, another route to that one.
pub(super) fn open(
    saved: WeakEntity<SavedEnvironments>,
    primary_id: Option<String>,
    route: Option<RouteTarget>,
    keymap: Rc<[Rule]>,
    window: &mut Window,
    cx: &mut App,
) {
    let (title, description) = match &route {
        Some(route) => (
            format!("Add a route to {}", route.label),
            "Pair this machine again over another address, such as its Tailscale name. It joins the existing routes instead of adding a second machine.",
        ),
        None => ("Add Environment".to_owned(), "Pair another environment to this client."),
    };
    let form = cx.new(|cx| AddForm::new(saved, primary_id, route, keymap, window, cx));
    let host = form.read(cx).host.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let t = theme::tokens(cx);
        dialog
            .title(title.clone())
            .w(px(768.))
            .child(text_sm(div().mb(px(16.)).text_color(t.muted_foreground)).child(description))
            .child(form.clone())
    });
    // After the dialog takes focus for itself.
    host.update(cx, |input, cx| input.focus(window, cx));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environments::{Credential, Route};

    fn host(alias: &str, hostname: &str, source: HostSource) -> DiscoveredHost {
        DiscoveredHost { alias: alias.into(), hostname: hostname.into(), source }
    }

    #[test]
    fn saved_ssh_hosts_drop_out_of_the_suggestions() {
        let target = SshTarget { alias: "box".into(), hostname: "10.0.0.5".into(), username: None, port: None };
        let environment = SavedEnvironment {
            environment_id: "e".into(),
            label: "Box".into(),
            enabled: true,
            routes: vec![
                Route::direct("https://e".into(), Credential::Missing),
                Route { address: Address::Ssh(target), credential: Credential::Missing },
            ],
        };
        let keys = saved_ssh_keys(&[environment]);
        assert_eq!(keys, ["box", "10.0.0.5"]);
        let hosts = [
            host("box", "10.0.0.5", HostSource::SshConfig),
            // A config alias for the same address is another way in, so it stays.
            host("other", "10.0.0.5", HostSource::SshConfig),
            host("10.0.0.5", "10.0.0.5", HostSource::KnownHosts),
            host("lab", "lab", HostSource::KnownHosts),
        ];
        let aliases: Vec<_> = unsaved_hosts(&hosts, &keys).into_iter().map(|host| host.alias).collect();
        assert_eq!(aliases, ["other", "lab"]);
    }

    #[test]
    fn arrow_keys_loop_over_the_suggestions() {
        assert_eq!(step_highlight(None, 3, true), Some(0));
        assert_eq!(step_highlight(None, 3, false), Some(2));
        assert_eq!(step_highlight(Some(2), 3, true), Some(0));
        assert_eq!(step_highlight(Some(0), 3, false), Some(2));
        assert_eq!(step_highlight(Some(1), 0, true), None);
    }

    #[test]
    fn ssh_errors_fall_back_to_a_message() {
        assert_eq!(ssh_error_message("  "), "Failed to connect SSH host.");
        assert_eq!(ssh_error_message(" refused "), "refused");
    }
}
