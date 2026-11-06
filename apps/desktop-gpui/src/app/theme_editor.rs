//! The floating theme editor (`ThemeEditorPanel.tsx`, `ThemeColorPicker.tsx`, `ThemeEditorHost.tsx`):
//! guided (background + accent) and advanced (every color family) editing, a live preview of
//! the draft over the whole app, and saving into the theme library. The web's "Inspect"
//! picker and usage counts read the DOM; this client has no equivalent and leaves them out.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, Context, DispatchPhase, Entity, Focusable as _, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement as _, Pixels, Point, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, WeakEntity, Window, canvas, div, linear_color_stop, linear_gradient, px, rems,
};
use serde_json::{Map, Value, json};

use super::Workspace;
use super::pages::settings::theme_library::toast;
use crate::theme::{self, Tokens};
use crate::theme_color::{self, hex_to_hsv, hsv_to_hex};
use crate::theme_file::{THEME_FILE_VERSION, parse_theme_file, theme_id_from_name};
use crate::themes::{self, Library, Roles, ThemeChoice, ThemeColors, ThemeDefinition};

/// `ThemeEditorSessionInput`.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) struct Session {
    pub editing: Option<String>,
    pub seed: Option<String>,
    pub seed_name: Option<String>,
    pub dark: bool,
}

impl Session {
    pub fn new(dark: bool) -> Self {
        Self { editing: None, seed: None, seed_name: None, dark }
    }

    /// A card's Duplicate: seeded from it, named "<label> copy".
    pub fn duplicate(seed: Option<&str>, label: &str, dark: bool) -> Self {
        Self { seed: seed.map(str::to_owned), seed_name: Some(format!("{label} copy")), ..Self::new(dark) }
    }
}

const SIMPLE_ROLES: [&str; 2] = ["canvas", "accent"];

struct Family {
    label: &'static str,
    role: &'static str,
    roles: &'static [&'static str],
}

const fn family(label: &'static str, role: &'static str, roles: &'static [&'static str]) -> Family {
    Family { label, role, roles }
}

/// `THEME_EDITOR_ROLE_GROUPS`.
const GROUPS: &[(&str, &[Family])] = &[
    (
        "Foundation",
        &[
            family("Background", "canvas", &["canvas", "chrome", "toolbar"]),
            family("Surface", "surface", &["surface"]),
            family("Raised surface", "surfaceRaised", &["surfaceRaised"]),
            family("Overlay", "surfaceOverlay", &["surfaceOverlay"]),
            family("Text", "text", &["text", "toolbarForeground", "toolbarControlForeground"]),
            family(
                "Muted text",
                "mutedForeground",
                &[
                    "textMuted",
                    "mutedForeground",
                    "placeholder",
                    "secondaryLabel",
                    "iconMuted",
                    "sidebarMutedForeground",
                ],
            ),
            family("Border", "border", &["border", "toolbarBorder", "sidebarBorder"]),
            family("Input", "input", &["input"]),
        ],
    ),
    (
        "Brand & content",
        &[
            family("Subtle surface", "secondary", &["secondary", "secondaryForeground", "muted", "toolbarControl"]),
            family(
                "Highlight surface",
                "accentSurface",
                &["accentSurface", "accentSurfaceForeground", "toolbarControlHover"],
            ),
            family(
                "Accent",
                "accent",
                &[
                    "accent",
                    "accentForeground",
                    "focus",
                    "update",
                    "updateForeground",
                    "updateSurface",
                    "terminalCursor",
                ],
            ),
            family("Action", "messageAction", &["messageAction", "messageActionForeground", "messageActionHover"]),
            family("Message surface", "messageSurface", &["messageSurface", "messageForeground"]),
            family("Code surface", "codeBackground", &["codeBackground", "codeForeground"]),
        ],
    ),
    (
        "Search",
        &[
            family("Search matches", "searchMatchBackground", &["searchMatchBackground", "searchMatchForeground"]),
            family(
                "Current match",
                "searchMatchActiveBackground",
                &["searchMatchActiveBackground", "searchMatchActiveForeground"],
            ),
        ],
    ),
    (
        "Context",
        &[
            family("Sidebar background", "sidebar", &["sidebar", "sidebarForeground"]),
            family("Sidebar controls", "sidebarControlSurface", &["sidebarControlSurface"]),
            family(
                "Sidebar selection",
                "sidebarRowSelected",
                &["sidebarRowHover", "sidebarRowActive", "sidebarRowSelected"],
            ),
            family(
                "Terminal background",
                "terminalBackground",
                &[
                    "terminalBackground",
                    "terminalForeground",
                    "terminalSelection",
                    "terminalScrollbar",
                    "terminalScrollbarHover",
                ],
            ),
        ],
    ),
    (
        "Status",
        &[
            family("Error", "error", &["error", "errorForeground", "errorSurface"]),
            family("Warning", "warning", &["warning", "warningForeground", "warningSurface"]),
        ],
    ),
];

fn families() -> impl Iterator<Item = &'static Family> {
    GROUPS.iter().flat_map(|(_, families)| families.iter())
}

/// `getThemeRoleLabel`.
pub(in crate::app) fn role_label(role: &str) -> String {
    let named = match role {
        "canvas" => "Background",
        "toolbar" => "Toolbar background",
        "toolbarForeground" => "Toolbar text",
        "toolbarBorder" => "Toolbar border",
        "toolbarControl" => "Toolbar control",
        "toolbarControlForeground" => "Toolbar control text",
        "toolbarControlHover" => "Toolbar control hover",
        "accent" => "Accent color",
        "errorForeground" => "Error text",
        "errorSurface" => "Error background",
        "warningForeground" => "Warning text",
        "warningSurface" => "Warning background",
        "updateForeground" => "Update text",
        "updateSurface" => "Update background",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_owned();
    }
    let mut label = String::new();
    for (i, c) in role.chars().enumerate() {
        if c.is_ascii_uppercase() {
            label.push(' ');
        }
        label.push(if i == 0 { c.to_ascii_uppercase() } else { c });
    }
    label
}

