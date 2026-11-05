//! The prompt stash (`promptStashStore.ts`, `useComposerStash.ts`, `ComposerStashBadge.tsx`,
//! `ComposerStashMenu.tsx`): `composer.stash` sets the composer's prompt and attachments
//! aside on this device, and the Stash tab above the composer brings one back.
//!
//! Entries live in `<T3 home>/userdata/prompt-stash/stash.json`, newest first, with each
//! attachment's bytes in a file beside it. The web re-encodes images into localStorage and
//! keeps files by upload id; files on disk have no quota, so nothing is dropped and a
//! restore uploads the bytes again.

use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Icon, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, ElementId, Focusable as _, FontWeight, InteractiveElement as _, IntoElement, Keystroke,
    MouseButton, ObjectFit, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Window, div, img, px, relative,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::attachments::{Attachment, MAX_ATTACHMENTS};
use super::{Workspace, new_id};
use crate::keybindings::{self, Shortcut};
use crate::theme::Tokens;

pub(super) const MAX_STASH_ENTRIES: usize = 20;
const SNIPPET_MAX_CHARS: usize = 90;
const STORE_VERSION: u64 = 1;
/// `ComposerBanner.Attachment` inset and the strip the composer covers.
const DRAWER_INSET: f32 = 22.;
const OVERLAP: f32 = 17.;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct StashedAttachment {
    /// The attachment's composer id, which the prompt's chips name.
    local_id: String,
    kind: String,
    name: String,
    mime_type: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct StashEntry {
    pub id: String,
    pub created_at: String,
    pub prompt: String,
    #[serde(default)]
    attachments: Vec<StashedAttachment>,
}

impl StashEntry {
    fn images(&self) -> impl Iterator<Item = &StashedAttachment> {
        self.attachments.iter().filter(|a| a.kind == "image")
    }

    fn file_count(&self) -> usize {
        self.attachments.iter().filter(|a| a.kind != "image").count()
    }
}

/// An attachment taken out of the stash; `None` bytes when its file could not be read.
pub(super) struct TakenAttachment {
    pub local_id: String,
    pub kind: String,
    pub name: String,
    pub mime: String,
    pub bytes: Option<Vec<u8>>,
}

/// The stash on disk; `dir` is `None` only when there is no home directory.
pub(super) struct Store {
    dir: Option<PathBuf>,
    entries: Vec<StashEntry>,
}

impl Store {
    pub(super) fn load() -> Self {
        Self::open(crate::settings::userdata_dir().map(|dir| dir.join("prompt-stash")))
    }

    fn open(dir: Option<PathBuf>) -> Self {
        let entries = dir
            .as_deref()
            .and_then(|dir| std::fs::read_to_string(dir.join("stash.json")).ok())
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|document| serde_json::from_value(document["entries"].clone()).ok())
            .unwrap_or_default();
        Self { dir, entries }
    }

    pub(super) fn entries(&self) -> &[StashEntry] {
        &self.entries
    }

    fn blob_path(dir: &Path, entry_id: &str, attachment: &StashedAttachment) -> PathBuf {
        let extension = attachment.mime_type.rsplit('/').next().filter(|e| e.chars().all(char::is_alphanumeric));
        dir.join(format!("{entry_id}-{}.{}", attachment.local_id, extension.unwrap_or("bin")))
    }

    fn persist(&self, entries: &[StashEntry]) -> std::io::Result<()> {
        let Some(dir) = &self.dir else { return Err(std::io::ErrorKind::NotFound.into()) };
        let document = json!({ "version": STORE_VERSION, "entries": entries });
        crate::settings::write_atomically(&dir.join("stash.json"), &document)
    }

    fn remove_blobs(&self, entry: &StashEntry) {
        let Some(dir) = &self.dir else { return };
        for attachment in &entry.attachments {
            let _ = std::fs::remove_file(Self::blob_path(dir, &entry.id, attachment));
        }
    }

    /// Writes the entry and its attachments ahead of the rest, evicting the oldest past
    /// [`MAX_STASH_ENTRIES`]. On failure nothing is kept.
    pub(super) fn stash(
        &mut self,
        prompt: String,
        attachments: &[&Attachment],
        created_at: String,
    ) -> std::io::Result<Option<StashEntry>> {
        let Some(dir) = self.dir.clone() else { return Err(std::io::ErrorKind::NotFound.into()) };
        let entry = StashEntry {
            id: new_id(),
            created_at,
            prompt,
            attachments: attachments
                .iter()
                .map(|a| StashedAttachment {
                    local_id: a.local_id().to_owned(),
                    kind: a.kind_str().to_owned(),
                    name: a.name().to_owned(),
                    mime_type: a.mime().to_owned(),
                })
                .collect(),
        };
        let written = std::fs::create_dir_all(&dir).and_then(|()| {
            entry.attachments.iter().zip(attachments).try_for_each(|(stashed, attachment)| {
                std::fs::write(Self::blob_path(&dir, &entry.id, stashed), attachment.bytes().as_slice())
            })
        });
        let mut next = Vec::with_capacity(self.entries.len() + 1);
        next.push(entry.clone());
        next.extend(self.entries.iter().cloned());
        let evicted = (next.len() > MAX_STASH_ENTRIES).then(|| next.pop()).flatten();
        if let Err(error) = written.and_then(|()| self.persist(&next)) {
            self.remove_blobs(&entry);
            return Err(error);
        }
        self.entries = next;
        if let Some(evicted) = &evicted {
            self.remove_blobs(evicted);
        }
        Ok(evicted)
    }

    /// Removes an entry and hands back its contents; `false` when the removal could not be
    /// saved, so the entry would come back on the next launch.
    pub(super) fn take(&mut self, id: &str) -> Option<(StashEntry, Vec<TakenAttachment>, bool)> {
        let entry = self.entries.iter().find(|e| e.id == id)?.clone();
        let taken = entry
            .attachments
            .iter()
            .map(|a| TakenAttachment {
                local_id: a.local_id.clone(),
                kind: a.kind.clone(),
                name: a.name.clone(),
                mime: a.mime_type.clone(),
                bytes: self.dir.as_deref().and_then(|dir| std::fs::read(Self::blob_path(dir, &entry.id, a)).ok()),
            })
            .collect();
        let durable = self.delete(id);
        Some((entry, taken, durable))
    }

    /// `false` when the removal could not be saved.
    pub(super) fn delete(&mut self, id: &str) -> bool {
        let Some(index) = self.entries.iter().position(|e| e.id == id) else { return true };
        let next: Vec<StashEntry> = self.entries.iter().filter(|e| e.id != id).cloned().collect();
        let durable = self.persist(&next).is_ok();
        let removed = self.entries.remove(index);
        if durable {
            self.remove_blobs(&removed);
        }
        durable
    }

    fn thumbnail(&self, entry: &StashEntry, attachment: &StashedAttachment) -> Option<PathBuf> {
        Some(Self::blob_path(self.dir.as_deref()?, &entry.id, attachment))
    }
}

