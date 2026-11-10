//! `BrowserImportWizard` (`apps/web/src/components/settings/BrowserImportWizard.tsx` and
//! `browserImportWizard.logic.ts`): choosing a browser profile's cookies and the profile they go
//! into, through the browser-running, Full Disk Access, importing, done and blocked steps.

use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogDescription, DialogEntrance, DialogFooter, DialogHeader, DialogTitle};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, FontWeight, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Task, WeakEntity, Window, div, px,
};

use crate::app::Workspace;
use crate::app::preview::{ImportOutcome, ImportTarget};
use crate::browser_import::{Reason, Source, SourceProfile};
use crate::preview::{self, Profile, ProfileKind};
use crate::theme::{self, Tokens};

const TARGET_MISSING: &str = "That profile is no longer available. Choose where to import these cookies.";
const TARGET_UNCREATABLE: &str = "You've reached the profile limit. Choose an existing profile to import into.";
/// `POLL_INTERVAL_MS` of `usePermissionStatus`.
const PERMISSION_POLL: Duration = Duration::from_millis(1500);
const FULL_DISK_ACCESS_SETTINGS: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// `WizardTargetSelection`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TargetSelection {
    New,
    Existing(String),
}

/// `initialTargetSelection`.
fn initial_target(can_create: bool, targets: &[Profile]) -> TargetSelection {
    match targets.first() {
        Some(first) if !can_create => TargetSelection::Existing(first.id.clone()),
        _ => TargetSelection::New,
    }
}

