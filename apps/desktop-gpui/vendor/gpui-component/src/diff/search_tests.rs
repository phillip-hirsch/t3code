use std::{cell::RefCell, rc::Rc};

use gpui::{
    AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement as _, Render,
    Styled as _, TestAppContext, VisualTestContext, Window, div, px,
};

use super::{
    Diff, DiffEvent, DiffFile, DiffSide, DiffState,
    document::fixture,
    search::{SearchOptions, collect_matches, step},
    state::DisplayRow,
};

const PATCH: &str = "diff --git a/first.txt b/first.txt\n--- a/first.txt\n+++ b/first.txt\n@@ -1,3 +1,3 @@\n one two\n-two\n+TWO\n three\ndiff --git a/second.txt b/second.txt\n--- a/second.txt\n+++ b/second.txt\n@@ -10 +10 @@\n-ten\n+ten two\n";

struct Host {
    state: Entity<DiffState>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(600.))
            .h(px(300.))
            .child(Diff::new(&self.state).size_full())
    }
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
}

fn mount(
    files: Vec<DiffFile>,
    cx: &mut TestAppContext,
) -> (Entity<DiffState>, &mut VisualTestContext) {
    cx.update(crate::init);
    let state = cx.new(|cx| DiffState::new(files, cx));
    let (_, cx) = cx.add_window_view({
        let state = state.clone();
        move |_, _| Host { state }
    });
    for _ in 0..3 {
        draw(cx);
    }
    (state, cx)
}

/// Opens find and types `query` into it.
fn find(state: &Entity<DiffState>, query: &str, cx: &mut VisualTestContext) {
    cx.update(|window, cx| state.update(cx, |state, cx| state.open_search(window, cx)));
    draw(cx);
    cx.simulate_input(query);
    draw(cx);
}

/// The current match as (file, side, line index, byte range).
fn current(
    state: &Entity<DiffState>,
    cx: &mut VisualTestContext,
) -> Option<(usize, DiffSide, usize, std::ops::Range<usize>)> {
    cx.update(|_, cx| {
        let search = state.read(cx).search()?;
        let found = search.matches.get(search.current?)?;
        Some((found.file, found.side, found.line, found.range.clone()))
    })
}

#[test]
fn matches_follow_the_unified_order_and_the_panel_toggles() {
    let files = DiffFile::parse(PATCH).unwrap();
    let find = |query: &str, options: SearchOptions| {
        collect_matches(&files, query, options)
            .into_iter()
            .map(|found| {
                (
                    found.file,
                    found.side,
                    found.line,
                    found.range,
                    found.context,
                )
            })
            .collect::<Vec<_>>()
    };
    let (original, modified) = (DiffSide::Original, DiffSide::Modified);
    // The unchanged line once, then the deletion before its addition, then the next file.
    assert_eq!(
        find("two", SearchOptions::default()),
        [
            (0, modified, 0, 4..7, true),
            (0, original, 1, 0..3, false),
            (0, modified, 1, 0..3, false),
            (1, modified, 0, 4..7, false),
        ]
    );
    let case = SearchOptions {
        case_sensitive: true,
        ..SearchOptions::default()
    };
    assert_eq!(find("TWO", case), [(0, modified, 1, 0..3, false)]);
    let word = SearchOptions {
        whole_word: true,
        ..SearchOptions::default()
    };
    assert!(find("tw", word).is_empty());
    assert_eq!(
        find("ten", word),
        [(1, original, 0, 0..3, false), (1, modified, 0, 0..3, false)]
    );
    let regex = SearchOptions {
        regex: true,
        ..SearchOptions::default()
    };
    assert_eq!(find("^t.o$", regex).len(), 2);
    // JavaScript RegExp syntax: lookaround and backreferences.
    assert_eq!(find("ten(?= )", regex), [(1, modified, 0, 0..3, false)]);
    assert_eq!(find("(?<=ten )two", regex), [(1, modified, 0, 4..7, false)]);
    assert_eq!(find("(?<!ten )two", regex).len(), 3);
    assert_eq!(find("(E)\\1", regex), [(0, modified, 2, 3..5, true)]);
    assert!(
        find(
            "(E)\\1",
            SearchOptions {
                case_sensitive: true,
                ..regex
            }
        )
        .is_empty(),
        "a backreference follows Match Case"
    );
    assert!(
        find("(", regex).is_empty(),
        "an invalid pattern finds nothing"
    );
    assert!(find("(", SearchOptions::default()).is_empty());
    assert!(find("one\ntwo", SearchOptions::default()).is_empty());
    assert!(find("", SearchOptions::default()).is_empty());
}