/// `stashEntrySnippet`: the prompt on one line, or what it carried.
fn snippet(entry: &StashEntry) -> String {
    let collapsed = entry.prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if !collapsed.is_empty() {
        return match collapsed.char_indices().nth(SNIPPET_MAX_CHARS) {
            Some((at, _)) => format!("{}…", &collapsed[..at]),
            None => collapsed,
        };
    }
    let images = entry.images().count();
    let files = entry.file_count();
    match images + files {
        0 => "(empty)".into(),
        count => {
            let label = if images > 0 && files > 0 {
                "attachment"
            } else if files > 0 {
                "file"
            } else {
                "image"
            };
            format!("({count} {label}{})", if count == 1 { "" } else { "s" })
        }
    }
}

/// A restored prompt goes after what the composer holds, a blank line apart.
fn restored_prompt(current: &str, restored: &str) -> String {
    if restored.is_empty() {
        current.to_owned()
    } else if current.trim().is_empty() {
        restored.to_owned()
    } else {
        format!("{}\n\n{restored}", current.trim_end())
    }
}

/// `formatRelativeElapsed`.
fn relative_time(elapsed_ms: i64) -> String {
    let minutes = elapsed_ms.max(0) / 60_000;
    match minutes {
        0 => "just now".into(),
        1..60 => format!("{minutes}m ago"),
        60..1440 => format!("{}h ago", minutes / 60),
        _ => format!("{}d ago", minutes / 1440),
    }
}