/// `resolveWizardTarget`: `None` once the chosen profile is gone.
fn resolve_target(selection: &TargetSelection, new_profile_id: &str, targets: &[Profile]) -> Option<ImportTarget> {
    match selection {
        TargetSelection::New => Some(ImportTarget::New { profile_id: new_profile_id.to_owned() }),
        TargetSelection::Existing(id) => targets
            .iter()
            .find(|profile| &profile.id == id)
            .map(|profile| ImportTarget::Existing { profile_id: id.clone(), name: profile.name.clone() }),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Resume {
    Configure,
    Import,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Check {
    Browser,
    FullDiskAccess,
}

/// `WizardStep`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    Quit,
    FullDiskAccess { resume: Resume, checked: bool },
    Configure,
    Checking(Check),
    Importing,
    Done { imported: usize, skipped: usize, skipped_domains: Vec<String>, target_name: String },
    Blocked { reason: Reason, message: Option<String> },
}

impl Step {
    /// `canCloseWizard`: an import under way finishes first.
    fn closable(&self) -> bool {
        *self != Self::Importing
    }

    /// `initialWizardStep`.
    fn initial(source: &Source) -> Self {
        match source.unavailable {
            Some(Reason::BrowserRunning) => Self::Quit,
            Some(Reason::NeedsFullDiskAccess) => Self::FullDiskAccess { resume: Resume::Configure, checked: false },
            Some(reason) => Self::Blocked { reason, message: None },
            None if source.profiles.is_empty() => Self::Blocked { reason: Reason::UnknownSourceProfile, message: None },
            None => Self::Configure,
        }
    }

    /// `outcomeToStep`.
    fn after(outcome: ImportOutcome) -> Self {
        match outcome {
            ImportOutcome::Imported { imported, skipped, skipped_domains, target_name } => {
                Self::Done { imported, skipped, skipped_domains, target_name }
            }
            ImportOutcome::Blocked { reason: Reason::BrowserRunning, .. } => Self::Quit,
            ImportOutcome::Blocked { reason: Reason::NeedsFullDiskAccess, .. } => {
                Self::FullDiskAccess { resume: Resume::Import, checked: true }
            }
            ImportOutcome::Blocked { reason, message } => Self::Blocked { reason, message },
        }
    }

    /// `refreshedSourceStep`.
    fn refreshed(source: Option<&Source>) -> Self {
        source.map_or(Self::Blocked { reason: Reason::UnknownSource, message: None }, Self::initial)
    }

    /// `fullDiskAccessRecheckStep`: still without access, the step says it checked.
    fn rechecked_full_disk_access(source: Option<&Source>) -> Self {
        match Self::refreshed(source) {
            Self::FullDiskAccess { resume, .. } => Self::FullDiskAccess { resume, checked: true },
            next => next,
        }
    }
}

/// `refreshedSourceProfileDirectory`.
fn refreshed_profile_directory(current: &str, source: &Source) -> String {
    if source.profiles.iter().any(|profile| profile.directory == current) {
        return current.to_owned();
    }
    source.profiles.first().map(|profile| profile.directory.clone()).unwrap_or_default()
}

/// `isRetryableReason`.
fn retryable(reason: Reason) -> bool {
    matches!(
        reason,
        Reason::NeedsKeychainApproval
            | Reason::KeychainItemMissing
            | Reason::KeychainUnavailable
            | Reason::ReadFailed
            | Reason::SessionUnavailable
            | Reason::ProfileNotSaved
    )
}

/// `formatSkippedDomains`.
fn format_skipped_domains(domains: &[String]) -> String {
    match domains {
        [] => String::new(),
        [only] => only.clone(),
        [rest @ .., last] if domains.len() <= 3 => format!("{} and {last}", rest.join(", ")),
        _ => format!("{} and {} more", domains[..3].join(", "), domains.len() - 3),
    }
}

/// `toLocaleString()` for counts: thousands separated by commas.
fn grouped(count: u64) -> String {
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `cookieResultCount`.
fn cookie_count(count: u64) -> String {
    format!("{} {}", grouped(count), if count == 1 { "cookie" } else { "cookies" })
}

/// `cookieCountLabel`: a profile tile's subtitle.
fn cookie_count_label(count: Option<u64>) -> Option<String> {
    match count? {
        0 => Some("no cookies".into()),
        count => Some(cookie_count(count)),
    }
}

/// The Done step's title and description.
fn done_copy(imported: usize, skipped: usize, target_name: &str, environment: &str) -> (String, String) {
    let count = |n: usize| cookie_count(u64::try_from(n).unwrap_or(u64::MAX));
    if imported > 0 {
        let skipped = if skipped > 0 { format!(" {} skipped.", count(skipped)) } else { String::new() };
        (format!("Imported {}", count(imported)), format!("Added to {target_name} for {environment}.{skipped}"))
    } else if skipped > 0 {
        (format!("Skipped {}", count(skipped)), format!("No cookies were imported for {environment}."))
    } else {
        ("No cookies found".into(), format!("There were no cookies to import for {environment}."))
    }
}

pub(in crate::app) struct CookieImportWizard {
    workspace: WeakEntity<Workspace>,
    source: Source,
    environment_name: String,
    step: Step,
    source_profile: String,
    target: TargetSelection,
    target_error: Option<&'static str>,
    /// The id a new profile gets, fixed for the wizard so a retry reuses its storage.
    new_profile_id: String,
    /// Set as the import starts, so a second click can't start another.
    importing: bool,
    /// Full Disk Access as the step's poll last saw it (`usePermissionStatus`).
    full_disk_access: bool,
    poll: Option<Task<()>>,
    work: Option<Task<()>>,
}

impl CookieImportWizard {
    fn new(
        workspace: WeakEntity<Workspace>,
        source: Source,
        environment_name: String,
        (targets, can_create): (Vec<Profile>, bool),
        cx: &mut Context<Self>,
    ) -> Self {
        let mut wizard = Self {
            workspace,
            step: Step::initial(&source),
            source_profile: source.profiles.first().map(|profile| profile.directory.clone()).unwrap_or_default(),
            target: initial_target(can_create, &targets),
            source,
            environment_name,
            target_error: None,
            new_profile_id: format!("profile-{}", uuid::Uuid::new_v4()),
            importing: false,
            full_disk_access: false,
            poll: None,
            work: None,
        };
        wizard.sync_poll(cx);
        wizard
    }

    /// The profiles cookies can go into, and whether a new one may be made, as settings are now.
    fn targets(&self, cx: &App) -> (Vec<Profile>, bool) {
        self.workspace.upgrade().map_or_else(Default::default, |workspace| {
            let settings = &workspace.read(cx).client_settings.values;
            (listed_profiles(settings), !preview::profiles_full(settings))
        })
    }

    fn set_step(&mut self, step: Step, cx: &mut Context<Self>) {
        self.step = step;
        self.sync_poll(cx);
        cx.notify();
    }

    /// Polls Full Disk Access while its step shows; leaving the step (or closing) stops it.
    fn sync_poll(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.step, Step::FullDiskAccess { .. }) {
            self.poll = None;
            return;
        }
        if self.poll.is_some() {
            return;
        }
        self.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                let granted =
                    cx.background_executor().spawn(async { crate::browser_import::full_disk_access_granted() }).await;
                let updated = this.update(cx, |this, cx| {
                    if this.full_disk_access != granted {
                        this.full_disk_access = granted;
                        cx.notify();
                    }
                });
                if updated.is_err() {
                    return;
                }
                cx.background_executor().timer(PERMISSION_POLL).await;
            }
        }));
    }

    /// `runImport`.
    fn run_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.importing {
            return;
        }
        let (targets, _) = self.targets(cx);
        let Some(target) = resolve_target(&self.target, &self.new_profile_id, &targets) else {
            self.target_error = Some(TARGET_MISSING);
            return self.set_step(Step::Configure, cx);
        };
        self.target_error = None;
        self.importing = true;
        self.set_step(Step::Importing, cx);
        let (source, directory) = (self.source.id, self.source_profile.clone());
        let task = self
            .workspace
            .update(cx, |workspace, cx| workspace.import_browser_cookies(source, directory, target, window, cx));
        self.work = Some(cx.spawn(async move |this, cx| {
            let outcome = match task {
                Ok(task) => task.await,
                Err(_) => ImportOutcome::Blocked { reason: Reason::ReadFailed, message: None },
            };
            let _ = this.update(cx, |this, cx| {
                this.importing = false;
                this.set_step(Step::after(outcome), cx);
            });
        }));
    }

    /// `recheckSource`: lists the browsers again, then moves on with the source as it is now.
    fn recheck(&mut self, check: Check, next: fn(Option<&Source>) -> Step, cx: &mut Context<Self>) {
        self.set_step(Step::Checking(check), cx);
        let id = self.source.id;
        let task = self.workspace.update(cx, |workspace, cx| workspace.browser_import_source(id, cx));
        self.work = Some(cx.spawn(async move |this, cx| {
            let refreshed = match task {
                Ok(task) => task.await,
                Err(_) => None,
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(source) = &refreshed {
                    this.source_profile = refreshed_profile_directory(&this.source_profile, source);
                    this.source = source.clone();
                }
                this.set_step(next(refreshed.as_ref()), cx);
            });
        }));
    }
}