#[test]
fn regex_classes_boundaries_and_case_follow_javascript() {
    let patch = "diff --git a/u.txt b/u.txt\n--- a/u.txt\n+++ b/u.txt\n@@ -1 +1,3 @@\n-x\n+éfooé foo\n+٣3 a\u{85}b\n+sſS kK\u{212A}\n";
    let files = DiffFile::parse(patch).unwrap();
    let find = |query: &str, options: SearchOptions| {
        collect_matches(&files, query, options)
            .into_iter()
            .map(|found| (found.line, found.range))
            .collect::<Vec<_>>()
    };
    let regex = SearchOptions {
        regex: true,
        ..SearchOptions::default()
    };
    // `\w`, `\d` and `\b` are ASCII; `\s` is JavaScript's set, which leaves out U+0085.
    assert_eq!(find(r"\bfoo\b", regex), [(0, 2..5), (0, 8..11)]);
    assert_eq!(find(r"\Bfoo", regex), []);
    assert_eq!(find(r"\d+", regex), [(1, 2..3)]);
    assert_eq!(find(r"\s", regex), [(0, 7..8), (1, 3..4), (2, 4..5)]);
    assert_eq!(find(r"[\w-]+o", regex), [(0, 2..5), (0, 8..11)]);
    // Ignoring case folds `é` with `É` but never `ſ` with `s` or the Kelvin sign with `k`.
    let any_case = SearchOptions::default();
    assert_eq!(find("s", any_case), [(2, 0..1), (2, 3..4)]);
    assert_eq!(find("ſ", any_case), [(2, 1..3)]);
    assert_eq!(find("k", any_case), [(2, 5..6), (2, 6..7)]);
    assert_eq!(
        find("[a-z]+", regex),
        [
            (0, 0..1),
            (0, 2..5),
            (0, 8..11),
            (1, 4..5),
            (1, 7..8),
            (2, 0..1),
            (2, 3..4),
            (2, 5..7)
        ]
    );
    assert_eq!(find("FOOÉ", any_case), [(0, 2..7)]);
}

#[test]
fn regex_character_classes_follow_javascript() {
    let patch = "diff --git a/c.txt b/c.txt\n--- a/c.txt\n+++ b/c.txt\n@@ -0,0 +1 @@\n+[a]&~-b\n";
    let files = DiffFile::parse(patch).unwrap();
    let find = |query: &str| {
        collect_matches(
            &files,
            query,
            SearchOptions {
                regex: true,
                ..SearchOptions::default()
            },
        )
        .into_iter()
        .map(|found| found.range)
        .collect::<Vec<_>>()
    };
    // Classes do not nest: `[` is a literal inside one, and `]` ends it.
    assert_eq!(find("[[]"), [0..1]);
    assert_eq!(find("[[a]]"), [1..3]);
    assert_eq!(find("[^]+"), [0..7], "`[^]` is any character");
    assert!(find("[]").is_empty(), "`[]` is no character");
    // `&&` and `~~` are no set operations.
    assert_eq!(find("[a&&b]"), [1..2, 3..4, 6..7]);
    assert_eq!(find("[a~~b]"), [1..2, 4..5, 6..7]);
    assert_eq!(find(r"[\w-]+"), [1..2, 5..7]);
}

#[test]
fn steps_wrap_at_both_ends() {
    assert_eq!(step(None, 3, true), Some(0));
    assert_eq!(step(None, 3, false), Some(2));
    assert_eq!(step(Some(2), 3, true), Some(0));
    assert_eq!(step(Some(0), 3, false), Some(2));
    assert_eq!(step(Some(1), 3, false), Some(0));
    assert_eq!(
        step(Some(9), 3, true),
        Some(0),
        "a stale index clamps first"
    );
    assert_eq!(step(None, 0, true), None);
}