/// `formatShortcutLabel` for this platform.
fn shortcut_label(shortcut: &Shortcut) -> String {
    let key = match shortcut.key.as_str() {
        " " | "space" => "Space".to_owned(),
        key if key.chars().count() == 1 => key.to_uppercase(),
        key => {
            let mut chars = key.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
        }
    };
    let mac = cfg!(target_os = "macos");
    let meta = shortcut.meta_key || (shortcut.mod_key && mac);
    let ctrl = shortcut.ctrl_key || (shortcut.mod_key && !mac);
    if mac {
        let glyph = |on: bool, glyph: &'static str| if on { glyph } else { "" };
        return format!(
            "{}{}{}{}{key}",
            glyph(ctrl, "⌃"),
            glyph(shortcut.alt_key, "⌥"),
            glyph(shortcut.shift_key, "⇧"),
            glyph(meta, "⌘")
        );
    }
    let mut parts: Vec<String> =
        [(ctrl, "Ctrl"), (shortcut.alt_key, "Alt"), (shortcut.shift_key, "Shift"), (meta, "Meta")]
            .into_iter()
            .filter(|(on, _)| *on)
            .map(|(_, label)| label.to_owned())
            .collect();
    parts.push(key);
    parts.join("+")
}

/// The stash and its drawer.
pub(super) struct PromptStash {
    store: Store,
    pub open: bool,
    highlighted: Option<String>,
}

impl Default for PromptStash {
    fn default() -> Self {
        Self { store: Store::load(), open: false, highlighted: None }
    }
}

fn toast(window: &mut Window, cx: &mut Context<Workspace>, notification: Notification) {
    window.push_notification(notification, cx);
}