/// `listedProfiles`: every profile but Incognito, which cannot keep imported cookies.
fn listed_profiles(settings: &crate::settings::Settings) -> Vec<Profile> {
    preview::resolve_profiles(&preview::user_profiles(settings))
        .into_iter()
        .filter(|profile| profile.kind != ProfileKind::Incognito)
        .collect()
}

fn header(title: String, description: String) -> impl IntoElement {
    DialogHeader::new()
        .p_6()
        .pb_3()
        .pr(px(48.))
        .child(DialogTitle::new().text_xl().child(title))
        .child(DialogDescription::new().child(description))
}

fn panel() -> gpui_kit::Div {
    v_flex().px_6().pt_1().pb_6().gap_4()
}

fn footer(t: &Tokens) -> DialogFooter {
    DialogFooter::new().border_t_1().border_color(t.border).bg(t.muted.opacity(0.72)).px_6().py_4().gap_2()
}

fn close_button(id: &'static str, label: &'static str) -> Button {
    Button::new(id).outline().label(label).on_click(|_, window, cx| window.close_dialog(cx))
}

fn progress(label: &'static str, t: &Tokens) -> impl IntoElement {
    h_flex()
        .gap_3()
        .py_2()
        .child(Spinner::new().color(t.muted_foreground))
        .child(div().text_sm().text_color(t.muted_foreground).child(label))
}