#[gpui::test]
fn stepping_reaches_matches_in_folded_files_and_unfolds_them(cx: &mut TestAppContext) {
    let (state, cx) = mount(DiffFile::parse(PATCH).unwrap(), cx);
    let events = Rc::new(RefCell::new(Vec::new()));
    let _subscription = cx.update(|_, cx| {
        let events = events.clone();
        cx.subscribe(&state, move |_, event: &DiffEvent, _| {
            events.borrow_mut().push(event.clone())
        })
    });
    state.update(cx, |state, cx| {
        state.set_file_collapsed("second.txt", true, cx)
    });
    draw(cx);

    find(&state, "two", cx);
    assert_eq!(
        cx.update(|_, cx| state.read(cx).search().unwrap().matches.len()),
        4
    );
    assert_eq!(current(&state, cx), Some((0, DiffSide::Modified, 0, 4..7)));
    assert!(state.read_with(cx, |state, _| state.is_file_collapsed("second.txt")));

    for _ in 0..3 {
        state.update(cx, |state, cx| state.step_search(true, cx));
    }
    assert_eq!(current(&state, cx), Some((1, DiffSide::Modified, 0, 4..7)));
    state.read_with(cx, |state, _| {
        assert!(
            !state.is_file_collapsed("second.txt"),
            "find unfolds the file"
        );
        assert!(
            state.rows().iter().any(|row| matches!(
                row,
                DisplayRow::Code {
                    file: 1,
                    modified: Some(0),
                    ..
                }
            )),
            "and its line has a row"
        );
    });
    assert!(matches!(
        events.borrow().as_slice(),
        [DiffEvent::FileExpanded(path)] if path.as_str() == "second.txt"
    ));

    state.update(cx, |state, cx| state.step_search(true, cx));
    assert_eq!(
        current(&state, cx),
        Some((0, DiffSide::Modified, 0, 4..7)),
        "next wraps"
    );
    state.update(cx, |state, cx| state.step_search(false, cx));
    assert_eq!(
        current(&state, cx),
        Some((1, DiffSide::Modified, 0, 4..7)),
        "previous wraps"
    );

    // Split mode paints the unchanged line's match on both sides.
    cx.update(|_, cx| {
        let paint = state.read(cx).search_paint().unwrap();
        assert_eq!(paint.line(0, DiffSide::Original, 0).count(), 1);
        assert_eq!(paint.line(0, DiffSide::Modified, 0).count(), 1);
        assert_eq!(
            paint.line(1, DiffSide::Modified, 0).collect::<Vec<_>>(),
            [(4..7, true)]
        );
    });
}

#[gpui::test]
fn a_match_in_collapsed_context_expands_it(cx: &mut TestAppContext) {
    let original = (1..=60)
        .map(|line| format!("row{line}\n"))
        .collect::<String>();
    let modified = original.replace("row60\n", "changed\n");
    let (state, cx) = mount(
        vec![fixture::modified("long.txt", &original, &modified)],
        cx,
    );
    let row = |state: &DiffState| {
        state.rows().iter().position(|row| {
            matches!(
                row,
                DisplayRow::Code {
                    modified: Some(9),
                    ..
                }
            )
        })
    };
    assert_eq!(
        state.read_with(cx, |state, _| row(state)),
        None,
        "row 10 starts folded"
    );
    find(&state, "row10", cx);
    assert_eq!(current(&state, cx), Some((0, DiffSide::Modified, 9, 0..5)));
    assert!(state.read_with(cx, |state, _| row(state)).is_some());
}

#[gpui::test]
fn the_current_match_survives_new_files_and_closing_restores_focus(cx: &mut TestAppContext) {
    let (state, cx) = mount(DiffFile::parse(PATCH).unwrap(), cx);
    find(&state, "two", cx);
    state.update(cx, |state, cx| state.step_search(true, cx));
    assert_eq!(current(&state, cx), Some((0, DiffSide::Original, 1, 0..3)));

    // The same patch again, with its files swapped: the match moves with its line.
    state.update(cx, |state, cx| {
        state.set_files(DiffFile::parse(PATCH).unwrap().into_iter().rev(), cx)
    });
    assert_eq!(current(&state, cx), Some((1, DiffSide::Original, 1, 0..3)));
    // A file without the match drops it, and the count stays.
    state.update(cx, |state, cx| {
        state.set_files(DiffFile::parse(PATCH).unwrap().into_iter().skip(1), cx)
    });
    assert_eq!(current(&state, cx), None);
    assert_eq!(
        cx.update(|_, cx| state.read(cx).search().unwrap().matches.len()),
        1
    );

    cx.update(|window, cx| state.update(cx, |state, cx| state.close_search(window, cx)));
    cx.update(|window, cx| {
        assert!(!state.read(cx).is_search_open());
        assert!(state.read(cx).focus_handle(cx).is_focused(window));
    });
}
