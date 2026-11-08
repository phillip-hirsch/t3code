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

**Hover follows the layout under a still pointer.** 0.3.11 checks a shown tooltip against the
bounds its element had when the tooltip appeared, ignoring anything drawn over it (its own TODO),
so a tooltip stayed up where its button used to be, or over a palette that opened on top of it.
And `on_hover` re-checks only on a mouse move or when its element paints, so a cached view (the
sidebar) under a newly opened overlay kept its hover card. The patch:

- `src/elements/div.rs`: `InteractiveElementState::tooltip_source_hitbox`, the element's hitbox
  this frame, set in `prepaint`; `check_is_hovered_during_prepaint` asks whether that hitbox is
  hovered, so moving away and being covered both hide the tooltip. `on_hover` also listens for
  `HoverRecheckEvent`.
- `src/window.rs`: in a frame with a tooltip to show, `draw_roots` hit-tests this frame's hitboxes
  before the tooltip prepaints, and after every frame it dispatches `HoverRecheckEvent`
  (`on_hover_recheck`) when a hitbox kept from the last frame started or stopped being hovered.
- Tests in `div.rs`: `tooltip_hides_when_its_element_moves_from_under_a_still_mouse`,
  `an_overlay_over_a_still_mouse_hides_tooltips_and_unhovers_cached_views`.

**An element that appears under a still pointer unhovers when the pointer leaves.** 0.3.11 paints
a `hover` style from the hitbox, but its mouse-move listener compares against a flag only mouse
moves set, so an element drawn hovered without a move (a settings nav row back under the pointer
after a search result closes) kept its hover until the pointer crossed it again. The patch, in
`src/elements/div.rs`: painting records the hover state it drew. Test:
`an_element_that_appears_under_a_still_mouse_unhovers_when_the_mouse_leaves`.