fn section_label(label: &'static str, t: &Tokens) -> impl IntoElement {
    div().text_xs().font_weight(FontWeight::MEDIUM).text_color(t.muted_foreground).child(label)
}

/// `SelectableTile`: a pressable choice with a radio mark.
fn tile(id: SharedString, selected: bool, title: &str, subtitle: Option<String>, t: &Tokens, cx: &App) -> Button {
    let mark = div()
        .size(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(if selected { t.primary } else { t.input })
        .when(selected, |mark| {
            mark.bg(t.primary).child(Icon::new(IconName::Check).size(px(10.)).text_color(t.primary_foreground))
        });
    let text = v_flex()
        .min_w_0()
        .flex_1()
        .child(
            div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(t.foreground).child(title.to_owned()),
        )
        .children(subtitle.map(|subtitle| div().truncate().text_xs().text_color(t.muted_foreground).child(subtitle)));
    let (fill, hover) = if selected {
        (t.primary.opacity(0.08), t.primary.opacity(0.08))
    } else {
        (t.background.opacity(0.), t.muted.opacity(0.4))
    };
    Button::new(id)
        .custom(ButtonCustomVariant::new(cx).color(fill).foreground(t.foreground).hover(hover).active(hover))
        .w_full()
        .h_auto()
        .p_0()
        .rounded(px(8.))
        .child(
            h_flex()
                .w_full()
                .px(px(12.))
                .py(px(8.))
                .gap(px(12.))
                .justify_between()
                .rounded(px(8.))
                .border_1()
                .border_color(if selected { t.primary } else { t.border.opacity(0.6) })
                .child(text)
                .child(mark),
        )
}

impl Render for CookieImportWizard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let name = self.source.name();
        let body: Vec<AnyElement> = match self.step.clone() {
            Step::Quit => vec![
                header(
                    format!("Quit {name} to import"),
                    format!("{name} is open, so its cookies can\u{2019}t be read yet. Quit it, then continue."),
                )
                .into_any_element(),
                footer(&t)
                    .child(close_button("cookie-import-cancel", "Cancel"))
                    .child(
                        Button::new("cookie-import-quit")
                            .primary()
                            .label("I\u{2019}ve quit it")
                            .on_click(cx.listener(|this, _, _, cx| this.recheck(Check::Browser, Step::refreshed, cx))),
                    )
                    .into_any_element(),
            ],
            Step::FullDiskAccess { resume, checked } => self.render_full_disk_access(resume, checked, &t, cx),
            Step::Configure => self.render_configure(&t, cx),
            Step::Checking(check) => vec![
                header(
                    format!("Checking {name}"),
                    match check {
                        Check::FullDiskAccess => "Checking Full Disk Access.",
                        Check::Browser => "Checking whether the browser has closed.",
                    }
                    .into(),
                )
                .into_any_element(),
                panel()
                    .child(progress(
                        if check == Check::FullDiskAccess { "Checking access\u{2026}" } else { "Checking\u{2026}" },
                        &t,
                    ))
                    .into_any_element(),
            ],
            Step::Importing => vec![
                header("Importing cookies".into(), "This may take a moment.".into()).into_any_element(),
                panel().child(progress("Importing\u{2026}", &t)).into_any_element(),
            ],
            Step::Done { imported, skipped, skipped_domains, target_name } => {
                let (title, description) = done_copy(imported, skipped, &target_name, &self.environment_name);
                let skipped = (!skipped_domains.is_empty()).then(|| {
                    panel()
                        .gap_1()
                        .child(section_label("SKIPPED", &t))
                        .child(div().text_sm().text_color(t.foreground).child(format_skipped_domains(&skipped_domains)))
                        .into_any_element()
                });
                [header(title, description).into_any_element()]
                    .into_iter()
                    .chain(skipped)
                    .chain([footer(&t)
                        .child(Button::new("cookie-import-done").primary().label("Done").on_click(|_, window, cx| {
                            window.close_dialog(cx);
                        }))
                        .into_any_element()])
                    .collect()
            }
            Step::Blocked { reason, message } => {
                let retry = (message.is_none() && retryable(reason)).then(|| {
                    Button::new("cookie-import-retry")
                        .primary()
                        .label("Try again")
                        .on_click(cx.listener(|this, _, window, cx| this.run_import(window, cx)))
                });
                vec![
                    header(
                        format!("Couldn\u{2019}t import from {name}"),
                        message.unwrap_or_else(|| reason.copy().to_owned()),
                    )
                    .into_any_element(),
                    footer(&t).child(close_button("cookie-import-close", "Close")).children(retry).into_any_element(),
                ]
            }
        };
        v_flex().w_full().children(body)
    }
}

