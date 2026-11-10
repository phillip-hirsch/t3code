//! `BrowserDeviceToolbar`: over a fixed viewport, the device select, the editable width and height,
//! the aspect-ratio lock, rotate, and closing back to Fill.

use std::collections::HashSet;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, ClickEvent, Context, Entity, FocusHandle, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Styled as _, Subscription, Window, div, px,
};
use serde_json::json;

use super::viewport::{self, Axis, Draft, TOOLBAR_HEIGHT};
use super::{Workspace, icon_button};
use crate::preview::{PRESETS, Session, TabRef, Viewport};
use crate::theme;

pub(super) struct DeviceToolbar {
    /// The tab the fields and the edit belong to.
    tab: Option<TabRef>,
    /// The fields' text while being edited (`customSize`); none shows the tab's size.
    draft: Option<Draft>,
    width: Entity<InputState>,
    height: Entity<InputState>,
    focus: FocusHandle,
    /// Tabs whose aspect ratio is locked (`aspectRatioLocked`), and those with a resize in flight.
    locked: HashSet<TabRef>,
    pending: HashSet<TabRef>,
    pub(super) menu_open: bool,
    _subscriptions: [Subscription; 3],
}

impl DeviceToolbar {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Self {
        // `type="number"`: only digits go in.
        let mut field = |cx: &mut Context<Workspace>| {
            cx.new(|cx| InputState::new(window, cx).validate(|text, _| text.chars().all(|c| c.is_ascii_digit())))
        };
        let (width, height) = (field(cx), field(cx));
        let focus = cx.focus_handle();
        let subscriptions = [
            cx.subscribe_in(&width, window, |this, _, event: &InputEvent, window, cx| {
                this.toolbar_input(Axis::Width, event, window, cx);
            }),
            cx.subscribe_in(&height, window, |this, _, event: &InputEvent, window, cx| {
                this.toolbar_input(Axis::Height, event, window, cx);
            }),
            // Leaving the toolbar commits the typed size, as its `onBlur` does.
            cx.on_focus_out(&focus, window, |this, _, _, cx| this.apply_toolbar_draft(cx)),
        ];
        Self {
            tab: None,
            draft: None,
            width,
            height,
            focus,
            locked: HashSet::new(),
            pending: HashSet::new(),
            menu_open: false,
            _subscriptions: subscriptions,
        }
    }
}

impl DeviceToolbar {
    /// The ratio `tab`'s lock holds its `size` to (`lockedAspectRatio`).
    pub(super) fn locked_ratio(&self, tab: &TabRef, size: (f32, f32)) -> Option<f64> {
        self.locked.contains(tab).then(|| f64::from(size.0) / f64::from(size.1.max(1.)))
    }

    /// Whether `tab` has a `preview.resize` in flight.
    pub(super) fn resizing(&self, tab: &TabRef) -> bool {
        self.pending.contains(tab)
    }
}

#[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "viewport sizes are whole pixels")]
fn whole(size: (f32, f32)) -> (u32, u32) {
    (size.0 as u32, size.1 as u32)
}

impl Workspace {
    /// The toolbar's tab, its fixed size, and the ratio a lock holds it to.
    fn toolbar_setting(&self) -> Option<(TabRef, (u32, u32), Option<f64>)> {
        let tab = self.preview.toolbar.tab.clone()?;
        let size = whole(self.preview.threads.session(&tab)?.viewport.size()?);
        let ratio = self.preview.toolbar.locked.contains(&tab).then(|| f64::from(size.0) / f64::from(size.1));
        Some((tab, size, ratio))
    }