impl Workspace {
    /// `handleStashShortcut`.
    /// The `composer.stash` command (Mod+S).
    pub(super) fn stash_shortcut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() || self.page.is_some() || self.settings.is_some() {
            return;
        }
        if self.thread.as_ref().is_some_and(|t| self.rewinding.contains(&t.id)) {
            return;
        }
        if self.question.is_some() {
            self.toggle_stash_menu(cx);
            return;
        }
        if self.target.is_none() {
            return;
        }
        self.stash_current_prompt(window, cx);
    }

    /// `stashCurrentPrompt`: an empty composer restores the only entry or opens the stash.
    fn stash_current_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.composer.read(cx).value().trim().to_owned();
        if prompt.is_empty() && self.attachments.is_empty() {
            match self.prompt_stash.store.entries() {
                [only] => {
                    let id = only.id.clone();
                    self.restore_stash_entry(&id, window, cx);
                }
                _ => self.toggle_stash_menu(cx),
            }
            return;
        }
        let attachments: Vec<&Attachment> = self.attachments.iter().collect();
        let stashed = self.prompt_stash.store.stash(
            prompt,
            &attachments,
            crate::timestamp::iso(crate::timestamp::now_ms()).unwrap_or_default(),
        );
        let evicted = match stashed {
            Ok(evicted) => evicted,
            Err(error) => {
                log::warn!("could not stash the prompt: {error}");
                let message = "The stash could not be saved, so the composer was left as-is.";
                toast(window, cx, Notification::error(message).title("Could not stash this prompt"));
                return;
            }
        };
        // The bytes are on disk; a restore uploads them again.
        if let Some(client) = self.client() {
            for id in self.attachments.iter().filter_map(Attachment::uploaded_id) {
                let (client, input) = (client.clone(), json!({ "attachmentId": id }));
                cx.spawn(async move |_, _| client.request("attachments.delete", input).await).detach();
            }
        }
        self.attachments.clear();
        self.set_composer_text("", window, cx);
        self.prompt_history = None;
        if evicted.is_some() {
            let message = format!("The stash holds {MAX_STASH_ENTRIES} prompts; the oldest was removed to make room.");
            toast(window, cx, Notification::warning(message).title("Oldest stashed prompt discarded"));
        }
        cx.notify();
    }

    pub(super) fn toggle_stash_menu(&mut self, cx: &mut Context<Self>) {
        self.prompt_stash.open = !self.prompt_stash.open;
        self.prompt_stash.highlighted = self.prompt_stash.store.entries().first().map(|e| e.id.clone());
        cx.notify();
    }

    pub(super) fn close_stash_menu(&mut self, cx: &mut Context<Self>) {
        if self.prompt_stash.open {
            self.prompt_stash.open = false;
            cx.notify();
        }
    }

    /// `restoreStashEntry`: the prompt goes after the composer's text, its attachments
    /// after the composer's, up to the attachment limit.
    fn restore_stash_entry(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.target.is_none() || self.thread.as_ref().is_some_and(|t| self.rewinding.contains(&t.id)) {
            return;
        }
        self.prompt_stash.open = false;
        let Some((entry, taken, durable)) = self.prompt_stash.store.take(id) else { return };
        if !durable {
            let message = "The stash file could not be updated, so this entry could still be there after a restart.";
            toast(window, cx, Notification::warning(message).title("Restored prompt may reappear in the stash"));
        }
        let mut unreadable = Vec::new();
        let mut over_limit = Vec::new();
        for attachment in taken {
            let Some(bytes) = attachment.bytes else {
                unreadable.push(attachment.name);
                continue;
            };
            if self.attachments.len() >= MAX_ATTACHMENTS {
                over_limit.push(attachment.name);
                continue;
            }
            let restored =
                Attachment::restored_as(attachment.local_id, &attachment.kind, attachment.name, attachment.mime, bytes);
            let local_id = restored.local_id().to_owned();
            self.attachments.push(restored);
            self.upload(local_id, cx);
        }
        match &mut self.question {
            Some(question) => question.prompt = restored_prompt(&question.prompt, &entry.prompt),
            None => {
                let current = self.composer.read(cx).value().to_string();
                let next = restored_prompt(&current, &entry.prompt);
                if next != current {
                    self.set_composer_text(next, window, cx);
                    self.composer.update(cx, |state, cx| {
                        let end = state.value().len();
                        state.set_selected_range(end..end, cx);
                    });
                    self.composer.read(cx).focus_handle(cx).focus(window, cx);
                }
            }
        }
        let mut reasons = Vec::new();
        if !unreadable.is_empty() {
            reasons.push(format!("{} could not be read when this prompt was restored.", unreadable.join(", ")));
        }
        if !over_limit.is_empty() {
            reasons.push(format!(
                "{} could not be restored: the composer is at its {MAX_ATTACHMENTS}-attachment limit.",
                over_limit.join(", ")
            ));
        }
        if !reasons.is_empty() {
            toast(window, cx, Notification::warning(reasons.join(" ")).title("Some attachments were not restored"));
        }
        cx.notify();
    }

    /// `deleteStashEntry`; the drawer closes with its last entry.
    fn delete_stash_entry(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.prompt_stash.store.entries().iter().any(|e| e.id != id) {
            self.prompt_stash.open = false;
        }
        if self.prompt_stash.highlighted.as_deref() == Some(id) {
            let entries = self.prompt_stash.store.entries();
            let at = entries.iter().position(|e| e.id == id).unwrap_or(0);
            let next = entries.get(at + 1).or_else(|| at.checked_sub(1).and_then(|i| entries.get(i)));
            self.prompt_stash.highlighted = next.map(|e| e.id.clone());
        }
        if !self.prompt_stash.store.delete(id) {
            let message = "The stash file could not be updated, so this prompt could reappear after a restart.";
            toast(window, cx, Notification::warning(message).title("Stash entry may come back"));
        }
        cx.notify();
    }

    /// The open drawer's keys: Escape, Up/Down, Enter restores, Mod+Backspace deletes.
    pub(super) fn stash_menu_key(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.prompt_stash.open {
            return false;
        }
        let in_composer = self.composer.read(cx).focus_handle(cx).is_focused(window) || self.focus.is_focused(window);
        if !in_composer {
            return false;
        }
        let m = keystroke.modifiers;
        let entries = self.prompt_stash.store.entries();
        let highlighted = self.prompt_stash.highlighted.clone().filter(|id| entries.iter().any(|e| &e.id == id));
        let highlighted = highlighted.or_else(|| entries.first().map(|e| e.id.clone()));
        match keystroke.key.as_str() {
            "escape" => self.close_stash_menu(cx),
            "up" | "down" if !entries.is_empty() => {
                let current = entries.iter().position(|e| Some(&e.id) == highlighted.as_ref());
                let len = entries.len() as isize;
                let step = if keystroke.key == "down" { 1 } else { -1 };
                let from = current.map_or(if step == 1 { -1 } else { 0 }, |i| i as isize);
                let next = (from + step).rem_euclid(len) as usize;
                self.prompt_stash.highlighted = Some(entries[next].id.clone());
                cx.notify();
            }
            "enter" if !m.shift && !m.alt => {
                let Some(id) = highlighted else { return false };
                self.restore_stash_entry(&id, window, cx);
            }
            "backspace" if m.control || m.platform => {
                let Some(id) = highlighted else { return false };
                self.delete_stash_entry(&id, window, cx);
            }
            _ => return false,
        }
        true
    }

    /// `ComposerStashBadge`: the "Stash" tab on the composer's shoulder.
    pub(super) fn render_stash_badge(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let count = self.prompt_stash.store.entries().len();
        if count == 0 {
            return None;
        }
        let open = self.prompt_stash.open;
        let row = h_flex()
            .id("stash-badge")
            .h(px(24.))
            .gap(px(4.))
            .rounded(px(12.))
            .cursor_pointer()
            .text_color(if open { t.foreground } else { t.muted_foreground })
            .when(!open, |el| el.hover(|s| s.bg(t.foreground.opacity(0.05)).text_color(t.foreground)))
            .child(banner_icon(Lucide::Bookmark))
            .child("Stash")
            .child(count_label(count, t))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .when(!open, |el| el.on_click(cx.listener(|this, _, _, cx| this.toggle_stash_menu(cx))));
        Some(
            div()
                .flex_none()
                .mb(px(-OVERLAP))
                .px(px(4.))
                .pt(px(5.))
                .pb(px(5. + OVERLAP))
                .rounded_t(px(16.))
                .border_1()
                .border_color(banner_outline(t))
                .bg(t.card.opacity(t.glass))
                .text_xs()
                .line_height(px(16.))
                .child(row)
                .into_any_element(),
        )
    }

    /// `ComposerStashMenu`, in the command menu's place above the composer body.
    pub(super) fn render_stash_menu(&self, max_height: f32, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.prompt_stash.open || self.composer_menu_open() {
            return None;
        }
        let entries = self.prompt_stash.store.entries();
        let highlighted = self
            .prompt_stash
            .highlighted
            .as_deref()
            .filter(|id| entries.iter().any(|e| e.id == *id))
            .or_else(|| entries.first().map(|e| e.id.as_str()));
        let header = h_flex()
            .id("stash-close")
            .w_full()
            .h(px(24.))
            .gap(px(4.))
            .rounded(px(12.))
            .cursor_pointer()
            .hover(|s| s.bg(t.foreground.opacity(0.05)))
            .child(banner_icon(Lucide::Bookmark).text_color(t.muted_foreground))
            .child(div().flex_1().min_w_0().text_color(t.muted_foreground).child("Stash"))
            .child(count_label(entries.len(), t))
            .child(icon_slot().child(Icon::new(Lucide::ChevronDown).size(px(14.))))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener(|this, _, _, cx| this.close_stash_menu(cx)));
        let rows: Vec<AnyElement> = if entries.is_empty() {
            let shortcut = self.stash_shortcut_label();
            let text = match shortcut {
                Some(label) => format!("Nothing stashed yet. Press {label} with a prompt in the composer to stash it."),
                None => "Nothing stashed yet.".into(),
            };
            vec![
                h_flex()
                    .min_h(px(20.))
                    .gap(px(4.))
                    .child(icon_slot())
                    .child(div().flex_1().min_w_0().text_color(t.muted_foreground).child(text))
                    .into_any_element(),
            ]
        } else {
            let now = crate::timestamp::now_ms();
            entries
                .iter()
                .map(|entry| self.render_stash_row(entry, highlighted == Some(&entry.id), now, t, cx))
                .collect()
        };
        Some(
            div()
                .absolute()
                .left(px(DRAWER_INSET))
                .right(px(DRAWER_INSET))
                .bottom(relative(1.))
                .mb(px(-OVERLAP))
                .child(
                    v_flex()
                        .id("stash-menu")
                        .occlude()
                        .px(px(4.))
                        .pt(px(4.))
                        .pb(px(4. + OVERLAP))
                        .rounded_t(px(16.))
                        .border_1()
                        .border_color(banner_outline(t))
                        .bg(t.background.blend(t.card.opacity(t.glass)))
                        .text_xs()
                        .line_height(px(16.))
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_stash_menu(cx)))
                        .child(header)
                        .child(
                            v_flex()
                                .id("stash-entries")
                                .max_h(px(max_height.min(384.)))
                                .overflow_y_scroll()
                                .gap(px(1.))
                                .children(rows),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_stash_row(
        &self,
        entry: &StashEntry,
        highlighted: bool,
        now_ms: i64,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = entry.id.clone();
        let restore_id = id.clone();
        let delete_id = id.clone();
        let thumbs: Vec<PathBuf> =
            entry.images().take(3).filter_map(|a| self.prompt_stash.store.thumbnail(entry, a)).collect();
        let files = entry.file_count();
        let age = crate::timestamp::parse_ms(&entry.created_at).map(|at| relative_time(now_ms - at));
        h_flex()
            .id(ElementId::Name(format!("stash-entry-{id}").into()))
            .min_h(px(20.))
            .gap(px(4.))
            .rounded(px(4.))
            .cursor_pointer()
            .when(highlighted, |el| el.bg(t.accent).text_color(t.foreground))
            .child(banner_icon(Lucide::FileText).text_color(t.muted_foreground))
            .child(div().flex_1().min_w_0().truncate().text_color(t.foreground.opacity(0.8)).child(snippet(entry)))
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(4.))
                    .when(!thumbs.is_empty(), |el| {
                        el.child(h_flex().children(thumbs.into_iter().enumerate().map(|(index, path)| {
                            img(path)
                                .size(px(16.))
                                .when(index > 0, |el| el.ml(px(-6.)))
                                .rounded(px(4.))
                                .border_1()
                                .border_color(t.border.opacity(0.7))
                                .object_fit(ObjectFit::Cover)
                        })))
                    })
                    .when(files > 0, |el| {
                        el.child(
                            h_flex()
                                .gap(px(4.))
                                .text_color(t.muted_foreground)
                                .child(Icon::new(Lucide::File).size(px(12.)))
                                .child(files.to_string()),
                        )
                    })
                    .children(age.map(|age| div().text_color(t.muted_foreground).child(age)))
                    .child(
                        icon_slot()
                            .id(ElementId::Name(format!("stash-delete-{id}").into()))
                            .rounded(px(8.))
                            .cursor_pointer()
                            .text_color(t.foreground)
                            .hover(|s| s.bg(t.accent))
                            .child(Icon::new(Lucide::X).size(px(14.)))
                            .tooltip(|window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new("Delete stashed prompt").build(window, cx)
                            })
                            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.delete_stash_entry(&delete_id, window, cx);
                            })),
                    ),
            )
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                if this.prompt_stash.highlighted.as_deref() != Some(id.as_str()) {
                    this.prompt_stash.highlighted = Some(id.clone());
                    cx.notify();
                }
            }))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener(move |this, _, window, cx| this.restore_stash_entry(&restore_id, window, cx)))
            .into_any_element()
    }

    /// The last `composer.stash` rule's shortcut.
    fn stash_shortcut_label(&self) -> Option<String> {
        let rules = keybindings::merge_with_defaults(&self.server_info.keybindings);
        rules.iter().rev().find(|rule| rule.command == "composer.stash").map(|rule| shortcut_label(&rule.shortcut))
    }
}