impl CookieImportWizard {
    fn render_full_disk_access(
        &mut self,
        resume: Resume,
        still_required: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let name = self.source.name();
        let granted = self.full_disk_access;
        let status: AnyElement = if granted {
            h_flex()
                .gap_1()
                .text_xs()
                .text_color(t.success_foreground)
                .child(Icon::new(IconName::CircleCheck).size(px(16.)))
                .child("Allowed")
                .into_any_element()
        } else {
            Button::new("cookie-import-allow")
                .outline()
                .xsmall()
                .label("Allow")
                .on_click(|_, _, cx| cx.open_url(FULL_DISK_ACCESS_SETTINGS))
                .into_any_element()
        };
        let checklist = h_flex()
            .gap_3()
            .rounded(px(8.))
            .border_1()
            .border_color(t.border)
            .px_3()
            .py_2()
            .child(Icon::new(IconName::HardDrive).size(px(32.)).text_color(t.muted_foreground))
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .child(div().font_weight(FontWeight::MEDIUM).text_color(t.foreground).child("Full Disk Access"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(t.muted_foreground)
                            .child(format!("Read {name}'s cookies for this import.")),
                    ),
            )
            .child(status);
        let hint = (!granted).then(|| {
            div().text_xs().text_color(t.muted_foreground).child(if still_required {
                "Access is still required. Quit and reopen T3 Code if you just allowed it, then retry the import."
            } else {
                "If access doesn't update after you allow it, quit and reopen T3 Code, then retry the import."
            })
        });
        vec![
            header(
                format!("Let T3 Code read {name}\u{2019}s cookies"),
                format!(
                    "To import cookies from {name}, T3 Code needs Full Disk Access. Turn it on in System Settings, \
                     then come back to finish the import. You can revoke it again once the import is done."
                ),
            )
            .into_any_element(),
            panel().gap_3().child(checklist).children(hint).into_any_element(),
            footer(t)
                .child(close_button("cookie-import-cancel", "Cancel"))
                .child(Button::new("cookie-import-continue").primary().label("Continue").disabled(!granted).on_click(
                    cx.listener(move |this, _, window, cx| match resume {
                        Resume::Import => this.run_import(window, cx),
                        Resume::Configure => {
                            this.recheck(Check::FullDiskAccess, Step::rechecked_full_disk_access, cx);
                        }
                    }),
                ))
                .into_any_element(),
        ]
    }