    fn toolbar_input(&mut self, axis: Axis, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, size, ratio)) = self.toolbar_setting() else { return };
        let toolbar = &mut self.preview.toolbar;
        match event {
            InputEvent::Focus => {
                toolbar.draft.get_or_insert_with(|| Draft::seeded(size));
            }
            // `updateCustomDimension`; a locked ratio fills in the other field.
            InputEvent::Change => {
                let (field, other) = match axis {
                    Axis::Width => (&toolbar.width, toolbar.height.clone()),
                    Axis::Height => (&toolbar.height, toolbar.width.clone()),
                };
                let value = field.read(cx).value().to_string();
                let next = Draft::edited(toolbar.draft.as_ref(), size, axis, &value, ratio);
                let linked = if axis == Axis::Width { next.height.clone() } else { next.width.clone() };
                toolbar.draft = Some(next);
                if other.read(cx).value() != linked {
                    other.update(cx, |input, cx| input.set_value(linked, window, cx));
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } => self.apply_toolbar_draft(cx),
            InputEvent::Blur => {}
        }
    }

    /// `applyCustomSize`: sends a valid, changed size; anything else just drops the edit.
    fn apply_toolbar_draft(&mut self, cx: &mut Context<Self>) {
        let Some((tab, size, ratio)) = self.toolbar_setting() else { return };
        let Some(draft) = self.preview.toolbar.draft.clone() else { return };
        match draft.committed(size, ratio) {
            Some((width, height)) => self.resize_viewport(tab, Viewport::Freeform { width, height }, None, cx),
            None => {
                self.preview.toolbar.draft = None;
                cx.notify();
            }
        }
    }

    /// `commitBrowserViewportChange`: `preview.resize`, one at a time per tab (the toolbar is
    /// disabled meanwhile). `lock` sets the aspect lock once it lands, as `commitViewportAndAspectRatio`.
    pub(super) fn resize_viewport(
        &mut self,
        tab: TabRef,
        viewport: Viewport,
        lock: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        if self.preview.toolbar.pending.contains(&tab) {
            return;
        }
        let Some(client) = self.env_client(&tab.thread.environment) else { return };
        self.preview.toolbar.pending.insert(tab.clone());
        let input = json!({"threadId": tab.thread.thread_id, "tabId": tab.tab_id, "viewport": viewport.to_json()});
        cx.spawn(async move |this, cx| {
            let result = client.request("preview.resize", input).await;
            let _ = this.update(cx, |this, cx| {
                this.preview.toolbar.pending.remove(&tab);
                this.settle_viewport_resize(&tab);
                let session = result.map_err(|e| e.to_string()).and_then(|value| {
                    serde_json::from_value::<Session>(value)
                        .map_err(|e| format!("Unexpected preview.resize result: {e}"))
                });
                match session {
                    Ok(session) => {
                        this.preview.threads.entry(&tab.thread).apply_snapshot(session, false);
                        let toolbar = &mut this.preview.toolbar;
                        match lock {
                            Some(true) => {
                                toolbar.locked.insert(tab.clone());
                            }
                            Some(false) => {
                                toolbar.locked.remove(&tab);
                            }
                            None => {}
                        }
                        if toolbar.tab.as_ref() == Some(&tab) {
                            toolbar.draft = None;
                        }
                    }
                    Err(error) => this.error = Some(format!("Unable to resize browser viewport: {error}").into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The More menu's "Show device toolbar" / "Hide device toolbar" (`handleToggleDeviceToolbar`).
    pub(super) fn toggle_device_toolbar(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else { return };
        let Some(session) = self.preview.threads.session(&tab) else { return };
        let next = if session.viewport == Viewport::Fill {
            let default: Viewport =
                serde::Deserialize::deserialize(self.client_settings.values.get("browserDefaultViewport"))
                    .unwrap_or_default();
            let panel = self.preview.panel_area.as_ref().filter(|(shown, _)| *shown == tab).map(|(_, size)| *size);
            viewport::toolbar_viewport(&default, panel, self.tab_look(&tab, session).0)
        } else {
            Viewport::Fill
        };
        self.resize_viewport(tab, next, None, cx);
    }

    /// The toolbar over `tab`'s fixed `setting`, `width` wide.
    pub(super) fn render_device_toolbar(
        &mut self,
        tab: &TabRef,
        setting: &Viewport,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let size = setting.size().map_or((0, 0), whole);
        let toolbar = &mut self.preview.toolbar;
        // Another tab's edit doesn't carry over.
        if toolbar.tab.as_ref() != Some(tab) {
            toolbar.tab = Some(tab.clone());
            toolbar.draft = None;
        }
        let locked = toolbar.locked.contains(tab);
        let ratio = locked.then(|| f64::from(size.0) / f64::from(size.1.max(1)));
        let pending = toolbar.pending.contains(tab);
        let shown = toolbar.draft.clone().unwrap_or_else(|| Draft::seeded(size));
        for (field, text) in [(&toolbar.width, &shown.width), (&toolbar.height, &shown.height)] {
            if field.read(cx).value().as_ref() != text.as_str() {
                field.update(cx, |input, cx| input.set_value(text.clone(), window, cx));
            }
        }
        let valid = shown.resolved(ratio).is_some();
        let choice = setting.choice().to_owned();
        let label = PRESETS.iter().find(|p| p.id == choice).map_or("Responsive", |p| p.label);
        let view = cx.entity();
        let picked = setting.clone();
        let pick_tab = tab.clone();
        let select = Button::new("device-toolbar-preset")
            .ghost()
            .xsmall()
            .h(px(24.))
            .w(px(if width >= 440. { 144. } else { 96. }))
            .px(px(8.))
            .rounded(px(6.))
            .disabled(pending)
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap(px(4.))
                    .text_xs()
                    .text_color(t.foreground)
                    .child(div().min_w_0().truncate().child(label))
                    .child(Icon::new(IconName::ChevronDown).size(px(14.)).text_color(t.muted_foreground.opacity(0.7))),
            )
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopLeft, move |menu: PopupMenu, window, _| {
                let pick = |choice: &'static str| {
                    let (setting, tab) = (picked.clone(), pick_tab.clone());
                    window.listener_for(&view, move |this: &mut Workspace, _: &ClickEvent, _, cx| {
                        if let Some(next) = viewport::picked(&setting, choice) {
                            this.resize_viewport(tab.clone(), next, None, cx);
                        }
                    })
                };
                let mut menu = menu
                    .item(PopupMenuItem::new("Responsive").checked(choice == "responsive").on_click(pick("responsive")))
                    .label("Standard");
                for preset in &PRESETS {
                    let item = PopupMenuItem::element(move |_, cx| {
                        let muted = theme::tokens(cx).muted_foreground;
                        h_flex().w_full().justify_between().gap(px(20.)).child(preset.label).child(
                            div().text_xs().text_color(muted).child(format!("{} × {}", preset.width, preset.height)),
                        )
                    });
                    menu = menu.item(item.checked(choice == preset.id).on_click(pick(preset.id)));
                }
                menu.max_h(px(360.)).scrollable(true)
            })
            .open(toolbar.menu_open)
            .on_open_change(cx.listener(|this, open: &bool, _, cx| {
                this.preview.toolbar.menu_open = *open;
                cx.notify();
            }));
        let field = |state: &Entity<InputState>, id: &'static str| {
            Input::new(state)
                .id(id)
                .xsmall()
                .h(px(24.))
                .w(px(if width >= 360. { 56. } else { 52. }))
                .font_family(t.fonts.mono)
                .text_xs()
                .disabled(pending)
                .when(!valid, |input| input.border_color(t.error))
        };
        let lock_label = if locked { "Unlock aspect ratio" } else { "Lock aspect ratio" };
        let lock_tab = tab.clone();
        let lock = icon_button(
            "device-toolbar-lock",
            if locked { IconName::Link2 } else { IconName::Unlink2 },
            !pending && valid,
            &t,
            cx,
        )
        // `variant="secondary"` while locked.
        .when(locked, |button| {
            button.custom(
                ButtonCustomVariant::new(cx)
                    .color(t.secondary)
                    .foreground(t.foreground)
                    .hover(t.secondary)
                    .active(t.secondary),
            )
        })
        .tooltip(lock_label)
        .on_click(cx.listener(move |this, _, _, cx| {
            let locked = &mut this.preview.toolbar.locked;
            if !locked.remove(&lock_tab) {
                locked.insert(lock_tab.clone());
            }
            cx.notify();
        }));
        let (rotate_tab, rotate_setting) = (tab.clone(), setting.clone());
        let rotate = Button::new("device-toolbar-rotate")
            .ghost()
            .xsmall()
            .size(px(24.))
            .p_0()
            .rounded(px(6.))
            .disabled(pending)
            .child(Icon::empty().path("icons/app/screen-rotation.svg").size(px(14.)).text_color(t.muted_foreground))
            .on_click(cx.listener(move |this, _, _, cx| {
                let next = viewport::rotated(&rotate_setting, this.preview.toolbar.draft.as_ref(), ratio);
                this.resize_viewport(rotate_tab.clone(), next, None, cx);
            }));
        let close_tab = tab.clone();
        let close = icon_button("device-toolbar-close", IconName::X, !pending, &t, cx)
            .tooltip("Close device toolbar")
            .on_click(cx.listener(move |this, _, _, cx| {
                this.resize_viewport(close_tab.clone(), Viewport::Fill, Some(false), cx);
            }));
        let toolbar = &self.preview.toolbar;
        h_flex()
            .id("device-toolbar")
            .track_focus(&toolbar.focus)
            .w(px(width))
            .h(px(TOOLBAR_HEIGHT))
            .flex_none()
            .items_center()
            .gap(px(2.))
            .px(px(6.))
            .overflow_hidden()
            .border_b_1()
            .border_color(t.border.opacity(0.7))
            .bg(t.background.opacity(0.95))
            .shadow_xs()
            .when(width >= 560., |row| {
                row.child(
                    div()
                        .mr(px(2.))
                        .flex_none()
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.muted_foreground)
                        .child("Dimensions"),
                )
            })
            .child(select)
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(2.))
                    .items_center()
                    .child(field(&toolbar.width, "device-toolbar-width"))
                    .child(div().text_xs().text_color(t.muted_foreground).child("×"))
                    .child(field(&toolbar.height, "device-toolbar-height")),
            )
            .child(lock)
            .child(rotate)
            .child(div().ml_auto().flex_none().child(close))
            .into_any_element()
    }
}
