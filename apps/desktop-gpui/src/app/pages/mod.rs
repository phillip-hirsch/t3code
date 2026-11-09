//! Where the window is: a chat target, a page beside the chat (Pull requests,
//! Usage), or the Settings modal on top, plus back/forward history over them.

mod pull_request_detail;
mod pull_request_surface;
mod pull_requests;
pub(super) mod settings;
mod usage;
mod welcome;

use gpui_kit::{AnyElement, Context, SharedString, Window};

pub(super) use pull_requests::PullRequestsState;
pub(super) use usage::UsageState;
pub(super) use welcome::WelcomeState;

use super::{Target, Workspace};

/// Every setting this client offers, with its section's label, for the palette's search.
pub(super) fn palette_settings() -> impl Iterator<Item = (&'static crate::settings::SearchItem, &'static str)> {
    settings::searchable_settings()
}

/// A page that replaces the chat in the main column (`/_chat/pull-requests`, `/_chat/usage`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Page {
    PullRequests,
    Usage,
    /// An app link being opened (`/_chat/new`); its state is `DesktopShell::link_page`.
    Link,
    /// First-run setup (`/welcome`), shown without the sidebar; see `welcome.rs`.
    Welcome,
}

impl Page {
    pub(super) fn title(self) -> &'static str {
        match self {
            Page::PullRequests => "Pull requests",
            Page::Usage => "Usage",
            Page::Link => "T3 Code",
            // The web's `WorkspacePageHeader` keeps only the titlebar strip here.
            Page::Welcome => "",
        }
    }

    /// Usage opens as a modal over the last main view (`AppRouteModal`), as Settings does.
    pub(super) fn is_modal(self) -> bool {
        self == Page::Usage
    }
}

/// One entry in the back/forward history.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Location {
    Chat(Option<Target>),
    Page(Page),
    /// The Settings modal on one section (its route segment, e.g. "general").
    Settings(SharedString),
}

/// Browser-style history: navigating truncates the forward entries.
pub(super) struct History {
    entries: Vec<Location>,
    index: usize,
}

impl Default for History {
    fn default() -> Self {
        Self { entries: vec![Location::Chat(None)], index: 0 }
    }
}

impl History {
    pub(super) fn push(&mut self, location: Location) {
        if self.entries.get(self.index) == Some(&location) {
            return;
        }
        self.entries.truncate(self.index + 1);
        self.entries.push(location);
        self.index = self.entries.len() - 1;
    }

    pub(super) fn can_go_back(&self) -> bool {
        self.index > 0
    }

    pub(super) fn can_go_forward(&self) -> bool {
        self.index + 1 < self.entries.len()
    }

    fn step(&mut self, delta: isize) -> Option<Location> {
        let index = self.index.checked_add_signed(delta).filter(|i| *i < self.entries.len())?;
        self.index = index;
        Some(self.entries[index].clone())
    }
}

impl Workspace {
    fn location(&self) -> Location {
        if let Some(section) = &self.settings {
            return Location::Settings(section.clone());
        }
        match self.page {
            Some(page) => Location::Page(page),
            None => Location::Chat(self.target.clone()),
        }
    }

    /// Records where the window is now; navigation calls this after it lands.
    pub(super) fn record_location(&mut self) {
        self.sidebar_open.close_sheet();
        let location = self.location();
        self.history.push(location);
    }