    fn render_configure(&mut self, t: &Tokens, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let (targets, can_create) = self.targets(cx);
        let missing =
            matches!(&self.target, TargetSelection::Existing(id) if !targets.iter().any(|profile| &profile.id == id));
        let uncreatable = self.target == TargetSelection::New && !can_create;
        let feedback = self
            .target_error
            .or_else(|| if missing { Some(TARGET_MISSING) } else { uncreatable.then_some(TARGET_UNCREATABLE) });

        let from: Vec<AnyElement> = self
            .source
            .profiles
            .iter()
            .enumerate()
            .map(|(index, profile): (usize, &SourceProfile)| {
                let directory = profile.directory.clone();
                tile(
                    SharedString::from(format!("cookie-import-from-{index}")),
                    self.source_profile == profile.directory,
                    &profile.name,
                    cookie_count_label(profile.cookie_count),
                    t,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.source_profile = directory.clone();
                    cx.notify();
                }))
                .into_any_element()
            })
            .collect();
        let new = can_create.then(|| {
            ("cookie-import-into-new".into(), TargetSelection::New, "New profile", "Created for these cookies")
        });
        let existing = targets.iter().map(|profile| {
            let id = SharedString::from(format!("cookie-import-into-{}", profile.id));
            (id, TargetSelection::Existing(profile.id.clone()), profile.name.as_str(), "Existing profile")
        });
        let into: Vec<AnyElement> = new
            .into_iter()
            .chain(existing)
            .map(|(id, selection, title, subtitle)| {
                tile(id, self.target == selection, title, Some(subtitle.into()), t, cx)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.target = selection.clone();
                        this.target_error = None;
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();

        let wide = t.breakpoint.pick(false, true);
        let arrow = div()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .text_color(t.muted_foreground)
            .child(Icon::new(if wide { IconName::ArrowRight } else { IconName::ArrowDown }).size(px(16.)));
        let column = |label, tiles: Vec<AnyElement>| {
            v_flex().flex_1().min_w_0().gap_2().child(section_label(label, t)).children(tiles)
        };
        let choices = div()
            .flex()
            .when(wide, |el| el.flex_row().items_center().gap_4())
            .when(!wide, |el| el.flex_col().gap_3())
            .child(column("FROM", from))
            .child(arrow)
            .child(column("INTO", into));
        vec![
            header(
                format!("Import from {}", self.source.name()),
                format!("Choose which cookies to import for {}.", self.environment_name),
            )
            .into_any_element(),
            panel()
                .gap_3()
                .child(choices)
                .children(feedback.map(|feedback| div().text_sm().text_color(t.error).child(feedback)))
                .into_any_element(),
            footer(t)
                .child(close_button("cookie-import-cancel", "Cancel"))
                .child(
                    Button::new("cookie-import-run")
                        .primary()
                        .label("Import")
                        .disabled(self.source_profile.is_empty() || missing || uncreatable)
                        .on_click(cx.listener(|this, _, window, cx| this.run_import(window, cx))),
                )
                .into_any_element(),
        ]
    }
}

