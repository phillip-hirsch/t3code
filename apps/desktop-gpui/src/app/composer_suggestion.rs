//! Settings › Labs › Suggest next prompt: the provider's predicted next message shows in the empty
//! composer, Tab fills it in and Escape dismisses it (`usePromptSuggestion.ts`, `composerCommandKey.ts`).

use std::collections::HashSet;

use gpui_kit::{
    BorderStyle, Bounds, Context, Edges, Font, FontWeight, IntoElement, Keystroke, ParentElement as _, Pixels,
    SharedString, Styled as _, StyledText, Task, TextAlign, TextRun, Window, canvas, div, font, point, px, quad, rems,
    size, transparent_black,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Target, Workspace};
use crate::rpc::StreamEvent;
use crate::theme::Tokens;

/// `OrchestrationV2PromptSuggestion`: the unpersisted prediction for the run that produced it.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Suggestion {
    run_id: String,
    text: SharedString,
}

/// One environment's thread, as the stream and the dismissals key it.
type ThreadKey = (SharedString, String);

#[derive(Default)]
pub(super) struct PromptSuggestions {
    /// The thread `orchestration.subscribePromptSuggestion` watches.
    watching: Option<ThreadKey>,
    /// The stream's latest item, with the thread it came from.
    latest: Option<(ThreadKey, Suggestion)>,
    /// Dismissed suggestions by environment and run: run ids derive from thread ids, which can
    /// repeat across environments. Kept for the session, as the web keeps them across remounts.
    dismissed: HashSet<(SharedString, String)>,
    _task: Option<Task<()>>,
}

impl PromptSuggestions {
    /// Drops the stream with its socket; the next sync subscribes again.
    pub(super) fn disconnect(&mut self) {
        self.watching = None;
        self._task = None;
    }

    /// The suggestion for `thread` while `latest_run` (its id and status) produced it, completed,
    /// and was not dismissed.
    fn offered(&self, thread: &ThreadKey, latest_run: Option<(&str, &str)>) -> Option<&Suggestion> {
        let (key, suggestion) = self.latest.as_ref().filter(|(key, _)| key == thread)?;
        let (run_id, status) = latest_run?;
        (suggestion.run_id == run_id && status == "completed")
            .then_some(suggestion)
            .filter(|s| !self.dismissed.contains(&(key.0.clone(), s.run_id.clone())))
    }

    fn apply(&mut self, thread: &ThreadKey, value: Value) {
        if self.watching.as_ref() != Some(thread) {
            return;
        }
        let next = serde_json::from_value::<Option<Suggestion>>(value).ok().flatten();
        self.latest = next.map(|suggestion| (thread.clone(), suggestion));
    }

    fn dismiss(&mut self, env: &SharedString, run_id: &str) {
        self.dismissed.insert((env.clone(), run_id.to_owned()));
    }
}

/// What a key does to an offered suggestion; the caller checks the composer is empty.
#[derive(Debug, PartialEq)]
enum SuggestionKey {
    Accept,
    Dismiss,
}

fn suggestion_key(keystroke: &Keystroke) -> Option<SuggestionKey> {
    let m = keystroke.modifiers;
    match keystroke.key.as_str() {
        "escape" => Some(SuggestionKey::Dismiss),
        "tab" if !m.shift && !m.alt && !m.control && !m.platform && !m.function => Some(SuggestionKey::Accept),
        _ => None,
    }
}