/// What a filter matches: the family's label and its roles' labels.
fn family_matches(family: &Family, query: &str) -> bool {
    query.is_empty()
        || std::iter::once(family.label.to_owned())
            .chain(family.roles.iter().map(|role| role_label(role)))
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
            .contains(query)
}

/// `getManagedEditorColors`: the guided palette from the draft's two seeds.
fn managed(dark: bool, colors: &Roles) -> Roles {
    let defaults = themes::standard_roles(dark);
    let seed = |role: &str| {
        let value = colors.get(role).trim();
        if theme_color::is_color(value) { value.to_owned() } else { defaults.get(role).to_owned() }
    };
    theme_color::vivid(dark, &seed("canvas"), &seed("accent"))
}

/// The text a field shows: hex for the canonical `oklch(...)` form, else the value as typed.
fn field_text(value: &str) -> String {
    if value.trim().to_lowercase().starts_with("oklch(") {
        theme_color::to_hex(value).unwrap_or_else(|| value.to_owned())
    } else {
        value.to_owned()
    }
}

const MARGIN: f32 = 8.;
const MIN_WIDTH: f32 = 280.;
const MIN_HEIGHT: f32 = 220.;

/// `clampPanelPosition`: inside the window horizontally; only the header must stay visible.
fn clamp_position(viewport: (f32, f32), x: f32, y: f32, width: f32) -> (f32, f32) {
    let max_x = (viewport.0 - width - MARGIN).max(MARGIN);
    let max_y = (viewport.1 - 48.).max(MARGIN);
    (x.clamp(MARGIN, max_x), y.clamp(MARGIN, max_y))
}

enum Drag {
    Move { dx: f32, dy: f32, width: f32 },
    Resize { x: f32, y: f32, left: f32, top: f32, width: f32, height: f32 },
    Plane,
    Hue,
}

/// The open color picker (`ThemeColorPickerPanel`).
struct Picker {
    role: &'static str,
    label: String,
    hsv: [f32; 3],
    /// The edited color's alpha as two hex digits, kept through picks.
    alpha: String,
    hex: Entity<InputState>,
    rgb: Entity<InputState>,
    plane: Rc<Cell<Bounds<Pixels>>>,
    hue: Rc<Cell<Bounds<Pixels>>>,
    _subscriptions: [Subscription; 2],
}

/// The draft before a color edit, restored when the picker closes with Escape.
type Snapshot = ([Roles; 2], [bool; 2], bool);

pub(in crate::app) struct ThemeEditor {
    workspace: WeakEntity<Workspace>,
    /// The saved themes, kept current by `Workspace::store_themes`.
    library: Library,
    editing: Option<ThemeDefinition>,
    name: Entity<InputState>,
    filter: Entity<InputState>,
    dark: bool,
    advanced: bool,
    colors: [Roles; 2],
    simple_dirty: [bool; 2],
    regenerate: bool,
    error: Option<String>,
    minimized: bool,
    selected: Option<&'static str>,
    /// The merge target whose appearances last moved the draft (`appliedMergeKey`).
    merge_key: Option<String>,
    fields: HashMap<&'static str, Entity<InputState>>,
    picker: Option<Picker>,
    snapshot: Option<Snapshot>,
    position: Option<(f32, f32, f32)>,
    size: Option<(f32, f32)>,
    drag: Option<Drag>,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    _subscriptions: Vec<Subscription>,
}