impl Workspace {
    /// Opens the wizard for `source`, importing for the primary environment, as the web does.
    pub(super) fn open_cookie_import(&mut self, source: Source, window: &mut Window, cx: &mut Context<Self>) {
        let Some(environment) = self.primary_environment().cloned() else { return };
        let label = self.primary_info().environment_label.clone();
        let environment_name = crate::app::palette::environment_option_label(true, &environment, label.as_deref());
        let settings = &self.client_settings.values;
        let targets = (listed_profiles(settings), !preview::profiles_full(settings));
        let workspace = cx.weak_entity();
        let wizard = cx.new(|cx| CookieImportWizard::new(workspace, source, environment_name, targets, cx));
        window.open_dialog(cx, move |dialog, _, cx| {
            let closable = wizard.read(cx).step.closable();
            dialog
                .entrance(DialogEntrance::None)
                .w(px(512.))
                .p_0()
                .overlay_closable(closable)
                .keyboard(closable)
                .close_button(closable)
                .child(wizard.clone())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser_import::SourceId;

    fn source(unavailable: Option<Reason>, directories: &[&str]) -> Source {
        let profiles = directories
            .iter()
            .map(|directory| SourceProfile {
                directory: (*directory).into(),
                name: (*directory).into(),
                cookie_count: None,
            })
            .collect();
        Source { id: SourceId::Chrome, profiles, unavailable }
    }

    #[test]
    fn steps_follow_the_source_and_the_import() {
        assert_eq!(Step::initial(&source(Some(Reason::BrowserRunning), &[])), Step::Quit);
        assert_eq!(
            Step::initial(&source(Some(Reason::NeedsFullDiskAccess), &[])),
            Step::FullDiskAccess { resume: Resume::Configure, checked: false }
        );
        assert_eq!(
            Step::initial(&source(None, &[])),
            Step::Blocked { reason: Reason::UnknownSourceProfile, message: None }
        );
        assert_eq!(Step::initial(&source(None, &["Default"])), Step::Configure);
        assert_eq!(Step::refreshed(None), Step::Blocked { reason: Reason::UnknownSource, message: None });
        assert_eq!(
            Step::rechecked_full_disk_access(Some(&source(Some(Reason::NeedsFullDiskAccess), &[]))),
            Step::FullDiskAccess { resume: Resume::Configure, checked: true },
            "still without access"
        );

        let blocked = |reason| ImportOutcome::Blocked { reason, message: None };
        assert_eq!(Step::after(blocked(Reason::BrowserRunning)), Step::Quit);
        assert_eq!(
            Step::after(blocked(Reason::NeedsFullDiskAccess)),
            Step::FullDiskAccess { resume: Resume::Import, checked: true }
        );
        let policy = ImportOutcome::Blocked { reason: Reason::ReadFailed, message: Some("Acme says no.".into()) };
        assert_eq!(
            Step::after(policy),
            Step::Blocked { reason: Reason::ReadFailed, message: Some("Acme says no.".into()) }
        );
        assert!(!Step::Importing.closable());
        assert!(Step::Configure.closable());
        assert!(retryable(Reason::ProfileNotSaved) && !retryable(Reason::ProfileLimitReached));
    }

    #[test]
    fn targets_resolve_against_the_profiles_as_they_are() {
        let work = Profile { id: "work".into(), name: "Work".into(), kind: ProfileKind::Persistent };
        assert_eq!(initial_target(true, std::slice::from_ref(&work)), TargetSelection::New);
        assert_eq!(initial_target(false, std::slice::from_ref(&work)), TargetSelection::Existing("work".into()));
        assert_eq!(initial_target(false, &[]), TargetSelection::New);
        assert_eq!(
            resolve_target(&TargetSelection::Existing("work".into()), "new-id", std::slice::from_ref(&work)),
            Some(ImportTarget::Existing { profile_id: "work".into(), name: "Work".into() })
        );
        assert_eq!(resolve_target(&TargetSelection::Existing("gone".into()), "new-id", &[work]), None);
        assert_eq!(
            resolve_target(&TargetSelection::New, "new-id", &[]),
            Some(ImportTarget::New { profile_id: "new-id".into() })
        );

        let refreshed = source(None, &["Profile 1", "Profile 2"]);
        assert_eq!(refreshed_profile_directory("Profile 2", &refreshed), "Profile 2");
        assert_eq!(refreshed_profile_directory("Gone", &refreshed), "Profile 1");
        assert_eq!(refreshed_profile_directory("Gone", &source(None, &[])), "");
    }

    #[test]
    fn counts_and_skipped_sites_read_like_the_web() {
        let domains = |n: usize| (0..n).map(|i| format!("s{i}.com")).collect::<Vec<_>>();
        assert_eq!(format_skipped_domains(&domains(1)), "s0.com");
        assert_eq!(format_skipped_domains(&domains(3)), "s0.com, s1.com and s2.com");
        assert_eq!(format_skipped_domains(&domains(5)), "s0.com, s1.com, s2.com and 2 more");
        assert_eq!(cookie_count_label(Some(0)).as_deref(), Some("no cookies"));
        assert_eq!(cookie_count_label(Some(1)).as_deref(), Some("1 cookie"));
        assert_eq!(cookie_count_label(Some(12_345)).as_deref(), Some("12,345 cookies"));
        assert_eq!(cookie_count_label(None), None);
        assert_eq!(
            done_copy(3, 1, "Chrome", "This device"),
            ("Imported 3 cookies".into(), "Added to Chrome for This device. 1 cookie skipped.".into())
        );
        assert_eq!(done_copy(0, 2, "Chrome", "Studio").0, "Skipped 2 cookies");
        assert_eq!(done_copy(0, 0, "Chrome", "Studio").1, "There were no cookies to import for Studio.");
    }
}