**A press hides the pressed element's tooltip until the pointer leaves.** 0.3.11 clears a shown
tooltip on mouse down but not one still waiting out its show delay, so a quick click opened a menu
and the tooltip appeared over it; and the next mouse move over the element scheduled it again.
Browsers (and Base UI's tooltips) close a tooltip when its trigger is pressed and keep it closed
while the pointer stays. The patch, in `src/elements/div.rs`:

- `ActiveTooltip::Pressed`: set by a mouse down on the element (capture phase), which drops a
  pending or shown tooltip; a mouse move with no button held and the pointer off the element ends it.
- Test: `pressing_a_tooltip_trigger_hides_its_tooltip_until_the_pointer_leaves`.

**Keyboard focus shows a tooltip.** Browsers (and Base UI's tooltips) open a trigger's tooltip
when it gets focus from the keyboard (`:focus-visible`), centered over the trigger, and keep it
until focus leaves. 0.3.11 shows tooltips only on hover, placed at the pointer. The patch:

- `src/elements/div.rs`: `InteractiveElementState::tooltip_opened_by_focus`; an element with a
  tooltip and a tracked focus handle that is focused while the last input was the keyboard shows
  its tooltip at once, deferred past the draw so a cached view draws it, and its visibility check
  also holds while that focus lasts; it replaces a press's suppression or a pending hover, survives
  scrolling (`register_tooltip_mouse_handlers` takes the flag) and follows its trigger.
  `show_tooltip` takes the anchor.
- `src/app.rs`: `AnyTooltip::anchor`, the trigger's bounds for a focus-shown tooltip.
- `src/window.rs`: `prepaint_tooltip` centers an anchored tooltip above its trigger (below when
  there is no room) instead of at the pointer; `TooltipBounds::bounds` is crate-visible for tests.
- Test: `a_trigger_focused_from_the_keyboard_shows_its_tooltip_until_it_loses_focus`.

**Tooltips sit 4px off their trigger.** Base UI positions a tooltip from its trigger, not the
pointer: `sideOffset` 4px above it (below when there is no room) and 5px in from the window's
edges (`collisionPadding`). The patch:

- `src/elements/div.rs`: a hovered element's tooltip shows anchored to the element's bounds too
  (`register_tooltip_mouse_handlers` and `handle_tooltip_mouse_move` take the anchor); text-range
  tooltips (`text.rs`) stay at the pointer.
- `src/window.rs`: `prepaint_tooltip` keeps the 4px and 5px; a pointer-placed tooltip keeps 13px
  from the pointer, the 1px plus the 12px margin gpui-component's tooltip used to carry.
- Tests: the focus test above and `tooltip_hides_when_its_element_moves_from_under_a_still_mouse`
  check the placement.

**Text can drop macOS font smoothing.** The web's Font smoothing setting (on by default) sets
`-webkit-font-smoothing: antialiased`, thin grayscale text. 0.3.11 always applies macOS's
color-dependent stroke dilation when the user allows smoothing. The patch, in
`src/text_system.rs`: `TextSystem::set_thin_font_smoothing`, which makes
`glyph_dilation_for_color` return 0. Dilation is part of the glyph cache key, so the next frame
paints with the change. No test: only the macOS text system dilates.

The crate's tests need `--features test-support`; run them from a scratch copy outside this
repository (`cargo test --features test-support --lib`). The copy needs the published crate's
missing pieces: put it two levels below a folder whose `assets/fonts/ibm-plex-sans` and
`assets/fonts/lilex` hold the fonts `svg_renderer.rs` includes (empty files compile, and only the
`svg_renderer` tests then fail), and drop `style.rs`'s `util_macros::perf` attributes.

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
same item. Its item gap is a quarter of the body size; the web's `li + li` margin is 0.25rem, and
the patch uses that. The patch marks every item of a loose list (`list.spread`) as spread. Items of a loose
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

**Soft breaks can render as line breaks.** A user message keeps its typed newlines, as the web's
`lineBreaks` (`remark-breaks`) does after parsing. 0.9.0 always reflows a soft break to a space,
and breaking lines before parsing changed headings and tables. The patch adds
`MarkdownExtensions::line_breaks()` (and `TextView::markdown_line_breaks`), a parser setting: a
soft break inside a text node becomes a newline. Files:

- `src/text/markdown_ext.rs`: the `line_breaks` flag, its builder, and its place in the parser
  configuration.
- `src/text/text_view.rs`: `markdown_line_breaks`.
- `src/text/format/markdown.rs`: the `Node::Text` arm of `parse_paragraph`; one test,
  `test_line_breaks_keep_soft_breaks_and_structure`.

**Selectable text takes highlights.** A tool call's arguments read `key value` with the key muted,
and stay selectable. `src/selectable_text.rs`: `SelectableText::highlights`, passing through to its
`StyledText`.

**Task checkboxes can toggle.** The web's rendered Markdown file and pull request description
check a task off by editing its `[ ]` (`onTaskListChange`). 0.9.0's checkboxes only show their state. The patch adds
`TextView::on_task_toggle`: with a handler, a task's checkbox takes clicks and passes the list
item's source offset and the state it asks for; the app edits the source. Files:

- `src/text/text_view.rs`: `TaskToggleFn`, the `task_toggle_handler` field and builder.
- `src/text/state.rs`: the handler, passed into each frame's `NodeContext`.
- `src/text/node.rs`: `NodeContext::task_toggle_handler`; `render_list_item` and
  `render_list_item_row` make the checkbox clickable; a press on it doesn't start a selection.

gpui-component's `src/text/compat.rs` forwards `on_task_toggle` from its `TextView`.

**Several ranges change in one undo step.** Enter in a numbered list adds an item and renumbers
the ones after it, which may hold chips; one replacement over all of it would drop them. The
patch, in `src/input/base/inline_tokens.rs`: `replace_ranges`, a public front for the crate's
`replace_text_in_ranges` with the atomic intent `replace` uses. It takes the caret the edit
leaves, which `replace_text_in_ranges_selecting` in `src/input/base/state.rs` sets before the step
records its cursors, so redo restores that caret rather than one per range. Test there:
`test_replace_ranges_redo_restores_chosen_selection`.

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

**Menus can take a platform's text metrics.** Electron shows thread menus as native menus, which
on Windows set Segoe UI at 9pt in 28px rows. 0.9.0's `PopupMenu` rows are always the theme's
`text_sm` at 26px. The patch adds `PopupMenu::item_text(family, size, height)`; without it the
menu is unchanged. Files:

- `src/menu/popup_menu.rs`: the `item_text` field and builder, read in `render_item` and `render`.

**Inputs grow below the web's `sm` breakpoint.** The web's `Input` and `Textarea` are 0.25rem taller
with 1rem text in a window narrower than 640px (`h-9 sm:h-8`, `text-base sm:text-sm`). 0.9.0 sizes
an input the same at every width. Files:

- `src/input/input.rs`: `Input::render` adds the 0.25rem and `text_base` below 640px, before the
  caller's style refinement, so a call site's own height or text size still wins.
- `src/sizing.rs`: `Size::input_height_rems`, the height `input_h` sets, in rems so it scales with the
  interface font size.

**A button's text size reaches its label.** 0.9.0's `Button` sets its label row's text size from
its `Size`, so `.text_base()` on the button never showed. `src/button/button.rs` now keeps the
`Size`'s text size only when the caller set none.

**Text views pass task toggles through.** `src/text/compat.rs`: `TextView::on_task_toggle` forwards
to gpui-base's (see its "Task-list checkboxes can be pressed").

**The tooltip looks like the web's.** 0.9.0's `Tooltip` wraps its popup in a 12px margin, the gap
from the pointer; gpui-pre's window now sets the gap (see gpui-pre's "Tooltips sit 4px off their
trigger"), so `src/tooltip.rs` drops the `m_3`. Its popup also takes `TooltipPopup`'s
`rounded-md text-xs leading-snug px-2 py-1` in place of 0.9.0's `text_sm` and `py_0p5`.

**A selected radio shows a dot.** The web's `Radio` marks the selection with a centered dot
(`size-2 sm:size-1.5`) in a `size-4.5 sm:size-4` circle; 0.9.0 draws the checkbox's checkmark at one
size. `src/radio.rs` draws the dot and grows the default size below 640px.

**A radio group moves with the arrow keys.** The web's `RadioGroup` (Base UI) is one tab stop, the
selected radio or else the first enabled one, and the arrow keys move focus and the selection to the
next enabled radio, wrapping; it can carry an accessible name. 0.9.0's `RadioGroup` made every radio a
tab stop and ignored the arrows. `src/radio.rs`: the group owns its radios' focus handles, sets their
tab stops, handles Up/Down/Left/Right (`arrow_target`) and takes `accessibility_label`. Tests:
`arrow_keys_wrap_and_skip_disabled_radios`, and in `tests/integration/controlled_change_callbacks.rs`,
`radio_group_arrows_move_focus_and_selection_from_the_selected_radio`.

**Dialogs take the web's default geometry.** The web's `DialogPopup` centers in the window,
`max-w-lg` (512px) wide with `rounded-2xl` corners; 0.9.0 sat dialogs a tenth of the viewport down,
448px wide with `radius_lg`. `src/dialog/dialog.rs` makes centered and 512px the defaults (an
explicit `margin_top` still places one from the top) and rounds the surface 16px;
`src/dialog/footer.rs` rounds the footer 15px to sit inside the border.

**Tooltips follow keyboard focus.** The web's `TooltipTrigger` opens on keyboard focus
(`useFocus({ visibleOnly })`), closes on blur and dismisses on Escape; 0.9.0's managed tooltips
answer the pointer only. `src/tooltip.rs` adds `managed_tooltip_focusable` (and
`ComponentTooltip::apply_focusable`), which watches the trigger's focus handle at prepaint and
shows the tooltip at once on a keyboard focus, hides it on blur, and hides it on Escape;
`Button`, `Checkbox` and `Radio` pass their focus handles.

**A select's popup is at least its trigger's width.** The web's select and combobox popups set
their own width with `min-w-(--anchor-width) max-w-(--available-width)`; 0.9.0's `Select` with a
`menu_width` used that width alone, so a wide trigger had a narrower popup. In `src/select.rs` a
set width is at least the trigger's and at most the window's less the 8px margins.

**Switches can be mixed.** gpui-base's `Switch` reports only on or off; `src/switch.rs` (in
gpui-base) adds `mixed`, exposed as `aria-checked="mixed"` like the web's mixed `Switch`.

**A dropdown menu can open from a shortcut.** The web opens the composer's Workspace menu with
`composer.workspace` (Mod+Shift+X) and the right panel's "+" menu with `rightPanel.new`. 0.9.0's `DropdownMenuPopover` opens only from its trigger.
`src/menu/dropdown_menu.rs` adds `DropdownMenuPopover::open`, passed to its `Popover` as
`Popover::open` is, so a caller can hold the open state with `on_open_change`.

**Soft breaks can render as line breaks.** `src/text/compat.rs`: `TextView::markdown_line_breaks`,
passing through to gpui-base's (see its "Soft breaks can render as line breaks").

**A thread's timeline reopens where it was left.** 0.9.0's `MessageScrollerState` scrolls only to
a row's top. `src/message_scroller.rs`: `scroll_to_offset` scrolls to a row and an offset into
it, the `logical_scroll_top` it reported.

**A diff says which files are near the viewport, and pins the current file's header.** The web
reads a large diff's patches as their headers come within 600px of the viewport, and its file
headers are sticky. The patch, in `src/diff/state.rs` and `src/diff/mod.rs`:

- `DiffState::files_near_viewport`: the file indices with a row in the viewport or within a margin
  below it, counting rows never laid out at the row height.
- `DiffState::sticky_header` and `render_diff`'s `diff-sticky-header` overlay: once the list
  scrolls past a file's header, that header draws pinned over the top of the body, pushed up by
  the next file's header as it arrives; a canvas records its height for the push.