/// The placeholder with the suggestion's `<Kbd variant="outline">⇥ Tab</Kbd>` as its inline
/// suffix (`placeholderSuffix`). Text cannot hold a box, so the text ends in a non-breaking gap
/// the key's width, which wraps with it, and the key is painted into that gap.
pub(super) fn suggested_placeholder(text: SharedString, t: &Tokens, window: &Window) -> impl IntoElement {
    const KEY: &str = "⇥ Tab";
    const NBSP: &str = "\u{a0}";
    let rem = |value: f32| rems(value).to_pixels(window.rem_size());
    let shape = |text: &str, font: Font, size: Pixels| {
        let run = TextRun {
            len: text.len(),
            font,
            color: t.muted_foreground,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        window.text_system().shape_line(text.to_owned().into(), size, &[run], None)
    };
    // `h-5 min-w-5 px-1.5 rounded border border-b-2 font-medium text-xs`, after `ml-2`.
    let label = shape(KEY, Font { weight: FontWeight::MEDIUM, ..font(t.fonts.sans) }, rem(0.75));
    let (label_height, margin, radius) = (rem(1.), rem(0.5), rem(0.25));
    let key = size((label.width + rem(0.75) + px(2.)).max(rem(1.25)), rem(1.25));
    let prompt = shape(NBSP, font(t.fonts.composer), px(t.fonts.prompt_size));
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a few gap characters")]
    let gap = ((margin + key.width) / prompt.width.max(px(1.))).ceil() as usize;
    let text_end = text.len();
    let styled = StyledText::new(format!("{text} {}", NBSP.repeat(gap)));
    let layout = styled.layout().clone();
    let border = t.foreground.opacity(0.15);
    let paint_key = canvas(
        |_, _, _| {},
        move |_, (), window, cx| {
            let (Some(end), Some(gap)) = (layout.position_for_index(text_end), layout.position_for_index(text_end + 1))
            else {
                return;
            };
            // After the text on its row, or at the start of the row the gap wrapped to.
            let left = if gap.y == end.y { end.x } else { gap.x } + margin;
            // `align-[1px]`: the key's baseline sits 1px above the prompt's.
            let baseline = gap.y + (layout.line_height() - prompt.ascent - prompt.descent) / 2. + prompt.ascent;
            let label_top = baseline - px(1.) - (label_height - label.ascent - label.descent) / 2. - label.ascent;
            // The label's line is centred between the 1px top and 2px bottom borders.
            let top = label_top - px(1.) - (key.height - px(3.) - label_height) / 2.;
            let edges = Edges { top: px(1.), right: px(1.), bottom: px(2.), left: px(1.) };
            let bounds = Bounds::new(point(left, top), key);
            window.paint_quad(quad(bounds, radius, transparent_black(), edges, border, BorderStyle::Solid));
            let origin = point(left + (key.width - label.width) / 2., label_top);
            let _ = label.paint(origin, label_height, TextAlign::Left, None, window, cx);
        },
    );
    div().relative().child(styled).child(paint_key.absolute().size_full())
}

impl Workspace {
    /// The open thread on its environment, with its latest run's id and status (`latestRun`).
    fn suggestion_thread(&self) -> Option<(ThreadKey, Option<(&str, &str)>)> {
        let Some(Target::Thread { env, id }) = &self.target else { return None };
        let shell = self.shell.threads.iter().find(|t| &t.id == id && &t.environment_id == env);
        let run = shell.and_then(|t| {
            let status = if t.status == "idle" { "completed" } else { t.status.as_str() };
            Some((t.latest_run_id.as_deref()?, status))
        });
        Some(((env.clone(), id.clone()), run))
    }

    /// Keeps one `subscribePromptSuggestion` stream on the open thread while the target
    /// environment has the Labs setting on; a draft has no run, and so no suggestion, until it sends.
    pub(super) fn sync_prompt_suggestion(&mut self, cx: &mut Context<Self>) {
        // Older servers decode the missing setting as false, so true also proves the method exists.
        let enabled = self.server_info().settings.bool("promptSuggestionsEnabled");
        let wanted = self.suggestion_thread().filter(|(_, run)| enabled && run.is_some()).map(|(thread, _)| thread);
        if wanted.is_none() {
            self.prompt_suggestions.latest = None;
        }
        if wanted == self.prompt_suggestions.watching {
            return;
        }
        // Without a socket the watch waits, so it subscribes once one connects.
        let Some(client) = self.client().filter(|_| wanted.is_some()) else {
            self.prompt_suggestions.disconnect();
            return;
        };
        let Some(thread) = wanted else { return };
        let suggestions = &mut self.prompt_suggestions;
        if suggestions.latest.as_ref().is_some_and(|(key, _)| key != &thread) {
            suggestions.latest = None;
        }
        suggestions.watching = Some(thread.clone());
        let stream = client.stream("orchestration.subscribePromptSuggestion", json!({"threadId": thread.1}));
        suggestions._task = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = stream.next().await {
                let ended = this.update(cx, |this, cx| match event {
                    StreamEvent::Item(value) => {
                        this.prompt_suggestions.apply(&thread, value);
                        cx.notify();
                        false
                    }
                    StreamEvent::End(_) => true,
                });
                if ended.unwrap_or(true) {
                    break;
                }
            }
        }));
    }

    /// The suggestion, offered only where the default placeholder would show, so it never hides a
    /// pending question, approval or plan; nor after `/clear`, whose fresh session can't see what
    /// it refers to.
    pub(super) fn offered_prompt_suggestion(&self, approval_shown: bool) -> Option<SharedString> {
        let (thread, run) = self.suggestion_thread()?;
        let suggestion = self.prompt_suggestions.offered(&thread, run)?;
        let running = self.open_projection().is_some_and(crate::model::ThreadProjection::is_running);
        let held = running
            || self.sending
            || self.saving_queue_edit()
            || self.fresh_session_arm().is_some()
            || self.has_attachments()
            || !self.context.records.is_empty()
            || approval_shown
            || self.question.is_some()
            || self.server_info().model_choices().is_empty();
        (!held).then(|| suggestion.text.clone())
    }

    /// Tab fills an offered suggestion into the empty composer; Escape dismisses it, so Tab moves
    /// focus again. Open menus take both keys first.
    pub(super) fn prompt_suggestion_key(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(action) = suggestion_key(keystroke) else { return false };
        if !self.composer.read(cx).value().is_empty() {
            return false;
        }
        let approval_shown = !self.shown_approvals().is_empty();
        let Some(text) = self.offered_prompt_suggestion(approval_shown) else { return false };
        match action {
            SuggestionKey::Accept => {
                // An edit, not `set_composer_text`, so the typing guard sees this draft and an
                // approval that lands next is not answered with it.
                self.composer.update(cx, |state, cx| state.insert(text.to_string(), window, cx));
            }
            SuggestionKey::Dismiss => {
                let Some(((env, _), Some((run_id, _)))) = self.suggestion_thread() else { return false };
                let run_id = run_id.to_owned();
                self.prompt_suggestions.dismiss(&env, &run_id);
                cx.notify();
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(env: &str, thread: &str) -> ThreadKey {
        (env.into(), thread.to_owned())
    }

    fn watching(thread: &ThreadKey, item: Value) -> PromptSuggestions {
        let mut suggestions = PromptSuggestions { watching: Some(thread.clone()), ..Default::default() };
        suggestions.apply(thread, item);
        suggestions
    }

    #[test]
    fn a_suggestion_shows_only_for_the_threads_latest_completed_run() {
        let thread = key("env-a", "t1");
        let suggestions = watching(&thread, json!({"runId": "r2", "text": "Run the tests"}));
        let text = |run| suggestions.offered(&thread, run).map(|s| s.text.to_string());
        assert_eq!(text(Some(("r2", "completed"))).as_deref(), Some("Run the tests"));
        // A newer run, one still going, a failed run, or no run at all hides it.
        assert_eq!(text(Some(("r3", "completed"))), None);
        assert_eq!(text(Some(("r2", "running"))), None);
        assert_eq!(text(Some(("r2", "failed"))), None);
        assert_eq!(text(None), None);
        // Another thread, or the same thread id on another environment, never sees it.
        assert!(suggestions.offered(&key("env-a", "t2"), Some(("r2", "completed"))).is_none());
        assert!(suggestions.offered(&key("env-b", "t1"), Some(("r2", "completed"))).is_none());
    }

    #[test]
    fn dismissing_hides_that_run_on_that_environment_only() {
        let thread = key("env-a", "t1");
        let mut suggestions = watching(&thread, json!({"runId": "r2", "text": "Ship it"}));
        suggestions.dismiss(&"env-b".into(), "r2");
        assert!(suggestions.offered(&thread, Some(("r2", "completed"))).is_some());
        suggestions.dismiss(&"env-a".into(), "r2");
        assert!(suggestions.offered(&thread, Some(("r2", "completed"))).is_none());
        // The next run's suggestion shows again.
        suggestions.apply(&thread, json!({"runId": "r3", "text": "Now deploy"}));
        assert!(suggestions.offered(&thread, Some(("r3", "completed"))).is_some());
    }

    #[test]
    fn a_null_item_clears_it_and_a_stale_streams_items_are_ignored() {
        let thread = key("env-a", "t1");
        let mut suggestions = watching(&thread, json!({"runId": "r2", "text": "Ship it"}));
        suggestions.apply(&thread, Value::Null);
        assert!(suggestions.latest.is_none());
        suggestions.apply(&key("env-a", "t0"), json!({"runId": "r9", "text": "Old"}));
        assert!(suggestions.latest.is_none());
    }

    #[test]
    fn plain_tab_accepts_and_escape_dismisses() {
        let parse = |text: &'static str| suggestion_key(&Keystroke::parse(text).unwrap());
        assert_eq!(parse("tab"), Some(SuggestionKey::Accept));
        assert_eq!(parse("escape"), Some(SuggestionKey::Dismiss));
        // Shift+Tab stays the Build/Plan toggle; modified Tabs keep their bindings.
        for text in ["shift-tab", "alt-tab", "ctrl-tab", "cmd-tab", "enter", "a"] {
            assert_eq!(parse(text), None, "{text}");
        }
    }
}