impl ThemeEditor {
    fn new(
        session: Session,
        workspace: WeakEntity<Workspace>,
        library: &Library,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editing = session.editing.as_deref().and_then(|id| library.custom_theme(id)).cloned();
        let seed = session.seed.as_deref().and_then(|id| library.find(id)).cloned();
        let source = editing.clone().or(seed);
        let mut colors = [themes::standard_roles(false).clone(), themes::standard_roles(true).clone()];
        let mut dark = session.dark;
        if let Some(source) = &source {
            colors[usize::from(source.dark)] = source.roles.clone();
            if let Some(variant) = &source.variant_roles {
                colors[usize::from(!source.dark)] = variant.clone();
            }
            if source.colors_for(dark).is_none() {
                dark = source.dark;
            }
        }
        let name_value = editing.as_ref().map(|theme| theme.label.clone()).or(session.seed_name).unwrap_or_default();
        let placeholder = if editing.is_some() { "Theme name" } else { "e.g. Aurora" };
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(name_value));
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter colors"));
        let mut subscriptions = vec![
            cx.subscribe_in(&name, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.error = None;
                    this.follow_merge_target(cx);
                    cx.notify();
                }
            }),
            cx.subscribe(&filter, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        let mut fields = HashMap::new();
        for role in SIMPLE_ROLES.into_iter().chain(families().map(|family| family.role)) {
            if fields.contains_key(role) {
                continue;
            }
            let input =
                cx.new(|cx| InputState::new(window, cx).default_value(field_text(colors[usize::from(dark)].get(role))));
            subscriptions.push(cx.subscribe_in(&input, window, move |this, input, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::Change => {
                        let value = input.read(cx).value().to_string();
                        this.update_color(role, &value, window, cx);
                    }
                    InputEvent::Focus => this.select_role(role, cx),
                    _ => {}
                }
            }));
            fields.insert(role, input);
        }
        let unmanaged = source.as_ref().is_some_and(|source| !source.managed);
        let mut editor = Self {
            workspace,
            library: library.clone(),
            editing,
            name,
            filter,
            dark,
            advanced: unmanaged,
            colors,
            simple_dirty: [false; 2],
            regenerate: unmanaged,
            error: None,
            minimized: false,
            selected: None,
            merge_key: None,
            fields,
            picker: None,
            snapshot: None,
            position: None,
            size: None,
            drag: None,
            bounds: Rc::default(),
            _subscriptions: subscriptions,
        };
        editor.follow_merge_target(cx);
        editor.preview(window, cx);
        editor
    }

    fn name(&self, cx: &App) -> String {
        self.name.read(cx).value().to_string()
    }

    /// A saved theme that already has this name: the draft adds its missing palette to it.
    fn merge_target(&self, cx: &App) -> Option<&ThemeDefinition> {
        let name = self.name(cx);
        let normalized = name.trim().to_lowercase();
        if normalized.is_empty() {
            return None;
        }
        let id = theme_id_from_name(&name);
        let editing = self.editing.as_ref().map(|theme| theme.id.as_str());
        self.library.custom().iter().find(|theme| {
            Some(theme.id.as_str()) != editing && (theme.id == id || theme.label.trim().to_lowercase() == normalized)
        })
    }

    /// Moves the draft off an appearance the merge target already has.
    fn follow_merge_target(&mut self, cx: &mut Context<Self>) {
        let target = self.merge_target(cx).filter(|_| self.editing.is_none());
        let key = target.map(|t| format!("{}:{:?}", t.id, t.modes()));
        let only_this_mode = target.is_some_and(|t| t.modes() == [self.dark]);
        if key == self.merge_key {
            return;
        }
        self.merge_key = key;
        if only_this_mode {
            self.dark = !self.dark;
        }
    }

    /// Why an appearance cannot be edited, if it cannot.
    fn lock_reason(&self, dark: bool, cx: &App) -> Option<String> {
        let mode = if dark { "dark" } else { "light" };
        if let Some(editing) = &self.editing {
            if editing.colors_for(dark).is_none() {
                return Some(format!(
                    "“{}” has no {mode} palette. Create a theme with the same name to add one.",
                    editing.label
                ));
            }
            return None;
        }
        let target = self.merge_target(cx)?;
        target.colors_for(dark).is_some().then(|| format!("“{}” already has a {mode} palette.", target.label))
    }

    /// `applyThemeColorPreview`: the draft paints the app while the editor is open.
    fn preview(&self, window: &mut Window, cx: &mut App) {
        let fallback = ThemeColors::from_roles(themes::standard_roles(self.dark), None);
        let colors = ThemeColors::from_roles(&self.colors[usize::from(self.dark)], fallback.as_ref());
        theme::set_editor_preview(colors.map(|colors| (self.dark, colors)), window, cx);
    }

    /// The fields and the open picker show the draft, except where the person is typing.
    fn sync_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let colors = &self.colors[usize::from(self.dark)];
        for (role, input) in &self.fields {
            let text = field_text(colors.get(role));
            let focused = input.read(cx).focus_handle(cx).is_focused(window);
            if !focused && input.read(cx).value() != text.as_str() {
                input.update(cx, |input, cx| input.set_value(text, window, cx));
            }
        }
        if let Some(picker) = &mut self.picker {
            let hex = theme_color::to_hex(colors.get(picker.role)).unwrap_or_else(|| "#000000".into());
            let opaque = hex.get(..7).unwrap_or("#000000").to_owned();
            if hsv_to_hex(picker.hsv) != opaque {
                picker.hsv = hex_to_hsv(&opaque);
            }
            sync_picker_inputs(picker, &opaque, window, cx);
        }
    }

    /// `updateColor`.
    fn update_color(&mut self, role: &'static str, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        let index = usize::from(self.dark);
        let current = &self.colors[index];
        let simple = !self.advanced && SIMPLE_ROLES.contains(&role) && theme_color::is_color(value.trim());
        self.colors[index] = if self.advanced {
            theme_color::update_family(self.dark, current, role, value)
        } else {
            let mut next = current.clone();
            next.set(role, value.to_owned());
            if simple { managed(self.dark, &next) } else { next }
        };
        if simple {
            self.simple_dirty[index] = true;
        }
        if self.advanced {
            self.regenerate = true;
        }
        self.sync_fields(window, cx);
        self.preview(window, cx);
        cx.notify();
    }

    /// `selectThemeRole`: a role outside the guided pair opens the advanced editor.
    fn select_role(&mut self, role: &'static str, cx: &mut Context<Self>) {
        let visible = families().find(|family| family.roles.contains(&role)).map_or(role, |family| family.role);
        self.selected = Some(visible);
        if !SIMPLE_ROLES.contains(&visible) && !self.advanced {
            self.advanced = true;
        }
        cx.notify();
    }

    /// `handleAdvancedChange`: leaving advanced mode regenerates from the two seeds once
    /// something was hand-tuned.
    fn set_advanced(&mut self, advanced: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.advanced = advanced;
        if !advanced {
            if self.selected.is_some_and(|role| !SIMPLE_ROLES.contains(&role)) {
                self.selected = None;
            }
            if self.regenerate {
                let both = self.editing.as_ref().is_some_and(|theme| theme.modes().len() > 1);
                let appearances: Vec<bool> = if both { vec![false, true] } else { vec![self.dark] };
                for dark in appearances {
                    self.simple_dirty[usize::from(dark)] = true;
                    self.colors[usize::from(dark)] = managed(dark, &self.colors[usize::from(dark)]);
                }
                self.regenerate = false;
            }
        }
        self.sync_fields(window, cx);
        self.preview(window, cx);
        cx.notify();
    }

    fn set_appearance(&mut self, dark: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.dark == dark || self.lock_reason(dark, cx).is_some() {
            return;
        }
        self.dark = dark;
        self.sync_fields(window, cx);
        self.preview(window, cx);
        cx.notify();
    }

    fn open_picker(&mut self, role: &'static str, label: String, window: &mut Window, cx: &mut Context<Self>) {
        self.snapshot = Some((self.colors.clone(), self.simple_dirty, self.regenerate));
        self.select_role(role, cx);
        self.selected = Some(role);
        let hex =
            theme_color::to_hex(self.colors[usize::from(self.dark)].get(role)).unwrap_or_else(|| "#000000".into());
        let opaque = hex.get(..7).unwrap_or("#000000").to_owned();
        let alpha = hex.get(7..9).filter(|alpha| *alpha != "ff").unwrap_or_default().to_owned();
        let hex_input = cx.new(|cx| InputState::new(window, cx).default_value(opaque.clone()));
        let rgb_input = cx.new(|cx| InputState::new(window, cx).default_value(rgb_text(&opaque)));
        let subscriptions = [
            cx.subscribe_in(&hex_input, window, |this, input, event: &InputEvent, window, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let value = input.read(cx).value().to_string();
                let valid =
                    value.len() == 7 && value.starts_with('#') && value[1..].bytes().all(|b| b.is_ascii_hexdigit());
                if valid {
                    this.pick(hex_to_hsv(&value), Some(value.to_lowercase()), window, cx);
                }
            }),
            cx.subscribe_in(&rgb_input, window, |this, input, event: &InputEvent, window, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                if let Some(hex) = rgb_to_hex(&input.read(cx).value()) {
                    this.pick(hex_to_hsv(&hex), Some(hex), window, cx);
                }
            }),
        ];
        self.picker = Some(Picker {
            role,
            label,
            hsv: hex_to_hsv(&opaque),
            alpha,
            hex: hex_input,
            rgb: rgb_input,
            plane: Rc::default(),
            hue: Rc::default(),
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    /// Closes the picker; Escape puts the draft back as it was before the edit.
    fn close_picker(&mut self, cancel: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        if let (true, Some((colors, dirty, regenerate))) = (cancel, self.snapshot.take()) {
            (self.colors, self.simple_dirty, self.regenerate) = (colors, dirty, regenerate);
            self.sync_fields(window, cx);
            self.preview(window, cx);
        }
        self.snapshot = None;
        cx.notify();
    }

    /// A pick in the picker: its HSV, and the hex the person typed when that is what changed.
    fn pick(&mut self, hsv: [f32; 3], typed: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = &mut self.picker else { return };
        picker.hsv = hsv;
        let hex = typed.unwrap_or_else(|| hsv_to_hex(hsv));
        let (role, value) = (picker.role, format!("{hex}{}", picker.alpha));
        self.update_color(role, &value, window, cx);
    }

    fn drag_to(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = window.viewport_size();
        let viewport = (f32::from(viewport.width), f32::from(viewport.height));
        let (x, y) = (f32::from(position.x), f32::from(position.y));
        match &self.drag {
            Some(Drag::Move { dx, dy, width }) => {
                let (left, top) = clamp_position(viewport, x - dx, y - dy, *width);
                self.position = Some((left, top, *width));
            }
            Some(Drag::Resize { x: x0, y: y0, left, top, width, height }) => {
                let max_w = (viewport.0 - MARGIN - left).max(MIN_WIDTH);
                let max_h = (viewport.1 - MARGIN - top).max(MIN_HEIGHT);
                self.size =
                    Some(((width + x - x0).clamp(MIN_WIDTH, max_w), (height + y - y0).clamp(MIN_HEIGHT, max_h)));
            }
            Some(Drag::Plane) => {
                let Some(picker) = &self.picker else { return };
                let bounds = picker.plane.get();
                let s = ((position.x - bounds.left()) / bounds.size.width).clamp(0., 1.);
                let v = 1. - ((position.y - bounds.top()) / bounds.size.height).clamp(0., 1.);
                let hsv = [picker.hsv[0], s, v];
                self.pick(hsv, None, window, cx);
                return;
            }
            Some(Drag::Hue) => {
                let Some(picker) = &self.picker else { return };
                let bounds = picker.hue.get();
                let h = ((position.x - bounds.left()) / bounds.size.width).clamp(0., 1.) * 360.;
                let hsv = [h, picker.hsv[1], picker.hsv[2]];
                self.pick(hsv, None, window, cx);
                return;
            }
            None => return,
        }
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = self.workspace.update(cx, |workspace, cx| workspace.close_theme_editor(window, cx));
    }

    /// `handleSubmit`: creates, edits, or merges the draft into a theme of the same name.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name(cx);
        if name.trim().is_empty() {
            self.error = Some("Name your theme first.".into());
            cx.notify();
            return;
        }
        match self.save(&name, cx) {
            Ok((library, saved, outcome)) => {
                let _ = self
                    .workspace
                    .update(cx, |workspace, cx| workspace.theme_saved(library, saved, outcome, window, cx));
            }
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }

    fn save(&self, name: &str, cx: &App) -> Result<(Library, ThemeDefinition, Saved), String> {
        let library = self.library.clone();
        let colors: [Roles; 2] = if self.advanced {
            self.colors.clone()
        } else {
            [false, true].map(|dark| {
                let colors = &self.colors[usize::from(dark)];
                if self.simple_dirty[usize::from(dark)] { managed(dark, colors) } else { colors.clone() }
            })
        };
        let file =
            |id: Option<&str>, name: &str, dark: bool, base: &Roles, variants: Map<String, Value>, managed: bool| {
                let mut file = json!({
                    "version": THEME_FILE_VERSION,
                    "name": name,
                    "appearance": if dark { "dark" } else { "light" },
                    "colors": base.to_json(),
                });
                if let Some(id) = id {
                    file["id"] = id.into();
                }
                if !variants.is_empty() {
                    file["variants"] = Value::Object(variants);
                }
                if managed {
                    file["managed"] = true.into();
                }
                parse_theme_file(&file)
            };
        let mode = |dark: bool| if dark { "dark" } else { "light" };
        let variants_of = |theme: &ThemeDefinition| {
            let mut map = Map::new();
            if let Some(variant) = &theme.variant_roles {
                map.insert(mode(!theme.dark).into(), variant.to_json());
            }
            map
        };
        let target = self.merge_target(cx).cloned();
        let keep = |theme: ThemeDefinition, collection: &Option<themes::Collection>| ThemeDefinition {
            collection: collection.clone(),
            ..theme
        };
        match (&self.editing, target) {
            (Some(editing), Some(target)) => {
                let edited = editing.modes();
                if let Some(collision) = edited.iter().find(|dark| target.colors_for(**dark).is_some()) {
                    return Err(format!(
                        "“{}” already has a {} palette. Pick another name.",
                        target.label,
                        mode(*collision)
                    ));
                }
                let mut variants = variants_of(&target);
                for dark in &edited {
                    variants.insert(mode(*dark).into(), colors[usize::from(*dark)].to_json());
                }
                variants.remove(mode(target.dark));
                let managed = target.managed && !self.advanced;
                let saved = keep(
                    file(Some(&target.id), &target.label, target.dark, &target.roles, variants, managed)?,
                    &target.collection,
                );
                let library = library.update(saved.clone())?.remove(std::slice::from_ref(&editing.id));
                Ok((library, saved, Saved::Merged(edited[0])))
            }
            (Some(editing), None) => {
                let base = editing.dark;
                let mut variants = Map::new();
                if editing.modes().len() > 1 {
                    variants.insert(mode(!base).into(), colors[usize::from(!base)].to_json());
                }
                let saved = keep(
                    file(Some(&editing.id), name, base, &colors[usize::from(base)], variants, !self.advanced)?,
                    &editing.collection,
                );
                Ok((library.update(saved.clone())?, saved, Saved::Edited))
            }
            (None, Some(target)) => {
                if target.colors_for(self.dark).is_some() {
                    return Err(format!("“{}” already has light and dark palettes. Pick another name.", target.label));
                }
                let mut variants = variants_of(&target);
                variants.insert(mode(self.dark).into(), colors[usize::from(self.dark)].to_json());
                let managed = target.managed && !self.advanced;
                let saved = keep(
                    file(Some(&target.id), &target.label, target.dark, &target.roles, variants, managed)?,
                    &target.collection,
                );
                Ok((library.update(saved.clone())?, saved, Saved::Merged(self.dark)))
            }
            (None, None) => {
                let saved = file(None, name, self.dark, &colors[usize::from(self.dark)], Map::new(), !self.advanced)?;
                Ok((library.install(saved.clone())?, saved, Saved::Created))
            }
        }
    }
}

/// How a save changed the library, for the toast and the pick (`onSaved`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::app) enum Saved {
    Created,
    Edited,
    /// The appearance whose palette was added to a theme of the same name.
    Merged(bool),
}

