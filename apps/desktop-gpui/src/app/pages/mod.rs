//! Where the window is: a chat target, a page beside the chat (Pull requests,
//! Usage), or the Settings modal on top, plus back/forward history over them.

mod pull_request_detail;
mod pull_requests;
pub(super) mod settings;
mod usage;

use gpui_kit::{AnyElement, Context, SharedString, Window};

pub(super) use pull_requests::PullRequestsState;
pub(super) use usage::UsageState;

use super::{Target, Workspace};

/// Every Settings section as (group heading, route id, label), for search.
pub(super) fn settings_sections() -> impl Iterator<Item = (Option<&'static str>, &'static str, &'static str)> {
    settings::SETTINGS
        .iter()
        .flat_map(|(group, items)| items.iter().map(move |(id, label)| (*group, *id, *label)))
        .filter(|(_, id, _)| settings::section_enabled(id))
}

/// A page that replaces the chat in the main column (`/_chat/pull-requests`, `/_chat/usage`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Page {
    PullRequests,
    Usage,
}

impl Page {
    pub(super) fn title(self) -> &'static str {
        match self {
            Page::PullRequests => "Pull requests",
            Page::Usage => "Usage",
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
        let location = self.location();
        self.history.push(location);
    }

    pub(super) fn open_page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.settings = None;
        self.page = Some(page);
        self.record_location();
        self.page_opened(cx);
        self.notify_sidebar(cx);
        cx.notify();
    }

    /// Starts the open page's reads.
    pub(super) fn page_opened(&mut self, cx: &mut Context<Self>) {
        match self.page {
            Some(Page::PullRequests) => self.pull_requests_opened(cx),
            Some(Page::Usage) => self.usage_opened(cx),
            None => {}
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
        self.settings = Some(section.into());
        self.record_location();
        cx.notify();
    }

    pub(super) fn close_settings(&mut self, cx: &mut Context<Self>) {
        if self.settings.take().is_some() {
            self.record_location();
            cx.notify();
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
        self.settings = None;
        self.page = None;
        match location {
            Location::Chat(Some(Target::Thread(thread_id))) => self.select_thread(thread_id, window, cx),
            Location::Chat(Some(Target::Draft { project_id })) => self.start_draft(project_id, window, cx),
            Location::Chat(None) => {}
            Location::Page(page) => {
                self.page = Some(page);
                self.page_opened(cx);
            }
            Location::Settings(section) => self.settings = Some(section),
        }
        self.notify_sidebar(cx);
        cx.notify();
    }

    /// The main column while a page covers the chat.
    pub(super) fn render_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.page? {
            Page::PullRequests => Some(self.render_pull_requests(window, cx)),
            Page::Usage => None,
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
        Location::Chat(Some(Target::Draft { project_id: id.into() }))
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
