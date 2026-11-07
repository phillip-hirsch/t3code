# Vendored crates

Each folder here is a crates.io crate copied byte for byte from
`~/.cargo/registry/src/index.crates.io-*/<name>-<version>` (without Cargo's `.cargo-ok` marker)
and used through `[patch.crates-io]` in `../Cargo.toml`. Keep each identical to the published
crate except for the patches below. `vendor/rustfmt.toml` and the `fmt.ignorePatterns` entries in
the repo's `vite.config.ts` keep formatters off them.

To upgrade gpui-kit: copy the new versions over these folders, re-apply each patch below (or drop
it once upstream has an equivalent), and update the versions here and in `../Cargo.toml`
(`[patch]` comments, and the exact `gpui-pre-windows` dependency, which must match the version
gpui-kit's `gpui-pre-platform` pins). Each patch was committed separately from its unmodified
copy, so `git log -p -- vendor/<name>` shows it.

## gpui-pre 0.3.11

GPUI's core (Zed's `gpui`), which gpui-kit re-exports.

**A tooltip hides once its element moves away from the pointer.** 0.3.11 checks a shown tooltip
against the bounds its element had when the tooltip appeared, so a layout change under a still
pointer (a panel opening, a row reflowing) left the tooltip floating where its button used to be
until the pointer left that old spot. The patch keeps the element's bounds in its element state,
updated every frame, and the tooltip's check reads them. Files:

- `src/elements/div.rs`: `InteractiveElementState::tooltip_source_bounds`, set in `prepaint` and
  read by `check_is_hovered_during_prepaint`; one test,
  `tooltip_hides_when_its_element_moves_from_under_a_still_mouse`.

The crate's tests need `--features test-support`; run them from a scratch copy outside this
repository (`cargo test --features test-support --lib`).

## gpui-pre-windows 0.3.11

GPUI's Windows platform (Zed's `gpui_windows`), which gpui-kit pulls in through `gpui-pre-platform`.

**DirectComposition off without the environment variable.** The preview browser's WebView2 is a
child window, and GPUI's DirectComposition swap chain draws over child windows, so the app must
run GPUI without it. 0.3.11 reads that choice only from `GPUI_DISABLE_DIRECT_COMPOSITION` in the
process environment, and setting a variable in a running process needs `unsafe`
(`std::env::set_var`), which this crate forbids. The app used to restart itself with the variable
set, which cost a second process start on every launch. The patch adds a safe
`gpui_windows::disable_direct_composition()` that `main` calls before the platform is created;
the environment variable still works. Files:

- `src/platform.rs`: the `DIRECT_COMPOSITION_DISABLED` flag, its setter, and the check in `WindowsPlatform::new`.
- `src/gpui_windows.rs`: exports the setter.

## gpui-pre-linux 0.3.11

GPUI's Linux platform (Zed's `gpui_linux`), which gpui-kit pulls in through `gpui-pre-platform`.

**X11 client-side decorations without a compositor.** Electron's Linux window is frameless
(`titleBarStyle: "hidden"` with a `titleBarOverlay`) whether or not a compositor runs, so the app
always draws its own caption buttons, drag region and resize edges. 0.3.11 falls back to
server-side decorations unless a compositor owns `_NET_WM_CM_S0` and the window manager supports
`_GTK_FRAME_EXTENTS` (WSLg's Weston has the first but not the second; bare window managers have
neither), and the app then shows a system titlebar and none of its own controls. The patch keeps
the client-side request and, like GTK's solid CSD, draws no shadow there: the window reports every
edge as tiled, so gpui-component's frame sits flush with no shadow inset, and zero frame extents,
so saved bounds include no shadow. The app keeps its resize edges for that case
(`render_resize_edges` in `src/app/titlebar.rs`). Files:

- `src/linux/x11/window.rs`: `request_decorations`, `window_decorations` and `set_client_inset`.

## gpui-base 0.9.0

**Text decorations on the textarea.** The composer needs atomic inline tokens (its chips),
auto-grow and styled ranges at once. 0.9.0's `TextareaState` has tokens and auto-grow but no
decorations (`type Extras = ()`), and `EditorState` has decorations but drops tokens and cannot
auto-grow. The patch gives `TextareaMode` a `TextareaExtras` with the same text-decoration
collections the editor has: they slide with edits and reset with the text, and the renderer's
existing non-editor path (`highlight_lines` in `base/element.rs`) already paints them in source
bytes, so they compose with tokens unchanged. `TextDecorationCollection` takes the mode as a
type parameter (default `EditorMode`, so editor code is unaffected), and its methods and
`create_decorations_collection` exist for both modes. Files:

- `src/input/base/kind.rs`: `TextareaExtras` and `TextareaMode`'s annotation hooks.
- `src/input/editor/decorations.rs`: the generic handle; its methods move into a macro (`git diff -w` shows the real change).
- `src/input/mod.rs`: exports `TextareaExtras`.
- `src/input/base/state.rs`: one test, `test_textarea_decorations_follow_edits_beside_tokens`.