fn rgb_text(hex: &str) -> String {
    let value = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
    format!("{}, {}, {}", value >> 16, (value >> 8) & 255, value & 255)
}

/// `themeRgbToHex`: `rgb(r, g, b)` or `r g b` with integer channels.
fn rgb_to_hex(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();
    let body = lower.strip_prefix("rgb(").map_or(trimmed, |rest| rest.trim_end_matches(')').trim());
    let channels: Vec<u8> =
        body.split([',', ' ']).filter(|part| !part.is_empty()).map(|part| part.parse().ok()).collect::<Option<_>>()?;
    let [r, g, b] = channels[..] else { return None };
    Some(format!("#{r:02x}{g:02x}{b:02x}"))
}

fn sync_picker_inputs(picker: &Picker, hex: &str, window: &mut Window, cx: &mut App) {
    for (input, text) in [(&picker.hex, hex.to_owned()), (&picker.rgb, rgb_text(hex))] {
        let focused = input.read(cx).focus_handle(cx).is_focused(window);
        if !focused && input.read(cx).value() != text.as_str() {
            input.update(cx, |input, cx| input.set_value(text, window, cx));
        }
    }
}

impl Render for ThemeEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let editing = self.editing.is_some();
        let viewport = window.viewport_size();
        let viewport = (f32::from(viewport.width), f32::from(viewport.height));
        let width =
            self.size.map_or((416f32).min(viewport.0 - 32.), |(w, _)| w.min(viewport.0 - MARGIN * 2.).max(MIN_WIDTH));
        let height = self.size.map(|(_, h)| h.min(viewport.1 - MARGIN * 2.).max(MIN_HEIGHT));
        let position =
            self.position.map(|(x, y, w)| clamp_position(viewport, x, y, if self.size.is_some() { width } else { w }));
        let subtitle: Option<String> = (!self.minimized).then(|| match self.selected {
            Some(role) if self.advanced => {
                families().find(|f| f.role == role).map_or_else(|| role_label(role), |f| f.label.to_owned())
            }
            Some(role) => role_label(role),
            None => "Select a color below".to_owned(),
        });
        let header =
            h_flex()
                .id("theme-editor-header")
                .gap(px(4.))
                .px(px(12.))
                .py(px(8.))
                .border_b_1()
                .border_color(t.border.opacity(0.7))
                .cursor_grab()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        let bounds = this.bounds.get();
                        let (x, y) = (f32::from(event.position.x), f32::from(event.position.y));
                        let (left, top) = (f32::from(bounds.left()), f32::from(bounds.top()));
                        this.drag = Some(Drag::Move { dx: x - left, dy: y - top, width: f32::from(bounds.size.width) });
                        cx.notify();
                    }),
                )
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .items_baseline()
                        .gap(px(8.))
                        .child(div().flex_none().text_sm().font_weight(FontWeight::MEDIUM).child(if editing {
                            "Edit theme"
                        } else {
                            "Create theme"
                        }))
                        .children(subtitle.map(|text| {
                            div().min_w_0().truncate().text_xs().text_color(t.muted_foreground).child(text)
                        })),
                )
                .child(
                    Button::new("theme-editor-minimize")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(if self.minimized { IconName::ChevronUp } else { IconName::ChevronDown }))
                        .tooltip(if self.minimized { "Expand the theme editor" } else { "Minimize the theme editor" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.minimized = !this.minimized;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("theme-editor-close")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(IconName::X))
                        .tooltip("Close the theme editor")
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                );
        let entity = cx.entity().downgrade();
        let bounds = self.bounds.clone();
        let dragging = self.drag.is_some();
        let tracker = canvas(
            move |element_bounds, _, _| bounds.set(element_bounds),
            move |_, (), window, _| {
                if !dragging {
                    return;
                }
                let (moved, released) = (entity.clone(), entity);
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture {
                        return;
                    }
                    // A release that came before these handlers existed leaves no button held.
                    let held = event.pressed_button == Some(MouseButton::Left);
                    let _ = moved.update(cx, |this, cx| {
                        if held {
                            this.drag_to(event.position, window, cx);
                        } else {
                            this.drag = None;
                            cx.notify();
                        }
                    });
                });
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture {
                        let _ = released.update(cx, |this, cx| {
                            this.drag = None;
                            cx.notify();
                        });
                    }
                });
            },
        )
        .absolute()
        .size_full();
        let panel = v_flex()
            .id("theme-editor")
            .absolute()
            .map(|el| match position {
                Some((x, y)) => el.left(px(x)).top(px(y)),
                None => el.right(px(16.)).bottom(px(16.)),
            })
            .w(px(width))
            .map(|el| match (height, self.minimized) {
                (Some(height), false) => el.h(px(height)).max_h(px(viewport.1 - 16.)),
                (_, true) => el,
                (None, false) => el.max_h(px(672f32.min(viewport.1 - 96.))),
            })
            .overflow_hidden()
            .rounded(px(12.))
            .border_1()
            .border_color(t.border)
            .bg(t.background.blend(t.popover.opacity(t.glass)))
            .text_color(t.foreground)
            .shadow_lg()
            .occlude()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && this.picker.is_some() {
                    this.close_picker(true, window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(tracker)
            .child(header);
        if self.minimized {
            return panel;
        }
        panel.child(self.render_body(&t, window, cx)).child(self.render_footer(cx)).child(
            div()
                .id("theme-editor-resize")
                .absolute()
                .bottom_0()
                .right_0()
                .size(px(20.))
                .flex()
                .items_end()
                .justify_end()
                .p(px(4.))
                .cursor_nwse_resize()
                .text_color(t.muted_foreground.opacity(0.7))
                .child(Icon::new(IconName::GripHorizontal).size(px(10.)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        let bounds = this.bounds.get();
                        let viewport = window.viewport_size();
                        let (left, top) = clamp_position(
                            (f32::from(viewport.width), f32::from(viewport.height)),
                            f32::from(bounds.left()),
                            f32::from(bounds.top()),
                            f32::from(bounds.size.width),
                        );
                        this.position = Some((left, top, f32::from(bounds.size.width)));
                        this.drag = Some(Drag::Resize {
                            x: f32::from(event.position.x),
                            y: f32::from(event.position.y),
                            left,
                            top,
                            width: f32::from(bounds.size.width),
                            height: f32::from(bounds.size.height),
                        });
                        cx.stop_propagation();
                        cx.notify();
                    }),
                ),
        )
    }
}