fn banner_outline(t: &Tokens) -> gpui_kit::Hsla {
    if t.dark { gpui_kit::hsla(0., 0., 1., 0.05) } else { gpui_kit::hsla(0., 0., 0., 0.08) }
}

/// The banner row's 24px icon column.
fn icon_slot() -> gpui_kit::Div {
    div().size(px(24.)).flex_none().flex().items_center().justify_center()
}

fn banner_icon(icon: Lucide) -> gpui_kit::Div {
    icon_slot().child(Icon::new(icon).size(px(12.)))
}

/// `ComposerBanner.Count`.
fn count_label(count: usize, t: &Tokens) -> impl IntoElement {
    div()
        .min_w(px(24.))
        .flex()
        .flex_none()
        .justify_center()
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.muted_foreground)
        .child(SharedString::from(count.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(prompt: &str, kinds: &[&str]) -> StashEntry {
        StashEntry {
            id: "e".into(),
            created_at: String::new(),
            prompt: prompt.into(),
            attachments: kinds
                .iter()
                .enumerate()
                .map(|(i, kind)| StashedAttachment {
                    local_id: i.to_string(),
                    kind: (*kind).into(),
                    name: format!("a{i}"),
                    mime_type: "image/png".into(),
                })
                .collect(),
        }
    }

    #[test]
    fn snippets_collapse_whitespace_or_name_the_attachments() {
        assert_eq!(snippet(&entry("  fix\n\n the   bug ", &[])), "fix the bug");
        let long = "é".repeat(SNIPPET_MAX_CHARS + 1);
        assert_eq!(snippet(&entry(&long, &[])), format!("{}…", "é".repeat(SNIPPET_MAX_CHARS)));
        assert_eq!(snippet(&entry("", &[])), "(empty)");
        assert_eq!(snippet(&entry(" ", &["image"])), "(1 image)");
        assert_eq!(snippet(&entry("", &["file", "file"])), "(2 files)");
        assert_eq!(snippet(&entry("", &["image", "file"])), "(2 attachments)");
    }

    #[test]
    fn restored_prompts_follow_the_composer_text() {
        assert_eq!(restored_prompt("", "stashed"), "stashed");
        assert_eq!(restored_prompt("  \n", "stashed"), "stashed");
        assert_eq!(restored_prompt("typed  \n", "stashed"), "typed\n\nstashed");
        assert_eq!(restored_prompt("typed", ""), "typed");
    }

    #[test]
    fn the_store_keeps_twenty_entries_on_disk_and_takes_one_back_with_its_bytes() {
        let dir = std::env::temp_dir().join(format!("t3-gpui-prompt-stash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = Store::open(Some(dir.clone()));
        let image = Attachment::restored_as("img1".into(), "image", "shot.png".into(), "image/png".into(), vec![1, 2]);
        store.stash("first".into(), &[&image], "2026-01-01T00:00:00Z".into()).unwrap();
        let first = store.entries()[0].id.clone();
        for i in 1..MAX_STASH_ENTRIES {
            assert_eq!(store.stash(format!("p{i}"), &[], String::new()).unwrap(), None);
        }
        let evicted = store.stash("newest".into(), &[], String::new()).unwrap();
        assert_eq!(evicted.map(|e| e.id), Some(first.clone()), "the oldest goes first");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "its bytes went with it");

        let reloaded = Store::open(Some(dir.clone()));
        assert_eq!(reloaded.entries(), store.entries());
        assert_eq!(reloaded.entries()[0].prompt, "newest");

        store.stash("with image".into(), &[&image], String::new()).unwrap();
        let id = store.entries()[0].id.clone();
        let (taken, attachments, durable) = store.take(&id).unwrap();
        assert!(durable);
        assert_eq!(taken.prompt, "with image");
        assert_eq!((attachments[0].local_id.as_str(), attachments[0].bytes.as_deref()), ("img1", Some(&[1u8, 2][..])));
        assert!(store.take(&id).is_none());
        assert_eq!(Store::open(Some(dir.clone())).entries().len(), MAX_STASH_ENTRIES - 1, "the take was saved");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn relative_times_match_the_web() {
        assert_eq!(relative_time(59_999), "just now");
        assert_eq!(relative_time(5 * 60_000), "5m ago");
        assert_eq!(relative_time(3 * 3_600_000), "3h ago");
        assert_eq!(relative_time(2 * 86_400_000), "2d ago");
    }
}