**Toasts above dialogs.** Dialogs paint in a deferred pass (priority `10 + layer`), but 0.9.0's
`ToastStack` paints in the normal pass, so a toast raised while a dialog is open (an error from
the dialog itself) is hidden under it; the web app's toasts draw above dialogs. The patch paints
the stack deferred at `TOAST_PRIORITY` (50): above dialogs, below popups (100) and tooltips (200).
Files:

- `src/toast.rs`: `TOAST_PRIORITY` and the deferred stack in `ToastStack::render`.

**Inline code wraps at its spaces.** The chat's inline code wraps like browser text
(`white-space: normal`): a span breaks at its spaces and hyphens and keeps the paragraph's width.
0.9.0 hands a span that fits the line to the line wrapper as one element, so the whole span moves
to the next line. The patch hands its spaces and hyphens over as text, so they are break
opportunities, and its words as measured elements, with the code insets on the span's ends.
Files:

- `src/text/inline_flow.rs`: `push_text_wrap_fragments`.

**List spacing like HTML lists.** The chat's markdown CSS gives lists, and the paragraphs of a
loose list's items, a paragraph gap. 0.9.0 spaces every item by the tight-list gap only, puts no
gap around a nested list, and draws a marker for a paragraph that follows a nested list inside the
same item. The patch marks every item of a loose list (`list.spread`) as spread. Items of a loose
list, and an item after one that ends with a nested list or blockquote, sit the paragraph gap below
the previous item instead of the item gap, as the web's collapsed margins do; a nested list keeps
the paragraph gap from what comes before it, and a paragraph after a nested list continues the
item, indented and without a marker.
Files:

- `src/text/format/markdown.rs`: loose lists in `ast_to_node`.
- `src/text/node.rs`: `render_list_item` and the list loop that calls it.

**Plugins can keep a node's children as prose.** A chat link to a file with its own text shows
that text, then the file chip, and the text wraps like the sentence around it (the web's
`whitespace-normal` span). 0.9.0's inline plugins replace the whole claimed node with one atomic
object, so a sentence-long label could not wrap and overflowed the transcript. The patch adds
`MarkdownNode::after_children()`: the converter lays the claimed node's children out as ordinary
inline Markdown (their own text, marks and source ranges) before the object, which keeps the
node's full source range for copying. The object's Markdown already spells out the label, so those
runs are flagged `covered`, never merge with the text around them under a shared mark, and are
skipped when the object is serialized (a table's Copy as Markdown, or a selection that takes in
the object). Objects and images inside the label (an inline-code file chip in a link's text) are
covered too and skipped with it. A selection of the label alone still copies its text and the
chips inside it. Files:

- `src/text/markdown_ext.rs`: the `after_children` flag and builder.
- `src/text/format/markdown.rs`: `parse_paragraph`, `merge_children_with_mark`, `push_merged`;
  one test, `inline_extensions_can_keep_a_nodes_children_as_prose`.
- `src/text/node.rs`: `InlineNode::covered`, `Paragraph::to_markdown`, `selected_markdown`; one
  test, `selected_object_copies_the_label_it_keeps_as_prose_once`.

The crate's own tests need `--features test-support` and `include_str!` a `README.md` four levels
above `src/text`, which this folder lacks: run them from a scratch copy laid out as
`<dir>/crates/gpui-base` with a `<dir>/README.md`.

## gpui-component 0.9.0

**The diff view draws like the web's.** The web renders diffs with Pierre diffs, whose unified
view shows one line number per row (the new line, or the old one for a deletion), marks changed
rows with a 4px bar at the leading edge (dashed for deletions) and separates hunks with an
"N unmodified lines" bar. 0.9.0's Unified mode always draws both number lanes, its bars sit after
the numbers, and its line-info separator reads "N lines not in patch" on a flat strip. The patch
draws one lane per Unified row, moves the bar to the row's edge, and draws the separator as the
web's rounded bar with Pierre's wording. The app picks `DiffHunkSeparator::LineInfo` and
`DiffChangeIndicator::Bars`, Pierre's defaults. Expanding unchanged lines needs the full files,
which the app doesn't load, so the separator has no expand buttons. Files:

- `src/diff/mod.rs`: `code_width`, the lanes and bar in `code_line`, `change_bar`, and the
  `LineInfo` arm of `render_hunk_separator`.
- `locales/ui.yml`: `Diff.HiddenLine` and the English `Diff.HiddenLines`.

**Lazily read files join a diff without resetting it.** The app reads a large diff one file at a
time and shows each batch as it settles. 0.9.0's only way to change the files is `set_files`,
which clears text and line selection, expanded context and horizontal scroll and prepares syntax
emphasis for every file again, so each batch reset the whole view. The patch adds
`DiffState::update_files`: a file that is a clone of one already shown (`DiffFile::is_same`, the
same parsed document) keeps its expansion, conflict choices and prepared emphasis, the rows splice
in place so the viewport stays, and selections stay while they lie in such files. Files:

- `src/diff/state.rs`: `update_files`.
- `src/diff/document.rs`: `DiffFile::is_same`.
- `src/diff/tests.rs`: one test, `updating_files_keeps_the_state_of_files_that_stay`.