impl ThemeEditor {
    fn render_body(&self, t: &Tokens, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let row = |label: &'static str, control: AnyElement| {
            h_flex()
                .gap(px(12.))
                .child(
                    div()
                        .w(gpui_kit::relative(1. / 3.))
                        .flex_none()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(label),
                )
                .child(div().flex_1().min_w_0().child(control))
        };
        let appearance = h_flex().p(px(2.)).gap(px(2.)).rounded(px(8.)).bg(t.input.opacity(t.input.a * 0.4)).children(
            [false, true].map(|dark| {
                let lock = self.lock_reason(dark, cx);
                let pressed = self.dark == dark;
                div()
                    .id(if dark { "theme-editor-dark" } else { "theme-editor-light" })
                    .flex_1()
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .map(|el| {
                        if pressed {
                            el.text_color(t.foreground).bg(if t.dark { t.input.opacity(0.72) } else { t.background })
                        } else {
                            el.text_color(t.muted_foreground)
                        }
                    })
                    .when(lock.is_some(), |el| el.opacity(0.5))
                    .when(lock.is_none() && !pressed, |el| el.cursor_pointer().hover(|s| s.text_color(t.foreground)))
                    .child(if dark { "Dark" } else { "Light" })
                    .when_some(lock, |el, lock| {
                        let lock: SharedString = lock.into();
                        el.tooltip(move |window, cx| Tooltip::new(lock.clone()).build(window, cx))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.set_appearance(dark, window, cx)))
            }),
        );
        let advanced = self.advanced;
        let colors_header = h_flex()
            .items_start()
            .gap(px(12.))
            .child(
                v_flex()
                    .w(gpui_kit::relative(1. / 3.))
                    .flex_none()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Colors"))
                    .when(!advanced, |el| {
                        el.child(div().text_xs().text_color(t.muted_foreground).child("Two colors, rest derived"))
                    }),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(12.))
                    .when(advanced, |el| el.child(div().flex_1().min_w_0().child(Input::new(&self.filter).small())))
                    .child(
                        h_flex()
                            .id("theme-editor-advanced")
                            .ml_auto()
                            .flex_none()
                            .gap(px(8.))
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .cursor_pointer()
                            .child("Advanced")
                            .child(super::pages::settings::widgets::switch(
                                &"theme-editor-advanced".into(),
                                advanced,
                                false,
                                t,
                            ))
                            .on_click(cx.listener(move |this, _, window, cx| this.set_advanced(!advanced, window, cx))),
                    ),
            );
        let fields: AnyElement = if advanced {
            let query = self.filter.read(cx).value().trim().to_lowercase();
            let groups: Vec<AnyElement> = GROUPS
                .iter()
                .filter_map(|(title, families)| {
                    let shown: Vec<AnyElement> = families
                        .iter()
                        .filter(|family| family_matches(family, &query))
                        .map(|family| self.render_field(family.role, family.label.to_owned(), t, window, cx))
                        .collect();
                    (!shown.is_empty()).then(|| {
                        v_flex()
                            .gap(px(8.))
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(*title))
                            .child(v_flex().gap(px(4.)).children(shown))
                            .into_any_element()
                    })
                })
                .collect();
            let empty = groups.is_empty();
            v_flex()
                .gap(px(20.))
                .children(groups)
                .when(empty, |el| el.child(div().text_xs().text_color(t.muted_foreground).child("No matches.")))
                .into_any_element()
        } else {
            v_flex()
                .gap(px(4.))
                .children(SIMPLE_ROLES.map(|role| {
                    let label = if role == "canvas" { "Background" } else { "Accent" };
                    self.render_field(role, label.to_owned(), t, window, cx)
                }))
                .into_any_element()
        };
        v_flex()
            .id("theme-editor-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap(px(12.))
            .pt(px(12.))
            .pb(px(12.))
            .pl(px(12.))
            .pr(px(6.))
            .child(row("Theme name", Input::new(&self.name).small().into_any_element()))
            .children(self.error.clone().map(|error| div().text_sm().text_color(t.error_foreground).child(error)))
            .child(row("Appearance", appearance.into_any_element()))
            .child(v_flex().gap(px(12.)).child(colors_header).child(fields))
            .into_any_element()
    }

    /// `ThemeColorField`: the label, a swatch that opens the picker, and the hex field.
    fn render_field(
        &self,
        role: &'static str,
        label: String,
        t: &Tokens,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = self.colors[usize::from(self.dark)].get(role);
        let valid = theme_color::is_color(value);
        let swatch = if valid { theme_color::to_srgba(value) } else { None }
            .map_or(Hsla::black(), |[r, g, b, a]| gpui_kit::Rgba { r, g, b, a }.into());
        let selected = self.selected == Some(role);
        let open = self.picker.as_ref().is_some_and(|picker| picker.role == role);
        let picker_label = label.clone();
        let trigger = Button::new(SharedString::from(format!("theme-swatch-{role}")))
            .ghost()
            .size(px(24.))
            .p_0()
            .flex_none()
            .rounded_full()
            .border_1()
            .border_color(t.foreground.opacity(0.3))
            .child(div().size_full().rounded_full().bg(swatch).shadow_sm())
            .tooltip(format!("Choose {label} color"));
        let entity = cx.entity().downgrade();
        let mut popover = Popover::new(SharedString::from(format!("theme-picker-{role}")))
            .anchor(gpui_kit::Anchor::TopRight)
            .open(open)
            .on_open_change(move |open, window, cx| {
                let label = picker_label.clone();
                let _ = entity.update(cx, |this, cx| {
                    if *open {
                        this.open_picker(role, label, window, cx);
                    } else {
                        this.close_picker(false, window, cx);
                    }
                });
            })
            .trigger(trigger)
            .p_0()
            .rounded(px(12.));
        if open {
            popover = popover.child(self.render_picker(t, cx));
        }
        let input = self.fields.get(role).map(|input| {
            div()
                .w(px(112.))
                .flex_none()
                .child(Input::new(input).xsmall().font_family(t.fonts.mono).when(!valid, |el| el.border_color(t.error)))
        });
        h_flex()
            .id(SharedString::from(format!("theme-field-{role}")))
            .min_h(px(44.))
            .min_w_0()
            .gap(px(8.))
            .px(px(8.))
            .py(px(6.))
            .rounded(px(8.))
            .when(selected, |el| el.bg(t.accent.opacity(0.6)).border_1().border_color(t.ring))
            .child(
                div()
                    .id(SharedString::from(format!("theme-field-label-{role}")))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .cursor_pointer()
                    .hover(|s| s.text_color(t.foreground))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = if this.selected == Some(role) { None } else { Some(role) };
                        cx.notify();
                    })),
            )
            .child(h_flex().ml_auto().flex_none().gap(px(8.)).child(popover).children(input))
            .into_any_element()
    }

    /// `ThemeColorPickerPanel`: a saturation/value plane, a hue slider, and hex and RGB fields.
    fn render_picker(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let Some(picker) = &self.picker else { return div().into_any_element() };
        let [h, s, v] = picker.hsv;
        let current: Hsla = theme_color::to_srgba(&hsv_to_hex(picker.hsv))
            .map_or(Hsla::black(), |[r, g, b, a]| gpui_kit::Rgba { r, g, b, a }.into());
        let hue_color = gpui_kit::hsla(h / 360., 1., 0.5, 1.);
        let (plane_bounds, hue_bounds) = (picker.plane.clone(), picker.hue.clone());
        let thumb = |size: f32| {
            div().absolute().size(px(size)).rounded_full().border_2().border_color(gpui_kit::white()).shadow_sm()
        };
        let plane = div()
            .id("theme-picker-plane")
            .relative()
            .h(px(128.))
            .rounded(px(8.))
            .overflow_hidden()
            .bg(hue_color)
            .cursor_crosshair()
            .child(div().absolute().inset_0().bg(linear_gradient(
                90.,
                linear_color_stop(gpui_kit::white(), 0.),
                linear_color_stop(gpui_kit::white().opacity(0.), 1.),
            )))
            .child(div().absolute().inset_0().bg(linear_gradient(
                0.,
                linear_color_stop(gpui_kit::black(), 0.),
                linear_color_stop(gpui_kit::black().opacity(0.), 1.),
            )))
            .child(canvas(move |bounds, _, _| plane_bounds.set(bounds), |_, (), _, _| {}).absolute().size_full())
            .child(thumb(12.).left(gpui_kit::relative(s)).top(gpui_kit::relative(1. - v)).ml(px(-6.)).mt(px(-6.)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.drag = Some(Drag::Plane);
                    this.drag_to(event.position, window, cx);
                }),
            );
        // Six two-stop segments: GPUI gradients take two stops.
        let hues = [0xff0000, 0xffff00, 0x00ff00, 0x00ffff, 0x0000ff, 0xff00ff, 0xff0000];
        let track =
            h_flex().h(px(10.)).w_full().rounded_full().overflow_hidden().children(hues.windows(2).map(|pair| {
                div().flex_1().h_full().bg(linear_gradient(
                    90.,
                    linear_color_stop(gpui_kit::rgb(pair[0]), 0.),
                    linear_color_stop(gpui_kit::rgb(pair[1]), 1.),
                ))
            }));
        let slider = div()
            .id("theme-picker-hue")
            .relative()
            .h(px(24.))
            .flex()
            .items_center()
            .cursor_pointer()
            .child(track)
            .child(canvas(move |bounds, _, _| hue_bounds.set(bounds), |_, (), _, _| {}).absolute().size_full())
            .child(thumb(16.).left(gpui_kit::relative(h / 360.)).ml(px(-8.)).bg(hue_color))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.drag = Some(Drag::Hue);
                    this.drag_to(event.position, window, cx);
                }),
            );
        let field = |caption: &'static str, input: &Entity<InputState>, dot: bool| {
            v_flex()
                .min_w_0()
                .gap(px(4.))
                .child(
                    div()
                        .px(px(4.))
                        .text_size(rems(0.625))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.muted_foreground)
                        .child(caption),
                )
                .child(
                    Input::new(input)
                        .small()
                        .font_family(t.fonts.mono)
                        .when(dot, |el| el.prefix(div().size(px(14.)).rounded_full().bg(current))),
                )
        };
        v_flex()
            .w(px(288.))
            .child(
                h_flex()
                    .justify_between()
                    .px(px(16.))
                    .py(px(12.))
                    .border_b_1()
                    .border_color(t.border.opacity(0.7))
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(picker.label.clone()),
                            )
                            .child(
                                div().text_size(rems(0.6875)).text_color(t.muted_foreground).child("Choose a color"),
                            ),
                    )
                    .child(div().size(px(28.)).flex_none().rounded_full().bg(current).shadow_sm()),
            )
            .child(
                v_flex().gap(px(12.)).p(px(12.)).child(plane).child(slider).child(
                    h_flex()
                        .gap(px(8.))
                        .child(div().flex_1().min_w_0().child(field("HEX", &picker.hex, true)))
                        .child(div().w(gpui_kit::relative(0.54)).flex_none().child(field("RGB", &picker.rgb, false))),
                ),
            )
            .into_any_element()
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let target = self.merge_target(cx);
        let mode = if self.dark { "dark" } else { "light" };
        let (icon, label) = match (&self.editing, &target) {
            (Some(_), Some(target)) => (None, format!("Merge into “{}”", target.label)),
            (Some(_), None) => (None, "Save changes".to_owned()),
            (None, Some(_)) => (Some(IconName::Plus), format!("Add {mode} palette")),
            (None, None) => (Some(IconName::Paintbrush), "Create theme".to_owned()),
        };
        let mut submit = Button::new("theme-editor-submit")
            .primary()
            .small()
            .label(label)
            .disabled(self.name(cx).trim().is_empty())
            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)));
        if let Some(icon) = icon {
            submit = submit.icon(Icon::new(icon));
        }
        h_flex()
            .justify_end()
            .gap(px(8.))
            .px(px(12.))
            .py(px(8.))
            .border_t_1()
            .border_color(t.border.opacity(0.7))
            .child(
                Button::new("theme-editor-cancel")
                    .ghost()
                    .small()
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
            )
            .child(submit)
            .into_any_element()
    }
}