    pub(super) fn open_page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.settings = None;
        if self.page != Some(page) {
            self.page_entered(page);
        }
        self.page = Some(page);
        self.record_location();
        self.page_opened(cx);
        self.notify_sidebar(cx);
        cx.notify();
    }

    /// Arriving on a page from elsewhere: the Pull requests page takes focus (`startWorkspaceFocus`).
    fn page_entered(&mut self, page: Page) {
        if page == Page::PullRequests {
            self.pull_requests.focus_on_render = true;
        }
    }

    /// Starts the open page's reads.
    pub(super) fn page_opened(&mut self, cx: &mut Context<Self>) {
        match self.page {
            Some(Page::PullRequests) => self.pull_requests_opened(cx),
            Some(Page::Usage) => self.usage_opened(cx),
            Some(Page::Welcome) => self.welcome_opened(cx),
            Some(Page::Link) | None => {}
        }
    }

    /// Leaves a page for the chat it covered.
    pub(super) fn close_page(&mut self, cx: &mut Context<Self>) {
        self.page = None;
        self.record_location();
        self.notify_sidebar(cx);
        cx.notify();
    }

    pub(super) fn open_settings(&mut self, section: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.focus_settings_on_open();
        let section = section.into();
        // Each visit to Sharing & access starts on the primary, as the web's picker state does.
        if self.settings.as_ref() != Some(&section) {
            self.settings_ui.access_environment = None;
        }
        self.settings = Some(section);
        self.record_location();
        cx.notify();
    }

    pub(super) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            self.flush_settings_field(window, cx);
            self.settings = None;
            self.record_location();
            cx.notify();
        }
    }

    /// The brand's link to `/`: the index route, which opens a draft in the most recently active
    /// project (`IndexDraftLanding`), out of Settings or any page.
    pub(super) fn go_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings = None;
        let recent = crate::inbox::most_recent_project(&self.shell.projects, &self.shell.threads);
        match recent.map(|p| p.id.clone()) {
            Some(project_id) => self.start_draft(project_id, window, cx),
            None => self.close_page(cx),
        }
    }

    pub(super) fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(location) = self.history.step(-1) {
            self.go_to(location, window, cx);
        }
    }

    pub(super) fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(location) = self.history.step(1) {
            self.go_to(location, window, cx);
        }
    }

    /// Shows a history entry. The push that follows each step lands on the
    /// entry itself, so history is left where the step put it.
    fn go_to(&mut self, location: Location, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            self.flush_settings_field(window, cx);
        }
        self.settings = None;
        self.page = None;
        self.settings_ui.access_environment = None;
        match location {
            Location::Chat(Some(Target::Thread { id: thread_id, .. })) => self.select_thread(thread_id, window, cx),
            Location::Chat(Some(Target::Draft { project_id, .. })) => self.start_draft(project_id, window, cx),
            Location::Chat(None) => {}
            Location::Page(page) => {
                self.page_entered(page);
                self.page = Some(page);
                self.page_opened(cx);
            }
            Location::Settings(section) => {
                self.focus_settings_on_open();
                self.settings = Some(section);
            }
        }
        self.notify_sidebar(cx);
        cx.notify();
    }

    /// The main column while a page covers the chat.
    pub(super) fn render_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.page? {
            Page::PullRequests => Some(self.render_pull_requests(window, cx)),
            Page::Usage => None,
            Page::Link => Some(self.render_link_page(window, cx)),
            Page::Welcome => Some(self.render_welcome(window, cx)),
        }
    }

    /// The modal layer of a page that opens over the main view.
    pub(super) fn render_page_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.page.filter(|page| page.is_modal()).map(|_| self.render_usage_modal(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(id: &str) -> Location {
        Location::Chat(Some(Target::Draft { env: "e".into(), project_id: id.into() }))
    }

    #[test]
    fn history_walks_back_and_forward_and_truncates_on_a_new_visit() {
        let mut history = History::default();
        history.push(draft("a"));
        history.push(Location::Page(Page::Usage));
        history.push(Location::Page(Page::Usage));
        assert!(history.can_go_back() && !history.can_go_forward());
        assert_eq!(history.step(-1), Some(draft("a")));
        // Landing on the entry it stepped to records nothing new.
        history.push(draft("a"));
        assert_eq!(history.step(1), Some(Location::Page(Page::Usage)));
        history.step(-1);
        history.push(Location::Settings("general".into()));
        assert!(!history.can_go_forward(), "a new visit drops the forward entries");
        assert_eq!(history.step(-1), Some(draft("a")));
        assert_eq!(history.step(-5), None);
    }
}