impl Workspace {
    /// `openThemeEditor`: a new session replaces an open one.
    pub(in crate::app) fn open_theme_editor(&mut self, session: Session, window: &mut Window, cx: &mut Context<Self>) {
        let (workspace, library) = (cx.weak_entity(), self.client_settings.themes.clone());
        self.theme_editor = Some(cx.new(|cx| ThemeEditor::new(session, workspace, &library, window, cx)));
        cx.notify();
    }

    /// `restoreTheme` on close: the stored pick paints again.
    pub(in crate::app) fn close_theme_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.theme_editor.take().is_some() {
            theme::set_editor_preview(None, window, cx);
            cx.notify();
        }
    }

    /// `themeEditor.toggle` (`toggleThemeEditorForTheme`): closes an open editor, else opens
    /// one seeded from the theme the current appearance uses.
    pub(in crate::app) fn toggle_theme_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.theme_editor.is_some() {
            self.close_theme_editor(window, cx);
            return;
        }
        let dark = theme::tokens(cx).dark;
        let seed = self.client_settings.theme_choice().owner(dark);
        self.open_theme_editor(Session { seed, ..Session::new(dark) }, window, cx);
    }

    /// Stores the saved themes and the pick together, then paints.
    pub(in crate::app) fn store_themes(
        &mut self,
        library: Library,
        choice: ThemeChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = &self.theme_editor {
            let library = library.clone();
            editor.update(cx, |editor, cx| {
                editor.library = library;
                cx.notify();
            });
        }
        self.client_settings.set_themes(library, choice.clone());
        theme::set_preference(choice, window, cx);
        self.sync_composer_marks(cx);
        cx.notify();
    }

    /// `ThemeEditorHost.handleSaved`.
    fn theme_saved(
        &mut self,
        library: Library,
        saved: ThemeDefinition,
        outcome: Saved,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut choice = ThemeChoice { library: library.clone(), ..self.client_settings.theme_choice() };
        let (title, description) = match outcome {
            Saved::Merged(dark) => {
                choice.use_theme(Some(saved.id.clone()));
                (
                    format!("{} updated", saved.label),
                    format!("Its {} palette was added.", if dark { "dark" } else { "light" }),
                )
            }
            Saved::Edited => {
                let active = choice.base.as_deref() == Some(&saved.id)
                    || choice.halves.light.as_deref() == Some(&saved.id)
                    || choice.halves.dark.as_deref() == Some(&saved.id);
                let description = if active { "Your changes are now active." } else { "Your changes are saved." };
                (format!("{} saved", saved.label), description.to_owned())
            }
            Saved::Created => {
                choice.use_theme(Some(saved.id.clone()));
                (format!("{} created", saved.label), "It’s now active.".to_owned())
            }
        };
        self.theme_editor = None;
        theme::set_editor_preview(None, window, cx);
        self.store_themes(library, choice, window, cx);
        toast(window, cx, title, description);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_labels_and_filters_follow_the_web() {
        assert_eq!(role_label("canvas"), "Background");
        assert_eq!(role_label("sidebarRowHover"), "Sidebar Row Hover");
        let border = families().find(|f| f.role == "border").unwrap();
        assert!(family_matches(border, "toolbar border") && family_matches(border, ""));
        assert!(!family_matches(border, "terminal"));
        assert_eq!(families().count(), 22);
    }

    #[test]
    fn panel_positions_keep_the_header_reachable() {
        assert_eq!(clamp_position((1000., 800.), -50., 900., 400.), (MARGIN, 752.));
        assert_eq!(clamp_position((1000., 800.), 900., 20., 400.), (592., 20.));
        assert_eq!(rgb_to_hex("rgb(255, 0, 16)").as_deref(), Some("#ff0010"));
        assert_eq!(rgb_to_hex("1 2"), None);
        assert_eq!(rgb_text("#ff0010"), "255, 0, 16");
    }

    #[test]
    fn guided_saves_regenerate_from_both_seeds() {
        let mut colors = themes::standard_roles(true).clone();
        colors.set("canvas", "#101014".into());
        colors.set("accent", "not yet".into());
        let palette = managed(true, &colors);
        assert_eq!(theme_color::to_hex(palette.get("canvas")).as_deref(), Some("#101014"));
        assert_eq!(field_text(palette.get("canvas")), "#101014");
        assert_eq!(field_text("#12"), "#12");
    }
}
